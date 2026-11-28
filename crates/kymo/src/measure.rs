//! Measuring by ⇧-dragging the traces: the interval between two times, snapped to the
//! samples of the channel the drag started on, and that channel's readings at both ends.
//! A measurement is display state: never saved, gone with the recording.

use std::ops::Range;

use eframe::egui::{Pos2, Rect, Vec2, pos2};
use kymo_core::{ChannelId, Grid, Recording};

use crate::ui::axis::{Clock, format_time};
use crate::ui::readout::Reading;
use crate::ui::status::{format_significant, format_value, reading_text, time_decimals};

/// What an end without a reading shows (off screen, not cached yet, in a gap).
pub const NO_READING: &str = "\u{2013}";

/// A measurement from the time a ⇧-drag pressed at to the time it reached (in either
/// order), on the grid of `channel`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Measurement {
    pub channel: ChannelId,
    pub press: f64,
    pub reach: f64,
}

/// A measurement's ends as measured: on a grid with sample times, the samples nearest to
/// its times.
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    /// The earlier end's time and the later one's, in seconds from the start.
    pub start: f64,
    pub end: f64,
    /// `end - start`: on a regular run, the samples between the ends over its rate, exact
    /// to the sample.
    pub dt: f64,
    /// The samples between the ends (the later's index less the earlier's); `None` on an
    /// irregular grid, whose ends are the pointer's times.
    pub samples: Option<u64>,
    /// The grid's rate (an irregular grid's average), for the time's decimals.
    pub rate: f64,
    /// The samples from the earlier end to the later, both included; on an irregular grid,
    /// those at times between them.
    pub inside: Range<u64>,
}

impl Measurement {
    pub fn span(&self, recording: &Recording) -> Span {
        self.span_on(recording.grid(recording.channel(self.channel).grid))
    }

    /// [`span`](Self::span) on `grid`, its channel's.
    pub fn span_on(&self, grid: &Grid) -> Span {
        let (a, b) = (self.press.min(self.reach), self.press.max(self.reach));
        if grid.is_empty() {
            return Span {
                start: a,
                end: b,
                dt: b - a,
                samples: None,
                rate: grid.rate(),
                inside: 0..0,
            };
        }
        if grid.times().is_some() {
            let len = grid.len();
            let mut first = grid.sample_at(a).min(len);
            if first < len && grid.time_of(first) < a {
                first += 1;
            }
            let mut end = grid.sample_at(b).min(len - 1);
            end = if grid.time_of(end) <= b { end + 1 } else { end };
            return Span {
                start: a,
                end: b,
                dt: b - a,
                samples: None,
                rate: grid.rate(),
                inside: first..end.max(first),
            };
        }
        let (first, last) = (nearest_sample(grid, a), nearest_sample(grid, b));
        let samples = last - first;
        let dt = match (grid.regular_run(first), grid.regular_run(last)) {
            (Some(run), Some(other)) if run == other => samples as f64 / run.rate,
            // Across pieces of a segmented grid, whose break isn't whole samples.
            _ => grid.time_of(last) - grid.time_of(first),
        };
        Span {
            start: grid.time_of(first),
            end: grid.time_of(last),
            dt,
            samples: Some(samples),
            rate: grid.rate(),
            inside: first..last + 1,
        }
    }
}

/// The time a measurement's end at `time` lies at: its nearest sample's on a grid with
/// sample times, `time` itself on an irregular or empty one.
pub fn snap(grid: &Grid, time: f64) -> f64 {
    if grid.is_empty() || grid.times().is_some() {
        time
    } else {
        grid.time_of(nearest_sample(grid, time))
    }
}

/// Where a value label `size` large goes next to the point `anchor` it reads, `gap` away:
/// its top-left corner. Above and right of the point, else left of it where it would
/// leave `area` on the right, else below it where it would leave it at the top; within
/// `area` as far as it fits.
pub fn value_label_pos(anchor: Pos2, size: Vec2, area: Rect, gap: f32) -> Pos2 {
    let right = anchor.x + gap;
    let x = if right + size.x <= area.right() {
        right
    } else {
        anchor.x - gap - size.x
    };
    let above = anchor.y - gap - size.y;
    let y = if above >= area.top() {
        above
    } else {
        anchor.y + gap
    };
    let x = x.min(area.right() - size.x).max(area.left());
    let y = y.min(area.bottom() - size.y).max(area.top());
    pos2(x, y)
}

/// The sample of a non-empty `grid` nearest to `time`, its first or last beyond them.
pub fn nearest_sample(grid: &Grid, time: f64) -> u64 {
    let last = grid.len() - 1;
    let s = grid.sample_at(time).min(last);
    let next = (s + 1).min(last);
    if (grid.time_of(next) - time).abs() < (time - grid.time_of(s)).abs() {
        next
    } else {
        s
    }
}

/// The interval: in ms below a second ("47 ms"), else in seconds ("1.250 s"), to the
/// grid's sample at its rate's decimals.
pub fn duration_text(span: &Span) -> String {
    let decimals = time_decimals(span.rate);
    if span.dt < 1.0 {
        let decimals = decimals.saturating_sub(3);
        format!("{:.decimals$} ms", span.dt * 1e3)
    } else {
        format!("{:.decimals$} s", span.dt)
    }
}

/// 1/Δt with three significant digits ("0.800 Hz", "21.3 Hz", "256 Hz"), as many as a drag
/// is precise to; `None` for an interval of 0.
pub fn frequency_text(span: &Span) -> Option<String> {
    (span.dt > 0.0).then(|| format_significant(1.0 / span.dt, 3, "Hz"))
}

/// "86.40 µV PTP": the peak-to-peak amplitude in the band, – until it is known or where no
/// sample has a value.
pub fn amplitude_text(peak_to_peak: Option<f64>, unit: &str) -> String {
    match peak_to_peak {
        Some(v) => format!("{} PTP", format_value(v, unit)),
        None => format!("{NO_READING} PTP"),
    }
}

/// The status line's account of a measurement: "0:12.500 to 0:13.750 · 1.250 s (320
/// samples at 256 Hz) · Fz 12.30 µV → −4.100 µV, Δ −16.40 µV", the ends' times on `clock`,
/// the samples between them where the grid has them, and the channel's readings at the
/// ends in `unit` with their difference where both are values (an envelope's has none):
/// time, then channel and values, as everywhere.
pub fn details_text(
    name: &str,
    span: &Span,
    clock: Clock,
    unit: &str,
    ends: [Option<Reading>; 2],
) -> String {
    let decimals = time_decimals(span.rate);
    let times = format!(
        "{} to {}",
        format_time(span.start, decimals, clock),
        format_time(span.end, decimals, clock)
    );
    let mut interval = duration_text(span);
    if let Some(n) = span.samples {
        let samples = if n == 1 { "sample" } else { "samples" };
        interval += &format!(" ({n} {samples} at {})", rate_text(span.rate));
    }
    let text = |r: Option<Reading>| r.map_or(NO_READING.to_owned(), |r| reading_text(r, unit));
    let mut values = format!("{name} {} \u{2192} {}", text(ends[0]), text(ends[1]));
    if let [Some(Reading::Value(a)), Some(Reading::Value(b))] = ends {
        values += ", \u{394} ";
        values += &format_value(b - a, unit);
    }
    [times, interval, values].join(" \u{b7} ")
}

/// A sampling rate in full: "256 Hz", "0.5 Hz", "1017.25 Hz".
fn rate_text(rate: f64) -> String {
    let text = format!("{rate:.3}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    format!("{text} Hz")
}
