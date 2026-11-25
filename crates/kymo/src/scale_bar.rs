//! Scale bars: one per scale group on screen (a channel type's channels whose units measure
//! the same, or a channel scaled on its own; see [`ScaleGroup`]), as long as a round
//! amplitude (1, 2 or 5 times a power of ten) in the unit its readings are shown in, with
//! the prefix that suits the value ("2 mV" rather than 2000 µV; see [`fitted`]). Each
//! keeps to the top of the visible part of its group's first block of rows, as a list's
//! section header does: scrolling within the block leaves it where it is, and the block's
//! end pushes it out.

use kymo_core::{ChannelId, ChannelKind, Recording};

use crate::ui::readout::display_unit;
use crate::view::{ScaleGroup, ViewState};

/// A scale group's scale bar.
#[derive(Clone, Debug, PartialEq)]
pub struct ScaleBar {
    pub kind: ChannelKind,
    pub group: ScaleGroup,
    /// The first channel on screen of its block, whose unit it is in.
    pub channel: ChannelId,
    /// Where its middle is, in rows from the top of the trace area (0.5 is the middle of
    /// the top row).
    pub middle: f64,
    /// How much it stands for, in `unit`: the largest round value up to the rows'
    /// amplitude span (±amplitude spans a row).
    pub value: f64,
    /// The unit its readings are shown in ("µV", "fT"), with another prefix where that suits
    /// the value better (see [`fitted`]); empty if the channel has none.
    pub unit: String,
    /// Its length as a fraction of the row height, in (0.4, 1].
    pub fraction: f64,
}

impl ScaleBar {
    /// Its label: the value with as many decimals as it needs, then the unit ("20 µV",
    /// "0.5 mV", "200").
    pub fn label(&self) -> String {
        let value = format_value(self.value);
        if self.unit.is_empty() {
            value
        } else {
            format!("{value} {}", self.unit)
        }
    }
}

/// The scale bars of the rows `view` shows, one for each scale group but stim channels'
/// (whose rows hold codes, not amplitudes). A group's bar stands beside the first block of
/// consecutive rows of that group on screen where it fits whole: in the middle of the
/// block's top row on screen, or of its top row slot while the block goes on above the
/// screen, and no lower in the block than the middle of its last row, so a block leaving at
/// the top takes the bar along. A group without such a block, or without a usable
/// amplitude, gets none.
pub fn scale_bars(recording: &Recording, view: &ViewState) -> Vec<ScaleBar> {
    let top = view.top();
    let rows = view.rows() as f64;
    let first = view.visible_rows().start;
    let channels = view.visible_channels();
    let mut bars: Vec<ScaleBar> = Vec::new();
    let mut start = 0;
    while start < channels.len() {
        let group = view.scale_group(channels[start]);
        let end = start
            + channels[start..]
                .iter()
                .take_while(|&&c| view.scale_group(c) == group)
                .count();
        let block = ((first + start) as f64, (first + end) as f64);
        let channel = channels[start];
        let kind = view.kind_of(channel);
        start = end;
        if kind.is_stim() || bars.iter().any(|b| b.group == group) {
            continue;
        }
        let Some((value, unit, fraction)) = bar_value(recording, view, channel) else {
            continue;
        };
        let middle = (block.0.max(top) - top + 0.5).min(block.1 - top - 0.5);
        if middle < fraction / 2.0 || middle + fraction / 2.0 > rows {
            continue;
        }
        bars.push(ScaleBar {
            kind: kind.clone(),
            group,
            channel,
            middle,
            value,
            unit,
            fraction,
        });
    }
    bars
}

/// What `channel`'s scale bar stands for, in its unit ([`fitted`]), and its length as a
/// fraction of the row height; `None` without a usable amplitude.
fn bar_value(
    recording: &Recording,
    view: &ViewState,
    channel: ChannelId,
) -> Option<(f64, String, f64)> {
    let (unit, factor) = display_unit(view.kind_of(channel), &recording.channel(channel).unit);
    let span = 2.0 * view.channel_amplitude(channel) * factor;
    let value = round_below(span)?;
    let (fitted_value, fitted_unit) = fitted(value, &unit);
    Some((fitted_value, fitted_unit, value / span))
}

