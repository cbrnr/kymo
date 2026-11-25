//! Annotations to and from files.
//!
//! kymo saves a BIDS-style events table (`.tsv`): a header row, then per annotation its
//! onset and duration in seconds, its description as `trial_type`, and, if any annotation
//! has channels, their names joined by `;` (`\;` and `\\` inside a name), and, if any
//! has a stream, its id as `stream_id` (`n/a` for those without). Fields with a tab,
//! a quote or a line break are quoted as in CSV. Onsets count from the first sample, MNE's
//! time base for onsets, at full precision.
//!
//! It loads those tables (also with `description` or `value` in place of `trial_type`, and
//! `n/a` durations; a `stream_id` that isn't one of the recording's streams with channels
//! or annotations leaves the annotation without a stream) and MNE's CSV as
//! `Annotations.save` writes it: onsets as dates and times, the start time (or 1970-01-01
//! without one) plus the time since the first sample, and channel names joined by `:`
//! (`{COLON}` inside a name).
//!
//! kymo's onsets count from the header's start time, so a file's onset is kymo's minus the
//! first sample's time: annotations land where MNE puts the recording's own.

use std::io::{Read, Write};
use std::path::Path;

use jiff::civil::DateTime;

use crate::{AnnotationData, ChannelId, Origin, Recording, StreamContent};

/// The file formats, told apart by their extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// kymo's BIDS-style events table, `.tsv`.
    Table,
    /// MNE's CSV, `.csv`.
    MneCsv,
}

impl Format {
    /// The format of `path` by its extension, if it has one of them.
    pub fn of(path: &Path) -> Option<Format> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        match extension.as_str() {
            "tsv" => Some(Format::Table),
            "csv" => Some(Format::MneCsv),
            _ => None,
        }
    }
}

/// What a file's times and channels mean: taken from the recording it is for.
#[derive(Clone, Debug, PartialEq)]
pub struct Context {
    /// The header's start time, which kymo's onsets count from.
    pub start_time: Option<DateTime>,
    /// The first sample's time, which the files' onsets count from.
    pub first_sample: f64,
    /// The end of the recording, in kymo's time.
    pub end: f64,
    /// Channel names, by id.
    pub channels: Vec<String>,
    /// Per stream (by index into [`Recording::streams`]) its id, `None` for a stream that
    /// isn't shown and so holds no annotations.
    pub streams: Vec<Option<u32>>,
    /// The start time's offset from UTC where the file's date is in UTC
    /// ([`Recording::utc_offset`]): MNE's CSV then has UTC times.
    pub utc_offset: Option<jiff::tz::Offset>,
}

impl Context {
    pub fn of(recording: &Recording) -> Context {
        Context {
            start_time: recording.start_time(),
            first_sample: recording
                .grids()
                .iter()
                .map(|g| g.time_of(0))
                .reduce(f64::min)
                .unwrap_or(0.0),
            end: recording.duration(),
            channels: recording
                .channels()
                .iter()
                .map(|c| c.name.clone())
                .collect(),
            streams: recording
                .streams()
                .iter()
                .map(|s| (!matches!(s.content, StreamContent::NotShown(_))).then_some(s.id))
                .collect(),
            utc_offset: recording.utc_offset(),
        }
    }
}

/// Annotations read from a file, ready to add, and how many were skipped.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Loaded {
    /// With origin [`Origin::Viewer`], in the file's order.
    pub annotations: Vec<AnnotationData>,
    /// Wholly outside the recording.
    pub outside: usize,
    /// Naming a channel the recording doesn't have.
    pub unknown_channels: usize,
}

/// Why a file couldn't be read.
#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("line {line}: {message}")]
    Line { line: u64, message: String },
    #[error("{0}")]
    Header(String),
}

