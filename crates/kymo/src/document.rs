//! An open recording and how it gets opened: on a background thread for the window, directly
//! for `--info`, through the same [`open`].

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use anyhow::{Context, Result, anyhow};
use kymo_core::{AnnotationStore, Recording};

use crate::ui::axis::{Clock, format_time};

/// A recording opened for viewing, with the annotation store beside it.
pub struct Document {
    pub path: PathBuf,
    pub recording: Arc<Recording>,
    pub annotations: AnnotationStore,
}

/// Opens and validates a recording. Reads the header and index, never the samples.
pub fn open(path: &Path) -> Result<Document> {
    let dir = kymo_core::cache::default_dir().unwrap_or_else(std::env::temp_dir);
    open_with(path, Some(&dir))
}

/// [`open`], with the reader's scratch files in `scratch`, or in memory without one (see
/// [`kymo_core::open_with`]).
pub fn open_with(path: &Path, scratch: Option<&Path>) -> Result<Document> {
    // The core's errors already name the file.
    let opened = kymo_core::open_with(path, scratch)?;
    let (recording, annotations) =
        Recording::new(opened).with_context(|| format!("invalid recording {}", path.display()))?;
    Ok(Document {
        path: path.to_owned(),
        recording: Arc::new(recording),
        annotations: AnnotationStore::from_data(annotations),
    })
}

impl Document {
    pub fn summary(&self) -> Summary {
        let rec = &self.recording;
        Summary {
            format: rec.format(),
            fields: rec
                .extra()
                .iter()
                .map(|(key, value)| (one_line(key), one_line(value)))
                .filter(|(key, value)| !key.is_empty() && !value.is_empty())
                .collect(),
            start: rec.start_time(),
            duration: rec.duration(),
            channels: rec.channels().len(),
            streams: (
                rec.streams().len(),
                rec.streams()
                    .iter()
                    .filter(|s| matches!(s.content, kymo_core::StreamContent::Channels(_)))
                    .count(),
            ),
            annotations: self.annotations.len(),
            grids: rec
                .grids()
                .iter()
                .map(|g| GridSummary {
                    rate: g.rate(),
                    irregular: g.times().is_some(),
                    samples: g.len(),
                    segments: g.segments().len(),
                    stream: grid_stream(rec, g.channels()),
                    channels: g
                        .channels()
                        .iter()
                        .map(|&c| ChannelSummary::of(rec, c))
                        .collect(),
                })
                .collect(),
            warnings: rec.warnings().to_vec(),
        }
    }

    /// The file name, for window titles.
    pub fn name(&self) -> String {
        file_name(&self.path)
    }
}

/// The smallest stream holding all of `channels` (an XDF stream's grid is its own; BVRF's
/// participants share one, "All participants"), if the recording has several streams: its
/// name, with its type where it has one, as streams may share a name ("Muse (ACC)").
fn grid_stream(rec: &Recording, channels: &[kymo_core::ChannelId]) -> Option<String> {
    if rec.streams().len() < 2 {
        return None;
    }
    rec.streams()
        .iter()
        .filter_map(|s| match &s.content {
            kymo_core::StreamContent::Channels(own) => Some((s, own)),
            _ => None,
        })
        .filter(|(_, own)| channels.iter().all(|c| own.contains(c)))
        .min_by_key(|(_, own)| own.len())
        .map(|(s, _)| match (s.name.trim(), s.kind.trim()) {
            (name, "") => name.to_owned(),
            ("", kind) => kind.to_owned(),
            (name, kind) => format!("{name} ({kind})"),
        })
        .filter(|name| !name.is_empty())
}

/// The file name of `path`, or the whole path if it has none.
pub fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// What the header panel and `--info` show.
#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    pub format: &'static str,
    /// The format's own header fields (version, layout, patient, streams, ...), key and
    /// value, each on one line; empty ones left out.
    pub fields: Vec<(String, String)>,
    /// Wall-clock start, if the file has one.
    pub start: Option<jiff::civil::DateTime>,
    /// Seconds.
    pub duration: f64,
    pub channels: usize,
    /// The file's streams (XDF's, BVRF's participants) and how many of them are shown as
    /// channels; 0 for formats without streams.
    pub streams: (usize, usize),
    pub annotations: usize,
    pub grids: Vec<GridSummary>,
    pub warnings: Vec<String>,
}

