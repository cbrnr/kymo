//! The minimap's geometry: its height, where times and the window lie on it, where a press
//! moves the window, and which columns show gaps (against brute force over the samples).

use std::ops::Range;

use eframe::egui::{Color32, Rangef};
use kymo::annotations::{AnnotationColor, Mark, Marks, Span};
use kymo::ui::minimap::{
    DEFAULT_HEIGHT, MAX_FRACTION, MIN_HEIGHT, MIN_WINDOW, gaps, height, pixels, start_for, time_at,
    window_on, x_at,
};
use kymo_core::testing::{MemData, opened};
use kymo_core::{Recording, SampleType};
use proptest::prelude::*;

use crate::common::{channel, grid};

#[test]
fn the_height_stays_within_its_limits() {
    assert_eq!(height(60.0, 800.0), 60.0);
    assert_eq!(height(10.0, 800.0), MIN_HEIGHT);
    assert_eq!(height(500.0, 800.0), 800.0 * MAX_FRACTION);
    // Too short a region for both: the lowest height wins.
    assert_eq!(height(60.0, 40.0), MIN_HEIGHT);
    assert_eq!(height(f32::NAN, 800.0), DEFAULT_HEIGHT);
    assert_eq!(height(f32::INFINITY, 800.0), 800.0 * MAX_FRACTION);
}

#[test]
fn times_and_positions_map_to_each_other() {
    assert_eq!(x_at(150.0, 600.0, 800.0), 200.0);
    assert_eq!(time_at(200.0, 600.0, 800.0), 150.0);
    for t in [0.0, 0.1, 299.7, 600.0] {
        let back = time_at(x_at(t, 600.0, 813.0), 600.0, 813.0);
        assert!((back - t).abs() < 1e-9, "{t} came back as {back}");
    }
    // Empty recordings and strips put everything at the start.
    assert_eq!(x_at(5.0, 0.0, 800.0), 0.0);
    assert_eq!(time_at(5.0, 600.0, 0.0), 0.0);
}

#[test]
fn the_window_lies_where_its_times_are_and_stays_visible() {
    let track = Rangef::new(100.0, 700.0);
    // 60 s of 600 s from 300 s: a tenth of the track, from its middle.
    let w = window_on(track, 600.0, &(300.0..360.0), 1.0);
    assert_eq!((w.min, w.max), (400.0, 460.0));
    // 1 s of an hour: widened around its middle to the narrowest it gets.
    let w = window_on(track, 3600.0, &(1800.0..1801.0), 1.0);
    assert_eq!(w.span(), MIN_WINDOW);
    assert!((w.center() - 400.0).abs() <= 0.5, "{w:?}");
    // At either end, widened inward.
    let w = window_on(track, 3600.0, &(0.0..1.0), 1.0);
    assert_eq!((w.min, w.span()), (track.min, MIN_WINDOW));
    let w = window_on(track, 3600.0, &(3599.0..3600.0), 1.0);
    assert_eq!(w.max, track.max);
    // The whole recording: the whole track.
    assert_eq!(window_on(track, 600.0, &(0.0..600.0), 1.0), track);
    // A track narrower than the narrowest window: all of it.
    let narrow = Rangef::new(10.0, 13.0);
    assert_eq!(window_on(narrow, 600.0, &(0.0..1.0), 1.0), narrow);
}

proptest! {
    /// Moving the window keeps its width, and its ends stay on physical pixels.
    #[test]
    fn the_window_keeps_its_width_as_it_moves(
        span in 0.5..600.0f64,
        starts in proptest::collection::vec(0.0..1.0f64, 2..20),
        ppp in prop_oneof![Just(1.0f32), Just(1.5f32), Just(2.0f32)],
        width_px in 50..2000u32,
    ) {
        let duration = 3600.0;
        let track = Rangef::new(100.0, 100.0 + width_px as f32 / ppp);
        // In whole physical pixels: the ends' points are k / ppp, not exact in f32.
        let widths: Vec<f32> = starts
            .iter()
            .map(|s| {
                let start = s * (duration - span);
                let w = window_on(track, duration, &(start..start + span), ppp);
                for x in [w.min, w.max] {
                    prop_assert!(((x * ppp) - (x * ppp).round()).abs() < 1e-3, "{w:?}");
                }
                prop_assert!(track.min <= w.min && w.max <= track.max, "{w:?}");
                Ok((w.span() * ppp).round())
            })
            .collect::<Result<_, _>>()?;
        prop_assert!(widths.windows(2).all(|p| p[0] == p[1]), "{widths:?}");
    }
}

#[test]
fn a_press_moves_the_window_as_far_as_the_recording_goes() {
    // Half the span centers the window on the time.
    assert_eq!(start_for(300.0, 10.0, 20.0, 600.0), 290.0);
    // Near the ends, it stops there.
    assert_eq!(start_for(5.0, 10.0, 20.0, 600.0), 0.0);
    assert_eq!(start_for(595.0, 10.0, 20.0, 600.0), 580.0);
    // Where the window was grabbed stays under the pointer.
    assert_eq!(start_for(300.0, 3.0, 20.0, 600.0), 297.0);
    // A window as long as the recording, or longer, starts at its start.
    assert_eq!(start_for(300.0, 300.0, 600.0, 600.0), 0.0);
    assert_eq!(start_for(300.0, 400.0, 800.0, 600.0), 0.0);
    assert_eq!(start_for(f64::NAN, 10.0, 20.0, 600.0), 0.0);
}

