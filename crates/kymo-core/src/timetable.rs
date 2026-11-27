//! The times of an irregular grid's samples ([`Timebase::Irregular`](crate::Timebase)):
//! one f64 per sample slot, in seconds since the recording start, never held in memory.
//!
//! A table is written once, when its recording opens, into a temporary file in kymo's cache
//! directory that only this process can reach (removed at once on Unix, deleted on close on
//! Windows), and memory-mapped. Beside each slot's time it holds every pyramid
//! level's bucket start times, so a tile's item times are one contiguous read at any level,
//! and it keeps the time of every [`TILE_LEN`]th slot in memory: a lookup by time searches
//! that index, then one tile's 32 KB of the table, which the page cache serves after the
//! open wrote it. Where no file can be written, the table is kept in memory.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use memmap2::MmapMut;

use crate::tile::{Level, TILE_LEN, max_level};

/// Times that can't form a table.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum TimeTableError {
    #[error("the time {time} of sample {sample} is not a number or comes before the previous one")]
    Unordered { sample: u64, time: f64 },
    #[error("the end {end} of the last sample is not a number or comes before its time")]
    End { end: f64 },
    #[error("{got} times given for {expected} samples")]
    Count { expected: u64, got: u64 },
}

/// Where the bytes live. The mapping stays writable, but nothing writes to it after
/// [`fill`].
enum Storage {
    Memory(Box<[u8]>),
    Mapped(MmapMut),
}

/// The time of every sample slot of a grid, non-decreasing, and the end of the last one.
///
/// Gap slots have times too (the reader chooses them, between their neighbors'), so the
/// table can be searched by time like any other.
pub struct TimeTable {
    storage: Storage,
    len: u64,
    end: f64,
    /// The time of every `TILE_LEN`th slot.
    index: Vec<f64>,
    /// Per level from 1 up, the first entry of its bucket start times.
    levels: Vec<usize>,
}

impl TimeTable {
    /// A table kept in memory, for small grids and tests: `times` of each slot, `end` where
    /// the last one ends (where its value would hold until).
    pub fn new(times: &[f64], end: f64) -> Result<TimeTable, TimeTableError> {
        let len = times.len() as u64;
        let mut writer = TimeTableWriter::new(Storage::memory(len), len);
        for &t in times {
            writer.push(t);
        }
        writer.finish(end)
    }

    /// A table of `len` slots written from `times` into a temporary file in kymo's cache
    /// directory (else the system's temporary directory), or kept in memory where no file
    /// can be written.
    pub fn write(
        len: u64,
        times: impl IntoIterator<Item = f64>,
        end: f64,
    ) -> Result<TimeTable, TimeTableError> {
        let mut writer = Self::writer(len);
        for t in times {
            writer.push(t);
        }
        writer.finish(end)
    }

    /// [`write`](Self::write) into `dir`.
    pub fn write_in(
        dir: &Path,
        len: u64,
        times: impl IntoIterator<Item = f64>,
        end: f64,
    ) -> Result<TimeTable, TimeTableError> {
        let mut writer = Self::writer_in(dir, len);
        for t in times {
            writer.push(t);
        }
        writer.finish(end)
    }

    /// A writer of a table of `len` slots, as [`write`](Self::write) makes it, which takes
    /// the times one by one.
    pub fn writer(len: u64) -> TimeTableWriter {
        let dir = crate::cache::default_dir().unwrap_or_else(std::env::temp_dir);
        Self::writer_in(&dir, len)
    }

    /// A writer of a table of `len` slots kept in memory, which writes nothing to disk.
    pub fn writer_in_memory(len: u64) -> TimeTableWriter {
        TimeTableWriter::new(Storage::memory(len), len)
    }

    /// [`writer`](Self::writer) into `dir`.
    pub fn writer_in(dir: &Path, len: u64) -> TimeTableWriter {
        let storage = match scratch_map(dir, size_bytes(len)) {
            Some(map) => Storage::Mapped(map),
            None => Storage::memory(len),
        };
        TimeTableWriter::new(storage, len)
    }

