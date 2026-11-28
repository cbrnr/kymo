//! The pyramid sidecar: all pyramids of a recording in one memory-mapped cache file.
//!
//! The file is named after a BLAKE3 hash of each source file's canonical path, size,
//! modification time and first and last MiB, plus this format's version. It holds a header (magic,
//! version, that key, the layout of every grid and a completion flag), then per grid each
//! level's tiles in time order (all channels, channel-major, little-endian (min, max)
//! pairs) and the block sums (per channel and block, the little-endian f64 sum of the finite
//! values and the u64 count of the others; see [`BlockSum`]). A trailer after
//! them, written when the build finishes, holds the value changes of the stim channels
//! (see [`crate::stim`]), which have no size known in advance.
//!
//! A sidecar is built in a temporary file with a unique name, so no other process ever maps
//! a file that is being written. Builders publish a watermark after every step; readers on
//! other threads only read tiles below it, which are never written again. Once every grid is
//! built, the trailer is appended, the completion flag is set and the file is renamed into
//! place. Opening accepts a file only if its whole header matches what the recording
//! expects and its trailer is well-formed and ends the file; anything else (another
//! version, a changed source, truncation, corruption) is rebuilt.

// Writes and reads of the shared mapping go through `SharedMap`'s unsafe methods.
#![allow(unsafe_code)]

mod map;

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::UNIX_EPOCH;

use map::SharedMap;

use super::{BlockSum, Pyramid, PyramidBuilder, PyramidStorage};
use crate::stim::Change;
use crate::tile::{Level, TILE_LEN, TileData, max_level};
use crate::{Grid, GridId, ReadError, Recording, SampleType, lock};

const MAGIC: &[u8; 8] = b"KYMOPYR\0";
/// Bump whenever the layout or the meaning of the contents changes: it is part of the file
/// name, so a sidecar of another version is never opened, just rebuilt.
const VERSION: u32 = 5;
/// Bytes per stored block sum: the sum (f64) and the count of missing samples (u64).
const SUM: usize = 16;
const TRAILER_MAGIC: &[u8; 8] = b"KYMOSTIM";
/// Bytes per stored change: sample (u64), value (i32), padding.
const CHANGE: usize = 16;
/// Bytes before the grid entries.
const FIXED: usize = 64;
/// Bytes per grid entry.
const ENTRY: usize = 64;
/// Offset of the completion flag.
const COMPLETE_AT: usize = 56;
/// Bytes hashed at each end of the source.
const EDGE: u64 = 1 << 20;
const EXTENSION: &str = "kymo-pyramid";

/// A sidecar could not be opened, created, built or finished.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SidecarError {
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("a sidecar of {0} bytes does not fit in the address space")]
    TooLarge(u64),
    #[error("the recording has no source files to key a sidecar by")]
    NoSources,
    #[error(transparent)]
    Read(#[from] ReadError),
}

fn io_error(path: &Path) -> impl FnOnce(io::Error) -> SidecarError + '_ {
    move |source| SidecarError::Io {
        path: path.to_owned(),
        source,
    }
}

/// Where one grid's pyramid lies in the file.
#[derive(Clone, Debug, PartialEq)]
struct GridLayout {
    len: u64,
    channels: usize,
    sample_type: SampleType,
    top: Level,
    /// Offset of each level from 1 to `top`.
    levels: Vec<u64>,
    sums: u64,
    end: u64,
}

fn align(x: u64) -> u64 {
    x.div_ceil(64) * 64
}

/// The layout of all grids, the header length and the file size.
fn layout(grids: &[Grid]) -> (Vec<GridLayout>, usize, u64) {
    let header_len = (FIXED + ENTRY * grids.len()).div_ceil(4096) * 4096;
    let mut cursor = header_len as u64;
    let mut layouts = Vec::with_capacity(grids.len());
    for grid in grids {
        let (len, channels) = (grid.len(), grid.channels().len() as u64);
        let size = grid.sample_type().size() as u64;
        let top = max_level(len);
        let mut levels = Vec::new();
        let mut level = Level::RAW;
        while let Some(up) = level.up().filter(|_| level < top) {
            level = up;
            cursor = align(cursor);
            levels.push(cursor);
            cursor += len.div_ceil(level.bucket_len()) * channels * 2 * size;
        }
        cursor = align(cursor);
        let sums = cursor;
        cursor += len.div_ceil(TILE_LEN) * channels * SUM as u64;
        layouts.push(GridLayout {
            len,
            channels: channels as usize,
            sample_type: grid.sample_type(),
            top,
            levels,
            sums,
            end: cursor,
        });
    }
    (layouts, header_len, cursor)
}

