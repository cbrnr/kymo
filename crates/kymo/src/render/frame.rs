//! The CPU side of trace drawing: where each tile goes on screen and how its native values
//! become pixel rows. Everything the shader needs is computed here in f64 and handed over
//! as small f32 numbers, so huge offsets and long recordings keep sub-pixel precision.

use std::collections::HashMap;
use std::ops::{Range, RangeInclusive};
use std::sync::Arc;

use kymo_core::tile::{ChannelBlock, Placed, Query, Tile};
use kymo_core::{ChannelId, Grid, Recording, SampleType};

use crate::motion::Slot;
use crate::view::ViewState;

/// Pixels by which a tile's columns reach past its data on either side, so that the
/// shader's f32 positions never put an item in a column the f64 placement left out.
pub const EDGE_TOLERANCE: f32 = 1e-3;

/// Pixels a clipped row's bottom edge is moved up, so that a trace along it stays in the
/// row's last pixel row instead of reaching into the next row's first.
const CLIP_INSET: f64 = 1e-3;

/// One channel's row in the trace area. Positions are physical pixels from the top.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub channel: ChannelId,
    /// Where `baseline` is drawn: the middle of the row.
    pub center: f64,
    /// Height of the row; the loading hatch fills it.
    pub height: f64,
    /// Pixels per physical unit; larger values are drawn higher.
    pub px_per_unit: f64,
    /// The physical value drawn at `center`: the channel's DC value with DC removal on,
    /// otherwise 0.
    pub baseline: f64,
    /// Premultiplied RGBA, written as given.
    pub color: [f32; 4],
    /// Whether the trace stays inside the row, drawn along its edge where it would leave
    /// it (stim channels, whose codes reach far beyond one row at their scale).
    pub clip: bool,
}

/// What the trace area shows, in its own pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Scene {
    /// Seconds since the recording start, left edge to right edge.
    pub time: Range<f64>,
    /// Width and height of the trace area in physical pixels.
    pub size: [u32; 2],
    /// The channels drawn; others are skipped.
    pub rows: Vec<Row>,
    /// Premultiplied RGBA of the hatch over parts that are still loading.
    pub hatch: [f32; 4],
    /// Whether traces are anti-aliased: each pixel drawn with the share of it the trace
    /// covers, the trace widened by half a pixel on either side. Otherwise traces fill
    /// whole pixels.
    pub antialias: bool,
}

impl Scene {
    /// The visible rows of `view` in a trace area of `size` physical pixels, all in
    /// `color`, without DC removal. As in MNE, a channel's amplitude fills half a row:
    /// ±amplitude spans the row.
    pub fn from_view(view: &ViewState, size: [u32; 2], color: [f32; 4]) -> Scene {
        let rows = view.rows() as f64;
        let slots: Vec<_> = view
            .visible_rows()
            .zip(view.visible_channels())
            .map(|(i, &channel)| {
                let slot = Slot {
                    top: (i as f64 - view.top()) / rows,
                    height: 1.0 / rows,
                    opacity: 1.0,
                };
                (channel, slot)
            })
            .collect();
        Scene::from_slots(view, &slots, size, color)
    }

    /// As [`from_view`](Self::from_view), with the rows where `slots` puts them (see
    /// [`RowMotion::rows`](crate::motion::RowMotion::rows)): a row's trace scales with its
    /// height, and its color with its opacity.
    pub fn from_slots(
        view: &ViewState,
        slots: &[(ChannelId, Slot)],
        size: [u32; 2],
        color: [f32; 4],
    ) -> Scene {
        let height = f64::from(size[1]);
        let rows = slots
            .iter()
            .map(|&(channel, slot)| {
                let amplitude = view.channel_amplitude(channel);
                let row_height = slot.height * height;
                Row {
                    channel,
                    center: slot.center() * height,
                    height: row_height,
                    px_per_unit: if amplitude > 0.0 && amplitude.is_finite() {
                        row_height / (2.0 * amplitude)
                    } else {
                        0.0
                    },
                    baseline: 0.0,
                    color: faded(color, slot.opacity),
                    clip: view.kind_of(channel).is_stim(),
                }
            })
            .collect();
        Scene {
            time: view.time(),
            size,
            rows,
            hatch: color.map(|c| c * 0.25),
            antialias: false,
        }
    }
}

