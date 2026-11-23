//! The trace renderer on a real GPU: frames rendered off-screen in whole pixels and
//! anti-aliased, every pixel column checked
//! against an f64 reference computed from the raw samples.
//!
//! Without a GPU adapter the tests pass with a message; set `KYMO_REQUIRE_GPU` to make
//! them fail instead (CI does on Linux, with Mesa's software Vulkan driver).

use std::ops::{Deref, DerefMut};
use std::sync::{Mutex, OnceLock};

use crate::common::{
    Fixture, HATCH, Rng, Truth, channel, f32_rows, grid, i16_rows, i32_rows, view, walk,
};
use eframe::egui_wgpu::wgpu;
use kymo::render::{DEFAULT_GPU_BUDGET, EDGE_TOLERANCE, Frame, Scene, TraceRenderer};
use kymo_core::tile::{
    Level, Query, TILE_LEN, TileCache, TileKey, level_for, max_level, query, tiles_for_view,
};
use std::sync::Arc;

use kymo_core::{ChannelId, Grid, SampleType, TimeTable, Timebase};

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// Renderers given back, so their pipelines are built once per test thread at most:
    /// software drivers (lavapipe, WARP) take long to compile them.
    renderers: Mutex<Vec<TraceRenderer>>,
}

/// The test GPU, created once, with the limits eframe asks for.
fn gpu() -> Option<&'static Gpu> {
    static GPU: OnceLock<Option<Gpu>> = OnceLock::new();
    let gpu = GPU
        .get_or_init(|| {
            let mut desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
            if wgpu::Backends::from_env().is_none() {
                desc.backends = wgpu::Backends::PRIMARY;
            }
            let instance = wgpu::Instance::new(desc);
            let adapter = pollster::block_on(
                instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
            )
            .ok()?;
            eprintln!("GPU tests on {:?}", adapter.get_info());
            let (device, queue) =
                pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                    label: Some("kymo test"),
                    required_limits: wgpu::Limits::default(),
                    ..Default::default()
                }))
                .ok()?;
            Some(Gpu {
                device,
                queue,
                renderers: Mutex::default(),
            })
        })
        .as_ref();
    if gpu.is_none() {
        assert!(
            std::env::var_os("KYMO_REQUIRE_GPU").is_none_or(|v| v.is_empty()),
            "no GPU adapter, and KYMO_REQUIRE_GPU is set"
        );
        eprintln!("skipped: no GPU adapter");
    }
    gpu
}

impl Gpu {
    /// A renderer as a new one starts: no tiles on the GPU and the default budget.
    fn renderer(&self) -> Pooled<'_> {
        let reused = self.renderers.lock().unwrap().pop();
        let renderer = match reused {
            Some(mut renderer) => {
                renderer.clear();
                renderer.set_budget(DEFAULT_GPU_BUDGET);
                renderer
            }
            None => TraceRenderer::new(&self.device, wgpu::TextureFormat::Rgba8Unorm, 1).unwrap(),
        };
        Pooled {
            gpu: self,
            renderer: Some(renderer),
        }
    }

    /// Renders `frame` into a cleared, transparent image.
    fn render(&self, renderer: &mut TraceRenderer, frame: &Frame) -> Image {
        let [width, height] = frame.size;
        let extent = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("kymo test target"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target = texture.create_view(&wgpu::TextureViewDescriptor::default());
        renderer.prepare(&self.device, &self.queue, frame);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kymo test pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            renderer.paint(&mut pass);
        }
        let row_bytes = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kymo test readback"),
            size: u64::from(row_bytes * height),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row_bytes),
                    rows_per_image: Some(height),
                },
            },
            extent,
        );
        self.queue.submit([encoder.finish()]);
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, |r| r.unwrap());
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        let bytes = buffer.slice(..).get_mapped_range().unwrap();
        let mut pixels = Vec::with_capacity((width * height) as usize);
        for y in 0..height as usize {
            let row = &bytes[y * row_bytes as usize..][..width as usize * 4];
            pixels.extend(row.chunks(4).map(|p| [p[0], p[1], p[2], p[3]]));
        }
        Image {
            width,
            height,
            pixels,
        }
    }
}

/// A renderer from [`Gpu::renderer`], given back when dropped.
struct Pooled<'a> {
    gpu: &'a Gpu,
    renderer: Option<TraceRenderer>,
}

impl Deref for Pooled<'_> {
    type Target = TraceRenderer;
    fn deref(&self) -> &TraceRenderer {
        self.renderer.as_ref().unwrap()
    }
}

impl DerefMut for Pooled<'_> {
    fn deref_mut(&mut self) -> &mut TraceRenderer {
        self.renderer.as_mut().unwrap()
    }
}

impl Drop for Pooled<'_> {
    fn drop(&mut self) {
        if let (Some(renderer), Ok(mut pool)) = (self.renderer.take(), self.gpu.renderers.lock()) {
            pool.push(renderer);
        }
    }
}

#[derive(PartialEq)]
struct Image {
    width: u32,
    height: u32,
    pixels: Vec<[u8; 4]>,
}

impl Image {
    fn at(&self, x: u32, y: u32) -> [u8; 4] {
        self.pixels[(y * self.width + x) as usize]
    }

    /// Trace pixels are white; hatch pixels have no red.
    fn trace(&self, x: u32, y: u32) -> bool {
        self.at(x, y)[0] > 0
    }

    /// The share of pixel (x, y) a white trace covers.
    fn cover(&self, x: u32, y: u32) -> f64 {
        f64::from(self.at(x, y)[3]) / 255.0
    }