/// The bytes of the envelopes and block sums of every grid of `recording`: its sidecar
/// without header and trailer, about what its pyramids take in memory.
pub fn pyramid_bytes(recording: &Recording) -> u64 {
    let (_, header_len, total) = layout(recording.grids());
    total - header_len as u64
}

/// The header a complete sidecar with this key and layout has.
fn header(key: &blake3::Hash, layouts: &[GridLayout], header_len: usize, total: u64) -> Vec<u8> {
    let mut h = vec![0u8; header_len];
    h[0..8].copy_from_slice(MAGIC);
    h[8..12].copy_from_slice(&VERSION.to_le_bytes());
    h[12..16].copy_from_slice(&(layouts.len() as u32).to_le_bytes());
    h[16..48].copy_from_slice(key.as_bytes());
    h[48..56].copy_from_slice(&total.to_le_bytes());
    h[COMPLETE_AT] = 1;
    for (g, l) in layouts.iter().enumerate() {
        let sample_type: u64 = match l.sample_type {
            SampleType::I16 => 1,
            SampleType::I32 => 2,
            SampleType::F32 => 3,
        };
        let first_level = l.levels.first().copied().unwrap_or(l.sums);
        let fields = [
            l.len,
            l.channels as u64,
            sample_type,
            u64::from(l.top.get()),
            first_level,
            l.sums,
            l.end,
        ];
        let entry = &mut h[FIXED + g * ENTRY..][..ENTRY];
        for (k, field) in fields.iter().enumerate() {
            entry[8 * k..8 * k + 8].copy_from_slice(&field.to_le_bytes());
        }
    }
    h
}

/// The identity of the source files: for each, its canonical path, size, modification time
/// and first and last MiB; and the sidecar format version. A single file is hashed as
/// before sources could be several files, so its sidecars stay valid. On failure, the file
/// that could not be read.
fn source_key(sources: &[PathBuf]) -> Result<blake3::Hash, (PathBuf, io::Error)> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(MAGIC);
    hasher.update(&VERSION.to_le_bytes());
    if let [_, _, ..] = sources {
        hasher.update(b"SOURCES\0");
        hasher.update(&(sources.len() as u64).to_le_bytes());
    }
    for source in sources {
        hash_source(&mut hasher, source).map_err(|e| (source.clone(), e))?;
    }
    Ok(hasher.finalize())
}

fn hash_source(hasher: &mut blake3::Hasher, source: &Path) -> io::Result<()> {
    let path = fs::canonicalize(source)?;
    let mut file = File::open(&path)?;
    let meta = file.metadata()?;
    let size = meta.len();
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    let name = path.as_os_str().as_encoded_bytes();
    hasher.update(&(name.len() as u64).to_le_bytes());
    hasher.update(name);
    hasher.update(&size.to_le_bytes());
    hasher.update(&mtime.to_le_bytes());
    let mut buf = vec![0u8; EDGE.min(size) as usize];
    file.read_exact(&mut buf)?;
    hasher.update(&buf);
    if size > EDGE {
        buf.resize(EDGE as usize, 0);
        file.seek(SeekFrom::Start(size - EDGE))?;
        file.read_exact(&mut buf)?;
        hasher.update(&buf);
    }
    Ok(())
}

/// The pyramids of one recording, kept in a sidecar file: complete and opened from the
/// cache, or still to be built.
///
/// Cheap to clone; clones share the file. [`pyramid`](Self::pyramid) gives readers at any
/// time, also while [`build`](Self::build) or a [`builder`](Self::builder) runs on another
/// thread.
#[derive(Clone)]
pub struct Sidecar {
    inner: Arc<Inner>,
}

struct Inner {
    map: SharedMap,
    layouts: Vec<GridLayout>,
    grids: Vec<Grid>,
    /// Published watermark per grid.
    done: Vec<AtomicU64>,
    /// Whether a builder was handed out per grid.
    claimed: Vec<AtomicBool>,
    /// Bytes of header and grids, before the trailer.
    total: u64,
    /// Per grid, the stim changes found so far, by row.
    changes: Vec<Mutex<Changes>>,
    final_path: PathBuf,
    state: Mutex<State>,
}

struct State {
    path: PathBuf,
    complete: bool,
    /// The temporary file while it is built, open and locked, so clearing the cache (see
    /// [`crate::cache::clear`]) leaves it alone; `None` once complete.
    building: Option<File>,
}