/// A premultiplied color at `opacity` of its own.
pub fn faded(color: [f32; 4], opacity: f64) -> [f32; 4] {
    color.map(|c| c * opacity.clamp(0.0, 1.0) as f32)
}

/// A native sample value: integers (int16, int32) widened to i32, or f32.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Native {
    Int(i32),
    Float(f32),
}

impl Native {
    /// The value's bits as the shader reads them.
    pub fn bits(self) -> u32 {
        match self {
            Native::Int(v) => v as u32,
            Native::Float(v) => v.to_bits(),
        }
    }
}

/// How one channel's native values become y positions:
/// `y = offset + (value − reference) · scale`, with the difference taken in the native
/// type (exactly for integers, saturating) before it becomes an f32.
///
/// The reference is the native value nearest the row's baseline, so the f32 part only
/// covers what is on screen: a 1e6 µV offset costs no precision.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coefficients {
    pub reference: Native,
    /// Pixels per native unit; negative, because larger values go up.
    pub scale: f32,
    /// y of `reference`.
    pub offset: f32,
}

impl Coefficients {
    /// For a channel of `sample_type` with physical value = native · `gain` + `offset`,
    /// drawn in `row`.
    pub fn new(sample_type: SampleType, gain: f64, offset: f64, row: &Row) -> Coefficients {
        let usable = gain.is_finite() && gain != 0.0;
        // The native value closest to the baseline (0 without a usable gain).
        let target = if usable {
            (row.baseline - offset) / gain
        } else {
            0.0
        };
        let reference = match sample_type {
            SampleType::F32 => Native::Float(if target.is_finite() {
                target as f32
            } else {
                0.0
            }),
            // Float-to-int casts saturate, and NaN becomes 0.
            _ => Native::Int(target.round() as i32),
        };
        let at_reference = match reference {
            Native::Int(r) => f64::from(r),
            Native::Float(r) => f64::from(r),
        };
        let gain = if usable { gain } else { 0.0 };
        // Physical value at the reference, relative to the baseline: within half a native
        // step for integers.
        let rest = at_reference * gain + offset - row.baseline;
        Coefficients {
            reference,
            scale: (-gain * row.px_per_unit) as f32,
            offset: (row.center - rest * row.px_per_unit) as f32,
        }
    }

    /// The y position of `value` as the shader computes it, in f32; `None` for values the
    /// shader skips (NaN and infinities) or of the wrong type.
    pub fn y(&self, value: Native) -> Option<f32> {
        let delta = match (value, self.reference) {
            (Native::Int(v), Native::Int(r)) => v.saturating_sub(r) as f32,
            (Native::Float(v), Native::Float(r)) if v.is_finite() => v - r,
            _ => return None,
        };
        Some(self.offset + delta * self.scale)
    }
}

/// One channel of a tile to draw.
#[derive(Clone, Debug, PartialEq)]
pub struct ChannelDraw {
    /// The channel's row in the tile (and its neighbors: the same channel block).
    pub row: u32,
    pub coefficients: Coefficients,
    /// Premultiplied RGBA.
    pub color: [f32; 4],
    /// The y range (top, bottom) the trace is clamped to, if its row clips it.
    pub clip: Option<[f32; 2]>,
}

/// Where the items of one tile lie on screen, and which of them may be read.
///
/// Items are samples at level 0 and buckets above. Item `i` starts at
/// `anchor_x + (i − anchor) · item_width` pixels, a bucket's envelope reaching to the start
/// of the next one; for a grid with an irregular time base at `xs[i − first]`, its end at
/// the next entry.
#[derive(Clone, Debug)]
pub struct Part {
    pub tile: Arc<Tile>,
    /// The items the query placed this tile for (for an irregular time base, those of them
    /// near the screen).
    pub items: RangeInclusive<u32>,
    /// An item near the screen (or near the seam, for a neighbor), for precise positions.
    pub anchor: u32,
    pub anchor_x: f32,
    /// Pixels per item.
    pub item_width: f32,
    /// 1 / `item_width`, computed in f64.
    pub inverse_width: f32,
    /// For an irregular time base, where each of `items` starts and, last, where the last
    /// one ends, in pixels (from the tile's item times, computed in f64).
    pub xs: Option<Vec<f32>>,
    /// Whether a sample's value holds until the next sample's start (drawn as steps: each
    /// sample an envelope of its one value).
    pub hold: bool,
}