/// Present samples as sorted, disjoint ranges of a grid `len` long, from random cut points.
fn segments(len: u64) -> impl Strategy<Value = Vec<Range<u64>>> {
    proptest::collection::btree_set(0..len, 0..12).prop_map(move |cuts| {
        let mut cuts: Vec<u64> = cuts.into_iter().collect();
        cuts.insert(0, 0);
        cuts.push(len);
        cuts.windows(2)
            .enumerate()
            .filter(|(i, w)| i % 2 == 0 && w[0] < w[1])
            .map(|(_, w)| w[0]..w[1])
            .collect()
    })
}

/// The columns no present sample of any grid touches: a sample takes its period from its
/// time, and touches every column that period overlaps (at least the column it starts in).
fn brute_gaps(rec: &Recording, width: u32) -> Vec<u32> {
    let duration = rec.duration();
    let w = f64::from(width);
    let mut covered = vec![false; width as usize];
    for grid in rec.grids() {
        for segment in grid.segments() {
            for n in segment.clone() {
                let t = grid.time_of(n);
                let x0 = x_at(t, duration, w);
                let x1 = x_at(t + 1.0 / grid.rate(), duration, w);
                for (c, covered) in covered.iter_mut().enumerate() {
                    let c = c as f64;
                    if (c < x1 && c + 1.0 > x0) || c == x0.floor() {
                        *covered = true;
                    }
                }
            }
        }
    }
    (0..width).filter(|&c| !covered[c as usize]).collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Two grids at different rates with gaps of their own: a column shows a gap exactly
    /// where neither has a sample, in runs that neither touch nor overlap.
    #[test]
    fn gaps_are_where_no_grid_has_samples(
        a in segments(600),
        b in segments(222),
        width in 1..400u32,
    ) {
        let grids = vec![
            grid(100.0, 600, a, SampleType::I16, 0..1),
            grid(37.0, 222, b, SampleType::I16, 1..2),
        ];
        let channels = vec![channel("a", 0, 1.0, 0.0), channel("b", 1, 1.0, 0.0)];
        let data = vec![MemData::I16(vec![vec![0; 600]]), MemData::I16(vec![vec![0; 222]])];
        let Ok((rec, _)) = Recording::new(opened(channels, grids, data)) else {
            // Both grids empty.
            return Ok(());
        };
        let runs = gaps(&rec, rec.duration(), width);
        for pair in runs.windows(2) {
            prop_assert!(pair[0].end < pair[1].start, "{runs:?}");
        }
        let got: Vec<u32> = runs.into_iter().flatten().collect();
        prop_assert_eq!(got, brute_gaps(&rec, width));
    }
}

#[test]
fn a_recording_without_gaps_shows_none() {
    let grids = vec![grid(100.0, 6000, vec![0..6000], SampleType::I16, 0..1)];
    let channels = vec![channel("a", 0, 1.0, 0.0)];
    let data = vec![MemData::I16(vec![vec![0; 6000]])];
    let (rec, _) = Recording::new(opened(channels, grids, data)).unwrap();
    for width in [1, 7, 800, 6000, 9000] {
        assert!(gaps(&rec, rec.duration(), width).is_empty(), "{width}");
    }
}

#[test]
fn the_row_of_pixels_lays_lines_over_bands_over_gaps() {
    let background = Color32::from_rgb(10, 10, 10);
    let gap = Color32::from_rgba_premultiplied(40, 40, 40, 128);
    let red = Color32::from_rgb(200, 0, 0);
    let green = Color32::from_rgb(0, 200, 0);
    let mark = |columns, color| Mark {
        columns,
        span: Span::All,
        color,
    };
    let marks = Marks {
        bands: vec![mark(2..6, AnnotationColor::Palette(0))],
        // One beyond the strip, which leaves it alone.
        lines: vec![
            mark(4..5, AnnotationColor::Palette(1)),
            mark(12..13, AnnotationColor::Palette(1)),
        ],
        labels: vec![],
    };
    let color_of = |c| match c {
        AnnotationColor::Palette(0) => red,
        _ => green,
    };
    let row = pixels(&marks, &[5..8], 10, background, gap, color_of);
    assert_eq!(row.len(), 10);
    let band = row[2];
    assert_eq!(row[3], band);
    assert_ne!(band, background);
    // Translucent, so the background shows through, the gap too.
    assert!(band.r() < red.r() && band.r() > background.r(), "{band:?}");
    assert_eq!(row[4], green);
    // The band over the gap: neither alone.
    assert_ne!(row[5], band);
    assert_ne!(row[5], background.blend(gap));
    assert_eq!(row[6], background.blend(gap));
    assert_eq!(row[7], row[6]);
    for c in [0, 1, 8, 9] {
        assert_eq!(row[c], background, "{c}");
    }
}