    /// Number of slots.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// The bytes the table takes, in memory or in its file.
    pub fn size_bytes(&self) -> u64 {
        size_bytes(self.len) as u64
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Where the last slot ends.
    pub fn end(&self) -> f64 {
        self.end
    }

    /// Whether the table lives in a mapped file rather than in memory.
    pub fn is_mapped(&self) -> bool {
        matches!(self.storage, Storage::Mapped(_))
    }

    /// The time of `slot`; [`end`](Self::end) from `len` on. Slots that start a tile
    /// (multiples of [`TILE_LEN`]) come from memory.
    pub fn time(&self, slot: u64) -> f64 {
        if slot >= self.len {
            return self.end;
        }
        if slot.is_multiple_of(TILE_LEN) {
            return self.index[(slot / TILE_LEN) as usize];
        }
        self.entry(slot as usize)
    }

    /// The start time of bucket `k` of `level` (its first slot's time); `end` past the last.
    pub fn bucket_time(&self, level: Level, k: u64) -> f64 {
        let first = k.saturating_mul(level.bucket_len());
        if first >= self.len {
            return self.end;
        }
        match level.get() {
            0 => self.entry(first as usize),
            l => match self.levels.get(usize::from(l) - 1) {
                Some(&at) => self.entry(at + k as usize),
                None => self.entry(first as usize),
            },
        }
    }

    /// The last slot whose time is at or before `t`: 0 before the first slot (and for NaN),
    /// `len` from [`end`](Self::end) on.
    pub fn slot_at(&self, t: f64) -> u64 {
        // NaN compares as neither.
        if self.len == 0 || t.partial_cmp(&self.entry(0)).is_none_or(|o| o.is_lt()) {
            return 0;
        }
        if t >= self.end {
            return self.len;
        }
        // The last tile starting at or before `t`; the slot is in it, as the next starts
        // after `t`.
        let k = self.index.partition_point(|&x| x <= t).max(1) - 1;
        let mut lo = k as u64 * TILE_LEN;
        let mut hi = (lo + TILE_LEN).min(self.len);
        // The first slot after `t` in lo..hi, at least lo + 1.
        lo += 1;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.entry(mid as usize) <= t {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo - 1
    }

    fn bytes(&self) -> &[u8] {
        match &self.storage {
            Storage::Memory(b) => b,
            Storage::Mapped(m) => m,
        }
    }

    fn entry(&self, i: usize) -> f64 {
        let mut b = [0u8; 8];
        b.copy_from_slice(&self.bytes()[8 * i..8 * i + 8]);
        f64::from_le_bytes(b)
    }
}

/// Writes a [`TimeTable`] slot by slot.
pub struct TimeTableWriter {
    storage: Storage,
    len: u64,
    got: u64,
    previous: f64,
    index: Vec<f64>,
    error: Option<TimeTableError>,
}

impl TimeTableWriter {
    fn new(storage: Storage, len: u64) -> TimeTableWriter {
        TimeTableWriter {
            storage,
            len,
            got: 0,
            previous: f64::NEG_INFINITY,
            index: Vec::with_capacity(len.div_ceil(TILE_LEN) as usize),
            error: None,
        }
    }

    /// The next slot's time: finite and not before the previous one's. Times beyond the
    /// table's length are counted, not stored.
    pub fn push(&mut self, t: f64) {
        if self.error.is_some() {
            return;
        }
        if !(t.is_finite() && t >= self.previous) {
            self.error = Some(TimeTableError::Unordered {
                sample: self.got,
                time: t,
            });
            return;
        }
        if self.got < self.len {
            if self.got.is_multiple_of(TILE_LEN) {
                self.index.push(t);
            }
            let i = 8 * self.got as usize;
            self.storage.bytes_mut()[i..i + 8].copy_from_slice(&t.to_le_bytes());
        }
        self.previous = t;
        self.got += 1;
    }

    /// The table, the last slot ending at `end`.
    pub fn finish(mut self, end: f64) -> Result<TimeTable, TimeTableError> {
        if let Some(e) = self.error {
            return Err(e);
        }
        if self.got != self.len {
            return Err(TimeTableError::Count {
                expected: self.len,
                got: self.got,
            });
        }
        if !(end.is_finite() && end >= self.previous) {
            return Err(TimeTableError::End { end });
        }
        let levels = fill_levels(self.storage.bytes_mut(), self.len);
        Ok(TimeTable {
            storage: self.storage,
            len: self.len,
            end,
            index: self.index,
            levels,
        })
    }
}

impl Storage {
    fn memory(len: u64) -> Storage {
        Storage::Memory(vec![0u8; size_bytes(len)].into_boxed_slice())
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        match self {
            Storage::Memory(b) => b,
            Storage::Mapped(m) => m,
        }
    }
}

impl PartialEq for TimeTable {
    fn eq(&self, other: &Self) -> bool {
        let slots = 8 * self.len as usize;
        self.len == other.len
            && self.end.to_bits() == other.end.to_bits()
            && self.bytes()[..slots] == other.bytes()[..slots]
    }
}

impl fmt::Debug for TimeTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TimeTable")
            .field("len", &self.len)
            .field("first", &(self.len > 0).then(|| self.entry(0)))
            .field("end", &self.end)
            .field("mapped", &self.is_mapped())
            .finish()
    }
}