/// A tile placed on screen, with the tiles placed right before and after it in the same
/// channel block, if any.
///
/// The shader reads the three as one run of items, so lines and envelopes run on across
/// tile boundaries, and anti-aliasing sees the trace on either side of a pixel whichever
/// tile it comes from. Neighbors may be at another level (a coarser tile standing in for a
/// missing one); a level-0 line doesn't run on into an envelope, but an envelope runs on
/// into whatever follows.
#[derive(Clone, Debug)]
pub struct TileDraw {
    pub part: Part,
    pub prev: Option<Part>,
    pub next: Option<Part>,
    /// Pixel columns this draw fills. Neighboring draws of a block never share one: where
    /// a tile meets the next, the column holding the next tile's first item is this draw's
    /// ([`seam`](Self::seam)), so no pixel is drawn twice (which anti-aliasing would show
    /// as a darker column).
    pub columns: Range<u32>,
    /// The x range the tile's own items cover: from the first to the last, or to the next
    /// tile's first if there is a next tile.
    pub domain: [f32; 2],
    /// The column holding the next tile's first item, if there is a next tile.
    pub seam: Option<u32>,
    pub channels: Vec<ChannelDraw>,
}

/// A hatched rectangle over samples that have no tile yet: x0, y0, x1, y1 in pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Hatch {
    pub rect: [f32; 4],
    pub color: [f32; 4],
}

/// Everything the renderer draws in one frame.
#[derive(Clone, Debug, Default)]
pub struct Frame {
    /// The trace area in physical pixels.
    pub size: [u32; 2],
    pub draws: Vec<TileDraw>,
    pub hatches: Vec<Hatch>,
    /// See [`Scene::antialias`].
    pub antialias: bool,
}

/// Seconds to pixels, and the columns there are.
struct Pixels {
    start: f64,
    per_second: f64,
    /// Columns of the trace area.
    width: u32,
    /// Columns an anti-aliased trace reaches past its samples: 1, or 0 without
    /// anti-aliasing.
    reach: f64,
}

impl Pixels {
    fn x(&self, grid: &Grid, sample: u64) -> f64 {
        (grid.time_of(sample) - self.start) * self.per_second
    }
}

impl Frame {
    /// Places the tiles of `query` (for the view `scene` shows) and the loading hatches.
    /// Channels without a row in `scene` are skipped.
    pub fn new(recording: &Recording, scene: &Scene, query: &Query) -> Frame {
        let mut frame = Frame {
            size: scene.size,
            antialias: scene.antialias,
            ..Frame::default()
        };
        let [width, height] = scene.size;
        let span = scene.time.end - scene.time.start;
        if width == 0 || height == 0 || !(span > 0.0 && span.is_finite()) {
            return frame;
        }
        let pixels = Pixels {
            start: scene.time.start,
            per_second: f64::from(width) / span,
            width,
            reach: if scene.antialias { 1.0 } else { 0.0 },
        };
        let rows: HashMap<ChannelId, &Row> = scene.rows.iter().map(|r| (r.channel, r)).collect();

        // Tiles cut where a grid's samples stop being evenly spaced (the pieces of a
        // segmented time base), so each draw places its items along one line.
        let draws: Vec<Placed> = query
            .draw
            .iter()
            .flat_map(|p| split_at_runs(recording, p))
            .collect();
        // The seam column of the tile before, which the next draw leaves to it.
        let mut seam: Option<u32> = None;
        for (i, placed) in draws.iter().enumerate() {
            let prev = i.checked_sub(1).and_then(|i| draws.get(i));
            let next = draws.get(i + 1);
            let draw = tile_draw(recording, &pixels, &rows, placed, prev, next);
            if let Some(mut draw) = draw {
                if let Some(c) = seam {
                    draw.columns.start = draw.columns.start.max(c + 1);
                }
                seam = draw.seam;
                if !draw.columns.is_empty() {
                    frame.draws.push(draw);
                }
            } else {
                seam = None;
            }
        }

        let (width, height) = (f64::from(width), f64::from(height));
        for loading in &query.loading {
            let Some(grid) = block_grid(recording, loading.block) else {
                continue;
            };
            let x0 = pixels.x(grid, loading.samples.start).max(0.0);
            let x1 = pixels.x(grid, loading.samples.end).min(width);
            if (x0..x1).is_empty() {
                continue;
            }
            for id in loading.block.channels(grid) {
                let Some(row) = rows.get(id) else { continue };
                let y0 = (row.center - row.height / 2.0).max(0.0);
                let y1 = (row.center + row.height / 2.0).min(height);
                if y0 < y1 {
                    frame.hatches.push(Hatch {
                        rect: [x0 as f32, y0 as f32, x1 as f32, y1 as f32],
                        color: scene.hatch,
                    });
                }
            }
        }
        frame
    }
}