/// One sampling grid: channels sharing a rate and gap structure.
#[derive(Clone, Debug, PartialEq)]
pub struct GridSummary {
    /// For an irregular time base, the average rate.
    pub rate: f64,
    /// Whether the samples have times of their own (an irregular time base).
    pub irregular: bool,
    pub samples: u64,
    /// Sample ranges holding data; more than one means the recording has gaps.
    pub segments: usize,
    /// The stream it shows, in a recording of several ([`grid_stream`]).
    pub stream: Option<String>,
    pub channels: Vec<ChannelSummary>,
}

/// One channel as the file describes it, for the Recording Info window's tables.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChannelSummary {
    /// Its place among all the recording's channels, from 1.
    pub number: usize,
    pub name: String,
    /// The type the file gives it.
    pub kind: String,
    /// Its unit as a symbol ([`kymo_core::Unit::symbol`]).
    pub unit: String,
    /// Physical (min, max) as stored, where the format has one.
    pub range: Option<(f64, f64)>,
    pub prefilter: String,
    pub transducer: String,
}

impl ChannelSummary {
    fn of(rec: &Recording, id: kymo_core::ChannelId) -> ChannelSummary {
        let c = rec.channel(id);
        ChannelSummary {
            number: id.0 as usize + 1,
            name: c.name.clone(),
            kind: c.kind.name().to_owned(),
            unit: c.unit.symbol(),
            range: c.physical_range,
            prefilter: one_line(&c.prefilter),
            transducer: one_line(&c.transducer),
        }
    }
}

/// A channel known only by its name, for tests.
impl From<&str> for ChannelSummary {
    fn from(name: &str) -> ChannelSummary {
        ChannelSummary {
            name: name.to_owned(),
            ..ChannelSummary::default()
        }
    }
}

impl GridSummary {
    /// One line without the channel names, e.g. "512 Hz, 30720 samples, 5 channels", with
    /// the exact rate ([`exact_rate`]; "≈299.983742 Hz, ..." for the average rate of an
    /// irregular time base).
    pub fn describe(&self) -> String {
        let n = self.channels.len();
        let rate = exact_rate_text(self.rate, self.irregular);
        let mut s = format!("{rate}, {} samples, {n} channel{}", self.samples, plural(n));
        if self.segments != 1 {
            s += &format!(", {} segments", self.segments);
        }
        s
    }
}

impl Summary {
    /// The sidebar's rows, key and value. One "Sampling" row lists the rates, rounded (see
    /// [`format_rate`]), a line each with its channel count if there are several; hovering it
    /// shows per rate the exact value, channels and samples. A "Gaps" row only for
    /// recordings with gaps. No sample type: for EDF and BDF the format implies it, and ours
    /// is the type in memory (BDF's int24 is widened to int32), not the file's. No
    /// annotation count: the legend has it, with the stim channels' annotations.
    pub fn sidebar_rows(&self) -> Vec<SidebarRow> {
        // The start to the millisecond, every digit the file has on hover.
        let start = match self.start {
            Some(t) => {
                let shown = format_start(t, true);
                let full = format_start(t, false);
                SidebarRow {
                    hover: (full != shown).then_some(full),
                    ..SidebarRow::new("Start", shown)
                }
            }
            None => SidebarRow::new("Start", "Unknown".into()),
        };
        // The format's own fields are in the Recording Info window its click opens.
        let format = SidebarRow {
            info: true,
            ..SidebarRow::new("Format", self.format.to_owned())
        };
        let mut rows = vec![
            format,
            start,
            SidebarRow::new("Duration", format_duration(self.duration)),
            SidebarRow::new("Channels", self.channels.to_string()),
        ];
        // Only where there are several, as only XDF and BVRF files have them.
        let (streams, with_channels) = self.streams;
        if streams > 1 {
            let value = if with_channels == streams {
                streams.to_string()
            } else {
                format!("{streams} ({with_channels} shown as channels)")
            };
            rows.push(SidebarRow::new("Streams", value));
        }
        // Grids of the same rate (split by sample type) count as one; irregular ones show
        // their average rate.
        let mut rates: Vec<(f64, bool, usize, u64)> = Vec::new();
        for grid in &self.grids {
            let same = |r: &&mut (f64, bool, usize, u64)| r.0 == grid.rate && r.1 == grid.irregular;
            match rates.iter_mut().find(same) {
                Some((_, _, channels, samples)) => {
                    *channels += grid.channels.len();
                    *samples = (*samples).max(grid.samples);
                }
                None => rates.push((grid.rate, grid.irregular, grid.channels.len(), grid.samples)),
            }
        }
        let value = match &rates[..] {
            [(rate, irregular, ..)] => rate_text(*rate, *irregular),
            // A line each, so the row never truncates.
            _ => rates
                .iter()
                .map(|&(rate, irregular, n, _)| {
                    format!("{} ({n} channel{})", rate_text(rate, irregular), plural(n))
                })
                .collect::<Vec<_>>()
                .join("\n"),
        };
        let hover = rates
            .iter()
            .map(|&(rate, irregular, n, samples)| {
                let rate = exact_rate_text(rate, irregular);
                format!("{rate}: {n} channel{}, {samples} samples", plural(n))
            })
            .collect::<Vec<_>>()
            .join("\n");
        rows.push(SidebarRow {
            hover: Some(hover),
            ..SidebarRow::new("Sampling", value)
        });
        let gaps = self
            .grids
            .iter()
            .map(|g| g.segments.saturating_sub(1))
            .max();
        if let Some(gaps) = gaps.filter(|&n| n > 0) {
            rows.push(SidebarRow::new("Gaps", gaps.to_string()));
        }
        rows
    }
}

