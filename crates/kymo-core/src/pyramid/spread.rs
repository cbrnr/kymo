//! A grid's spread: the peak-to-peak range of each channel per bucket of the pyramid's top
//! level, for a coarse picture of the whole grid (the viewer's minimap).
//!
//! The top level covers the grid in one tile (see [`max_level`](crate::tile::max_level)),
//! so this is one read per channel block, made once. A grid of at most
//! [`TILE_LEN`](crate::tile::TILE_LEN) samples has no levels; its raw samples are taken in
//! buckets of [`LEVEL1_BUCKET`], as level 1 would hold them.

use std::ops::Range;

use super::Pyramid;
use crate::sample::NativeSample;
use crate::tile::{
    CHANNEL_BLOCK, ChannelBlock, LEVEL1_BUCKET, Level, SignalVersion, TileKey, TileSlice,
    compute_raw,
};
use crate::{ChannelId, Grid, Recording, TileError, Timebase};

/// The peak-to-peak range of each channel of a grid per bucket of its top level, and when
/// each bucket's present samples lie.
#[derive(Clone, Debug, PartialEq)]
pub struct Spread {
    channels: Vec<ChannelId>,
    buckets: usize,
    /// Per channel and bucket, channel-major: the largest native value minus the least, over
    /// the bucket's present samples that are not missing; NaN where there are none.
    values: Vec<f32>,
    /// When the buckets' present samples lie, in time order: a bucket in several parts
    /// where a gap, or a break of a segmented time base, lies inside it.
    parts: Vec<Part>,
}

#[derive(Clone, Debug, PartialEq)]
struct Part {
    bucket: u32,
    time: Range<f64>,
}

impl Spread {
    /// The spread of `pyramid`'s grid of `recording`, from its top level; `None` until the
    /// pyramid is complete.
    pub fn read(recording: &Recording, pyramid: &Pyramid) -> Result<Option<Spread>, TileError> {
        if !pyramid.is_complete() {
            return Ok(None);
        }
        let id = pyramid.grid();
        let grid = recording.grid(id);
        let top = pyramid.top();
        let bucket_len = if top == Level::RAW {
            LEVEL1_BUCKET
        } else {
            top.bucket_len()
        };
        let buckets = grid.len().div_ceil(bucket_len) as usize;
        let mut values = vec![f32::NAN; grid.channels().len() * buckets];
        // An empty grid has no tile to read.
        let blocks = if buckets > 0 {
            ChannelBlock::count(grid)
        } else {
            0
        };
        for index in 0..blocks {
            let key = TileKey {
                version: SignalVersion::RAW,
                level: top,
                block: ChannelBlock { grid: id, index },
                time: 0,
            };
            let rows = &mut values[index as usize * CHANNEL_BLOCK * buckets..];
            if top == Level::RAW {
                let tile = compute_raw(recording, key)?;
                let valid = tile.valid();
                match tile.data().as_slice() {
                    TileSlice::I16(d) => raw_ranges(d, tile.len(), valid, rows),
                    TileSlice::I32(d) => raw_ranges(d, tile.len(), valid, rows),
                    TileSlice::F32(d) => raw_ranges(d, tile.len(), valid, rows),
                }
            } else {
                let Some(tile) = pyramid.tile(key) else {
                    return Ok(None);
                };
                match tile.data().as_slice() {
                    TileSlice::I16(d) => level_ranges(d, rows),
                    TileSlice::I32(d) => level_ranges(d, rows),
                    TileSlice::F32(d) => level_ranges(d, rows),
                }
            }
        }
        Ok(Some(Spread {
            channels: grid.channels().to_vec(),
            buckets,
            values,
            parts: parts(grid, bucket_len),
        }))
    }

    /// The grid's channels, in the order of [`columns`](Self::columns)' rows.
    pub fn channels(&self) -> &[ChannelId] {
        &self.channels
    }