fn block_grid(recording: &Recording, block: ChannelBlock) -> Option<&Grid> {
    recording
        .grids()
        .get(block.grid.0 as usize)
        .filter(|grid| block.index < ChannelBlock::count(grid))
}

/// `placed` cut into the runs of evenly spaced samples of its grid ([`Grid::regular_run`]):
/// itself, unless a piece of a segmented time base starts inside it.
fn split_at_runs(recording: &Recording, placed: &Placed) -> Vec<Placed> {
    let Some(grid) = block_grid(recording, placed.tile.key().block) else {
        return vec![placed.clone()];
    };
    let mut parts = Vec::new();
    let mut start = placed.samples.start;
    while start < placed.samples.end {
        let end = grid.regular_run(start).map_or(placed.samples.end, |run| {
            run.samples.end.min(placed.samples.end)
        });
        parts.push(Placed {
            samples: start..end,
            ..placed.clone()
        });
        start = end;
    }
    parts
}

/// The items `placed` covers: (the tile's first sample, samples per item, first item, last
/// item), or `None` if it covers none.
fn placed_items(placed: &Placed) -> Option<(u64, u64, u64, u64)> {
    let tile = &placed.tile;
    let key = tile.key();
    let bucket = key.level.bucket_len();
    let first = key.sample_range().start;
    let tile_end = first + tile.len() as u64 * bucket;
    let samples = placed.samples.start.max(first)..placed.samples.end.min(tile_end);
    if samples.is_empty() {
        return None;
    }
    Some((
        first,
        bucket,
        (samples.start - first) / bucket,
        (samples.end - 1 - first) / bucket,
    ))
}

/// Whether `b` follows `a` directly in the same channel block, both ending and starting on
/// whole items, so that their items join into one run.
fn adjacent(a: &Placed, b: &Placed) -> bool {
    let (ka, kb) = (a.tile.key(), b.tile.key());
    let whole = |placed: &Placed, sample: u64| {
        let key = placed.tile.key();
        (sample - key.sample_range().start).is_multiple_of(key.level.bucket_len())
    };
    ka.block == kb.block
        && a.samples.end == b.samples.start
        && a.samples.end >= ka.sample_range().start
        && b.samples.start >= kb.sample_range().start
        && whole(a, a.samples.end)
        && whole(b, b.samples.start)
        && !a.tile.is_empty()
        && !b.tile.is_empty()
}

/// `placed` as a [`Part`], anchored at its item nearest `x_near` pixels. Its items lie on
/// the line of the run of evenly spaced samples it starts in (placed tiles are cut at runs),
/// or for an irregular time base at their times.
fn part(pixels: &Pixels, grid: &Grid, placed: &Placed, x_near: f64) -> Option<Part> {
    let (first, bucket, lo, last) = placed_items(placed)?;
    if let Some(times) = placed.tile.times() {
        return irregular_part(pixels, grid, placed, times, lo as usize..=last as usize);
    }
    let run = grid.regular_run(placed.samples.start)?;
    let x = |sample: u64| (run.time_of(sample) - pixels.start) * pixels.per_second;
    let item_width = bucket as f64 / run.rate * pixels.per_second;
    let anchor = ((x_near - x(first)) / item_width)
        .floor()
        .clamp(lo as f64, last as f64) as u64;
    Some(Part {
        tile: Arc::clone(&placed.tile),
        items: lo as u32..=last as u32,
        anchor: anchor as u32,
        anchor_x: x(first + anchor * bucket) as f32,
        item_width: item_width as f32,
        inverse_width: (1.0 / item_width) as f32,
        xs: None,
        hold: false,
    })
}

