//! The scrollbar's geometry and the status line's texts.

use std::sync::Arc;

use eframe::egui::Rangef;
use kymo::ui::axis::Clock;
use kymo::ui::readout::Reading;
use kymo::ui::scrollbar::{MIN_THUMB, start_at, thumb};
use kymo::ui::status::{
    annotations_text, build_text, format_value, pointer_text, range_decimals, range_text,
    reading_text, selection_text, time_decimals,
};
use kymo::ui::stepper::{format_number, parse_number, step_by};
use kymo_core::service::BuildProgress;
use proptest::prelude::*;

proptest! {
    #[test]
    fn the_thumb_shows_the_visible_part_and_moving_it_round_trips(
        track_start in -100.0..100.0f32,
        track_len in 30.0..2000.0f32,
        total in 1.0..1e6f64,
        share in 0.0..1.2f64,
        at in 0.0..1.0f64,
    ) {
        let track = Rangef::new(track_start, track_start + track_len);
        let len = total * share;
        let start = (total - len).max(0.0) * at;
        let visible = start..start + len;
        let t = thumb(track, total, &visible);
        prop_assert!(t.min >= track.min - 1e-3 && t.max <= track.max + 1e-3, "{t:?} in {track:?}");
        if len >= total {
            prop_assert_eq!(t, track);
            return Ok(());
        }
        prop_assert!(t.span() >= MIN_THUMB - 1e-3);
        let natural = (len / total) as f32 * track_len;
        if natural >= MIN_THUMB {
            prop_assert!((t.span() - natural).abs() < 1e-2 * natural.max(1.0));
        }
        // Moving the thumb to where it is gives the same start back, as exactly as an f32
        // position can say where on its travel the thumb is.
        let back = start_at(track, total, len, t.min);
        let travel = f64::from(track.span() - t.span());
        let precision = 4.0 * f64::from(f32::EPSILON * track.min.abs().max(track.max.abs()));
        let share = if travel > 0.0 { (precision / travel).max(1e-4) } else { 1.0 };
        prop_assert!((back - start).abs() <= (total - len) * share + 1e-9, "{back} vs {start}");
        // Its ends are the ends of the range.
        prop_assert_eq!(start_at(track, total, len, track.min - 50.0), 0.0);
        let end = start_at(track, total, len, track.max + 50.0);
        prop_assert!((end - (total - len)).abs() <= (total - len) * 1e-9);
    }
}

#[test]
fn thumbs_move_with_the_view() {
    let track = Rangef::new(0.0, 100.0);
    assert_eq!(thumb(track, 100.0, &(0.0..50.0)), Rangef::new(0.0, 50.0));
    assert_eq!(
        thumb(track, 100.0, &(50.0..100.0)),
        Rangef::new(50.0, 100.0)
    );
    assert_eq!(thumb(track, 100.0, &(25.0..75.0)), Rangef::new(25.0, 75.0));
    // A sliver of a long recording still gets a grabbable thumb, at the far end at the end.
    let t = thumb(track, 3600.0, &(3599.0..3600.0));
    assert_eq!(t, Rangef::new(100.0 - MIN_THUMB, 100.0));
}

#[test]
fn values_keep_four_significant_digits() {
    assert_eq!(format_value(-12.3456, "µV"), "−12.35 µV");
    assert_eq!(format_value(0.0012346, "V"), "0.001235 V");
    assert_eq!(format_value(1_000_123.4, "uV"), "1000123 uV");
    assert_eq!(format_value(0.0, "uV"), "0 uV");
    assert_eq!(format_value(-0.000_000_000_000_1, ""), "0.000000000000");
    assert_eq!(format_value(5.0, ""), "5.000");
    assert_eq!(
        reading_text(
            Reading::Range {
                min: -20.1,
                max: 15.27
            },
            "uV"
        ),
        "−20.10 … 15.27 uV"
    );
}

#[test]
fn the_range_text_shows_the_window_to_a_hundredth_of_its_span() {
    assert_eq!(range_decimals(20.0), 1);
    assert_eq!(range_decimals(10.0), 1);
    assert_eq!(range_decimals(5.0), 2);
    assert_eq!(range_decimals(100.0), 0);
    assert_eq!(range_decimals(3600.0), 0);
    assert_eq!(
        range_text(12.5, 32.5, 1, Clock::Minutes),
        "0:12.5 to 0:32.5"
    );
    assert_eq!(
        range_text(3600.0, 3900.0, 0, Clock::Hours),
        "1:00:00 to 1:05:00"
    );
    assert_eq!(range_text(0.0, 2.5, 2, Clock::Seconds), "0.00 s to 2.50 s");
}

