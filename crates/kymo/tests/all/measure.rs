//! Measurements: ends snapped to the nearest samples, Δt exact to the sample, and the
//! texts of the label and the status line.

use std::sync::Arc;

use kymo::measure::{
    Measurement, Span, amplitude_text, details_text, duration_text, frequency_text, nearest_sample,
};
use kymo::ui::axis::Clock;
use kymo::ui::readout::Reading;
use kymo_core::{ChannelId, Grid, Piece, SampleType, TimeTable, Timebase};
use proptest::prelude::*;

fn regular(rate: f64, start: f64, len: u64) -> Grid {
    Grid::continuous(
        Timebase::Regular { rate, start },
        len,
        SampleType::I16,
        vec![ChannelId(0)],
    )
    .unwrap()
}

fn measure(grid: &Grid, press: f64, reach: f64) -> Span {
    Measurement {
        channel: ChannelId(0),
        press,
        reach,
    }
    .span_on(grid)
}

proptest! {
    /// On a regular grid the ends are the samples nearest to the times (the first or
    /// last beyond them), and Δt is their distance in samples over the rate, either way.
    #[test]
    fn ends_snap_to_the_nearest_samples(
        rate in prop_oneof![Just(256.0), Just(1000.0), Just(2048.0), 0.5f64..5000.0],
        start in prop_oneof![Just(0.0), 0.0f64..100.0],
        len in 1u64..1_000_000,
        press in -10.0f64..1000.0,
        reach in -10.0f64..1000.0,
    ) {
        let grid = regular(rate, start, len);
        let span = measure(&grid, press, reach);
        prop_assert_eq!(&span, &measure(&grid, reach, press));
        let first = nearest_sample(&grid, press.min(reach));
        let last = nearest_sample(&grid, press.max(reach));
        prop_assert_eq!(span.samples, Some(last - first));
        prop_assert_eq!(span.dt, (last - first) as f64 / rate);
        prop_assert_eq!(span.start, grid.time_of(first));
        prop_assert_eq!(span.end, grid.time_of(last));
        prop_assert_eq!(span.inside, first..last + 1);
        // Nearest: no other sample is closer to the time.
        for (time, s) in [(press.min(reach), first), (press.max(reach), last)] {
            let off = |n: u64| (grid.time_of(n) - time).abs();
            prop_assert!(s < len);
            if s > 0 {
                prop_assert!(off(s) <= off(s - 1) + 1e-9, "{} vs {}", s, s - 1);
            }
            if s + 1 < len {
                prop_assert!(off(s) <= off(s + 1) + 1e-9, "{} vs {}", s, s + 1);
            }
        }
    }
}

#[test]
fn a_known_interval() {
    // 320 samples at 256 Hz from 2 s on: 1.25 s, 0.8 Hz.
    let grid = regular(256.0, 0.0, 256 * 60);
    // A little off the samples, as a pointer is.
    let span = measure(&grid, 3.251, 2.0013);
    assert_eq!(span.start, 2.0);
    assert_eq!(span.end, 3.25);
    assert_eq!(span.dt, 1.25);
    assert_eq!(span.samples, Some(320));
    assert_eq!(span.inside, 512..833);
    assert_eq!(duration_text(&span), "1.250 s");
    // Three significant digits.
    assert_eq!(frequency_text(&span).unwrap(), "0.800 Hz");
    // Both ends on one sample: no frequency.
    let span = measure(&grid, 2.0001, 2.0002);
    assert_eq!(duration_text(&span), "0 ms");
    assert_eq!(frequency_text(&span), None);
    assert_eq!(span.inside, 512..513);
    // Below a second in ms, to the sample: whole ms at 256 Hz.
    let span = measure(&grid, 2.0, 2.0 + 12.0 / 256.0);
    assert_eq!(duration_text(&span), "47 ms");
    assert_eq!(frequency_text(&span).unwrap(), "21.3 Hz");
    let span = measure(&grid, 2.0, 2.0 + 1.0 / 256.0);
    assert_eq!(frequency_text(&span).unwrap(), "256 Hz");
    // A tenth of a ms at 5 kHz.
    let grid = regular(5000.0, 0.0, 5000 * 60);
    let span = measure(&grid, 1.0, 1.0468);
    assert_eq!(duration_text(&span), "46.8 ms");
    let span = measure(&grid, 1.0, 13.5);
    assert_eq!(duration_text(&span), "12.5000 s");
}

#[test]
fn segmented_grids_measure_across_their_pieces() {
    // 100 samples at 100 Hz from 0 s, then 100 more from 1.5 s.
    let grid = Grid::continuous(
        Timebase::Segmented {
            rate: 100.0,
            pieces: vec![
                Piece {
                    first: 0,
                    start: 0.0,
                    rate: 100.0,
                },
                Piece {
                    first: 100,
                    start: 1.5,
                    rate: 100.0,
                },
            ],
        },
        200,
        SampleType::F32,
        vec![ChannelId(0)],
    )
    .unwrap();
    let within = measure(&grid, 1.5, 1.75);
    assert_eq!((within.samples, within.dt), (Some(25), 0.25));
    let across = measure(&grid, 0.5, 1.6);
    assert_eq!(across.samples, Some(60));
    assert!((across.dt - 1.1).abs() < 1e-12, "{across:?}");
}

