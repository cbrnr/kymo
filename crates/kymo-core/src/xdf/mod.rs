//! XDF (Lab Streaming Layer's LabRecorder, `.xdf`): one file of chunks, each a stream's
//! header, a run of its samples (each with an optional timestamp), a clock offset, or a
//! footer. Streams have their own rates (0 for irregular ones) and value formats.
//!
//! Streams are read in the modes MNELAB (mnextend's `read_raw_xdf`) gives them by default,
//! with these differences:
//!
//! - No resampling: every stream shown as channels keeps its own grid. `t = 0` is the
//!   earliest first timestamp of these streams.
//! - Irregular numeric streams that are not of an event type (Marker(s), Event(s),
//!   Trigger(s), Stim), with one channel or several, are channels on their own
//!   synchronized time stamps (an irregular time base, drawn as held values), where
//!   mnextend makes annotations of them. Steps longer than 1 s and 500 median steps are
//!   gaps; a time stamp before the previous one is taken as the previous one.
//! - Streams that can drop samples are on their own synchronized time stamps too, as pyxdf
//!   leaves them (drawn as lines, gaps at steps longer than 1 s and 2 periods); mnextend
//!   resamples them.
//! - Channels of type "trigger" (or "triggers") are stim channels, as are those of type
//!   "stim" and every channel of a regular stream of an event type; mnextend makes
//!   "trigger" channels misc.
//! - A label two streams share is prefixed by its stream's name, or, where streams share
//!   the name, its type, or else its id ("ACC X", "GYRO X"); labels left empty are named
//!   as unlabeled channels are (`"{stream name}_{n}"`).
//! - Time stamps are synchronized and dejittered as pyxdf does by default (see `time`),
//!   and each stream's segments between breaks keep their own rate: a grid with a segmented
//!   time base, its breaks gaps (mnextend joins the segments). Breaks are steps of more
//!   than 1 s and 2 sample periods (pyxdf's: 1 s and 500 periods, which leaves samples after
//!   a few seconds of lost ones early by that much). Samples before a stream's
//!   first time stamp count back from it (pyxdf counts them on from 0). Marker streams
//!   with a nominal rate are not dejittered (pyxdf dejitters them like signals): their
//!   events are irregular.
//! - A stream's `desc/synchronization/offset_mean` (the XDF meta-data's "subtracted from
//!   XDF timestamps before comparing streams") comes off its synchronized time stamps;
//!   pyxdf ignores it.
//! - Annotation samples without a timestamp take the previous one's plus a sample period
//!   (as in pyxdf); those before a stream's first timestamp are left out.

mod reader;
mod scan;
mod time;
mod xml;

use crate::ModelError;

pub use reader::{open, open_with};

/// The file is malformed, or holds nothing kymo shows.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum XdfError {
    #[error("not an XDF file: it doesn't start with \"XDF:\"")]
    NotXdf,
    #[error("the file has no streams with channels kymo shows{0}")]
    NoChannels(String),
    #[error(transparent)]
    Model(#[from] ModelError),
    #[error("a stream's time stamps can't be laid out: {0}")]
    Times(#[from] crate::TimeTableError),
}