/// `placed` of a grid with an irregular time base as a [`Part`]: of `items`, those that
/// reach the screen, and one more on either side (lines and steps run on to them), with
/// their positions from the tile's item `times`.
fn irregular_part(
    pixels: &Pixels,
    grid: &Grid,
    placed: &Placed,
    times: &[f64],
    items: RangeInclusive<usize>,
) -> Option<Part> {
    let (lo, last) = (*items.start(), *items.end());
    let seconds = |px: f64| pixels.start + px / pixels.per_second;
    // Items starting at or before the left edge, but the last of them; those starting
    // after the right edge, but the first of them.
    let left = seconds(-2.0);
    let right = seconds(f64::from(pixels.width) + 2.0);
    let from = times[lo..=last].partition_point(|&t| t <= left);
    let to = times[lo..=last].partition_point(|&t| t <= right);
    let a = (lo + from.saturating_sub(2)).min(last);
    let b = (lo + to + 1).min(last).max(a);
    let x = |t: f64| ((t - pixels.start) * pixels.per_second) as f32;
    let xs: Vec<f32> = times[a..=b + 1].iter().map(|&t| x(t)).collect();
    Some(Part {
        tile: Arc::clone(&placed.tile),
        items: a as u32..=b as u32,
        anchor: a as u32,
        anchor_x: xs[0],
        item_width: 0.0,
        inverse_width: 0.0,
        xs: Some(xs),
        hold: grid.holds_values(),
    })
}

fn tile_draw(
    recording: &Recording,
    pixels: &Pixels,
    rows: &HashMap<ChannelId, &Row>,
    placed: &Placed,
    prev: Option<&Placed>,
    next: Option<&Placed>,
) -> Option<TileDraw> {
    let key = placed.tile.key();
    let grid = block_grid(recording, key.block)?;
    let (first, bucket, _, last) = placed_items(placed)?;
    if placed.samples.end > grid.len() {
        return None;
    }
    let x = |sample: u64| pixels.x(grid, sample);
    let prev = prev.filter(|p| adjacent(p, placed));
    let next = next.filter(|n| adjacent(placed, n));
    let samples =
        placed.samples.start.max(first)..placed.samples.end.min(first + (last + 1) * bucket);
    // A held value and a bucket of irregular samples reach to the next item's start.
    let reaches = grid.times().is_some() && (grid.holds_values() || bucket > 1);
    let end = match next {
        Some(n) => x(n.samples.start),
        None if reaches => x(samples.end),
        None => x(samples.end - 1),
    };
    // At the shader's precision, so the columns below and the seam agree with where it
    // places the edges (a seam taken from 1236.99999 would be column 1236, while the shader
    // puts the next tile's first item at 1237.0).
    let domain = [x(samples.start), end].map(|d| f64::from(d as f32));
    let end = domain[1];
    let tolerance = f64::from(EDGE_TOLERANCE);
    // The column holding the next tile's first item ends this draw. Without a neighbor,
    // anti-aliased traces reach a column past the samples.
    let reach = pixels.reach;
    let seam = next.map(|_| end.floor());
    let last_column = seam.unwrap_or((domain[1] + tolerance).floor() + reach);
    let first_column = (domain[0] - tolerance).floor() - if prev.is_some() { 0.0 } else { reach };
    let columns = first_column.max(0.0)..(last_column + 1.0).min(f64::from(pixels.width));
    if columns.is_empty() {
        return None;
    }
    let seam = seam.filter(|&c| c >= columns.start && c < columns.end);

    let channels: Vec<ChannelDraw> = key
        .block
        .channels(grid)
        .iter()
        .enumerate()
        .filter_map(|(r, id)| {
            let row = rows.get(id)?;
            let (gain, offset) = recording.channel(*id).tile_scaling();
            Some(ChannelDraw {
                row: r as u32,
                coefficients: Coefficients::new(grid.sample_type(), gain, offset, row),
                clip: row.clip.then(|| {
                    let half = row.height / 2.0;
                    [
                        (row.center - half) as f32,
                        (row.center + half - CLIP_INSET) as f32,
                    ]
                }),
                color: row.color,
            })
        })
        .collect();
    if channels.is_empty() {
        return None;
    }

    Some(TileDraw {
        part: part(pixels, grid, placed, 0.0)?,
        prev: prev.and_then(|p| part(pixels, grid, p, domain[0])),
        next: next.and_then(|n| part(pixels, grid, n, domain[1])),
        columns: columns.start as u32..columns.end as u32,
        domain: domain.map(|d| d as f32),
        seam: seam.map(|c| c as u32),
        channels,
    })
}
