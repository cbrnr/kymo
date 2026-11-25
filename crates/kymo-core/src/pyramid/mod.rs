//! The min/max pyramid of one grid, and the block sums for DC removal.
//!
//! Level `L ≥ 1` holds the minimum and maximum of every bucket of `32 · 4^(L-1)` samples,
//! in the grid's native type, over present samples only: gaps never count, and a bucket that
//! lies entirely in a gap keeps the empty sentinel (`min > max`, see [`Sample::EMPTY_MIN`]).
//! Levels are stored as tiles of the fixed grid ([`crate::tile`]), all channels of the grid
//! together, so one read serves a whole screen of channels.
//!
//! Beside the levels sits one sum of native values per channel and block of [`TILE_LEN`]
//! samples (a level-0 tile span), over the block's present samples ([`BlockSum`]): a float
//! channel's missing samples (NaN, and infinities, values beyond float32's range) are left
//! out and counted, so a window mean is over the samples that are there. Missing samples
//! never change a minimum or maximum either, as the renderer skips them.
//!
//! The same pass records the value changes of the grid's stim channels ([`crate::stim`]).
//!
//! [`Sample::EMPTY_MIN`]: crate::Sample::EMPTY_MIN

mod build;
mod mean;
mod sidecar;
mod spread;

use std::collections::HashMap;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

pub use build::PyramidBuilder;
pub use sidecar::{Sidecar, SidecarError};
pub use spread::Spread;

use crate::stim::{Change, StimChanges};
use crate::tile::{
    CHANNEL_BLOCK, ChannelBlock, Level, SignalVersion, TILE_LEN, Tile, TileData, TileKey, max_level,
};
use crate::{ChannelId, ChannelKind, Grid, GridId, ReadError, Recording};

/// Where a pyramid is kept. A builder writes into it while any number of [`Pyramid`]s read
/// from it, possibly on other threads.
///
/// A level tile holds all channels of the grid in grid order, channel-major, with the
/// tile's (min, max) pairs interleaved as in [`TileData`]. The last tile of a level is cut
/// at the grid's end.
///
/// Progress is a watermark in samples: once `publish(done)` returns, every tile and sum
/// block whose span (cut at the grid's end) ends at or before `done` is stored. Readers only
/// ask for those.
pub trait PyramidStorage: Send + Sync {
    /// Stores tile `time` of `level`. Each tile is stored once, before it is published.
    fn put_tile(&self, level: Level, time: u64, data: TileData);
    /// Stores the sums of block `block`, one per channel in grid order.
    fn put_sums(&self, block: u64, sums: &[BlockSum]);
    /// Publishes everything stored for samples `..done`; called with increasing values.
    fn publish(&self, done: u64);
    /// The last published watermark.
    fn done(&self) -> u64;
    /// Rows `rows` of the published tile `time` of `level`, which holds `len` pairs per
    /// channel; `None` if it is not available.
    fn tile_rows(
        &self,
        level: Level,
        time: u64,
        rows: Range<usize>,
        len: usize,
    ) -> Option<TileData>;
    /// The sum of row `row` in the published block `block`; `None` if not available.
    fn sum(&self, block: u64, row: usize) -> Option<BlockSum>;
    /// Appends changes of the stim channel in row `row`: in sample order across calls, and
    /// before the samples they lie in are published.
    fn put_changes(&self, row: usize, changes: &[Change]);
    /// The changes of row `row` stored so far, possibly also past the watermark.
    fn changes(&self, row: usize) -> Vec<Change>;
}

/// A channel's native values summed over the present samples of a block (for DC removal),
/// missing ones left out and counted: a float channel's missing samples (GDF's, XDF's) are
/// NaN, and values beyond float32's range are infinite, which the renderer skips as well.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BlockSum {
    /// The sum of the finite values.
    pub sum: f64,
    /// How many of the values are missing (NaN or infinite).
    pub missing: u64,
}