/// Buckets of `level` in a grid of `len` slots.
fn buckets(len: u64, level: Level) -> u64 {
    len.div_ceil(level.bucket_len())
}

/// Levels with bucket start times: 1 up to the top of the grid's pyramid.
fn table_levels(len: u64) -> impl Iterator<Item = Level> {
    (1..=max_level(len).get()).filter_map(Level::new)
}

/// Bytes of a table of `len` slots.
fn size_bytes(len: u64) -> usize {
    let entries = len + table_levels(len).map(|l| buckets(len, l)).sum::<u64>();
    8 * entries as usize
}

/// Writes each level's bucket start times after the slots' times in `bytes`; returns each
/// level's first entry.
fn fill_levels(bytes: &mut [u8], len: u64) -> Vec<usize> {
    let mut levels = Vec::new();
    let mut at = len as usize;
    for level in table_levels(len) {
        levels.push(at);
        let step = level.bucket_len() as usize;
        for k in 0..buckets(len, level) as usize {
            let from = 8 * k * step;
            bytes.copy_within(from..from + 8, 8 * (at + k));
        }
        at += buckets(len, level) as usize;
    }
    levels
}

/// A private writable mapping of `size` bytes in a new file in `dir`, or `None` if there is
/// none to be had. The file can't be opened by anyone else: on Unix it is removed at once
/// (the mapping keeps it alive), on Windows it is deleted when the mapping closes.
#[allow(unsafe_code)]
fn scratch_map(dir: &Path, size: usize) -> Option<MmapMut> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    if size == 0 {
        return None;
    }
    fs::create_dir_all(dir).ok()?;
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = dir.join(format!("times.{}-{n}.tmp", std::process::id()));
    let file = create_private(&path).ok()?;
    let mapped = file.set_len(size as u64).and_then(|()| {
        // SAFETY: the file was just created under a unique name (`create_new`), and is
        // removed below (Unix) or opened for deletion on close (Windows), so no other
        // process can open it, and this one maps it only here. Nothing else changes or
        // truncates it while it is mapped; all access goes through bounds-checked slices.
        unsafe { MmapMut::map_mut(&file) }
    });
    #[cfg(not(windows))]
    let _ = fs::remove_file(&path);
    mapped.ok()
}

#[cfg(windows)]
fn create_private(path: &PathBuf) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
    // Read, write and delete sharing, so the mapping may open the file.
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .share_mode(7)
        .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
        .open(path)
}