    fn hatch(&self, x: u32, y: u32) -> bool {
        let p = self.at(x, y);
        p[0] == 0 && p[1] > 0
    }

    /// The trace rows of column `x`, if they form one run.
    fn run(&self, x: u32) -> Result<Option<(i64, i64)>, String> {
        let lit: Vec<i64> = (0..self.height)
            .filter(|&y| self.trace(x, y))
            .map(i64::from)
            .collect();
        match (lit.first(), lit.last()) {
            (Some(&a), Some(&b)) if b - a + 1 == lit.len() as i64 => Ok(Some((a, b))),
            (Some(_), _) => Err(format!("column {x}: rows {lit:?} not contiguous")),
            _ => Ok(None),
        }
    }
}

/// What the reference allows in a column: the line through the samples (level 0), or the
/// samples plus whole buckets on either side (levels ≥ 1, fallbacks), as wide as the
/// widest drawn near the column (`widths[x]` pixels).
#[derive(Clone, Debug)]
enum Shape {
    Line,
    /// Samples whose values hold until the next slot (an irregular time base's steps).
    Steps,
    Buckets {
        widths: Vec<f64>,
    },
}

impl Shape {
    /// The widest bucket anywhere.
    fn widest(&self) -> f64 {
        match self {
            Shape::Line | Shape::Steps => 0.0,
            Shape::Buckets { widths } => widths.iter().copied().fold(0.0, f64::max),
        }
    }
}

/// Pixels a value or position may be off from the f64 reference: f32 rounding.
const EPS: f64 = 2.0 * EDGE_TOLERANCE as f64;

/// The rows of `range` in an image `height` rows tall, its ends moved inwards by `shrink`
/// (outwards if negative).
fn rows(range: Option<(f64, f64)>, shrink: f64, height: u32) -> Option<(i64, i64)> {
    let (lo, hi) = range?;
    let top = ((lo + shrink).floor() as i64).max(0);
    let bottom = ((hi - shrink).floor() as i64).min(i64::from(height) - 1);
    (top <= bottom).then_some((top, bottom))
}

/// Checks every column: the trace rows form one run that holds every row the reference
/// must light and stays within the rows it may light.
fn check_columns(
    image: &Image,
    truth: &Truth,
    shape: &Shape,
    columns: &[std::ops::Range<u32>],
) -> Result<(), String> {
    for x in columns.iter().flat_map(Clone::clone) {
        let c = f64::from(x);
        let (inner, outer) = match shape {
            Shape::Line => (
                truth.line(c + EPS, c + 1.0 - EPS),
                truth.line(c - EPS, c + 1.0 + EPS),
            ),
            Shape::Steps => (
                truth.held(c + EPS, c + 1.0 - EPS),
                truth.held(c - EPS, c + 1.0 + EPS),
            ),
            Shape::Buckets { widths } => {
                let width = widths[x as usize];
                (
                    truth.samples(c + EPS, c + 1.0 - EPS),
                    truth.samples(c - width - EPS, c + 1.0 + width + EPS),
                )
            }
        };
        let must = rows(inner, EPS, image.height);
        let may = rows(outer, -EPS, image.height);
        let lit = image.run(x)?;
        let ok = match (lit, must, may) {
            (None, None, _) => true,
            (Some((a, b)), must, Some((lo, hi))) => {
                lo <= a && b <= hi && must.is_none_or(|(m0, m1)| a <= m0 && m1 <= b)
            }
            _ => false,
        };
        if !ok {
            return Err(format!(
                "column {x}: lit {lit:?}, must light {must:?}, may light {may:?} \
                 (inner {inner:?}, outer {outer:?})"
            ));
        }
    }
    // Where the data runs on without a gap, neighboring columns connect: their runs share
    // a row.
    for range in columns {
        for x in range.start..range.end.saturating_sub(1) {
            let c = f64::from(x);
            let slack = match shape {
                Shape::Line | Shape::Steps => 0.0,
                Shape::Buckets { widths } => widths[x as usize].max(widths[x as usize + 1]) + EPS,
            };
            if !truth.continuous(c - slack, c + 2.0 + slack) {
                continue;
            }
            if let (Some((a0, a1)), Some((b0, b1))) = (image.run(x)?, image.run(x + 1)?)
                && a0.max(b0) > a1.min(b1)
            {
                return Err(format!(
                    "columns {x} and {} don't connect: rows {a0}..={a1} and {b0}..={b1}",
                    x + 1
                ));
            }
        }
    }
    Ok(())
}

/// Least cover of a pixel the trace passes through: its center is at most √½ pixel away.
const MIN_COVER: f64 = 1.0 - std::f64::consts::FRAC_1_SQRT_2;

/// How much more than exactly the renderer may cover a pixel: it measures distances to
/// boxes 1/16 of two pixels wide around the trace (see `traces.wgsl`).
const OVER_COVER: f64 = 1.0 / 8.0;