impl Summary {
    /// What `--info` prints and the Recording Info window shows, a row each: the format
    /// and its own fields, start, duration, channels, streams (if several), annotations
    /// (the file's), each grid with its channels, and the warnings.
    pub fn info_rows(&self) -> Vec<InfoRow> {
        let row = |key: &str, value: String| InfoRow {
            key: key.to_owned(),
            field: None,
            value,
            grid: None,
        };
        let mut rows = vec![row("format", self.format.to_owned())];
        rows.extend(self.fields.iter().map(|(key, value)| InfoRow {
            key: String::new(),
            field: Some(key.clone()),
            value: value.clone(),
            grid: None,
        }));
        let start = self.start.map(|t| format_start(t, false));
        rows.push(row("start", start.unwrap_or_else(|| "Unknown".into())));
        rows.push(row("duration", format_duration(self.duration)));
        rows.push(row("channels", self.channels.to_string()));
        if self.streams.0 > 1 {
            let (streams, shown) = self.streams;
            rows.push(row(
                "streams",
                format!("{streams} ({shown} shown as channels)"),
            ));
        }
        rows.push(row("annotations", self.annotations.to_string()));
        for (i, grid) in self.grids.iter().enumerate() {
            let names: Vec<&str> = grid.channels.iter().map(|c| c.name.as_str()).collect();
            for (key, value) in [
                (format!("grid {}", i + 1), grid.describe()),
                (String::new(), names.join(", ")),
            ] {
                rows.push(InfoRow {
                    grid: Some(i),
                    ..row(&key, value)
                });
            }
        }
        rows.extend(self.warnings.iter().map(|w| row("warning", w.clone())));
        rows
    }
}

/// One row of [`Summary::info_rows`]: a key ("format", "grid 1"; empty where the row
/// continues the one above), the name of a format field, and the value.
#[derive(Clone, Debug, PartialEq)]
pub struct InfoRow {
    pub key: String,
    pub field: Option<String>,
    pub value: String,
    /// The grid a row describes (its line and its channels' names), by its index in
    /// [`Summary::grids`]; the Recording Info window shows a table for it instead.
    pub grid: Option<usize>,
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for row in self.info_rows() {
            match &row.field {
                Some(field) => writeln!(f, "{:<12} {field}: {}", row.key, row.value)?,
                None => writeln!(f, "{:<12} {}", row.key, row.value)?,
            }
        }
        Ok(())
    }
}

/// What `--info` prints for the recording at `path`: the path, then the summary.
pub fn info_text(path: &Path, summary: &Summary) -> String {
    format!("{}\n{summary}", path.display())
}

pub fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// A start time in ISO's order, which reads the same everywhere, with a space for its "T":
/// "2024-05-01 13:45:07.25". Its fraction has every digit it has, trailing zeros dropped, or
/// with `rounded` at most three (to the millisecond).
pub fn format_start(time: jiff::civil::DateTime, rounded: bool) -> String {
    let time = if rounded {
        // Only a time within a millisecond of the latest one there is fails to round.
        time.round(jiff::Unit::Millisecond).unwrap_or(time)
    } else {
        time
    };
    time.to_string().replacen('T', " ", 1)
}