/// The stim changes of one grid, by row.
type Changes = BTreeMap<usize, Vec<Change>>;

impl std::fmt::Debug for Sidecar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sidecar")
            .field("path", &self.path())
            .field("complete", &self.is_complete())
            .finish_non_exhaustive()
    }
}

impl Sidecar {
    /// The default cache directory: `kymo` in the platform's user cache directory (see
    /// [`crate::cache`]).
    pub fn default_dir() -> Option<PathBuf> {
        crate::cache::default_dir()
    }

    /// Opens the complete sidecar of `recording` in `dir`, found by its
    /// [source files](Recording::sources); or, if there is none or it is invalid, creates an
    /// empty one to build.
    pub fn open_or_create(dir: &Path, recording: &Recording) -> Result<Sidecar, SidecarError> {
        if recording.sources().is_empty() {
            return Err(SidecarError::NoSources);
        }
        let key = source_key(recording.sources())
            .map_err(|(path, source)| SidecarError::Io { path, source })?;
        let grids = recording.grids().to_vec();
        let (layouts, header_len, total) = layout(&grids);
        let size = usize::try_from(total).map_err(|_| SidecarError::TooLarge(total))?;
        let expected = header(&key, &layouts, header_len, total);
        let final_path = dir.join(format!("{}.{EXTENSION}", key.to_hex()));

        let (map, path, building, changes) =
            match open_complete(&final_path, &expected, size, &layouts) {
                Some((map, changes)) => (map, final_path.clone(), None, changes),
                None => {
                    let (map, path, file) = create(dir, &key, &expected, total)?;
                    let changes = layouts.iter().map(|_| Changes::new()).collect();
                    (map, path, Some(file), changes)
                }
            };
        let complete = building.is_none();
        let done = layouts
            .iter()
            .map(|l| AtomicU64::new(if complete { l.len } else { 0 }))
            .collect();
        let claimed = layouts.iter().map(|_| AtomicBool::new(complete)).collect();
        Ok(Sidecar {
            inner: Arc::new(Inner {
                map,
                layouts,
                grids,
                done,
                claimed,
                total,
                changes: changes.into_iter().map(Mutex::new).collect(),
                final_path,
                state: Mutex::new(State {
                    path,
                    complete,
                    building,
                }),
            }),
        })
    }

    /// The file: the final name once complete, a temporary one before.
    pub fn path(&self) -> PathBuf {
        self.inner.state().path.clone()
    }

    /// Whether every grid is built and the file is in place for the next open.
    pub fn is_complete(&self) -> bool {
        self.inner.state().complete
    }

    /// The pyramid of `grid`: tiles appear as its build passes them.
    pub fn pyramid(&self, grid: GridId) -> Option<Pyramid> {
        let g = self.inner.grids.get(grid.0 as usize)?;
        let storage = GridStorage {
            inner: Arc::clone(&self.inner),
            grid: grid.0 as usize,
            writer: false,
        };
        Some(Pyramid::new(grid, g.clone(), Arc::new(storage)))
    }