/// Checks an anti-aliased image against the same frame drawn in whole pixels (`whole`):
/// every pixel lit there is covered at least [`MIN_COVER`], and no pixel is covered that
/// isn't within one pixel of one lit there. Lines (level 0) are also checked pixel by pixel
/// against the f64 reference: each pixel covered by 1 − its center's distance to the line,
/// or up to [`OVER_COVER`] more.
fn check_antialiased(
    image: &Image,
    whole: &Image,
    truth: &Truth,
    shape: &Shape,
    columns: &[std::ops::Range<u32>],
) -> Result<(), String> {
    let (w, h) = (image.width, image.height);
    // Rounding to 8 bits, and f32 positions.
    let slack = 1.5 / 255.0 + 2.0 * EPS;
    for x in columns.iter().flat_map(Clone::clone) {
        for y in 0..h {
            let cover = image.cover(x, y);
            if whole.trace(x, y) && cover < MIN_COVER - slack {
                return Err(format!("pixel ({x}, {y}) lit, but covered only {cover:.3}"));
            }
            let near = (x.saturating_sub(1)..=(x + 1).min(w - 1))
                .any(|nx| (y.saturating_sub(1)..=(y + 1).min(h - 1)).any(|ny| whole.trace(nx, ny)));
            // On the border, the trace may lie just off screen.
            let border = x == 0 || y == 0 || x == w - 1 || y == h - 1;
            if cover > slack && !near && !border {
                return Err(format!(
                    "pixel ({x}, {y}) covered {cover:.3}, far from the trace"
                ));
            }
            if let Shape::Line = shape {
                let d = truth.distance(f64::from(x) + 0.5, f64::from(y) + 0.5, 1.5);
                let want = (1.0 - d).clamp(0.0, 1.0);
                let most = (1.0 - d + OVER_COVER).clamp(0.0, 1.0);
                if cover < want - slack || cover > most + slack {
                    return Err(format!(
                        "pixel ({x}, {y}) covered {cover:.3}, not {want:.3} to {most:.3} \
                         (distance {d:.4})"
                    ));
                }
            }
        }
    }
    Ok(())
}

/// The reference shape for what `query` draws: lines if everything is level 0 (steps where
/// values hold), otherwise buckets as wide as the widest drawn (for an irregular time base,
/// the widest item, a held sample's hold included).
fn shape(fx: &Fixture, scene: &Scene, query: &Query) -> Shape {
    if query.draw.iter().all(|p| p.tile.key().level == Level::RAW) {
        let holds = query
            .draw
            .iter()
            .any(|p| fx.recording.grid(p.tile.key().block.grid).holds_values());
        return if holds { Shape::Steps } else { Shape::Line };
    }
    let per_px = f64::from(scene.size[0]) / (scene.time.end - scene.time.start);
    let x = |t: f64| (t - scene.time.start) * per_px;
    // Each tile's samples on screen, and its bucket width.
    let placed: Vec<(f64, f64, f64)> = query
        .draw
        .iter()
        .map(|p| {
            let key = p.tile.key();
            let grid = fx.recording.grid(key.block.grid);
            let bucket = key.level.bucket_len();
            let width = match p.tile.times() {
                Some(times) => {
                    let first = key.sample_range().start;
                    let items =
                        (p.samples.start - first) / bucket..=(p.samples.end - 1 - first) / bucket;
                    items
                        .map(|i| (times[i as usize + 1] - times[i as usize]) * per_px)
                        .fold(0.0, f64::max)
                }
                None => bucket as f64 / grid.rate() * per_px,
            };
            (
                x(grid.time_of(p.samples.start)),
                x(grid.time_of(p.samples.end)),
                width,
            )
        })
        .collect();
    // The widest bucket of the tiles whose samples come within a pixel of the column.
    let widths = (0..scene.size[0])
        .map(|c| {
            let c = f64::from(c);
            placed
                .iter()
                .filter(|&&(a, b, _)| a <= c + 2.0 && c - 1.0 <= b)
                .map(|&(_, _, w)| w)
                .fold(0.0, f64::max)
        })
        .collect();
    Shape::Buckets { widths }
}

/// Renders `scene` from the tiles in `cache` and checks every column.
fn check(gpu: &Gpu, fx: &Fixture, scene: &Scene, cache: &TileCache) -> Image {
    check_some(gpu, fx, scene, cache, &[0..scene.size[0]])
}

/// Renders `scene` from the tiles in `cache` and checks the columns in `columns`.
fn check_some(
    gpu: &Gpu,
    fx: &Fixture,
    scene: &Scene,
    cache: &TileCache,
    columns: &[std::ops::Range<u32>],
) -> Image {
    let query = query(&fx.recording, &view(scene), cache);
    // Both ways, whatever the scene asks for: in whole pixels, and anti-aliased.
    let mut renderer = gpu.renderer();
    let mut render = |antialias| {
        let scene = Scene {
            antialias,
            ..scene.clone()
        };
        gpu.render(&mut renderer, &Frame::new(&fx.recording, &scene, &query))
    };
    let whole = render(false);
    let smooth = render(true);
    let shape = shape(fx, scene, &query);
    let margin = match shape {
        Shape::Line => 1.0,
        Shape::Buckets { .. } => shape.widest() + 2.0,
        Shape::Steps => 2.0,
    };
    let truth = Truth::new(fx, scene, margin);
    if let Some(dir) = std::env::var_os("KYMO_DUMP") {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let mut ppm = format!("P6 {} {} 255\n", smooth.width, smooth.height).into_bytes();
        ppm.extend(smooth.pixels.iter().flat_map(|p| [p[0], p[1], p[2]]));
        std::fs::write(std::path::Path::new(&dir).join(format!("{n:03}.ppm")), ppm).unwrap();
    }
    let checked = check_columns(&whole, &truth, &shape, columns)
        .and_then(|()| check_antialiased(&smooth, &whole, &truth, &shape, columns));
    if let Err(e) = checked {
        panic!(
            "{:?} over {:?} at {:?} ({}): {e}",
            scene.rows[0].channel,
            scene.time,
            query
                .draw
                .iter()
                .map(|p| (p.tile.key().level, p.samples.clone()))
                .collect::<Vec<_>>(),
            match shape {
                Shape::Line => "line".to_owned(),
                Shape::Steps => "steps".to_owned(),
                Shape::Buckets { .. } => format!("buckets up to {:.3} px", shape.widest()),
            },
        );
    }
    if scene.antialias { smooth } else { whole }
}

