//! A grid's spread from its pyramid's top level, checked column by column against brute
//! force over the samples.

use std::ops::Range;
use std::sync::Arc;

use kymo_core::pyramid::{Pyramid, PyramidBuilder, Spread};
use kymo_core::testing::{brute_minmax, opened};
use kymo_core::tile::{LEVEL1_BUCKET, Level, max_level};
use kymo_core::{
    Channel, ChannelId, Grid, GridId, Piece, Recording, SampleType, TimeTable, Timebase,
};
use proptest::prelude::*;

use crate::common::{is_present, len_and_segments, mem_data, sample_types, with_nan};

/// How a test grid's samples lie in time.
#[derive(Clone, Debug)]
enum Times {
    Regular {
        start: f64,
    },
    /// Pieces starting at these samples (after 0), each after a jump of this many seconds.
    Segmented(Vec<(u64, f64)>),
    /// Steps between slots, in seconds, cycled.
    Irregular(Vec<f64>),
}

fn times(len: u64) -> impl Strategy<Value = Times> {
    let cuts = proptest::collection::btree_map(1..len.max(2), 0.0f64..100.0, 0..5);
    let steps = proptest::collection::vec(
        proptest::sample::select(vec![0.0, 0.001, 0.01, 0.3, 50.0]),
        1..6,
    );
    prop_oneof![
        prop_oneof![Just(0.0), Just(3.5)].prop_map(|start| Times::Regular { start }),
        cuts.prop_map(|c| Times::Segmented(c.into_iter().collect())),
        steps.prop_map(Times::Irregular),
    ]
}

fn timebase(times: &Times, len: u64) -> Timebase {
    const RATE: f64 = 100.0;
    match times {
        &Times::Regular { start } => Timebase::Regular { rate: RATE, start },
        Times::Segmented(cuts) => {
            let mut pieces = vec![Piece {
                first: 0,
                start: 0.25,
                rate: RATE,
            }];
            for &(first, jump) in cuts.iter().filter(|&&(first, _)| first < len) {
                let prev = *pieces.last().unwrap();
                let rate = if pieces.len() % 2 == 0 { RATE } else { 256.0 };
                pieces.push(Piece {
                    first,
                    start: prev.start + (first - prev.first) as f64 / prev.rate + jump,
                    rate,
                });
            }
            Timebase::Segmented { rate: RATE, pieces }
        }
        Times::Irregular(steps) => {
            let mut t = 1.0;
            let slots: Vec<f64> = (0..len as usize)
                .map(|i| {
                    let now = t;
                    t += steps[i % steps.len()];
                    now
                })
                .collect();
            Timebase::Irregular {
                rate: RATE,
                hold: false,
                times: Arc::new(TimeTable::new(&slots, t + 0.01).unwrap()),
            }
        }
    }
}

/// The spread as the doc of [`Spread::columns`] defines it, sample by sample: each present
/// sample's period (to the next sample's time, on its own piece's line where the time base
/// has pieces) lies in columns, which take the range of the sample's bucket.
fn brute_columns(rec: &Recording, bucket_len: u64, duration: f64, width: u32) -> Vec<f32> {
    let grid = rec.grid(GridId(0));
    let mut out = vec![f32::NAN; grid.channels().len() * width as usize];
    if width == 0 || duration <= 0.0 {
        return out;
    }
    let w = f64::from(width);
    let end_of = |i: u64| match grid.regular_run(i) {
        Some(run) => run.time_of(i + 1),
        None => grid.time_of(i + 1),
    };
    for (row, &channel) in grid.channels().iter().enumerate() {
        let column = &mut out[row * width as usize..(row + 1) * width as usize];
        for k in 0..grid.len().div_ceil(bucket_len) {
            let samples = k * bucket_len..((k + 1) * bucket_len).min(grid.len());
            let Some((lo, hi)) = brute_minmax(rec, channel, samples.clone()).unwrap() else {
                continue;
            };
            let v = (hi - lo) as f32;
            for i in samples.filter(|&i| is_present(grid.segments(), i)) {
                let x0 = grid.time_of(i) / duration * w;
                let x1 = end_of(i) / duration * w;
                let first = x0.floor().clamp(0.0, w - 1.0) as usize;
                let end = (x1.ceil().clamp(0.0, w) as usize).max(first + 1);
                for c in &mut column[first..end] {
                    if c.is_nan() || v > *c {
                        *c = v;
                    }
                }
            }
        }
    }
    out
}

