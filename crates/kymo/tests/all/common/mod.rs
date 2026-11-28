//! Recordings, tiles and the f64 reference for the renderer tests, and the GPU every test
//! that renders shares.

use std::ops::Range;
use std::sync::{Arc, OnceLock};

use eframe::egui_wgpu::{WgpuSetupCreateNew, WgpuSetupExisting, wgpu};
use kymo::render::{Row, Scene};
use kymo_core::pyramid::Pyramid;
use kymo_core::testing::{MemData, opened};
use kymo_core::tile::{Level, SignalVersion, Tile, TileCache, TileKey, View, compute_raw};
use kymo_core::{Channel, ChannelId, Grid, GridId, Recording, SampleType, Timebase};

/// The wgpu device on Metal, Vulkan or DX12 (traces need storage buffers in vertex shaders,
/// which OpenGL lacks; `WGPU_BACKEND` picks another), as eframe would ask for it, created
/// once and shared by every test that renders, the renderer's and the windows'; `None`
/// without such an adapter.
///
/// More than one instance and device at a time, as several tests running at once made,
/// crashed lavapipe (Mesa's software Vulkan driver, which CI uses on Linux) now and then
/// with a segfault that took the whole test binary down.
pub fn shared_gpu() -> Option<&'static WgpuSetupExisting> {
    static GPU: OnceLock<Option<WgpuSetupExisting>> = OnceLock::new();
    GPU.get_or_init(|| {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
        if wgpu::Backends::from_env().is_none() {
            desc.backends = wgpu::Backends::PRIMARY;
        }
        let instance = wgpu::Instance::new(desc);
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .ok()?;
        eprintln!("GPU tests on {:?}", adapter.get_info());
        let device_descriptor = WgpuSetupCreateNew::without_display_handle().device_descriptor;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&device_descriptor(&adapter))).ok()?;
        Some(WgpuSetupExisting {
            instance,
            adapter,
            device,
            queue,
        })
    })
    .as_ref()
}

pub const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
/// Premultiplied, with no red, so hatch pixels never count as trace pixels.
pub const HATCH: [f32; 4] = [0.0, 0.5, 0.0, 0.5];