/// The tiles the view of `scene` wants, all cached.
fn exact(fx: &Fixture, scene: &Scene) -> TileCache {
    fx.cache(tiles_for_view(&fx.recording, &view(scene)))
}

const SIZE: [u32; 2] = [333, 120];

/// Two grids at different rates: int16 at 1000 Hz, and BDF-like int32 at 250 Hz with a huge
/// offset, one channel with a negative gain.
fn mixed() -> Fixture {
    let mut rng = Rng::new(7);
    let fast = 200_000;
    let slow = fast / 4;
    let data = vec![
        i16_rows(vec![
            walk(&mut rng, fast as usize, 300.0, -30_000.0, 30_000.0),
            walk(&mut rng, fast as usize, 20.0, -2_000.0, 2_000.0),
        ]),
        i32_rows(vec![
            walk(&mut rng, slow as usize, 2_000.0, -8_000_000.0, 8_000_000.0),
            walk(&mut rng, slow as usize, 50.0, -5_000.0, 5_000.0),
        ]),
    ];
    Fixture::new(
        vec![
            channel("fast", 0, 0.5, 0.0),
            channel("fast small", 0, 0.1, -3.0),
            channel("bdf", 1, 0.031_25, 1.0e6),
            channel("flipped", 1, -0.031_25, -2.5e5),
        ],
        vec![
            grid(1000.0, fast, vec![0..fast], SampleType::I16, 0..2),
            grid(250.0, slow, vec![0..slow], SampleType::I32, 2..4),
        ],
        data,
    )
}

/// Spans from the shortest window up to the whole recording, at several starts, including
/// windows around tile boundaries.
fn views(duration: f64, rate: f64) -> Vec<std::ops::Range<f64>> {
    let mut views = Vec::new();
    let mut span = 10.0 / rate;
    let mut rng = Rng::new(span.to_bits());
    loop {
        let span_now = span.min(duration);
        let boundary = TILE_LEN as f64 / rate;
        for start in [
            0.0,
            boundary - span_now / 2.0,
            // Ending inside a tile's last sample.
            boundary - 0.5 / rate - span_now,
            boundary * 3.0 - span_now / 3.0,
            rng.uniform() * (duration - span_now),
            duration - span_now,
        ] {
            let start = start.clamp(0.0, duration - span_now);
            views.push(start..start + span_now);
        }
        if span >= duration {
            return views;
        }
        span *= 7.3;
    }
}

#[test]
fn columns_match_the_samples_at_every_zoom() {
    let Some(gpu) = gpu() else { return };
    let fx = mixed();
    for id in 0..4 {
        let id = ChannelId(id);
        let rate = fx.grid_of(id).rate();
        for time in views(fx.recording.duration(), rate) {
            let scene = fx.scene(id, time, SIZE);
            check(gpu, &fx, &scene, &exact(&fx, &scene));
        }
    }
}

/// One channel on an irregular time base: 30,000 samples 1 ms apart, 2,000 jittered ones
/// 2 to 18 ms apart, 300 a tenth of a second apart, a gap of 20 s (40,000 slots), 60,000
/// samples 0.5 ms apart, and 5,000 jittered ones again; values held (steps) or joined by
/// lines.
fn irregular(hold: bool) -> Fixture {
    let mut rng = Rng::new(if hold { 21 } else { 22 });
    let mut jitter = Rng::new(3);
    let mut times = Vec::new();
    let mut t = 0.25;
    let mut run = |times: &mut Vec<f64>, t: &mut f64, n: usize, step: f64, jittered: bool| {
        for _ in 0..n {
            times.push(*t);
            *t += if jittered {
                step + 16e-3 * jitter.uniform()
            } else {
                step
            };
        }
    };
    run(&mut times, &mut t, 30_000, 1e-3, false);
    run(&mut times, &mut t, 2_000, 2e-3, true);
    run(&mut times, &mut t, 300, 0.1, false);
    let before = 0..times.len() as u64;
    // The gap's slots, as the XDF reader lays them out: the first a step after the sample
    // before it, then evenly at about the median step (0.5 ms) to the sample after it.
    let slots = 40_000;
    for j in 0..slots {
        times.push(t + 20.0 * f64::from(j) / f64::from(slots));
    }
    t += 20.0;
    let after = times.len() as u64;
    run(&mut times, &mut t, 60_000, 5e-4, false);
    run(&mut times, &mut t, 5_000, 2e-3, true);
    let segments = vec![before, after..times.len() as u64];
    let n = times.len() as u64;
    let end = t;
    let table = TimeTable::new(&times, end).unwrap();
    let grid = Grid::new(
        Timebase::Irregular {
            rate: n as f64 / (end - times[0]),
            hold,
            times: Arc::new(table),
        },
        n,
        segments,
        SampleType::I16,
        vec![ChannelId(0)],
    )
    .unwrap();
    Fixture::new(
        vec![channel("irregular", 0, 0.5, 0.0)],
        vec![grid],
        vec![i16_rows(vec![walk(
            &mut rng, n as usize, 300.0, -30_000.0, 30_000.0,
        )])],
    )
}