/// One row of the sidebar's summary.
#[derive(Clone, Debug, PartialEq)]
pub struct SidebarRow {
    pub key: String,
    pub value: String,
    /// What hovering the value shows: the exact rates, channels and samples for Sampling,
    /// the start with every digit where it has more than three decimals.
    pub hover: Option<String>,
    /// Whether a click on the value opens the Recording Info window (Format's).
    pub info: bool,
}

impl SidebarRow {
    fn new(key: &str, value: String) -> SidebarRow {
        SidebarRow {
            key: key.to_owned(),
            value,
            hover: None,
            info: false,
        }
    }
}

/// A rate as shown: at most two decimals, but at least three significant digits, trailing
/// zeros dropped ("512 Hz", "500.01 Hz", "0.5 Hz", "0.0333 Hz").
pub fn format_rate(rate: f64) -> String {
    if !(rate.is_finite() && rate > 0.0) {
        return exact_rate(rate);
    }
    let decimals = (2 - rate.log10().floor() as i32).max(2) as usize;
    let text = format!("{rate:.decimals$}");
    format!("{} Hz", text.trim_end_matches('0').trim_end_matches('.'))
}

/// A grid's rate as [`format_rate`] rounds it, "≈300 Hz" for the average rate of an
/// irregular time base.
pub fn rate_text(rate: f64, irregular: bool) -> String {
    if irregular {
        format!("\u{2248}{}", format_rate(rate))
    } else {
        format_rate(rate)
    }
}

/// A grid's rate as [`exact_rate`] gives it, "≈299.983742 Hz" for the average rate of an
/// irregular time base.
fn exact_rate_text(rate: f64, irregular: bool) -> String {
    if irregular {
        format!("\u{2248}{}", exact_rate(rate))
    } else {
        exact_rate(rate)
    }
}

/// The rate of `grid` as [`rate_text`] gives it.
pub fn grid_rate(grid: &kymo_core::Grid) -> String {
    rate_text(grid.rate(), grid.times().is_some())
}

/// A rate to the microhertz (six decimals, trailing zeros dropped: "500.006493 Hz", "256
/// Hz"), or to six significant digits below 1 Hz ("0.0333333 Hz"). Rates computed from
/// time stamps (XDF's effective rates) have more digits than they mean.
pub fn exact_rate(rate: f64) -> String {
    if !rate.is_finite() {
        return format!("{rate} Hz");
    }
    let decimals = if rate > 0.0 && rate < 1.0 {
        (5 - rate.log10().floor() as i32) as usize
    } else {
        6
    };
    let text = format!("{rate:.decimals$}");
    let text = if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.')
    } else {
        &text
    };
    format!("{text} Hz")
}

/// `text` with line breaks and tabs as spaces, trimmed.
fn one_line(text: &str) -> String {
    text.replace(['\r', '\n', '\t'], " ").trim().to_owned()
}

/// Seconds on the clock the time axis would use for them (see [`Clock::for_end`]), to the
/// millisecond, trailing zeros dropped: "12.5 s", "30:00", "1:02:03.5".
pub fn format_duration(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return format!("{seconds} s");
    }
    // Rounded first, so 59.9996 s reads as "1:00", not "60 s".
    let seconds = (seconds * 1000.0).round() / 1000.0;
    let text = format_time(seconds, 3, Clock::for_end(seconds));
    let (number, unit) = match text.strip_suffix(" s") {
        Some(number) => (number, " s"),
        None => (text.as_str(), ""),
    };
    format!(
        "{}{unit}",
        number.trim_end_matches('0').trim_end_matches('.')
    )
}

/// An error and its causes on one line, joined by ": ". A cause is left out when the message
/// before it already ends with it, as kymo-core's errors include their source.
pub fn error_text(error: &anyhow::Error) -> String {
    let mut text = String::new();
    for cause in error.chain().map(ToString::to_string) {
        if text.is_empty() {
            text = cause;
        } else if !text.ends_with(&cause) {
            text = format!("{text}: {cause}");
        }
    }
    text
}

/// A recording being opened on a background thread, so the window never waits for the disk.
pub struct Loading<T = Document> {
    path: PathBuf,
    result: Receiver<Result<T>>,
}