/// The unit `channel`'s readings are shown in, and the factor from its own unit to it: its
/// scale bar's (on screen or not), so a reading beside a "2 mV" bar is in mV, which changes
/// only as the scale does; the unit of [`display_unit`] where it has no bar's amplitude.
/// None for stim channels, whose values are codes.
pub fn reading_unit(recording: &Recording, view: &ViewState, channel: ChannelId) -> (String, f64) {
    let (unit, factor) = display_unit(view.kind_of(channel), &recording.channel(channel).unit);
    let span = 2.0 * view.channel_amplitude(channel) * factor;
    match round_below(span) {
        Some(value) => {
            let (fitted_value, fitted_unit) = fitted(value, &unit);
            (fitted_unit, factor * fitted_value / value)
        }
        None => (unit, factor),
    }
}

/// The largest of 1, 2 or 5 times a power of ten up to `limit`; `None` unless `limit` is
/// positive and finite.
pub fn round_below(limit: f64) -> Option<f64> {
    if !(limit.is_finite() && limit > 0.0) {
        return None;
    }
    let power = 10f64.powf(limit.log10().floor());
    // log10 may round across a power of ten; the steps below correct for it.
    [10.0, 5.0, 2.0, 1.0, 0.5]
        .into_iter()
        .map(|step| step * power)
        .find(|&v| v <= limit * (1.0 + 1e-12))
}

/// SI prefixes a scale bar's unit may take, largest first.
const PREFIXES: [(&str, f64); 6] = [
    ("", 1.0),
    ("m", 1e-3),
    ("\u{b5}", 1e-6),
    ("n", 1e-9),
    ("p", 1e-12),
    ("f", 1e-15),
];

/// `value` in `unit`, with the unit's SI prefix the largest one that keeps the value at
/// least 1 ("2000 µV" as 2 mV, "0.5 mV" as 500 µV, "5000 fT/cm" as 5 pT/cm, "2000 ms" as
/// 2 s): volts, siemens and seconds from nano to none, teslas (also per length) from femto
/// to none; with none of them (less than 1 nV, or 1000 V and more), the nearest. Other
/// units stay as they are.
pub fn fitted(value: f64, unit: &str) -> (f64, String) {
    let (numerator, per) = match unit.split_once('/') {
        Some((n, d)) => (n, format!("/{d}")),
        None => (unit, String::new()),
    };
    let Some(base) = numerator.chars().last() else {
        return (value, unit.to_owned());
    };
    let prefix = &numerator[..numerator.len() - base.len_utf8()];
    let smallest = match base {
        'V' | 'S' | 's' if per.is_empty() => 1e-9,
        'T' => 1e-15,
        _ => return (value, unit.to_owned()),
    };
    let Some(&(_, factor)) = PREFIXES.iter().find(|(p, _)| *p == prefix) else {
        return (value, unit.to_owned());
    };
    let si = value * factor;
    let allowed: Vec<(&str, f64)> = PREFIXES
        .into_iter()
        .filter(|(_, f)| *f >= smallest * (1.0 - 1e-9))
        .collect();
    let (prefix, factor) = allowed
        .iter()
        .find(|(_, f)| si / f >= 1.0 - 1e-9)
        .or(allowed.last())
        .copied()
        .expect("volts, siemens, seconds and teslas allow a prefix");
    // Round values stay round (2000 µV as exactly 2 mV).
    let scaled = si / factor;
    let digits = 10f64.powi(9 - scaled.abs().log10().floor() as i32);
    (
        (scaled * digits).round() / digits,
        format!("{prefix}{base}{per}"),
    )
}

/// `value`, a round value as [`round_below`] gives, with the decimals it needs and no
/// more: "20", "0.5", "0.002".
fn format_value(value: f64) -> String {
    // Rounded up with a little slack, as log10 of a power of ten can miss it slightly.
    let decimals = (-value.log10() - 1e-9).ceil().max(0.0) as usize;
    format!("{value:.decimals$}")
}