/// Writes `annotations` as a BIDS-style events table.
pub fn write_table<'a>(
    out: impl Write,
    context: &Context,
    annotations: impl IntoIterator<Item = &'a AnnotationData>,
) -> std::io::Result<()> {
    let annotations: Vec<&AnnotationData> = annotations.into_iter().collect();
    let with_channels = annotations.iter().any(|a| !a.channels.is_empty());
    let with_streams = annotations.iter().any(|a| a.stream.is_some());
    let mut writer = csv::WriterBuilder::new()
        .delimiter(b'\t')
        .terminator(csv::Terminator::Any(b'\n'))
        .from_writer(out);
    let mut header = vec!["onset", "duration", "trial_type"];
    if with_channels {
        header.push("channels");
    }
    if with_streams {
        header.push("stream_id");
    }
    writer.write_record(&header).map_err(io_error)?;
    for a in annotations {
        let mut record = vec![
            (a.onset - context.first_sample).to_string(),
            a.duration.to_string(),
            a.description.clone(),
        ];
        if with_channels {
            let names: Vec<String> = a
                .channels
                .iter()
                .map(|&c| escape(&channel_name(context, c)))
                .collect();
            record.push(names.join(";"));
        }
        if with_streams {
            let id = a
                .stream
                .and_then(|s| context.streams.get(s).copied().flatten());
            record.push(id.map_or_else(|| "n/a".to_owned(), |id| id.to_string()));
        }
        writer.write_record(&record).map_err(io_error)?;
    }
    writer.flush()
}

/// Reads a file of `format` for the recording `context` describes.
pub fn read(input: impl Read, format: Format, context: &Context) -> Result<Loaded, FileError> {
    let delimiter = match format {
        Format::Table => b'\t',
        Format::MneCsv => b',',
    };
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .flexible(false)
        .from_reader(input);
    let header = reader.headers().map_err(csv_error)?.clone();
    let column = |names: &[&str]| {
        // The reader skips a byte order mark, as Excel writes one.
        names
            .iter()
            .find_map(|name| header.iter().position(|h| h.trim() == *name))
    };
    let missing = |name: &str| FileError::Header(format!("no column \"{name}\""));
    let (description_names, channel_name): (&[&str], &str) = match format {
        Format::Table => (&["trial_type", "description", "value"], "channels"),
        Format::MneCsv => (&["description"], "ch_names"),
    };
    let onset_column = column(&["onset"]).ok_or_else(|| missing("onset"))?;
    let duration_column = column(&["duration"]).ok_or_else(|| missing("duration"))?;
    let description_column =
        column(description_names).ok_or_else(|| missing(description_names[0]))?;
    let channels_column = column(&[channel_name]);
    let stream_column = match format {
        Format::Table => column(&["stream_id"]),
        Format::MneCsv => None,
    };

    let mut loaded = Loaded::default();
    for record in reader.records() {
        let record = record.map_err(csv_error)?;
        let line = record.position().map_or(0, |p| p.line());
        let field = |i: usize| record.get(i).unwrap_or("").trim();
        let fail = |message: String| FileError::Line { line, message };
        let since_first = match format {
            Format::Table => number(field(onset_column), "onset").map_err(fail)?,
            Format::MneCsv => mne_time(field(onset_column), context).map_err(fail)?,
        };
        let duration = match field(duration_column) {
            "n/a" if format == Format::Table => 0.0,
            text => number(text, "duration").map_err(fail)?,
        };
        if duration < 0.0 {
            return Err(fail(format!("duration {duration} is negative")));
        }
        let onset = since_first + context.first_sample;
        let end = onset + duration;
        if !end.is_finite() {
            return Err(fail(format!(
                "onset {onset} plus duration {duration} is too large"
            )));
        }
        let names = match channels_column.map(|i| record.get(i).unwrap_or("")) {
            None | Some("") | Some("n/a") => Vec::new(),
            Some(text) => match format {
                Format::Table => split_escaped(text).map_err(fail)?,
                Format::MneCsv => text.split(':').map(|n| n.replace("{COLON}", ":")).collect(),
            },
        };
        let channels: Option<Vec<ChannelId>> = names
            .iter()
            .map(|name| {
                context
                    .channels
                    .iter()
                    .position(|c| c == name)
                    .map(|i| ChannelId(i as u32))
            })
            .collect();
        let Some(channels) = channels else {
            loaded.unknown_channels += 1;
            continue;
        };
        if end < context.first_sample || onset > context.end {
            loaded.outside += 1;
            continue;
        }
        loaded.annotations.push(AnnotationData {
            onset,
            duration,
            description: record.get(description_column).unwrap_or("").to_owned(),
            channels,
            origin: Origin::Viewer,
            stream: stream_column.and_then(|i| stream(field(i), context)),
        });
    }
    Ok(loaded)
}