#[test]
fn irregular_columns_match_the_samples_at_every_zoom() {
    let Some(gpu) = gpu() else { return };
    for hold in [true, false] {
        let fx = irregular(hold);
        let duration = fx.recording.duration();
        // Spans from 10 dense samples to everything, in dense, sparse and gap stretches.
        let mut span = 0.01;
        while span < duration * 2.0 {
            let span_now = span.min(duration);
            for start in [0.0, 30.5, 61.0, 62.5, duration - span_now] {
                let start = start.clamp(0.0, duration - span_now);
                let scene = fx.scene(ChannelId(0), start..start + span_now, SIZE);
                check(gpu, &fx, &scene, &exact(&fx, &scene));
            }
            span *= 6.7;
        }
    }
}

/// Zoomed out, an irregular time base's gap stays dark: no envelope is drawn over it,
/// beyond a pixel at either end. (Laid out as one slot, as long as it lasted, a bucket
/// holding it held samples from either side and was drawn across it.)
#[test]
fn irregular_gaps_stay_unlit_zoomed_out() {
    let Some(gpu) = gpu() else { return };
    for hold in [true, false] {
        let fx = irregular(hold);
        let grid = fx.recording.grid(kymo_core::GridId(0));
        // The gap's first slot, and the first sample after it.
        let segments = grid.segments();
        let (from, to) = (
            grid.time_of(segments[0].end),
            grid.time_of(segments[1].start),
        );
        let duration = fx.recording.duration();
        let mut coarse = false;
        for span in [duration, 90.0, 40.0, 25.0] {
            let middle = (from + to) / 2.0;
            let start = (middle - span / 2.0).clamp(0.0, duration - span);
            let scene = fx.scene(ChannelId(0), start..start + span, SIZE);
            let cache = exact(&fx, &scene);
            let q = query(&fx.recording, &view(&scene), &cache);
            coarse |= q.draw.iter().any(|p| p.tile.key().level > Level::RAW);
            for antialias in [false, true] {
                let scene = Scene {
                    antialias,
                    ..scene.clone()
                };
                let image = gpu.render(&mut gpu.renderer(), &Frame::new(&fx.recording, &scene, &q));
                let per_px = f64::from(SIZE[0]) / span;
                let (a, b) = ((from - start) * per_px, (to - start) * per_px);
                // Columns lying wholly inside the gap, more than a pixel from its ends
                // (anti-aliasing reaches a pixel).
                let first = (a + 2.0).ceil().max(0.0) as u32;
                let last = (b - 2.0).floor().min(f64::from(SIZE[0])) as u32;
                for x in first..last {
                    let lit = (0..SIZE[1]).find(|&y| image.cover(x, y) > 0.0);
                    assert!(
                        lit.is_none(),
                        "hold {hold}, {span} s, antialias {antialias}: column {x} of the \
                         gap ({a:.1} to {b:.1} px) lit at row {lit:?}"
                    );
                }
            }
        }
        assert!(coarse, "some view draws buckets");
    }
}

#[test]
fn every_level_is_drawn() {
    // The views of the test above reach level 0 and the top of the pyramid.
    let fx = mixed();
    let mut levels = std::collections::BTreeSet::new();
    for time in views(fx.recording.duration(), 1000.0) {
        let scene = fx.scene(ChannelId(0), time, SIZE);
        levels.insert(level_for(fx.grid_of(ChannelId(0)), &view(&scene)).get());
    }
    let top = max_level(fx.grid_of(ChannelId(0)).len()).get();
    assert!(top >= 2);
    assert_eq!(levels, (0..=top).collect());
}

#[test]
fn a_single_sample_spike_shows_at_every_zoom() {
    let Some(gpu) = gpu() else { return };
    let n = 1 << 20;
    let spike = 700_001;
    let mut flat = vec![0.0; n];
    flat[spike] = 1000.0;
    let fx = Fixture::new(
        vec![channel("spike", 0, 1.0, 0.0)],
        vec![grid(
            1000.0,
            n as u64,
            vec![0..n as u64],
            SampleType::I16,
            0..1,
        )],
        vec![i16_rows(vec![flat])],
    );
    let id = ChannelId(0);
    let at = spike as f64 / 1000.0;
    let mut span = 0.01;
    while span < fx.recording.duration() * 2.0 {
        let span_now = span.min(fx.recording.duration());
        let start = (at - span_now * 0.37).clamp(0.0, fx.recording.duration() - span_now);
        for antialias in [false, true] {
            let mut scene = fx.scene(id, start..start + span_now, SIZE);
            scene.antialias = antialias;
            let image = check(gpu, &fx, &scene, &exact(&fx, &scene));
            // A column next to the spike's reaches the spike's row, which the line runs
            // through.
            let row = &scene.rows[0];
            let x = ((at - start) / span_now * f64::from(SIZE[0])) as u32;
            let top = (row.center - (1000.0 - row.baseline) * row.px_per_unit).floor() as u32;
            let reaches = (x.saturating_sub(1)..=(x + 1).min(SIZE[0] - 1))
                .any(|x| image.cover(x, top) >= MIN_COVER - 0.01);
            assert!(
                reaches,
                "span {span_now}, antialias {antialias}: spike at column {x} doesn't reach row {top}"
            );
        }
        span *= 3.0;
    }
}