#[test]
fn the_pointer_text_names_time_channel_and_value() {
    assert_eq!(
        pointer_text(
            10.771,
            3,
            Clock::Hours,
            Some(("AF7", Some("−21235 uV".into())))
        ),
        "0:00:10.771 · AF7 −21235 uV"
    );
    assert_eq!(
        pointer_text(1.5, 3, Clock::Seconds, Some(("Cz", None))),
        "1.500 s · Cz"
    );
    assert_eq!(pointer_text(1.5, 0, Clock::Seconds, None), "2 s");
    assert_eq!(time_decimals(512.0), 3);
    assert_eq!(time_decimals(250.0), 3);
    assert_eq!(time_decimals(1000.0), 3);
    assert_eq!(time_decimals(10.0), 1);
    assert_eq!(time_decimals(1e9), 6);
}

#[test]
fn annotations_under_the_pointer_are_named_once_each() {
    assert_eq!(annotations_text([]), "");
    assert_eq!(annotations_text(["N2"]), "N2");
    assert_eq!(
        annotations_text(["spike", "N2", "spike", "spike"]),
        "spike ×3, N2"
    );
    assert_eq!(
        annotations_text(["a", "b", "a", "c", "d", "e", "d"]),
        "a ×2, b, c, +2 more"
    );
}

#[test]
fn build_progress_and_memory() {
    let progress = |done, total, complete| BuildProgress {
        done,
        total,
        complete,
        error: None,
    };
    assert_eq!(
        build_text(&progress(429, 1000, false)).as_deref(),
        Some("Building overview 42%")
    );
    assert_eq!(
        build_text(&progress(100, 100, false)).as_deref(),
        Some("Building overview 99%"),
        "100% only once complete"
    );
    assert_eq!(build_text(&progress(100, 100, true)), None);
    let failed = BuildProgress {
        error: Some(Arc::new(kymo_core::pyramid::SidecarError::Io {
            path: "x.kymo".into(),
            source: std::io::Error::other("disk full"),
        })),
        ..progress(10, 100, false)
    };
    let text = build_text(&failed).unwrap();
    assert!(text.starts_with("Overview stopped: "), "{text}");
    assert!(text.contains("disk full"), "{text}");
}

#[test]
fn the_selection_text_writes_lengths_on_their_own_clock() {
    let text = |onset, duration| selection_text("BAD", onset, duration, 3, Clock::Hours);
    assert_eq!(text(2.0, 2.0), "BAD at 0:00:02.000 for 2.000 s");
    assert_eq!(text(150.0, 95.0), "BAD at 0:02:30.000 for 1:35.000");
    assert_eq!(text(150.0, 0.0), "BAD at 0:02:30.000");
}

#[test]
fn steppers_step_to_multiples() {
    assert_eq!(step_by(20.0, 1.0, 1.0), 21.0);
    assert_eq!(step_by(20.0, 1.0, -1.0), 19.0);
    assert_eq!(step_by(20.0, 1.0, 3.0), 23.0);
    // Between two multiples: to the next one first.
    assert_eq!(step_by(7.5, 1.0, 1.0), 8.0);
    assert_eq!(step_by(7.5, 1.0, -1.0), 7.0);
    assert_eq!(step_by(7.5, 1.0, -2.0), 6.0);
    // Within rounding of a multiple, at it.
    assert!((step_by(0.1 + 0.2, 0.1, 1.0) - 0.4).abs() < 1e-12);
}

#[test]
fn stepper_fields_read_and_show_numbers() {
    assert_eq!(format_number(20.0), "20");
    assert_eq!(format_number(7.5), "7.5");
    assert_eq!(format_number(1.0 / 3.0), "0.333");
    assert_eq!(format_number(-0.0001), "0");
    assert_eq!(parse_number(" 12.5 "), Some(12.5));
    assert_eq!(parse_number("12,5"), Some(12.5));
    assert_eq!(parse_number(""), None);
    assert_eq!(parse_number("abc"), None);
    assert_eq!(parse_number("inf"), None);
    assert_eq!(parse_number("NaN"), None);
}