/// Equal, NaN where the other is NaN.
fn same(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| x == y || x.is_nan() && y.is_nan())
}

fn recording(
    sample_type: SampleType,
    n: usize,
    (len, segments): (u64, Vec<Range<u64>>),
    times: &Times,
    seed: u64,
    nan: bool,
) -> Recording {
    let ids: Vec<ChannelId> = (0..n as u32).map(ChannelId).collect();
    let channels = ids
        .iter()
        .map(|id| Channel::new(format!("ch{}", id.0), GridId(0)))
        .collect();
    let grid = Grid::new(timebase(times, len), len, segments, sample_type, ids).unwrap();
    let data = mem_data(sample_type, n, len, seed);
    let data = if nan { with_nan(data, seed) } else { data };
    Recording::new(opened(channels, vec![grid], vec![data]))
        .unwrap()
        .0
}

/// Raw grids, a few levels, or a second, partial channel block.
fn shapes() -> impl Strategy<Value = (usize, (u64, Vec<Range<u64>>))> {
    prop_oneof![
        (1..=3usize, len_and_segments(5000)),
        (1..=2usize, len_and_segments(400_000)),
        (33..=34usize, len_and_segments(20_000)),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Every column of every channel holds the largest range of the buckets lying in it,
    /// whatever the time base, gaps and missing samples; columns none lies in are NaN.
    #[test]
    fn spread_matches_brute_force(
        sample_type in sample_types(),
        (n, shape) in shapes(),
        times in times(400_000),
        width in prop_oneof![1..=8u32, 1..=3000u32],
        stretch in prop_oneof![Just(1.0), 0.3f64..3.0],
        nan in any::<bool>(),
        seed in any::<u64>(),
    ) {
        let len = shape.0;
        let rec = recording(sample_type, n, shape, &times, seed, nan);
        let pyramid = Pyramid::build(&rec, GridId(0)).unwrap();
        let spread = Spread::read(&rec, &pyramid).unwrap().unwrap();
        prop_assert_eq!(spread.channels(), rec.grid(GridId(0)).channels());
        let bucket_len = match max_level(len) {
            Level::RAW => LEVEL1_BUCKET,
            top => top.bucket_len(),
        };
        let duration = rec.duration() * stretch;
        let got = spread.columns(duration, width);
        let want = brute_columns(&rec, bucket_len, duration, width);
        prop_assert!(same(&got, &want), "got {:?}\nwant {:?}", got, want);
    }
}

#[test]
fn an_incomplete_pyramid_has_no_spread() {
    let rec = recording(
        SampleType::I16,
        2,
        (200_000, vec![0..200_000]),
        &Times::Regular { start: 0.0 },
        7,
        false,
    );
    let mut builder = PyramidBuilder::new(&rec, GridId(0))
        .unwrap()
        .with_chunk_len(10_000);
    assert!(!builder.step().unwrap());
    assert_eq!(Spread::read(&rec, &builder.pyramid()).unwrap(), None);
    let pyramid = builder.finish().unwrap();
    assert!(Spread::read(&rec, &pyramid).unwrap().is_some());
}

#[test]
fn an_empty_grid_has_an_empty_spread() {
    let rec = recording(
        SampleType::F32,
        1,
        (0, vec![]),
        &Times::Regular { start: 0.0 },
        1,
        false,
    );
    let pyramid = Pyramid::build(&rec, GridId(0)).unwrap();
    let spread = Spread::read(&rec, &pyramid).unwrap().unwrap();
    assert!(spread.columns(10.0, 4).iter().all(|v| v.is_nan()));
}