/// A flat channel, drawn at `center` (pixels from the top) in a scene of `SIZE`.
fn flat_scene(fx: &Fixture, center: f64, antialias: bool) -> Scene {
    let mut scene = fx.scene(ChannelId(0), 0.0..1.0, SIZE);
    scene.rows[0].center = center;
    scene.rows[0].baseline = 0.0;
    scene.antialias = antialias;
    scene
}

fn flat() -> Fixture {
    Fixture::new(
        vec![channel("flat", 0, 1.0, 0.0)],
        vec![grid(1000.0, 4000, vec![0..4000], SampleType::I16, 0..1)],
        vec![i16_rows(vec![vec![0.0; 4000]])],
    )
}

#[test]
fn a_flat_trace_covers_rows_in_proportion() {
    let Some(gpu) = gpu() else { return };
    let fx = flat();
    // The trace's y, and the rows it covers by how much with anti-aliasing.
    let cases: [(f64, &[(u32, f64)]); 4] = [
        (10.3, &[(9, 0.2), (10, 0.8)]),
        (10.0, &[(9, 0.5), (10, 0.5)]),
        (10.5, &[(10, 1.0)]),
        (10.8, &[(10, 0.7), (11, 0.3)]),
    ];
    for (y, want) in cases {
        let scene = flat_scene(&fx, y, true);
        let image = check(gpu, &fx, &scene, &exact(&fx, &scene));
        for x in 0..SIZE[0] {
            for row in 0..SIZE[1] {
                let expected = want.iter().find(|w| w.0 == row).map_or(0.0, |w| w.1);
                let got = image.cover(x, row);
                assert!(
                    (got - expected).abs() <= 1.5 / 255.0,
                    "y {y}: column {x}, row {row} covered {got:.3}, not {expected}"
                );
            }
        }
        // Without anti-aliasing, the row holding y, whole.
        let scene = flat_scene(&fx, y, false);
        let image = check(gpu, &fx, &scene, &exact(&fx, &scene));
        for row in 0..SIZE[1] {
            let expected = if row == y.floor() as u32 { 1.0 } else { 0.0 };
            assert_eq!(image.cover(7, row), expected, "y {y}: row {row}");
        }
    }
}

#[test]
fn antialiased_clipped_rows_stay_in_their_rows() {
    let Some(gpu) = gpu() else { return };
    // A walk far beyond its row, clipped to rows 40.3 to 60.6: the trace may light rows
    // 40 to 60, as without anti-aliasing, and runs along the edges at full cover.
    let mut rng = Rng::new(11);
    let n = 20_000u64;
    let fx = Fixture::new(
        vec![channel("stim", 0, 1.0, 0.0)],
        vec![grid(1000.0, n, vec![0..n], SampleType::I16, 0..1)],
        vec![i16_rows(vec![walk(
            &mut rng, n as usize, 2_000.0, -30_000.0, 30_000.0,
        )])],
    );
    for time in views(fx.recording.duration(), 1000.0) {
        for antialias in [false, true] {
            let mut scene = fx.scene(ChannelId(0), time.clone(), SIZE);
            scene.rows[0].center = 50.45;
            scene.rows[0].height = 20.3;
            scene.rows[0].px_per_unit = 0.05;
            scene.rows[0].clip = true;
            scene.antialias = antialias;
            let query = query(&fx.recording, &view(&scene), &exact(&fx, &scene));
            let frame = Frame::new(&fx.recording, &scene, &query);
            let image = gpu.render(&mut gpu.renderer(), &frame);
            for x in 0..SIZE[0] {
                if let Some((a, b)) = image.run(x).unwrap() {
                    assert!(
                        40 <= a && b <= 60,
                        "{time:?}, antialias {antialias}: column {x} lights rows {a}..={b}"
                    );
                }
            }
        }
    }
}

#[test]
fn huge_offsets_keep_sub_pixel_precision() {
    let Some(gpu) = gpu() else { return };
    // 24-bit BDF values around a 1e6 µV offset, drawn tall: 0.25 µV per pixel. In plain
    // f32 the offset alone would be off by a sizeable fraction of a pixel.
    let mut rng = Rng::new(3);
    let n = 50_000u64;
    let fx = Fixture::new(
        vec![channel("bdf", 0, 0.031_25, 1.0e6)],
        vec![grid(512.0, n, vec![0..n], SampleType::I32, 0..1)],
        vec![i32_rows(vec![walk(
            &mut rng,
            n as usize,
            5.0,
            7_999_000.0,
            8_000_000.0,
        )])],
    );
    let id = ChannelId(0);
    let (lo, hi) = fx.extent(id);
    assert!(lo > 1.24e6, "{lo}");
    for time in views(fx.recording.duration(), 512.0) {
        let mut scene = fx.scene(id, time, SIZE);
        scene.rows[0].px_per_unit = 4.0;
        scene.rows[0].baseline = (lo + hi) / 2.0;
        check(gpu, &fx, &scene, &exact(&fx, &scene));
    }
}

#[test]
fn extreme_values_stay_off_screen() {
    let Some(gpu) = gpu() else { return };
    // Values near the top of the int32 range around the baseline, and now and then the
    // bottom of the range: far below the screen, never wrapped around onto it.
    let n = 20_000u64;
    let top = f64::from(i32::MAX);
    let mut rng = Rng::new(17);
    let mut values = walk(&mut rng, n as usize, 20.0, top - 2000.0, top - 1.0);
    for i in (500..n as usize).step_by(1000) {
        values[i] = f64::from(i32::MIN);
    }
    let fx = Fixture::new(
        vec![channel("extreme", 0, 1.0, 0.0)],
        vec![grid(100.0, n, vec![0..n], SampleType::I32, 0..1)],
        vec![i32_rows(vec![values])],
    );
    for time in views(fx.recording.duration(), 100.0) {
        let mut scene = fx.scene(ChannelId(0), time, SIZE);
        scene.rows[0].baseline = top - 1000.0;
        scene.rows[0].px_per_unit = 0.05;
        check(gpu, &fx, &scene, &exact(&fx, &scene));
    }
}