impl BlockSum {
    /// Both sums together.
    pub fn plus(self, other: BlockSum) -> BlockSum {
        BlockSum {
            sum: self.sum + other.sum,
            missing: self.missing + other.missing,
        }
    }
}

/// Keeps a pyramid in memory.
#[derive(Debug, Default)]
pub struct MemStorage {
    tiles: Mutex<HashMap<(Level, u64), TileData>>,
    sums: Mutex<Vec<Option<Box<[BlockSum]>>>>,
    changes: Mutex<HashMap<usize, Vec<Change>>>,
    done: AtomicU64,
}

impl MemStorage {
    pub fn new() -> Self {
        Self::default()
    }
}

impl PyramidStorage for MemStorage {
    fn put_tile(&self, level: Level, time: u64, data: TileData) {
        let mut tiles = self.tiles.lock().unwrap_or_else(PoisonError::into_inner);
        tiles.insert((level, time), data);
    }

    fn put_sums(&self, block: u64, sums: &[BlockSum]) {
        let mut all = self.sums.lock().unwrap_or_else(PoisonError::into_inner);
        let block = block as usize;
        if all.len() <= block {
            all.resize(block + 1, None);
        }
        all[block] = Some(sums.into());
    }

    fn publish(&self, done: u64) {
        self.done.store(done, Ordering::Release);
    }

    fn done(&self) -> u64 {
        self.done.load(Ordering::Acquire)
    }

    fn tile_rows(
        &self,
        level: Level,
        time: u64,
        rows: Range<usize>,
        len: usize,
    ) -> Option<TileData> {
        let tiles = self.tiles.lock().unwrap_or_else(PoisonError::into_inner);
        let data = tiles.get(&(level, time))?.as_slice();
        let range = rows.start * len * 2..rows.end * len * 2;
        (range.end <= data.len()).then(|| data.to_data(range))
    }

    fn sum(&self, block: u64, row: usize) -> Option<BlockSum> {
        let sums = self.sums.lock().unwrap_or_else(PoisonError::into_inner);
        sums.get(block as usize)?.as_ref()?.get(row).copied()
    }

    fn put_changes(&self, row: usize, changes: &[Change]) {
        let mut all = self.changes.lock().unwrap_or_else(PoisonError::into_inner);
        all.entry(row).or_default().extend_from_slice(changes);
    }

    fn changes(&self, row: usize) -> Vec<Change> {
        let all = self.changes.lock().unwrap_or_else(PoisonError::into_inner);
        all.get(&row).cloned().unwrap_or_default()
    }
}

/// The pyramid of one grid of a recording; a clone reads the same storage.
#[derive(Clone)]
pub struct Pyramid {
    id: GridId,
    grid: Grid,
    top: Level,
    storage: Arc<dyn PyramidStorage>,
}

impl std::fmt::Debug for Pyramid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pyramid")
            .field("id", &self.id)
            .field("top", &self.top)
            .finish_non_exhaustive()
    }
}

impl Pyramid {
    /// Builds the pyramid of `grid` in memory, in one pass.
    pub fn build(recording: &Recording, grid: GridId) -> Result<Pyramid, ReadError> {
        PyramidBuilder::new(recording, grid)?.finish()
    }

    pub(crate) fn new(id: GridId, grid: Grid, storage: Arc<dyn PyramidStorage>) -> Pyramid {
        let top = max_level(grid.len());
        Pyramid {
            id,
            grid,
            top,
            storage,
        }
    }

    pub fn grid(&self) -> GridId {
        self.id
    }

    /// The highest level: one tile covers the whole grid. [`Level::RAW`] for grids of at most
    /// [`TILE_LEN`] samples, which have no pyramid levels.
    pub fn top(&self) -> Level {
        self.top
    }