/// A small deterministic random number generator (xorshift64*).
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.max(1))
    }

    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// Uniform in [0, 1).
    pub fn uniform(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// A random walk in `lo..=hi` with occasional single-sample spikes to the limits.
pub fn walk(rng: &mut Rng, n: usize, step: f64, lo: f64, hi: f64) -> Vec<f64> {
    let mut v = (lo + hi) / 2.0;
    (0..n)
        .map(|_| {
            v = (v + (rng.uniform() - 0.5) * 2.0 * step).clamp(lo, hi);
            match rng.next() % 2000 {
                0 => hi,
                1 => lo,
                _ => v.round(),
            }
        })
        .collect()
}

pub fn i16_rows(rows: Vec<Vec<f64>>) -> MemData {
    MemData::I16(
        rows.into_iter()
            .map(|r| r.into_iter().map(|v| v as i16).collect())
            .collect(),
    )
}

pub fn i32_rows(rows: Vec<Vec<f64>>) -> MemData {
    MemData::I32(
        rows.into_iter()
            .map(|r| r.into_iter().map(|v| v as i32).collect())
            .collect(),
    )
}

pub fn f32_rows(rows: Vec<Vec<f64>>) -> MemData {
    MemData::F32(
        rows.into_iter()
            .map(|r| r.into_iter().map(|v| v as f32).collect())
            .collect(),
    )
}

/// A channel of `grid` with physical = native · gain + offset.
pub fn channel(name: &str, grid: u32, gain: f64, offset: f64) -> Channel {
    let mut ch = Channel::new(name, GridId(grid));
    ch.gain = gain;
    ch.offset = offset;
    ch
}

/// A grid of `len` samples at `rate` holding `channels`, with data in `segments`.
pub fn grid(
    rate: f64,
    len: u64,
    segments: Vec<Range<u64>>,
    sample_type: SampleType,
    channels: Range<u32>,
) -> Grid {
    Grid::new(
        Timebase::Regular { rate, start: 0.0 },
        len,
        segments,
        sample_type,
        channels.map(ChannelId).collect(),
    )
    .unwrap()
}

/// A recording with its samples and pyramids, all in memory.
pub struct Fixture {
    pub recording: Recording,
    data: Vec<MemData>,
    pyramids: Vec<Pyramid>,
}

impl Fixture {
    pub fn new(channels: Vec<Channel>, grids: Vec<Grid>, data: Vec<MemData>) -> Fixture {
        let (recording, _) = Recording::new(opened(channels, grids, data.clone())).unwrap();
        let pyramids = (0..recording.grids().len() as u32)
            .map(|g| Pyramid::build(&recording, GridId(g)).unwrap())
            .collect();
        Fixture {
            recording,
            data,
            pyramids,
        }
    }

    /// The tile `key`, computed from scratch; `None` outside the recording.
    pub fn tile(&self, key: TileKey) -> Option<Arc<Tile>> {
        let tile = if key.level == Level::RAW {
            compute_raw(&self.recording, key).ok()
        } else {
            self.pyramids[key.block.grid.0 as usize].tile(key)
        };
        tile.map(Arc::new)
    }

    /// A cache holding the tiles `keys`.
    pub fn cache(&self, keys: impl IntoIterator<Item = TileKey>) -> TileCache {
        let cache = TileCache::new(usize::MAX);
        for key in keys {
            if let Some(tile) = self.tile(key) {
                cache.insert(tile);
            }
        }
        cache
    }

    pub fn grid_of(&self, id: ChannelId) -> &Grid {
        self.recording.grid(self.recording.channel(id).grid)
    }

    /// The physical value of sample `i`, or `None` in a gap or for NaN.
    pub fn physical(&self, id: ChannelId, i: u64) -> Option<f64> {
        let channel = self.recording.channel(id);
        let grid = self.recording.grid(channel.grid);
        let segments = grid.segments();
        let next = segments.partition_point(|s| s.end <= i);
        if !segments.get(next).is_some_and(|s| s.contains(&i)) {
            return None;
        }
        let row = grid.channels().iter().position(|&c| c == id).unwrap();
        let native = self.data[channel.grid.0 as usize].get(row, i as usize);
        (!native.is_nan()).then_some(native * channel.gain + channel.offset)
    }

    /// The smallest and largest physical value of a channel.
    pub fn extent(&self, id: ChannelId) -> (f64, f64) {
        let len = self.grid_of(id).len();
        (0..len)
            .filter_map(|i| self.physical(id, i))
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
                (lo.min(v), hi.max(v))
            })
    }

    /// One row for `id` in a trace area of `size`, scaled so the whole channel fills 80%
    /// of the height around its midpoint.
    pub fn scene(&self, id: ChannelId, time: Range<f64>, size: [u32; 2]) -> Scene {
        let (lo, hi) = self.extent(id);
        let height = f64::from(size[1]);
        let spread = if hi > lo { hi - lo } else { 1.0 };
        Scene {
            time,
            size,
            rows: vec![Row {
                channel: id,
                center: height / 2.0,
                height,
                px_per_unit: 0.8 * height / spread,
                baseline: (lo + hi) / 2.0,
                color: WHITE,
                clip: false,
            }],
            hatch: HATCH,
            antialias: false,
        }
    }
}

/// The view a scene's channels need.
pub fn view(scene: &Scene) -> View {
    View {
        version: SignalVersion::RAW,
        time: scene.time.clone(),
        width_px: scene.size[0],
        channels: scene.rows.iter().map(|r| r.channel).collect(),
    }
}

/// The samples of a scene's first row in pixels, computed in f64 straight from the
/// samples: positions, and y (`None` in gaps and for NaN).
pub struct Truth {
    pub xs: Vec<f64>,
    pub ys: Vec<Option<f64>>,
}