#[test]
fn gaps_and_nans_stay_unlit() {
    let Some(gpu) = gpu() else { return };
    // An EDF+D-like grid with gaps, and float samples with NaN runs.
    let n = 120_000u64;
    // One gap starts where tile 3 starts (sample 12,288), so does a NaN in tile 4.
    let segments = vec![
        0..10_000,
        10_001..10_002,
        10_050..12_288,
        12_300..60_000,
        61_000..n,
    ];
    let mut rng = Rng::new(11);
    let mut floats = walk(&mut rng, n as usize, 0.5, -100.0, 100.0);
    for i in (20_000..20_500).chain([30_000, 30_002, 16_384]) {
        floats[i] = f64::NAN;
    }
    let ints = walk(&mut rng, n as usize, 100.0, -20_000.0, 20_000.0);
    let fx = Fixture::new(
        vec![channel("ints", 0, 1.0, 0.0), channel("floats", 1, 1.0, 0.0)],
        vec![
            grid(500.0, n, segments.clone(), SampleType::I16, 0..1),
            grid(500.0, n, vec![0..n], SampleType::F32, 1..2),
        ],
        vec![i16_rows(vec![ints]), f32_rows(vec![floats])],
    );
    for id in [ChannelId(0), ChannelId(1)] {
        let mut times = views(fx.recording.duration(), 500.0);
        // Zoomed in on the gaps, the isolated sample and the NaNs.
        for at in [
            10_001.0, 10_050.0, 12_288.0, 16_384.0, 60_000.0, 20_000.0, 30_001.0,
        ] {
            times.push((at - 15.0) / 500.0..(at + 15.0) / 500.0);
        }
        for time in times {
            let scene = fx.scene(id, time, SIZE);
            check(gpu, &fx, &scene, &exact(&fx, &scene));
        }
    }
    // The long gap is empty when it fills the screen.
    let scene = fx.scene(ChannelId(0), 60_100.0 / 500.0..60_900.0 / 500.0, SIZE);
    let image = check(gpu, &fx, &scene, &exact(&fx, &scene));
    assert!(image.pixels.iter().all(|p| p[0] == 0));
}

#[test]
fn steps_at_tile_boundaries_connect() {
    let Some(gpu) = gpu() else { return };
    // The signal jumps where tiles of levels 1 and 2 meet. Zoomed out, the jump falls
    // between two samples 1/40 pixel apart; whichever column that is must show it, joining
    // the columns on either side.
    let n = 2_000_000u64;
    let boundaries = [131_072u64, 262_144, 524_288, 1_048_576];
    let values = (0..n)
        .map(|i| {
            if boundaries.iter().filter(|&&b| i >= b).count() % 2 == 1 {
                1000.0
            } else {
                0.0
            }
        })
        .collect();
    let fx = Fixture::new(
        vec![channel("steps", 0, 1.0, 0.0)],
        vec![grid(1000.0, n, vec![0..n], SampleType::I16, 0..1)],
        vec![i16_rows(vec![values])],
    );
    let id = ChannelId(0);
    for samples_per_px in [40.0, 170.0] {
        let span = f64::from(SIZE[0]) * samples_per_px / 1000.0;
        for boundary in boundaries {
            // The jump just before, at and just after the edge of column 100.
            for x in [99.999, 100.0, 100.001] {
                let start = (boundary as f64 - x * samples_per_px) / 1000.0;
                let scene = fx.scene(id, start..start + span, SIZE);
                let level = level_for(fx.grid_of(id), &view(&scene)).get();
                assert!(level >= 1, "{level}");
                check(gpu, &fx, &scene, &exact(&fx, &scene));
            }
        }
    }
}

#[test]
fn isolated_samples_on_column_edges_show() {
    let Some(gpu) = gpu() else { return };
    // Every fifth sample stands alone between gaps. Zoomed in at whole and half pixels per
    // sample, many land exactly on the edge between two columns; f32 rounding mustn't lose
    // them to both.
    let n = 50_000u64;
    let segments = (0..n).step_by(5).map(|i| i..i + 1).collect();
    let mut rng = Rng::new(23);
    let fx = Fixture::new(
        vec![channel("dots", 0, 1.0, 0.0)],
        vec![grid(1000.0, n, segments, SampleType::I16, 0..1)],
        vec![i16_rows(vec![walk(
            &mut rng, n as usize, 200.0, -10_000.0, 10_000.0,
        )])],
    );
    let id = ChannelId(0);
    let mut seen = 0;
    for px_per_sample in [1.0, 1.5, 2.0, 3.0, 7.0, 0.5] {
        for first in [0u64, 4096 - 101, 17_003, 33_333] {
            let start = first as f64 / 1000.0;
            let span = f64::from(SIZE[0]) / px_per_sample / 1000.0;
            let scene = fx.scene(id, start..start + span, SIZE);
            let image = check(gpu, &fx, &scene, &exact(&fx, &scene));
            let row = &scene.rows[0];
            for i in (first..first + (span * 1000.0) as u64).filter(|i| i % 5 == 0) {
                let x = (i - first) as f64 * px_per_sample;
                let y = row.center - (fx.physical(id, i).unwrap() - row.baseline) * row.px_per_unit;
                if x.fract() != 0.0 || x < 1.0 || !(1.0..f64::from(SIZE[1]) - 1.0).contains(&y) {
                    continue;
                }
                seen += 1;
                let (column, r) = (x as u32, y.floor() as u32);
                let lit = |c: u32| (r.saturating_sub(1)..=r + 1).any(|r| image.trace(c, r));
                assert!(
                    lit(column) || lit(column - 1),
                    "sample {i} at x {x}, y {y} vanished ({px_per_sample} px per sample)"
                );
            }
        }
    }
    assert!(seen > 500, "{seen}");
}

