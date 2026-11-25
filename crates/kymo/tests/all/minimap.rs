//! The minimap's geometry and pixels: its height, where times and the window lie on it,
//! where a press moves the window, how its row of pixels lays the marks, and how bright the
//! signals' rows are and which texture rows they and the channels' marks lie in.

use eframe::egui::{Color32, Rangef};
use kymo::annotations::{AnnotationColor, Mark, Marks, Span};
use kymo::ui::minimap::{
    DEFAULT_HEIGHT, MAX_FRACTION, MIN_HEIGHT, MIN_WINDOW, height, levels, pixels, row_pixels,
    signal_pixels, start_for, texture_row, texture_rows, time_at, window_on, x_at,
};
use proptest::prelude::*;

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

#[test]
fn the_row_of_pixels_lays_lines_over_bands() {
    let background = Color32::from_rgb(10, 10, 10);
    let red = Color32::from_rgb(200, 0, 0);
    let green = Color32::from_rgb(0, 200, 0);
    let mark = |columns, color| Mark {
        columns,
        span: Span::All,
        color,
    };
    let marks = Marks {
        bands: vec![
            mark(2..6, AnnotationColor::Palette(0)),
            mark(5..8, AnnotationColor::Palette(1)),
        ],
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
    let row = pixels(&marks, 10, background, color_of);
    assert_eq!(row.len(), 10);
    let band = row[2];
    assert_eq!(row[3], band);
    // Translucent, so the background shows through.
    assert!(band.r() < red.r() && band.r() > background.r(), "{band:?}");
    // A line is solid, over the band.
    assert_eq!(row[4], green);
    // Where two bands overlap, both show.
    assert_ne!(row[5], band);
    assert_ne!(row[5], row[6]);
    assert_eq!(row[7], row[6]);
    for c in [0, 1, 8, 9] {
        assert_eq!(row[c], background, "{c}");
    }
}

#[test]
fn levels_follow_the_spread_over_the_channels_median() {
    // The median is 4: a quarter of it and less is the darkest, eight times it and more the
    // brightest, in octaves between; columns without samples stay NaN.
    let got = levels(&[4.0, 1.0, 0.0, f32::NAN, 32.0, 100.0, 8.0, 4.0, 2.0]);
    assert_eq!(got[0], 0.4);
    assert_eq!(got[1], 0.0);
    assert_eq!(got[2], 0.0);
    assert!(got[3].is_nan());
    assert_eq!(got[4], 1.0);
    assert_eq!(got[5], 1.0);
    assert_eq!(got[6], 0.6);
    assert_eq!(got[8], 0.2);
    // Gain and offset don't matter: the spread is in the channel's own units.
    let scaled: Vec<f32> = [4.0, 1.0, 32.0, 8.0, 2.0].map(|v: f32| v * 1e-6).to_vec();
    assert_eq!(levels(&scaled), levels(&[4.0, 1.0, 32.0, 8.0, 2.0]));
    // A channel flat mostly (a stim channel) is the brightest where it spreads at all.
    assert_eq!(levels(&[0.0, 0.0, 3.0, 0.0]), [0.0, 0.0, 1.0, 0.0]);
    assert!(levels(&[f32::NAN, f32::NAN]).iter().all(|v| v.is_nan()));
    assert!(levels(&[]).is_empty());
}

#[test]
fn rows_share_texture_rows_only_where_they_outnumber_the_pixels() {
    assert_eq!(texture_rows(4, 48), 4);
    assert_eq!(texture_rows(256, 48), 48);
    assert_eq!(texture_rows(0, 48), 0);
    assert_eq!(texture_rows(3, 0), 1);
    // One texture row per row.
    assert_eq!(
        (0..4).map(|r| texture_row(r, 4, 4)).collect::<Vec<_>>(),
        [0, 1, 2, 3]
    );
    // Every texture row gets rows, in order.
    let rows: Vec<usize> = (0..256).map(|r| texture_row(r, 256, 48)).collect();
    assert!(rows.windows(2).all(|w| w[1] == w[0] || w[1] == w[0] + 1));
    assert_eq!((rows[0], rows[255]), (0, 47));
}

#[test]
fn signal_rows_show_the_brightest_of_their_channels_and_gaps_empty() {
    let background = Color32::from_rgb(10, 10, 10);
    let dark = Color32::from_rgb(30, 30, 30);
    let bright = Color32::from_rgb(200, 200, 200);
    let a = [0.0, 1.0, f32::NAN, 0.5];
    let b = [1.0, 0.0, f32::NAN, f32::NAN];
    let c = [0.0, 0.0, 0.0, 0.0];
    // Four rows in two texture rows: a and b share the first, c and an unread one the second.
    let pixels = signal_pixels(
        &[Some(&a), Some(&b), Some(&c), None],
        4,
        2,
        background,
        (dark, bright),
    );
    assert_eq!(
        pixels[..4],
        [bright, bright, background, dark.lerp_to_gamma(bright, 0.5)]
    );
    assert_eq!(pixels[4..], [dark; 4]);
    // A row each.
    let pixels = signal_pixels(&[Some(&a), Some(&b)], 4, 2, background, (dark, bright));
    assert_eq!(pixels[4..], [bright, dark, background, background]);
}

#[test]
fn channel_marks_lie_in_their_rows_and_blend_once_per_color() {
    let red = Color32::from_rgb(200, 0, 0);
    let green = Color32::from_rgb(0, 200, 0);
    let mark = |columns, span, color| Mark {
        columns,
        span,
        color,
    };
    let marks = Marks {
        bands: vec![
            // Rows 0 and 1 share the first texture row: their bands of one color blend
            // once where they overlap.
            mark(0..3, Span::Row(0), AnnotationColor::Palette(0)),
            mark(2..4, Span::Row(1), AnnotationColor::Palette(0)),
            mark(1..2, Span::Row(3), AnnotationColor::Palette(1)),
            // Over the strip's height: the row of pixels has it, not the rows.
            mark(0..6, Span::All, AnnotationColor::Palette(1)),
        ],
        lines: vec![mark(5..6, Span::Row(2), AnnotationColor::Palette(1))],
        labels: vec![],
    };
    let color_of = |c| match c {
        AnnotationColor::Palette(0) => red,
        _ => green,
    };
    let pixels = row_pixels(&marks, 4, 6, 2, color_of);
    let band = Color32::TRANSPARENT.blend(red.gamma_multiply(0.5));
    let none = Color32::TRANSPARENT;
    assert_eq!(pixels[..6], [band, band, band, band, none, none]);
    let green_band = Color32::TRANSPARENT.blend(green.gamma_multiply(0.5));
    assert_eq!(pixels[6..], [none, green_band, none, none, none, green]);
}