#[cfg(not(windows))]
fn create_private(path: &PathBuf) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(times: &[f64], end: f64) -> TimeTable {
        TimeTable::new(times, end).expect("valid times")
    }

    #[test]
    fn slots_are_found_by_time() {
        let t = table(&[1.0, 2.0, 2.0, 5.0], 6.0);
        assert_eq!(t.slot_at(0.5), 0);
        assert_eq!(t.slot_at(f64::NAN), 0);
        assert_eq!(t.slot_at(1.0), 0);
        assert_eq!(t.slot_at(1.999), 0);
        // Equal times: the last of them.
        assert_eq!(t.slot_at(2.0), 2);
        assert_eq!(t.slot_at(4.9), 2);
        assert_eq!(t.slot_at(5.0), 3);
        assert_eq!(t.slot_at(5.99), 3);
        assert_eq!(t.slot_at(6.0), 4);
        assert_eq!(t.slot_at(f64::INFINITY), 4);
        assert_eq!(t.time(3), 5.0);
        assert_eq!(t.time(4), 6.0);
        assert_eq!(t.time(u64::MAX), 6.0);
    }

    #[test]
    fn lookups_match_brute_force_across_tiles_and_levels() {
        // Uneven steps (some zero) over several tiles and levels, in memory and mapped.
        let n = 3 * TILE_LEN as usize * 40 + 17;
        let times: Vec<f64> = (0..n)
            .scan(10.0, |t, i| {
                *t += [0.001, 0.0, 0.0105, 0.37, 0.002][(i * 7 + i / 3) % 5];
                Some(*t)
            })
            .collect();
        let end = times[n - 1] + 0.01;
        let dir = std::env::temp_dir().join("kymo-timetable-test");
        let mapped = TimeTable::write_in(&dir, n as u64, times.iter().copied(), end).unwrap();
        assert!(mapped.is_mapped());
        let memory = table(&times, end);
        assert_eq!(mapped, memory);
        for t in [&mapped, &memory] {
            for level in (0..=max_level(n as u64).get()).filter_map(Level::new) {
                for k in 0..buckets(n as u64, level) + 2 {
                    let slot = k * level.bucket_len();
                    let want = times.get(slot as usize).copied().unwrap_or(end);
                    assert_eq!(t.bucket_time(level, k), want, "level {level:?} bucket {k}");
                }
            }
            // Above the top, buckets start at multiples of their length all the same.
            let above = Level::new(max_level(n as u64).get() + 1).unwrap();
            assert_eq!(t.bucket_time(above, 0), times[0]);
            assert_eq!(t.bucket_time(above, 1), times[above.bucket_len() as usize]);
            for i in (0..n).step_by(97).chain(n - 3..n) {
                for probe in [times[i], times[i] - 1e-9, times[i] + 1e-9] {
                    let want = times.partition_point(|&x| x <= probe).max(1) - 1;
                    let want = if probe < times[0] { 0 } else { want };
                    assert_eq!(t.slot_at(probe), want as u64, "{probe}");
                }
            }
        }
        // No file is left behind on Unix.
        #[cfg(unix)]
        assert!(fs::read_dir(&dir).map(|d| d.count() == 0).unwrap_or(true));
    }

    #[test]
    fn invalid_times_are_refused() {
        assert_eq!(
            TimeTable::new(&[1.0, 0.5], 2.0),
            Err(TimeTableError::Unordered {
                sample: 1,
                time: 0.5
            })
        );
        assert!(matches!(
            TimeTable::new(&[1.0, f64::NAN], 2.0),
            Err(TimeTableError::Unordered { sample: 1, .. })
        ));
        assert_eq!(
            TimeTable::new(&[1.0], 0.5),
            Err(TimeTableError::End { end: 0.5 })
        );
        assert_eq!(
            TimeTable::write_in(&std::env::temp_dir(), 3, [1.0, 2.0], 3.0),
            Err(TimeTableError::Count {
                expected: 3,
                got: 2
            })
        );
    }
}