    /// A builder that writes `grid`'s pyramid into the sidecar. `None` if the grid is built
    /// or a builder for it was already handed out, or if `recording`'s grid does not match.
    pub fn builder<'a>(
        &self,
        recording: &'a Recording,
        grid: GridId,
    ) -> Option<PyramidBuilder<'a>> {
        let g = grid.0 as usize;
        let ours = self.inner.grids.get(g)?;
        if recording.grids().get(g) != Some(ours) {
            return None;
        }
        if self.inner.claimed[g].swap(true, Ordering::AcqRel) {
            return None;
        }
        let storage = GridStorage {
            inner: Arc::clone(&self.inner),
            grid: g,
            writer: true,
        };
        let builder = PyramidBuilder::new(recording, grid).ok()?;
        Some(builder.with_storage(Arc::new(storage)))
    }

    /// Builds every grid no builder was handed out for, then [`finish`](Self::finish)es.
    /// Returns whether the sidecar is complete.
    pub fn build(&self, recording: &Recording) -> Result<bool, SidecarError> {
        for g in 0..self.inner.grids.len() {
            if let Some(builder) = self.builder(recording, GridId(g as u32)) {
                builder.finish()?;
            }
        }
        self.finish()
    }

    /// Once every grid is built: marks the file complete, flushes it and moves it into
    /// place. Returns whether the sidecar is complete.
    pub fn finish(&self) -> Result<bool, SidecarError> {
        let inner = &*self.inner;
        let mut state = inner.state();
        if state.complete {
            return Ok(true);
        }
        let built = inner
            .layouts
            .iter()
            .zip(&inner.done)
            .all(|(l, done)| done.load(Ordering::Acquire) >= l.len);
        if !built {
            return Ok(false);
        }
        let changes: Vec<Changes> = inner.changes.iter().map(|c| lock(c).clone()).collect();
        let trailer = encode_trailer(&changes);
        let append = |mut file: &File| -> io::Result<()> {
            // Through the locked handle: on Windows the lock keeps out writes from others.
            file.seek(SeekFrom::Start(trailer_at(inner.total)))?;
            file.write_all(&trailer)?;
            file.sync_data()
        };
        let Some(file) = &state.building else {
            return Ok(false);
        };
        append(file).map_err(io_error(&state.path))?;
        let flush = |map: &SharedMap| map.flush().map_err(io_error(&state.path));
        flush(&inner.map)?;
        // SAFETY: the flag lies in the header, which readers never touch after opening and
        // which only `finish` writes, under the state lock; the source is a local array.
        unsafe { inner.map.write(COMPLETE_AT, &[1]) };
        flush(&inner.map)?;
        fs::rename(&state.path, &inner.final_path).map_err(io_error(&state.path))?;
        state.path = inner.final_path.clone();
        state.complete = true;
        // Unlocked explicitly: on Linux the lock belongs to the open file, which the
        // writable mapping keeps open after the handle is closed.
        if let Some(file) = state.building.take() {
            let _ = file.unlock();
        }
        Ok(true)
    }
}

impl Inner {
    fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }
}

/// Where the trailer starts: after the `total` bytes of header and grids, 64-aligned.
fn trailer_at(total: u64) -> u64 {
    align(total)
}

/// The trailer: magic, body length, then per grid and stim row: grid (u32), row (u32),
/// count (u64) and the changes.
fn encode_trailer(changes: &[Changes]) -> Vec<u8> {
    let mut body = Vec::new();
    for (grid, rows) in changes.iter().enumerate() {
        for (&row, list) in rows {
            body.extend_from_slice(&(grid as u32).to_le_bytes());
            body.extend_from_slice(&(row as u32).to_le_bytes());
            body.extend_from_slice(&(list.len() as u64).to_le_bytes());
            for change in list {
                body.extend_from_slice(&change.sample.to_le_bytes());
                body.extend_from_slice(&change.value.to_le_bytes());
                body.extend_from_slice(&[0; 4]);
            }
        }
    }
    let mut trailer = TRAILER_MAGIC.to_vec();
    trailer.extend_from_slice(&(body.len() as u64).to_le_bytes());
    trailer.extend_from_slice(&body);
    trailer
}

/// Parses a trailer that must fill `bytes` exactly; `None` unless every entry fits the
/// layouts (known grid and row, once each, samples increasing inside the grid).
fn parse_trailer(bytes: &[u8], layouts: &[GridLayout]) -> Option<Vec<Changes>> {
    let u64_at = |b: &[u8], at: usize| -> Option<u64> {
        Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
    };
    let u32_at = |b: &[u8], at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
    };
    if bytes.get(..8)? != TRAILER_MAGIC || u64_at(bytes, 8)? != (bytes.len() - 16) as u64 {
        return None;
    }
    let mut changes: Vec<Changes> = layouts.iter().map(|_| Changes::new()).collect();
    let mut at = 16;
    while at < bytes.len() {
        let grid = u32_at(bytes, at)? as usize;
        let row = u32_at(bytes, at + 4)? as usize;
        let count = usize::try_from(u64_at(bytes, at + 8)?).ok()?;
        at += 16;
        let layout = layouts.get(grid)?;
        let end = count.checked_mul(CHANGE)?.checked_add(at)?;
        if row >= layout.channels || end > bytes.len() || changes[grid].contains_key(&row) {
            return None;
        }
        let mut list = Vec::with_capacity(count);
        let mut previous: Option<u64> = None;
        for entry in bytes[at..end].as_chunks::<CHANGE>().0 {
            let sample = u64_at(entry, 0)?;
            let value = u32_at(entry, 8)? as i32;
            if sample >= layout.len || previous.is_some_and(|p| p >= sample) {
                return None;
            }
            previous = Some(sample);
            list.push(Change { sample, value });
        }
        changes[grid].insert(row, list);
        at = end;
    }
    Some(changes)
}