    /// The spread on `width` pixel columns over `duration` seconds from the recording
    /// start: per channel (channel-major, `width` values each), the largest range of the
    /// buckets lying in each column, NaN in columns none lies in.
    ///
    /// A bucket's part from `t0` to `t1` lies in the columns from the one holding
    /// `x0 = t0 / duration · width` to the one holding `x1` (a column `x` holds `x..x + 1`),
    /// the column before where `x1` is whole, and at least that of `x0`; within the strip.
    pub fn columns(&self, duration: f64, width: u32) -> Vec<f32> {
        let mut out = vec![f32::NAN; self.channels.len() * width as usize];
        if width == 0 || duration.is_nan() || duration <= 0.0 {
            return out;
        }
        // Runs of buckets that lie in the same columns, in time order: each bucket's parts
        // in turn, a bucket in several parts in each of their runs.
        let mut runs: Vec<(Range<usize>, Range<usize>)> = Vec::new();
        for p in &self.parts {
            let bucket = p.bucket as usize;
            let span = column_span(&p.time, duration, width);
            match runs.last_mut() {
                Some((buckets, last)) if *last == span && buckets.end >= bucket => {
                    buckets.end = bucket + 1;
                }
                _ => runs.push((bucket..bucket + 1, span)),
            }
        }
        for (row, column) in out.chunks_exact_mut(width as usize).enumerate() {
            let values = &self.values[row * self.buckets..(row + 1) * self.buckets];
            for (buckets, span) in &runs {
                // `f32::max` leaves NaN (no samples) out.
                let v = values[buckets.clone()]
                    .iter()
                    .fold(f32::NAN, |m, &v| m.max(v));
                for c in &mut column[span.clone()] {
                    *c = c.max(v);
                }
            }
        }
        out
    }
}

/// The columns from the one holding `time.start` to the one holding `time.end` (see
/// [`Spread::columns`]).
fn column_span(time: &Range<f64>, duration: f64, width: u32) -> Range<usize> {
    let w = f64::from(width);
    let x0 = time.start / duration * w;
    let x1 = time.end / duration * w;
    let last = f64::from(width - 1);
    let first = x0.floor().clamp(0.0, last) as usize;
    let end = (x1.ceil().clamp(0.0, w) as usize).max(first + 1);
    first..end
}

/// The ranges of a level tile's (min, max) pairs into `rows`, a channel's buckets each;
/// an empty bucket (`min > max`) stays NaN.
fn level_ranges<T: NativeSample>(pairs: &[T], rows: &mut [f32]) {
    for (pair, out) in pairs.as_chunks::<2>().0.iter().zip(rows) {
        if pair[0] <= pair[1] {
            *out = (pair[1].to_f64() - pair[0].to_f64()) as f32;
        }
    }
}

/// The ranges of a raw tile's samples, `len` per channel with `valid` present, in buckets of
/// [`LEVEL1_BUCKET`] into `rows`; missing samples left out.
fn raw_ranges<T: NativeSample>(samples: &[T], len: usize, valid: &[Range<u32>], rows: &mut [f32]) {
    if len == 0 {
        return;
    }
    let bucket = LEVEL1_BUCKET as usize;
    let buckets = len.div_ceil(bucket);
    for (row, out) in samples.chunks_exact(len).zip(rows.chunks_mut(buckets)) {
        let mut bounds = vec![(T::EMPTY_MIN, T::EMPTY_MAX); buckets];
        for v in valid {
            let mut i = v.start as usize;
            while i < v.end as usize {
                let k = i / bucket;
                let end = ((k + 1) * bucket).min(v.end as usize);
                let (lo, hi) = &mut bounds[k];
                (*lo, *hi) = T::min_max(&row[i..end], *lo, *hi);
                i = end;
            }
        }
        for ((lo, hi), out) in bounds.into_iter().zip(out) {
            if lo <= hi {
                *out = (hi.to_f64() - lo.to_f64()) as f32;
            }
        }
    }
}

/// The parts of `grid`'s buckets of `bucket_len` samples: their present samples, split
/// where a segmented time base's piece starts, each from its first sample's time to where its
/// last sample's period ends.
fn parts(grid: &Grid, bucket_len: u64) -> Vec<Part> {
    let breaks: &[_] = match grid.timebase() {
        Timebase::Segmented { pieces, .. } => pieces,
        _ => &[],
    };
    let end_of = |last: u64| match grid.regular_run(last) {
        Some(run) => run.time_of(last + 1),
        None => grid.time_of(last + 1),
    };
    let mut parts = Vec::new();
    for present in grid.present_in(0..grid.len()) {
        let mut start = present.start;
        while start < present.end {
            let bucket = start / bucket_len;
            let mut end = ((bucket + 1) * bucket_len).min(present.end);
            let next = breaks.partition_point(|p| p.first <= start);
            if let Some(piece) = breaks.get(next) {
                end = end.min(piece.first);
            }
            let time = grid.time_of(start)..end_of(end - 1);
            if time.start.is_finite() && time.end.is_finite() {
                parts.push(Part {
                    // Below 2^32 buckets: the top level has at most TILE_LEN.
                    bucket: bucket as u32,
                    time,
                });
            }
            start = end;
        }
    }
    parts
}