/// The stream (an index into the recording's streams) whose id is `text`, if one with
/// channels or annotations has it.
fn stream(text: &str, context: &Context) -> Option<usize> {
    let id: u32 = text.parse().ok()?;
    context.streams.iter().position(|&s| s == Some(id))
}

/// A finite number of seconds.
fn number(text: &str, what: &str) -> Result<f64, String> {
    match text.parse::<f64>() {
        Ok(x) if x.is_finite() => Ok(x),
        _ => Err(format!("{what} \"{text}\" is not a number of seconds")),
    }
}

/// Seconds since the first sample of an MNE CSV onset: a date and time, counted from the
/// start time (or 1970-01-01 without one). MNE writes `meas_date + onset`, in UTC where the
/// file's date is UTC (FIFF), so it moves to the start time's offset there.
fn mne_time(text: &str, context: &Context) -> Result<f64, String> {
    let time: DateTime = text.parse().map_err(|_| {
        if text.parse::<f64>().is_ok() {
            format!(
                "onset \"{text}\" is a number: MNE's CSV has dates and times (a .tsv file \
                 has seconds)"
            )
        } else {
            format!("onset \"{text}\" is not a date and time")
        }
    })?;
    let time = match context.utc_offset {
        Some(offset) => time
            .checked_add(jiff::SignedDuration::from_secs(i64::from(offset.seconds())))
            .map_err(|_| format!("onset \"{text}\" is out of range"))?,
        None => time,
    };
    let origin = context
        .start_time
        .unwrap_or_else(|| jiff::civil::date(1970, 1, 1).at(0, 0, 0, 0));
    Ok(time.duration_since(origin).as_secs_f64())
}

/// A channel's name for a file: its own, or its number if the recording has no such channel.
fn channel_name(context: &Context, channel: ChannelId) -> String {
    context
        .channels
        .get(channel.0 as usize)
        .cloned()
        .unwrap_or_else(|| channel.0.to_string())
}

/// `;` and `\` escaped with `\`, so names can be joined by `;`.
fn escape(name: &str) -> String {
    name.replace('\\', "\\\\").replace(';', "\\;")
}

/// Names joined by `;`, with `\;` and `\\` inside them.
fn split_escaped(text: &str) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    let mut name = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(next @ ('\\' | ';')) => name.push(next),
                _ => return Err(format!("channels \"{text}\" has a stray backslash")),
            },
            ';' => names.push(std::mem::take(&mut name)),
            c => name.push(c),
        }
    }
    names.push(name);
    Ok(names)
}

pub(crate) fn io_error(error: csv::Error) -> std::io::Error {
    match error.into_kind() {
        csv::ErrorKind::Io(error) => error,
        other => std::io::Error::other(format!("{other:?}")),
    }
}

pub(crate) fn csv_error(error: csv::Error) -> FileError {
    let line = error.position().map_or(0, |p| p.line());
    match error.into_kind() {
        csv::ErrorKind::Io(error) => FileError::Io(error),
        csv::ErrorKind::Utf8 { .. } => FileError::Line {
            line,
            message: "not UTF-8 text".into(),
        },
        csv::ErrorKind::UnequalLengths {
            expected_len, len, ..
        } => FileError::Line {
            line,
            message: format!("{len} fields where the header has {expected_len}"),
        },
        other => FileError::Line {
            line,
            message: format!("{other:?}"),
        },
    }
}