    /// The tile `key` (level 1 to [`top`](Self::top), raw signal version, this grid), or
    /// `None` if the key is outside the pyramid or the tile is not built yet.
    pub fn tile(&self, key: TileKey) -> Option<Tile> {
        let level = key.level;
        if key.version != SignalVersion::RAW
            || key.block.grid != self.id
            || level == Level::RAW
            || level > self.top
            || key.block.index >= ChannelBlock::count(&self.grid)
        {
            return None;
        }
        let buckets = self.grid.len().div_ceil(level.bucket_len());
        let first = key.time.checked_mul(TILE_LEN)?;
        if first >= buckets || !self.ready(key.sample_range().end) {
            return None;
        }
        let len = (buckets - first).min(TILE_LEN) as usize;
        let start = key.block.index as usize * CHANNEL_BLOCK;
        let channels = key.block.channels(&self.grid).len();
        let rows = start..start + channels;
        let data = self.storage.tile_rows(level, key.time, rows, len)?;
        if data.len() != channels * len * 2 || data.sample_type() != self.grid.sample_type() {
            return None;
        }
        let valid = self.valid(level, first, len as u64);
        Some(Tile::new(key, channels as u32, len as u32, data, valid).with_times(&self.grid))
    }

    /// Whether everything up to sample `end` (cut at the grid's end) is built.
    fn ready(&self, end: u64) -> bool {
        end.min(self.grid.len()) <= self.storage.done()
    }

    /// Samples built so far; the pyramid is complete once this reaches the grid's length.
    pub fn done(&self) -> u64 {
        self.storage.done().min(self.grid.len())
    }

    pub fn is_complete(&self) -> bool {
        self.done() >= self.grid.len()
    }

    /// The value changes of the grid's stim channels (per `recording`'s channel kinds) that
    /// the build has passed, in grid order; see [`crate::stim`] for the events.
    pub fn stim_changes(&self, recording: &Recording) -> Vec<StimChanges> {
        // The watermark first: everything stored before it was published is visible then.
        let done = self.storage.done();
        let is_stim = |c: ChannelId| {
            recording
                .channels()
                .get(c.0 as usize)
                .is_some_and(|ch| ch.kind == ChannelKind::STIM && ch.events)
        };
        self.grid
            .channels()
            .iter()
            .enumerate()
            .filter(|&(_, &c)| is_stim(c))
            .map(|(row, &channel)| {
                let mut changes = self.storage.changes(row);
                changes.truncate(changes.partition_point(|c| c.sample < done));
                StimChanges { channel, changes }
            })
            .collect()
    }

    /// The buckets `first..first + len` of `level` that hold at least one present sample,
    /// relative to `first`.
    fn valid(&self, level: Level, first: u64, len: u64) -> Vec<Range<u32>> {
        let bucket = level.bucket_len();
        let samples = first * bucket..((first + len) * bucket).min(self.grid.len());
        let mut valid: Vec<Range<u32>> = Vec::new();
        for p in self.grid.present_in(samples) {
            // Offsets within one tile fit in u32.
            let a = (p.start / bucket - first) as u32;
            let b = (p.end.div_ceil(bucket) - first) as u32;
            match valid.last_mut() {
                Some(last) if last.end >= a => last.end = last.end.max(b),
                _ => valid.push(a..b),
            }
        }
        valid
    }

    /// Sum of the native values of `channel` over the present samples of block `block`
    /// (samples `block · TILE_LEN ..`), missing ones left out and counted, or `None` if the channel is
    /// not in this grid or the block is not built yet.
    pub fn block_sum(&self, channel: ChannelId, block: u64) -> Option<BlockSum> {
        let row = self.grid.channels().iter().position(|&c| c == channel)?;
        self.row_sum(block, row)
    }

    fn row_sum(&self, block: u64, row: usize) -> Option<BlockSum> {
        let first = block.checked_mul(TILE_LEN)?;
        if first >= self.grid.len() || !self.ready(first.saturating_add(TILE_LEN)) {
            return None;
        }
        self.storage.sum(block, row)
    }
}