impl Drop for Inner {
    fn drop(&mut self) {
        let state = self.state.get_mut().unwrap_or_else(PoisonError::into_inner);
        if !state.complete {
            // An unfinished build is useless to the next open.
            let _ = fs::remove_file(&state.path);
        }
    }
}

/// Maps `path` if it is a complete sidecar with exactly the `expected` header, whose
/// pyramids end at `size`, and a valid trailer; returns the trailer's stim changes too.
fn open_complete(
    path: &Path,
    expected: &[u8],
    size: usize,
    layouts: &[GridLayout],
) -> Option<(SharedMap, Vec<Changes>)> {
    let file = File::open(path).ok()?;
    let len = usize::try_from(file.metadata().ok()?.len()).ok()?;
    let trailer = usize::try_from(trailer_at(size as u64)).ok()?;
    if len < trailer + 16 {
        return None;
    }
    let map = SharedMap::read_only(&file).ok()?;
    if map.len() != len {
        return None;
    }
    // SAFETY: the mapping is read-only and nothing in this process writes the file.
    let header = unsafe { map.read(0, expected.len()) };
    if header != expected {
        return None;
    }
    // SAFETY: as above.
    let changes = parse_trailer(unsafe { map.read(trailer, len - trailer) }, layouts)?;
    crate::cache::touch(path);
    Some((map, changes))
}

/// Creates a sidecar file of `total` bytes under a unique temporary name, with the
/// `expected` header but not yet marked complete.
fn create(
    dir: &Path,
    key: &blake3::Hash,
    expected: &[u8],
    total: u64,
) -> Result<(SharedMap, PathBuf, File), SidecarError> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    fs::create_dir_all(dir).map_err(io_error(dir))?;
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = dir.join(format!("{}.{}-{n}.tmp", key.to_hex(), std::process::id()));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(io_error(&path))?;
    // Held until the file is complete: clearing the cache deletes only temporary files it
    // can lock, those whose builder is gone. A file system without locks keeps the build
    // unprotected rather than failing it.
    let _ = file.try_lock();
    let mapped = file
        .set_len(total)
        .and_then(|()| SharedMap::writable(&file));
    let map = match mapped {
        Ok(map) => map,
        Err(e) => {
            let _ = fs::remove_file(&path);
            return Err(io_error(&path)(e));
        }
    };
    let mut header = expected.to_vec();
    header[COMPLETE_AT] = 0;
    // SAFETY: the mapping was just created and is not shared with any other thread yet;
    // `header` is a separate vector.
    unsafe { map.write(0, &header) };
    Ok((map, path, file))
}

/// One grid's part of a sidecar, as a [`PyramidStorage`]. Only the handle given to the one
/// builder of the grid writes.
struct GridStorage {
    inner: Arc<Inner>,
    grid: usize,
    writer: bool,
}

/// Where a stored item lies: byte offset, number of values, and the sample its span ends at.
struct Place {
    offset: usize,
    values: usize,
    end: u64,
}

impl GridStorage {
    fn layout(&self) -> &GridLayout {
        &self.inner.layouts[self.grid]
    }

    fn published(&self) -> u64 {
        self.inner.done[self.grid].load(Ordering::Acquire)
    }

    /// Tile `time` of `level`, and its number of pairs per channel.
    fn tile_place(&self, level: Level, time: u64) -> Option<(Place, usize)> {
        let l = self.layout();
        if level == Level::RAW || level > l.top {
            return None;
        }
        let base = l.levels[usize::from(level.get()) - 1];
        let buckets = l.len.div_ceil(level.bucket_len());
        let first = time.checked_mul(TILE_LEN)?;
        if first >= buckets {
            return None;
        }
        let len = (buckets - first).min(TILE_LEN);
        let size = l.sample_type.size() as u64;
        let place = Place {
            offset: usize::try_from(base + first * l.channels as u64 * 2 * size).ok()?,
            values: usize::try_from(len * l.channels as u64 * 2).ok()?,
            end: time
                .saturating_add(1)
                .saturating_mul(level.tile_samples())
                .min(l.len),
        };
        Some((place, len as usize))
    }

    /// The sums of block `block`.
    fn sums_place(&self, block: u64) -> Option<Place> {
        let l = self.layout();
        if block >= l.len.div_ceil(TILE_LEN) {
            return None;
        }
        let channels = l.channels as u64;
        Some(Place {
            offset: usize::try_from(l.sums + block * channels * SUM as u64).ok()?,
            values: l.channels,
            end: ((block + 1) * TILE_LEN).min(l.len),
        })
    }