#[test]
fn irregular_grids_measure_the_pointer_times() {
    let times = TimeTable::new(&[0.0, 0.3, 0.35, 1.0], 1.1).unwrap();
    let grid = Grid::continuous(
        Timebase::Irregular {
            rate: 4.0,
            hold: false,
            times: Arc::new(times),
        },
        4,
        SampleType::F32,
        vec![ChannelId(0)],
    )
    .unwrap();
    let span = measure(&grid, 0.9, 0.4);
    assert_eq!((span.start, span.end, span.samples), (0.4, 0.9, None));
    assert_eq!(duration_text(&span), "500 ms");
    assert_eq!(frequency_text(&span).unwrap(), "2.00 Hz");
    // No sample between them.
    assert!(span.inside.is_empty(), "{:?}", span.inside);
    // The samples at times between the ends, both included.
    assert_eq!(measure(&grid, 0.3, 1.0).inside, 1..4);
    assert_eq!(measure(&grid, 0.31, 0.99).inside, 2..3);
    assert_eq!(measure(&grid, 0.0, 2.0).inside, 0..4);
    // Before the first sample, none.
    let times = TimeTable::new(&[0.2, 0.3, 1.0], 1.1).unwrap();
    let grid = Grid::continuous(
        Timebase::Irregular {
            rate: 3.0,
            hold: false,
            times: Arc::new(times),
        },
        3,
        SampleType::F32,
        vec![ChannelId(0)],
    )
    .unwrap();
    assert!(measure(&grid, 0.0, 0.1).inside.is_empty());
    assert_eq!(measure(&grid, 0.0, 0.25).inside, 0..1);
}

#[test]
fn amplitudes() {
    assert_eq!(amplitude_text(Some(86.4), "\u{b5}V"), "86.40 \u{b5}V PTP");
    assert_eq!(amplitude_text(Some(5.0), ""), "5.000 PTP");
    assert_eq!(amplitude_text(None, "\u{b5}V"), "\u{2013} PTP");
}

#[test]
fn details_name_the_ends_samples_and_values() {
    let grid = regular(256.0, 0.0, 256 * 60);
    let span = measure(&grid, 12.5, 13.75);
    let v = |x| Some(Reading::Value(x));
    assert_eq!(
        details_text("Fz", &span, Clock::Minutes, "\u{b5}V", [v(12.3), v(-4.1)]),
        "0:12.500 to 0:13.750 \u{b7} 1.250 s (320 samples at 256 Hz) \u{b7} \
         Fz 12.30 \u{b5}V \u{2192} \u{2212}4.100 \u{b5}V, \u{394} \u{2212}16.40 \u{b5}V"
    );
    // An envelope has no difference, nor has a missing end; one sample; a rate in full.
    let range = Some(Reading::Range {
        min: -1.0,
        max: 2.0,
    });
    let grid = regular(0.5, 0.0, 100);
    let span = measure(&grid, 2.0, 4.0);
    assert_eq!(
        details_text("Fz", &span, Clock::Seconds, "\u{b5}V", [range, v(1.0)]),
        "2 s to 4 s \u{b7} 2 s (1 sample at 0.5 Hz) \u{b7} \
         Fz \u{2212}1.000 \u{2026} 2.000 \u{b5}V \u{2192} 1.000 \u{b5}V"
    );
    let grid = regular(1017.25, 0.0, 10_000);
    let span = measure(&grid, 1.0, 2.0);
    let text = details_text("STI", &span, Clock::Seconds, "", [v(5.0), None]);
    assert!(text.contains(" at 1017.25 Hz) "), "{text}");
    assert!(
        text.ends_with(" \u{b7} STI 5.000 \u{2192} \u{2013}"),
        "{text}"
    );
    // Irregular: no samples.
    let times = TimeTable::new(&[0.0, 0.3, 0.35, 1.0], 1.1).unwrap();
    let grid = Grid::continuous(
        Timebase::Irregular {
            rate: 4.0,
            hold: false,
            times: Arc::new(times),
        },
        4,
        SampleType::F32,
        vec![ChannelId(0)],
    )
    .unwrap();
    let span = measure(&grid, 0.4, 0.9);
    assert_eq!(
        details_text("x", &span, Clock::Seconds, "", [None, None]),
        "0.4 s to 0.9 s \u{b7} 500 ms \u{b7} x \u{2013} \u{2192} \u{2013}"
    );
}

#[test]
fn value_labels_stay_beside_their_points_and_on_screen() {
    use eframe::egui::{Rect, pos2, vec2};
    use kymo::measure::value_label_pos;

    let area = Rect::from_min_max(pos2(0.0, 0.0), pos2(400.0, 300.0));
    let size = vec2(60.0, 16.0);
    // Above and right of the point.
    assert_eq!(
        value_label_pos(pos2(100.0, 100.0), size, area, 4.0),
        pos2(104.0, 80.0)
    );
    // Left of it near the right edge, below it near the top.
    assert_eq!(
        value_label_pos(pos2(380.0, 100.0), size, area, 4.0),
        pos2(316.0, 80.0)
    );
    assert_eq!(
        value_label_pos(pos2(100.0, 10.0), size, area, 4.0),
        pos2(104.0, 14.0)
    );
    // Within the area as far as it fits, wherever the point is.
    for anchor in [pos2(0.0, 0.0), pos2(400.0, 300.0), pos2(30.0, 295.0)] {
        let at = value_label_pos(anchor, size, area, 4.0);
        assert!(
            area.contains_rect(Rect::from_min_size(at, size)),
            "{anchor:?}"
        );
    }
}