impl Truth {
    /// Covers the screen plus `margin` pixels and two samples on either side.
    pub fn new(fx: &Fixture, scene: &Scene, margin: f64) -> Truth {
        let row = &scene.rows[0];
        let grid = fx.grid_of(row.channel);
        let per_px = f64::from(scene.size[0]) / (scene.time.end - scene.time.start);
        let m = margin / per_px;
        let first = grid.sample_at(scene.time.start - m).saturating_sub(2);
        let end = (grid.sample_at(scene.time.end + m) + 3).min(grid.len());
        let (mut xs, mut ys) = (Vec::new(), Vec::new());
        for i in first..end {
            xs.push((grid.time_of(i) - scene.time.start) * per_px);
            ys.push(
                fx.physical(row.channel, i)
                    .map(|p| row.center - (p - row.baseline) * row.px_per_unit),
            );
        }
        // Where the last sample ends (a held value lasts until then), without a value.
        if end == grid.len() {
            xs.push((grid.time_of(end) - scene.time.start) * per_px);
            ys.push(None);
        }
        Truth { xs, ys }
    }

    /// Indices of the samples with x in `a..=b`.
    fn within(&self, a: f64, b: f64) -> Range<usize> {
        self.xs.partition_point(|&x| x < a)..self.xs.partition_point(|&x| x <= b)
    }

    /// The distance from `(x, y)` to the line through consecutive present samples (and to
    /// samples without a present neighbor), looking at samples within `reach` pixels.
    pub fn distance(&self, x: f64, y: f64, reach: f64) -> f64 {
        let inside = self.within(x - reach, x + reach);
        let from = inside.start.saturating_sub(1);
        let to = (inside.end + 1).min(self.xs.len());
        let mut d = f64::INFINITY;
        for i in from..to {
            let Some(y0) = self.ys[i] else { continue };
            let x0 = self.xs[i];
            d = d.min((x - x0).hypot(y - y0));
            if let Some(&Some(y1)) = self.ys.get(i + 1) {
                let (dx, dy) = (self.xs[i + 1] - x0, y1 - y0);
                let t = (((x - x0) * dx + (y - y0) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
                d = d.min((x - (x0 + dx * t)).hypot(y - (y0 + dy * t)));
            }
        }
        d
    }

    /// Whether the line runs without a gap over x in `a..=b`: every sample there and the
    /// one on either side is present.
    pub fn continuous(&self, a: f64, b: f64) -> bool {
        let inside = self.within(a, b);
        inside.start > 0
            && inside.end < self.xs.len()
            && self.ys[inside.start - 1..=inside.end]
                .iter()
                .all(Option::is_some)
    }

    /// The y range of the samples with x in `a..=b`.
    pub fn samples(&self, a: f64, b: f64) -> Option<(f64, f64)> {
        let mut range = None;
        for y in self.ys[self.within(a, b)].iter().flatten() {
            extend(&mut range, *y);
        }
        range
    }

    /// The y range of the present samples whose values hold over part of x in `a..=b`: from
    /// their own x to the next slot's.
    pub fn held(&self, a: f64, b: f64) -> Option<(f64, f64)> {
        let mut range = None;
        let from = self.xs.partition_point(|&x| x <= a).saturating_sub(1);
        for i in from..self.xs.partition_point(|&x| x <= b) {
            let next = self.xs.get(i + 1).copied().unwrap_or(self.xs[i]);
            if let Some(y) = self.ys[i]
                && next >= a
            {
                extend(&mut range, y);
            }
        }
        range
    }

    /// The y range of the line through consecutive present samples, over x in `a..=b`.
    pub fn line(&self, a: f64, b: f64) -> Option<(f64, f64)> {
        let inside = self.within(a, b);
        let mut range = self.samples(a, b);
        // Where segments cross the ends.
        for (x, i) in [(a, inside.start), (b, inside.end)] {
            if i == 0 || i >= self.xs.len() {
                continue;
            }
            if let (Some(y0), Some(y1)) = (self.ys[i - 1], self.ys[i]) {
                let (x0, x1) = (self.xs[i - 1], self.xs[i]);
                if x0 < x && x < x1 {
                    extend(&mut range, y0 + (y1 - y0) * (x - x0) / (x1 - x0));
                }
            }
        }
        range
    }
}

fn extend(range: &mut Option<(f64, f64)>, y: f64) {
    *range = Some(match *range {
        None => (y, y),
        Some((lo, hi)) => (lo.min(y), hi.max(y)),
    });
}