/// The ancestor `levels` above `key`.
fn ancestor(key: TileKey, levels: u8) -> TileKey {
    (0..levels).fold(key, |k, _| k.parent().unwrap())
}

#[test]
fn a_coarser_tile_stands_in_for_missing_ones() {
    let Some(gpu) = gpu() else { return };
    let fx = mixed();
    for id in [ChannelId(0), ChannelId(3)] {
        let rate = fx.grid_of(id).rate();
        for time in views(fx.recording.duration(), rate) {
            let scene = fx.scene(id, time, SIZE);
            let wanted = tiles_for_view(&fx.recording, &view(&scene));
            let top = max_level(fx.grid_of(id).len());
            for up in [1, 2] {
                // Every other wanted tile, and coarser ones for the rest.
                let keys = wanted.iter().enumerate().map(|(i, &k)| {
                    if i % 2 == 0 && k.level.get() + up <= top.get() {
                        ancestor(k, up)
                    } else {
                        k
                    }
                });
                let cache = fx.cache(keys);
                check(gpu, &fx, &scene, &cache);
            }
        }
    }
}

#[test]
fn loading_parts_are_hatched() {
    let Some(gpu) = gpu() else { return };
    let fx = mixed();
    let id = ChannelId(0);
    // Three tiles at level 0; the middle one is missing.
    let start = 2.0;
    let scene = fx.scene(id, start..start + 2.0 * TILE_LEN as f64 / 1000.0, SIZE);
    let wanted = tiles_for_view(&fx.recording, &view(&scene));
    assert_eq!(wanted.len(), 3);
    let cache = fx.cache([wanted[0], wanted[2]]);
    // Where the missing tile starts and ends.
    let x = |t: f64| (t - start) / (scene.time.end - start) * f64::from(SIZE[0]);
    let (a, b) = (
        x(TILE_LEN as f64 / 1000.0),
        x(2.0 * TILE_LEN as f64 / 1000.0),
    );
    let image = check_some(
        gpu,
        &fx,
        &scene,
        &cache,
        &[0..a as u32 - 1, b as u32 + 2..SIZE[0]],
    );

    for x in 0..SIZE[0] {
        // Pixel centers decide.
        let center = f64::from(x) + 0.5;
        let missing = a <= center && center <= b;
        for y in 0..SIZE[1] {
            // Diagonal lines every 6 pixels, over the whole row.
            let line = (x + y) % 6 == 0;
            assert_eq!(image.hatch(x, y), missing && line, "hatch at ({x}, {y})");
            if a <= f64::from(x) && f64::from(x) + 1.0 <= b {
                assert!(!image.trace(x, y), "trace at ({x}, {y}) in {a}..{b}");
            }
        }
    }
    // 0.5 is 127.5 in 8 bits, which drivers may round either way.
    let x = a.ceil() as u32 + 1;
    let pixel = image.at(x, (6 - x % 6) % 6);
    let expected = HATCH.map(|c| c * 255.0);
    assert!(
        pixel
            .iter()
            .zip(expected)
            .all(|(&p, e)| (f32::from(p) - e).abs() <= 1.0),
        "hatch color {pixel:?}, expected {expected:?}"
    );
}

#[test]
fn the_gpu_budget_evicts_old_tiles() {
    let Some(gpu) = gpu() else { return };
    let fx = mixed();
    let id = ChannelId(0);
    let frame = |time: std::ops::Range<f64>| {
        let scene = fx.scene(id, time, SIZE);
        let cache = exact(&fx, &scene);
        let query = query(&fx.recording, &view(&scene), &cache);
        Frame::new(&fx.recording, &scene, &query)
    };
    let a = frame(10.0..20.0);
    let b = frame(100.0..110.0);
    let one_tile = 4096 / 32 * 4 + 2 * 4096 * 2;

    let mut renderer = gpu.renderer();
    let first = gpu.render(&mut renderer, &a);
    let after_a = renderer.gpu_bytes();
    assert!(after_a >= one_tile);

    // A budget too small for even one frame keeps that frame's tiles and nothing else.
    renderer.set_budget(1);
    let _ = gpu.render(&mut renderer, &b);
    assert_eq!(renderer.resident_tiles(), b.draws.len());
    let again = gpu.render(&mut renderer, &a);
    assert_eq!(renderer.resident_tiles(), a.draws.len());
    assert!(
        first == again,
        "the frame changed after evicting and uploading again"
    );

    // Enough room for both keeps both.
    renderer.set_budget(usize::MAX);
    let _ = gpu.render(&mut renderer, &b);
    assert_eq!(renderer.resident_tiles(), a.draws.len() + b.draws.len());
    renderer.clear();
    assert_eq!(renderer.gpu_bytes(), 0);
}