    /// Writes `bytes` at `place` unless this handle cannot write or the place is published.
    fn write(&self, place: &Place, bytes: &[u8]) {
        if !self.writer || place.end <= self.published() {
            return;
        }
        // SAFETY: this handle is the only writer of its grid's regions: the grid was claimed
        // for it, and only that builder's `step(&mut self)` calls `put_*`. The place is not
        // published yet, and readers only read published places, which are never written
        // again (checked above), so no other thread accesses these bytes during the write.
        // Callers pass freshly encoded vectors, never bytes of the mapping.
        unsafe { self.inner.map.write(place.offset, bytes) };
    }

    /// The bytes at `place`, if it is published.
    fn read(&self, place: &Place, offset: usize, len: usize) -> Option<&[u8]> {
        if place.end > self.published() {
            return None;
        }
        // SAFETY: the place is published, and published places are never written again
        // (see `write`), so nothing writes these bytes while the slice is alive.
        Some(unsafe { self.inner.map.read(place.offset + offset, len) })
    }
}

impl PyramidStorage for GridStorage {
    fn put_tile(&self, level: Level, time: u64, data: TileData) {
        let Some((place, _)) = self.tile_place(level, time) else {
            return;
        };
        if data.len() != place.values || data.sample_type() != self.layout().sample_type {
            return;
        }
        self.write(&place, &encode(&data));
    }

    fn put_sums(&self, block: u64, sums: &[BlockSum]) {
        let Some(place) = self.sums_place(block) else {
            return;
        };
        if sums.len() != place.values {
            return;
        }
        let bytes: Vec<u8> = sums
            .iter()
            .flat_map(|s| {
                s.sum
                    .to_le_bytes()
                    .into_iter()
                    .chain(s.missing.to_le_bytes())
            })
            .collect();
        self.write(&place, &bytes);
    }

    fn publish(&self, done: u64) {
        if self.writer {
            self.inner.done[self.grid].fetch_max(done, Ordering::Release);
        }
    }

    fn done(&self) -> u64 {
        self.published()
    }

    fn tile_rows(
        &self,
        level: Level,
        time: u64,
        rows: Range<usize>,
        len: usize,
    ) -> Option<TileData> {
        let (place, tile_len) = self.tile_place(level, time)?;
        let l = self.layout();
        if len != tile_len || rows.start > rows.end || rows.end > l.channels {
            return None;
        }
        let size = l.sample_type.size();
        let row_bytes = len * 2 * size;
        let bytes = self.read(&place, rows.start * row_bytes, rows.len() * row_bytes)?;
        Some(decode(l.sample_type, bytes))
    }

    fn sum(&self, block: u64, row: usize) -> Option<BlockSum> {
        let place = self.sums_place(block)?;
        if row >= place.values {
            return None;
        }
        let bytes = self.read(&place, row * SUM, SUM)?;
        let (sum, missing) = bytes.split_at(8);
        Some(BlockSum {
            sum: f64::from_le_bytes(sum.try_into().ok()?),
            missing: u64::from_le_bytes(missing.try_into().ok()?),
        })
    }

    fn put_changes(&self, row: usize, changes: &[Change]) {
        if self.writer && row < self.layout().channels {
            let mut all = lock(&self.inner.changes[self.grid]);
            all.entry(row).or_default().extend_from_slice(changes);
        }
    }

    fn changes(&self, row: usize) -> Vec<Change> {
        let all = lock(&self.inner.changes[self.grid]);
        all.get(&row).cloned().unwrap_or_default()
    }
}

fn encode(data: &TileData) -> Vec<u8> {
    match data {
        TileData::I16(d) => d.iter().flat_map(|x| x.to_le_bytes()).collect(),
        TileData::I32(d) => d.iter().flat_map(|x| x.to_le_bytes()).collect(),
        TileData::F32(d) => d.iter().flat_map(|x| x.to_le_bytes()).collect(),
    }
}

fn decode(sample_type: SampleType, bytes: &[u8]) -> TileData {
    match sample_type {
        SampleType::I16 => TileData::I16(
            bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| i16::from_le_bytes(*b))
                .collect(),
        ),
        SampleType::I32 => TileData::I32(
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| i32::from_le_bytes(*b))
                .collect(),
        ),
        SampleType::F32 => TileData::F32(
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b))
                .collect(),
        ),
    }
}