impl<T: Send + 'static> Loading<T> {
    /// Starts `open(path)` on a thread. `notify` runs once the result is ready (or the
    /// thread died), so the window can repaint and pick it up with [`poll`](Self::poll).
    pub fn start(
        path: PathBuf,
        open: impl FnOnce(&Path) -> Result<T> + Send + 'static,
        notify: impl FnOnce() + Send + 'static,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let thread_path = path.clone();
        // If spawning fails, `tx` is dropped with the closure and `poll` reports it.
        let _ = std::thread::Builder::new()
            .name("kymo-open".into())
            .spawn(move || {
                // Dropped after `tx` (locals drop in reverse order), so a panic still ends
                // with a disconnected channel and a notification.
                let _notify = OnDrop(Some(notify));
                let _ = tx.send(open(&thread_path));
            });
        Loading { path, result: rx }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The result, once it is there. Call again after it returned `Some` and it reports an
    /// error.
    pub fn poll(&self) -> Option<Result<T>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(anyhow!(
                "opening {} failed unexpectedly",
                self.path.display()
            ))),
        }
    }
}

struct OnDrop<F: FnOnce()>(Option<F>);

impl<F: FnOnce()> Drop for OnDrop<F> {
    fn drop(&mut self) {
        if let Some(f) = self.0.take() {
            f();
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::time::Duration;

    use kymo_testgen::TempFile;

    use super::*;

    /// Opens through the window's path: a background thread, then a notification.
    fn load(path: &Path) -> Result<Document> {
        let (tx, rx) = mpsc::channel();
        let loading = Loading::start(path.to_owned(), open, move || tx.send(()).unwrap());
        assert_eq!(loading.path(), path);
        rx.recv_timeout(Duration::from_secs(30))
            .expect("no notification");
        loading.poll().expect("notified without a result")
    }

    #[test]
    fn opens_a_generated_file() {
        let file = TempFile::preset("bdf+c-small");
        let doc = load(file.path()).unwrap();
        assert_eq!(doc.path, file.path());
        let summary = doc.summary();
        assert_eq!(summary.format, "BDF+C");
        assert_eq!(summary.duration, 60.0);
        assert_eq!(summary.channels, 5);
        assert!(summary.annotations > 0);
        assert!(summary.warnings.is_empty(), "{:?}", summary.warnings);
        assert_eq!(summary.grids.len(), 1);
        let grid = &summary.grids[0];
        assert_eq!(
            (grid.rate, grid.samples, grid.segments),
            (512.0, 60 * 512, 1)
        );
        let names: Vec<&str> = grid.channels.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names[..4], ["A1", "A2", "A3", "EXG1"]);
        assert_eq!(grid.stream, None);
        // As the file describes them: BDF's transducer, physical range and prefilter.
        let a1 = &grid.channels[0];
        assert_eq!(
            (a1.number, a1.kind.as_str(), a1.unit.as_str()),
            (1, "EEG", "\u{b5}V")
        );
        assert_eq!(a1.transducer, "AgAgCl electrode");
        assert_eq!(a1.range, Some((-262144.0, 262143.0)));
        assert_eq!(a1.prefilter, "HP:0.1Hz LP:100Hz");
        assert_eq!(grid.describe(), "512 Hz, 30720 samples, 5 channels");
    }

    #[test]
    fn summarises_grids_and_gaps() {
        let file = TempFile::preset("edf+d-mixed");
        let doc = load(file.path()).unwrap();
        let summary = doc.summary();
        assert_eq!(summary.format, "EDF+D");
        let rates: Vec<f64> = summary.grids.iter().map(|g| g.rate).collect();
        assert_eq!(rates, [256.0, 512.0, 64.0]);
        assert!(summary.grids.iter().all(|g| g.segments == 4));
        assert!(summary.grids[0].describe().ends_with(", 4 segments"));
        let text = summary.to_string();
        assert!(text.contains("EDF+D"), "{text}");
        assert!(text.contains("EEG DC"), "{text}");

        // Several rates in one row, with their channels; the samples on hover; the gaps
        // counted once, as the grids share them.
        let rows = summary.sidebar_rows();
        let row = |key: &str| rows.iter().find(|r| r.key == key).unwrap();
        assert_eq!(row("Format").value, "EDF+D");
        assert_eq!(row("Duration").value, format_duration(summary.duration));
        assert_eq!(
            row("Sampling").value,
            "256 Hz (4 channels)\n512 Hz (2 channels)\n64 Hz (1 channel)"
        );
        let lines: Vec<String> = summary
            .grids
            .iter()
            .map(|g| {
                let n = g.channels.len();
                format!(
                    "{} Hz: {n} channel{}, {} samples",
                    g.rate,
                    plural(n),
                    g.samples
                )
            })
            .collect();
        assert_eq!(row("Sampling").hover, Some(lines.join("\n")));
        assert_eq!(row("Gaps").value, "3");
        assert!(!rows.iter().any(|r| r.key.starts_with("Grid")));
    }

    #[test]
    fn sidebar_names_each_fact_once() {
        let file = TempFile::preset("bdf+c-small");
        let summary = load(file.path()).unwrap().summary();
        let rows = summary.sidebar_rows();
        let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(
            keys,
            ["Format", "Start", "Duration", "Channels", "Sampling"]
        );
        let value = |key: &str| &rows.iter().find(|r| r.key == key).unwrap().value;
        assert_eq!(value("Format"), "BDF+C");
        assert_eq!(value("Start"), "2024-01-02 03:04:05");
        assert_eq!(value("Duration"), "1:00");
        assert_eq!(value("Channels"), "5");
        assert_eq!(value("Sampling"), "512 Hz");
    }

    #[test]
    fn rates_show_two_decimals_but_three_significant_digits() {
        let cases = [
            (512.0, "512 Hz"),
            (500.006_492_535_299_46, "500.01 Hz"),
            (600.614_990_234_375, "600.61 Hz"),
            (2048.0, "2048 Hz"),
            (999.999, "1000 Hz"),
            (128.5, "128.5 Hz"),
            (10.0, "10 Hz"),
            (0.5, "0.5 Hz"),
            // One sample per 30 s, as hypnogram channels in EDF have.
            (1.0 / 30.0, "0.0333 Hz"),
            (0.001_234_5, "0.00123 Hz"),
        ];
        for (rate, text) in cases {
            assert_eq!(format_rate(rate), text, "{rate}");
        }
    }

    #[test]
    fn exact_rates_are_to_the_microhertz() {
        let cases = [
            (512.0, "512 Hz"),
            (500.006_492_535_299_46, "500.006493 Hz"),
            // XDF's effective rate of a nominal 500 Hz stream.
            (499.999_999_999_992_95, "500 Hz"),
            (2048.0, "2048 Hz"),
            (128.5, "128.5 Hz"),
            (1.0 / 30.0, "0.0333333 Hz"),
            (0.982_792_961_274_260_4, "0.982793 Hz"),
            (0.001_234_567_89, "0.00123457 Hz"),
        ];
        for (rate, text) in cases {
            assert_eq!(exact_rate(rate), text, "{rate}");
        }
    }

    /// The format's own fields show under `--info`'s format line (and so in Recording
    /// Info), a line each, each on one line, and not on hovering the Format row.
    #[test]
    fn format_fields_show_in_info() {
        let file = TempFile::preset("bv-int16-multiplexed");
        let summary = open(file.path()).unwrap().summary();
        let rows = summary.sidebar_rows();
        let format = rows.iter().find(|r| r.key == "Format").unwrap();
        assert_eq!(format.value, "BrainVision");
        assert!(format.info);
        assert_eq!(format.hover, None);
        let info = summary.to_string();
        assert!(
            info.starts_with(
                "format       BrainVision\n             version: 1.0\n             created: "
            ),
            "{info}"
        );
        assert!(
            info.contains("\n             binary format: INT_16\n"),
            "{info}"
        );

        let mut spec = kymo_testgen::Spec::preset("bdf+c-small").unwrap();
        spec.patient = "X Y".into();
        let file = TempFile::written(&spec);
        let summary = open(file.path()).unwrap().summary();
        assert_eq!(summary.fields[0], ("patient".to_owned(), "X Y".to_owned()));
        assert_eq!(one_line(" a\r\nb\tc "), "a  b c");
    }

    /// The sidebar shows the start to the millisecond at most, trailing zeros dropped, and
    /// every digit on hover where there are more; `--info` has every digit.
    #[test]
    fn the_start_shows_at_most_three_decimals() {
        let at = |s: &str| s.parse::<jiff::civil::DateTime>().unwrap();
        let cases = [
            ("2024-01-02T03:04:05", "2024-01-02 03:04:05", None),
            ("2024-01-02T03:04:05.25", "2024-01-02 03:04:05.25", None),
            ("2024-01-02T03:04:05.123", "2024-01-02 03:04:05.123", None),
            (
                "2024-01-02T03:04:05.123456789",
                "2024-01-02 03:04:05.123",
                Some("2024-01-02 03:04:05.123456789"),
            ),
            // Rounded, to the next second (and day) too.
            (
                "2024-01-02T03:04:05.9876",
                "2024-01-02 03:04:05.988",
                Some("2024-01-02 03:04:05.9876"),
            ),
            (
                "2024-12-31T23:59:59.9996",
                "2025-01-01 00:00:00",
                Some("2024-12-31 23:59:59.9996"),
            ),
        ];
        for (time, shown, hover) in cases {
            let summary = Summary {
                format: "EDF+C",
                fields: Vec::new(),
                start: Some(at(time)),
                duration: 2.0,
                channels: 1,
                streams: (0, 0),
                annotations: 0,
                grids: vec![GridSummary {
                    rate: 100.0,
                    irregular: false,
                    samples: 200,
                    segments: 1,
                    stream: None,
                    channels: vec!["Fz".into()],
                }],
                warnings: Vec::new(),
            };
            let rows = summary.sidebar_rows();
            let row = rows.iter().find(|r| r.key == "Start").unwrap();
            assert_eq!(row.value, shown, "{time}");
            assert_eq!(row.hover.as_deref(), hover, "{time}");
            let full = hover.unwrap_or(shown);
            assert!(
                summary
                    .to_string()
                    .contains(&format!("start        {full}\n")),
                "{time}"
            );
        }
    }

    /// A "Streams" row (and `--info` line) only where there are several streams.
    #[test]
    fn several_streams_are_counted() {
        let summary = |streams: (usize, usize)| Summary {
            format: "XDF",
            fields: Vec::new(),
            start: None,
            duration: 2.0,
            channels: 2,
            streams,
            annotations: 0,
            grids: vec![GridSummary {
                rate: 100.0,
                irregular: false,
                samples: 200,
                segments: 1,
                stream: None,
                channels: vec!["Fz".into(), "Cz".into()],
            }],
            warnings: Vec::new(),
        };
        let row = |s: &Summary| {
            s.sidebar_rows()
                .into_iter()
                .find(|r| r.key == "Streams")
                .map(|r| r.value)
        };
        // Without a start, "Unknown".
        let start = summary((0, 0))
            .sidebar_rows()
            .into_iter()
            .find(|r| r.key == "Start");
        assert_eq!(start.unwrap().value, "Unknown");
        assert!(
            summary((0, 0))
                .to_string()
                .contains("start        Unknown\n")
        );
        assert_eq!(row(&summary((0, 0))), None);
        assert_eq!(row(&summary((1, 1))), None);
        assert!(!summary((1, 1)).to_string().contains("streams"));
        assert_eq!(row(&summary((2, 2))).as_deref(), Some("2"));
        let nine = summary((9, 5));
        assert_eq!(row(&nine).as_deref(), Some("9 (5 shown as channels)"));
        let keys: Vec<String> = nine.sidebar_rows().into_iter().map(|r| r.key).collect();
        assert_eq!(keys[3..5], ["Channels", "Streams"]);
        assert!(
            nine.to_string()
                .contains("streams      9 (5 shown as channels)\n")
        );
    }

    /// The sidebar rounds a rate and shows the exact one on hover, with channels and samples;
    /// `--info` prints it exactly (to the microhertz).
    #[test]
    fn rounded_rates_keep_the_exact_one() {
        let grid = |rate: f64| GridSummary {
            rate,
            irregular: false,
            samples: 1000,
            segments: 1,
            stream: None,
            channels: vec!["Fz".into(), "Cz".into()],
        };
        let summary = |grids: Vec<GridSummary>| Summary {
            format: "BrainVision",
            fields: Vec::new(),
            start: None,
            duration: 2.0,
            channels: 2,
            streams: (0, 0),
            annotations: 0,
            grids,
            warnings: Vec::new(),
        };
        let exact = 500.006_492_535_299_46;
        let one = summary(vec![grid(exact)]);
        let rows = one.sidebar_rows();
        let sampling = rows.iter().find(|r| r.key == "Sampling").unwrap();
        assert_eq!(sampling.value, "500.01 Hz");
        assert_eq!(
            sampling.hover.as_deref(),
            Some("500.006493 Hz: 2 channels, 1000 samples")
        );
        assert!(
            rows.iter()
                .filter(|r| r.key != "Sampling")
                .all(|r| r.hover.is_none())
        );
        assert!(
            one.to_string()
                .contains("500.006493 Hz, 1000 samples, 2 channels"),
            "{one}"
        );

        let two = summary(vec![grid(exact), grid(250.0)]);
        let rows = two.sidebar_rows();
        let sampling = rows.iter().find(|r| r.key == "Sampling").unwrap();
        assert_eq!(
            sampling.value,
            "500.01 Hz (2 channels)\n250 Hz (2 channels)"
        );
        assert_eq!(
            sampling.hover.as_deref(),
            Some(
                "500.006493 Hz: 2 channels, 1000 samples\n\
                 250 Hz: 2 channels, 1000 samples"
            )
        );

        // Grids of one rate (split by sample type) are one rate.
        let split = summary(vec![grid(exact), grid(exact)]);
        let rows = split.sidebar_rows();
        let sampling = rows.iter().find(|r| r.key == "Sampling").unwrap();
        assert_eq!(sampling.value, "500.01 Hz");
        assert_eq!(
            sampling.hover.as_deref(),
            Some("500.006493 Hz: 4 channels, 1000 samples")
        );

        // An irregular grid shows its average rate as such, apart from a regular grid of
        // that rate.
        let irregular = GridSummary {
            irregular: true,
            ..grid(250.0)
        };
        let mixed = summary(vec![grid(250.0), irregular.clone()]);
        let rows = mixed.sidebar_rows();
        let sampling = rows.iter().find(|r| r.key == "Sampling").unwrap();
        assert_eq!(
            sampling.value,
            "250 Hz (2 channels)\n\u{2248}250 Hz (2 channels)"
        );
        assert!(
            sampling
                .hover
                .as_deref()
                .unwrap()
                .ends_with("\u{2248}250 Hz: 2 channels, 1000 samples")
        );
        assert!(
            irregular
                .describe()
                .starts_with("\u{2248}250 Hz, 1000 samples")
        );
    }

    #[test]
    fn info_and_window_open_the_same_way() {
        let file = TempFile::preset("bdf+c-small");
        let direct = open(file.path()).unwrap().summary();
        let loaded = load(file.path()).unwrap().summary();
        assert_eq!(direct, loaded);
    }

    #[test]
    fn a_missing_file_is_an_error_naming_it() {
        let file = TempFile::new("bdf");
        let err = load(file.path()).err().unwrap();
        let msg = error_text(&err);
        assert!(msg.starts_with("cannot read "), "{msg}");
        assert!(msg.contains(&file.path().display().to_string()), "{msg}");
        assert_eq!(msg.matches("os error").count(), 1, "cause repeated: {msg}");
        assert_eq!(
            open(file.path()).err().map(|e| error_text(&e)),
            Some(msg),
            "--info reports the same error"
        );
    }

    #[test]
    fn an_unsupported_file_is_an_error() {
        let file = TempFile::new("txt");
        std::fs::write(file.path(), "not a recording").unwrap();
        let msg = error_text(&load(file.path()).err().unwrap());
        assert!(msg.contains("not a supported file format"), "{msg}");
    }

    #[test]
    fn a_truncated_header_is_an_error() {
        let full = TempFile::preset("bdf+c-small");
        let cut = TempFile::new("bdf");
        let bytes = std::fs::read(full.path()).unwrap();
        std::fs::write(cut.path(), &bytes[..300]).unwrap();
        assert!(load(cut.path()).is_err());
    }

    #[test]
    fn a_dead_loader_thread_reports_an_error() {
        let (tx, rx) = mpsc::channel::<Result<Document>>();
        drop(tx);
        let loading = Loading {
            path: "x.edf".into(),
            result: rx,
        };
        let msg = loading.poll().unwrap().err().unwrap().to_string();
        assert!(msg.contains("x.edf"), "{msg}");
    }

    #[test]
    fn error_text_joins_causes_once() {
        let inner = anyhow!("disk on fire");
        assert_eq!(error_text(&inner), "disk on fire");
        let wrapped = inner.context("cannot read a.edf");
        assert_eq!(error_text(&wrapped), "cannot read a.edf: disk on fire");
        let repeated = anyhow!("disk on fire").context("cannot read a.edf: disk on fire");
        assert_eq!(error_text(&repeated), "cannot read a.edf: disk on fire");
    }

    #[test]
    fn durations() {
        assert_eq!(format_duration(0.0), "0 s");
        assert_eq!(format_duration(12.5), "12.5 s");
        assert_eq!(format_duration(10.0), "10 s");
        assert_eq!(format_duration(60.0), "1:00");
        assert_eq!(format_duration(1800.0), "30:00");
        assert_eq!(format_duration(90.25), "1:30.25");
        assert_eq!(format_duration(3723.5), "1:02:03.5");
        assert_eq!(format_duration(8.0 * 3600.0), "8:00:00");
        assert_eq!(format_duration(59.9996), "1:00");
        assert_eq!(format_duration(0.0004), "0 s");
    }
}
