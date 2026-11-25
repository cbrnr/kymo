//! Synthetic EDF/BDF, BrainVision, EEGLAB, GDF, FIFF, XDF and BVRF files for tests and
//! benchmarks.
//!
//! A [`Spec`] describes a file: format, channels (each with its own rate and scaling),
//! record layout, gaps (EDF+D/BDF+D) and annotations (TALs in EDF+/BDF+, markers in
//! BrainVision and BVRF, events in EEGLAB and GDF, marker streams in XDF). [`write()`]
//! streams it to disk. Records are EDF's and GDF's data records and FIFF's data buffers;
//! formats without them (BrainVision, EEGLAB, XDF, BVRF) only take their length from them.
//! The sample values come from a deterministic truth function, [`Spec::digital`], so tests
//! can check any reader output without keeping the data in memory.
//!
//! The truth function is a sine plus a sawtooth ramp plus seeded hash noise, together within
//! each channel's [`amplitude`](ChannelSpec::amplitude) of the middle of its digital range (15 uV
//! for EEG, so traces fit MNE's default scale), plus sparse **single-sample spikes** at the
//! digital minimum or maximum. Spikes are the only samples that reach the digital limits, so a
//! min/max pyramid that drops a peak is caught at once. Spikes are rare by default
//! ([`Spec::spike_every`]), so they don't cover the traces in a viewer.

mod brainvision;
mod bvrf;
mod eeglab;
pub mod fiff;
mod gdf;
mod header;
pub mod mat;
mod signal;
mod temp;
mod write;
pub mod xdf;

use std::ops::Range;

pub use gdf::MANUFACTURER as GDF_MANUFACTURER;
pub use temp::{TempDir, TempFile};
pub use write::{write, write_to};
pub use xdf::{MarkerValues, XdfBreak, XdfFormat, XdfMarkers, XdfOptions, XdfStamps, XdfStream};

/// File format to write.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    /// Plain EDF: no annotation signal.
    Edf,
    EdfPlusC,
    EdfPlusD,
    /// Plain BDF (BioSemi, 24-bit).
    Bdf,
    BdfPlusC,
    BdfPlusD,
    /// BrainVision: a header (`.vhdr`, the path written to), the samples (`.eeg`) and the
    /// markers (`.vmrk`) beside it. One rate, no offsets (physical = digital × resolution),
    /// no gaps; a "record" is only a unit of length.
    BrainVision(Orientation, Binary),
    /// EEGLAB: a MATLAB v6 file (`.set`, the path written to) holding the `EEG` struct, its
    /// samples in it or in a `.fdt` beside it. One rate, no offsets: the samples are stored
    /// as physical values (µV), `digital × gain`; no gaps; a "record" is only a unit of
    /// length. Annotations are events.
    Eeglab(Storage, Layout),
    /// GDF: data records as in EDF, each channel in its own sample type
    /// ([`ChannelSpec::stored`]), then an event table of the annotations (their texts are
    /// the type codes). No gaps.
    Gdf(GdfVersion),
    /// FIFF: a raw recording (`.fif`), big-endian. One rate, no offsets (physical = digital
    /// × range × cal, in SI units); a record is a data buffer (see [`FiffOptions`]); gaps
    /// are skips of whole records; `first_onset` is `first_samp / sfreq`; channel types
    /// come from [`ChannelSpec::kind`] (MNE's names, EEG if empty).
    Fiff(FiffType),
    /// XDF: streams of the channels, each in its own value format, timestamped (see
    /// [`XdfOptions`]), and marker streams of the annotations. No offsets: values are
    /// stored as physical values (`digital × gain`); no gaps; a "record" is only a unit of
    /// length.
    Xdf,
    /// BVRF: a header (`.bvrh`, the path written to), the samples (`.bvrd`, multiplexed)
    /// and the markers (`.bvrm`) beside it. One rate; physical = digital × resolution, with
    /// a channel's offset and any factor of [`BvrfOptions::scaling`] as linear
    /// coefficients; no gaps; a "record" is only a unit of length. Channel types come from
    /// [`ChannelSpec::kind`] (EEG if empty).
    Bvrf(BvrfData),
}

/// The numeric data type of a BVRF data file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BvrfData {
    Int16,
    Int32,
    /// The truth function's integers, stored as floats (exact up to 2^24).
    Single,
    /// The truth function's integers, stored as floats.
    Double,
}

impl BvrfData {
    /// The header's `NumericDataType`.
    pub fn name(self) -> &'static str {
        match self {
            BvrfData::Int16 => "Int16",
            BvrfData::Int32 => "Int32",
            BvrfData::Single => "Single",
            BvrfData::Double => "Double",
        }
    }
}

/// How a BVRF channel's numbers become its values.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum BvrfScaling {
    /// The gain as the resolution; the offset, if any, as linear coefficients `[offset, 1]`.
    #[default]
    Resolution,
    /// Linear coefficients `[offset, factor]`: the resolution is the gain over `factor`, as
    /// in the specification's BIP2AUX example (`Num: [0, 50]`).
    Linear(f64),
    /// Coefficients that aren't linear (`Num: [0, 1], Denom: [1, 0.5]`, unit µS), as the
    /// specification's GSR sensor has them. The resolution is the gain, the channel's
    /// dimension its `ResolutionUnit`: kymo shows that sensor output. No offset.
    Rational,
}

/// BVRF's own options.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BvrfOptions {
    /// Per channel, by index; channels past its end are [`BvrfScaling::Resolution`].
    pub scaling: Vec<BvrfScaling>,
    /// The `Participants` section's ids; with more than one, the markers have a
    /// `ParticipantId` column. Empty writes no section.
    pub participants: Vec<String>,
    /// Each channel's participant (an index into `participants`), by index; channels past
    /// its end, and `None`, belong to all participants.
    pub channel_participants: Vec<Option<usize>>,
    /// Each annotation's participant and channel, by index; annotations past its end are
    /// for all participants and channels.
    pub targets: Vec<BvrfTarget>,
}

/// Whom a BVRF marker is for.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BvrfTarget {
    /// An index into [`BvrfOptions::participants`]; `None` for all participants.
    pub participant: Option<usize>,
    /// A channel's label (of the participant, if any); `None` for all channels.
    pub channel: Option<String>,
}

/// The type of FIFF data buffers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FiffType {
    /// 16-bit integers as Neuromag acquisitions write them (DAU_PACK16).
    Pack16,
    Short,
    Int,
    /// The truth function's integers, stored as floats (exact up to 2^24).
    Float,
    Double,
    /// Buffers of short, int, float and double in turn; digital values of int16.
    Mixed,
}

impl FiffType {
    pub const ALL: [FiffType; 6] = [
        FiffType::Pack16,
        FiffType::Short,
        FiffType::Int,
        FiffType::Float,
        FiffType::Double,
        FiffType::Mixed,
    ];
}

/// FIFF's own options.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FiffOptions {
    /// Buffers skipped before the first one (a skip of that many records), which moves
    /// `first_samp` rather than leaving a gap; `first_onset` stays the first sample's.
    pub initial_skip: u64,
    /// Records written as two buffers of different lengths (where no skip precedes them).
    pub varying_buffers: bool,
    /// A MaxShield (Internal Active Shielding) recording.
    pub maxshield: bool,
    /// The local time's offset from UTC (`+HH:MM`), written as MNE does; the start stays
    /// the local time.
    pub utc_offset: Option<String>,
    /// Channel names written as the file's bad channels, as given.
    pub bads: Vec<String>,
    /// For each annotation (by index), the names of the channels it applies to; none (or
    /// an empty list) for all channels.
    pub annotation_channels: Vec<Vec<String>>,
    /// Written as split parts, each of this many records (the last of fewer); a gap before
    /// a part's first record shows as a jump in its first sample, not as a skip.
    pub split: Option<FiffSplit>,
}

/// Split parts of a FIFF recording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FiffSplit {
    pub records: u64,
    pub naming: SplitNaming,
}

/// How split parts are named and referred to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitNaming {
    /// `rec.fif`, `rec-1.fif`, ...; references with file names and numbers.
    Neuromag,
    /// As Neuromag, with references of numbers only, as acquisition systems write them.
    NumbersOnly,
    /// `rec.fif`, `rec_split-02.fif`, ...; references with file names and numbers.
    Bids,
}

/// The GDF version written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GdfVersion {
    /// GDF 1.25: text units, prefilters as text, events of mode 1 (points in time).
    V1,
    /// GDF 2.20: unit codes, filters as numbers, events of mode 3 (with durations).
    V2,
    /// GDF 2.20 with a tag-length-value part in the header (the manufacturer), which MNE
    /// doesn't read.
    V2Tagged,
}

/// A GDF channel's sample type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stored {
    I8,
    U8,
    I16,
    U16,
    I24,
    U24,
    I32,
    U32,
    I64,
    U64,
    F32,
    F64,
}

impl Stored {
    pub const ALL: [Stored; 12] = [
        Stored::I8,
        Stored::U8,
        Stored::I16,
        Stored::U16,
        Stored::I24,
        Stored::U24,
        Stored::I32,
        Stored::U32,
        Stored::I64,
        Stored::U64,
        Stored::F32,
        Stored::F64,
    ];

    /// Bytes per sample.
    pub fn size(self) -> usize {
        match self {
            Stored::I8 | Stored::U8 => 1,
            Stored::I16 | Stored::U16 => 2,
            Stored::I24 | Stored::U24 => 3,
            Stored::I32 | Stored::U32 | Stored::F32 => 4,
            Stored::I64 | Stored::U64 | Stored::F64 => 8,
        }
    }

    /// GDF's type code.
    pub fn code(self) -> u32 {
        match self {
            Stored::I8 => 1,
            Stored::U8 => 2,
            Stored::I16 => 3,
            Stored::U16 => 4,
            Stored::I32 => 5,
            Stored::U32 => 6,
            Stored::I64 => 7,
            Stored::U64 => 8,
            Stored::F32 => 16,
            Stored::F64 => 17,
            Stored::I24 => 279,
            Stored::U24 => 535,
        }
    }

    /// The digital values a channel of this type may use: within `i32` (the truth
    /// function's type), from 0 for unsigned types, exact integers for float32, and never
    /// a signed type's smallest value, which GDF readers take as a missing sample.
    pub fn limits(self) -> (i32, i32) {
        match self {
            Stored::I8 => (-127, 127),
            Stored::U8 => (0, 255),
            Stored::I16 => (-32_767, 32_767),
            Stored::U16 => (0, 65_535),
            Stored::I24 => (-8_388_607, 8_388_607),
            Stored::U24 => (0, 16_777_215),
            Stored::I32 => (-i32::MAX, i32::MAX),
            Stored::U32 | Stored::U64 => (0, i32::MAX),
            Stored::I64 | Stored::F64 => (i32::MIN, i32::MAX),
            Stored::F32 => (-16_777_216, 16_777_216),
        }
    }

    /// Writes `value` (within [`limits`](Self::limits)) little-endian.
    pub fn put(self, value: i32, out: &mut [u8]) {
        let v = i64::from(value);
        match self {
            Stored::I8 | Stored::U8 => out[0] = v as u8,
            Stored::I16 | Stored::U16 => out.copy_from_slice(&(v as u16).to_le_bytes()),
            Stored::I24 | Stored::U24 => out.copy_from_slice(&(v as u32).to_le_bytes()[..3]),
            Stored::I32 | Stored::U32 => out.copy_from_slice(&(v as u32).to_le_bytes()),
            Stored::I64 | Stored::U64 => out.copy_from_slice(&v.to_le_bytes()),
            Stored::F32 => out.copy_from_slice(&(value as f32).to_le_bytes()),
            Stored::F64 => out.copy_from_slice(&f64::from(value).to_le_bytes()),
        }
    }
}

/// Where and how an EEGLAB dataset stores its samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Storage {
    /// float32 in a `.fdt` beside the `.set`.
    Fdt,
    /// float32 inside the `.set`.
    Single,
    /// float64 inside the `.set`; stored as integers if all values are whole numbers, as
    /// MATLAB stores doubles.
    Double,
}

/// How an EEGLAB dataset stores its fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Layout {
    /// One `EEG` struct.
    Struct,
    /// Each field a variable of its own, as newer EEGLAB versions save.
    Flat,
}

/// How a BrainVision data file orders its samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Orientation {
    /// Sample-major: every channel's sample, then the next sample.
    Multiplexed,
    /// Channel-major: all samples of a channel, then the next channel.
    Vectorized,
}

/// The sample type of a BrainVision data file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Binary {
    Int16,
    /// Not read by MNE.
    UInt16,
    Int32,
    /// The truth function's integers, stored as floats (exact up to 2^24).
    Float32,
}

impl Binary {
    pub const ALL: [Binary; 4] = [
        Binary::Int16,
        Binary::UInt16,
        Binary::Int32,
        Binary::Float32,
    ];

    /// The `BinaryFormat` value of the header.
    pub fn name(self) -> &'static str {
        match self {
            Binary::Int16 => "INT_16",
            Binary::UInt16 => "UINT_16",
            Binary::Int32 => "INT_32",
            Binary::Float32 => "IEEE_FLOAT_32",
        }
    }
}

impl Format {
    /// The EDF and BDF formats.
    pub const EDF: [Format; 6] = [
        Format::Edf,
        Format::EdfPlusC,
        Format::EdfPlusD,
        Format::Bdf,
        Format::BdfPlusC,
        Format::BdfPlusD,
    ];

    /// Every BrainVision layout and sample type.
    pub const BRAINVISION: [Format; 8] = [
        Format::BrainVision(Orientation::Multiplexed, Binary::Int16),
        Format::BrainVision(Orientation::Multiplexed, Binary::UInt16),
        Format::BrainVision(Orientation::Multiplexed, Binary::Int32),
        Format::BrainVision(Orientation::Multiplexed, Binary::Float32),
        Format::BrainVision(Orientation::Vectorized, Binary::Int16),
        Format::BrainVision(Orientation::Vectorized, Binary::UInt16),
        Format::BrainVision(Orientation::Vectorized, Binary::Int32),
        Format::BrainVision(Orientation::Vectorized, Binary::Float32),
    ];

    /// Every GDF version.
    pub const GDF: [Format; 3] = [
        Format::Gdf(GdfVersion::V1),
        Format::Gdf(GdfVersion::V2),
        Format::Gdf(GdfVersion::V2Tagged),
    ];

    /// Every FIFF buffer type.
    pub const FIFF: [Format; 6] = [
        Format::Fiff(FiffType::Pack16),
        Format::Fiff(FiffType::Short),
        Format::Fiff(FiffType::Int),
        Format::Fiff(FiffType::Float),
        Format::Fiff(FiffType::Double),
        Format::Fiff(FiffType::Mixed),
    ];

    /// Every BVRF data type.
    pub const BVRF: [Format; 4] = [
        Format::Bvrf(BvrfData::Int16),
        Format::Bvrf(BvrfData::Int32),
        Format::Bvrf(BvrfData::Single),
        Format::Bvrf(BvrfData::Double),
    ];

    /// Every EEGLAB storage and layout.
    pub const EEGLAB: [Format; 6] = [
        Format::Eeglab(Storage::Fdt, Layout::Struct),
        Format::Eeglab(Storage::Single, Layout::Struct),
        Format::Eeglab(Storage::Double, Layout::Struct),
        Format::Eeglab(Storage::Fdt, Layout::Flat),
        Format::Eeglab(Storage::Single, Layout::Flat),
        Format::Eeglab(Storage::Double, Layout::Flat),
    ];

    /// The format's name as kymo reports it (`Recording::format`).
    pub fn name(self) -> &'static str {
        match self {
            Format::Edf => "EDF",
            Format::EdfPlusC => "EDF+C",
            Format::EdfPlusD => "EDF+D",
            Format::Bdf => "BDF",
            Format::BdfPlusC => "BDF+C",
            Format::BdfPlusD => "BDF+D",
            Format::BrainVision(..) => "BrainVision",
            Format::Eeglab(..) => "EEGLAB",
            Format::Gdf(..) => "GDF",
            Format::Fiff(..) => "FIFF",
            Format::Xdf => "XDF",
            Format::Bvrf(_) => "BVRF",
        }
    }

    pub fn is_fiff(self) -> bool {
        matches!(self, Format::Fiff(..))
    }

    pub fn is_xdf(self) -> bool {
        matches!(self, Format::Xdf)
    }

    pub fn is_gdf(self) -> bool {
        matches!(self, Format::Gdf(..))
    }

    pub fn is_bdf(self) -> bool {
        matches!(self, Format::Bdf | Format::BdfPlusC | Format::BdfPlusD)
    }

    pub fn is_brainvision(self) -> bool {
        matches!(self, Format::BrainVision(..))
    }

    pub fn is_bvrf(self) -> bool {
        matches!(self, Format::Bvrf(_))
    }

    pub fn is_eeglab(self) -> bool {
        matches!(self, Format::Eeglab(..))
    }

    /// EDF or BDF, with or without +.
    pub fn is_edf(self) -> bool {
        !self.is_brainvision()
            && !self.is_eeglab()
            && !self.is_gdf()
            && !self.is_fiff()
            && !self.is_xdf()
            && !self.is_bvrf()
    }

    /// EDF+ or BDF+: has an annotation signal with a time-keeping TAL in every record.
    pub fn is_plus(self) -> bool {
        matches!(
            self,
            Format::EdfPlusC | Format::EdfPlusD | Format::BdfPlusC | Format::BdfPlusD
        )
    }

    /// EDF+D or BDF+D: records may be separated by gaps.
    pub fn is_discontinuous(self) -> bool {
        matches!(self, Format::EdfPlusD | Format::BdfPlusD)
    }

    /// Whether physical values have an offset (`digital × gain + offset`); BrainVision's,
    /// EEGLAB's, FIFF's and XDF's are `digital × gain`. BVRF's come from linear coefficients.
    pub fn has_offset(self) -> bool {
        self.is_edf() || self.is_gdf() || self.is_bvrf()
    }

    /// Bytes per stored sample; for GDF, of the default sample type (int16, see
    /// [`ChannelSpec::stored`]).
    pub fn bytes_per_sample(self) -> usize {
        match self {
            Format::Gdf(_) => 2,
            Format::Fiff(t) => t.size(),
            Format::Xdf => 4,
            Format::BrainVision(_, Binary::Int16 | Binary::UInt16) => 2,
            Format::BrainVision(_, Binary::Int32 | Binary::Float32) => 4,
            Format::Bvrf(BvrfData::Int16) => 2,
            Format::Bvrf(BvrfData::Int32 | BvrfData::Single) => 4,
            Format::Bvrf(BvrfData::Double) => 8,
            Format::Eeglab(Storage::Double, _) => 8,
            Format::Eeglab(..) => 4,
            f if f.is_bdf() => 3,
            _ => 2,
        }
    }

    /// The range of digital values the format stores (for float32, the integers it stores
    /// exactly).
    pub fn digital_limits(self) -> (i32, i32) {
        match self {
            Format::BrainVision(_, Binary::Int16) => (-32_768, 32_767),
            Format::BrainVision(_, Binary::UInt16) => (0, 65_535),
            Format::BrainVision(_, Binary::Int32) => (i32::MIN, i32::MAX),
            Format::BrainVision(_, Binary::Float32) => (-16_777_216, 16_777_216),
            Format::Bvrf(BvrfData::Int16) => (-32_768, 32_767),
            Format::Bvrf(BvrfData::Single) => (-16_777_216, 16_777_216),
            Format::Bvrf(_) => (i32::MIN, i32::MAX),
            // Stored as physical values, so any digital range.
            Format::Eeglab(..) => (i32::MIN, i32::MAX),
            // The default type, int16 (see `ChannelSpec::stored`).
            Format::Gdf(_) => Stored::I16.limits(),
            Format::Fiff(FiffType::Pack16 | FiffType::Short | FiffType::Mixed) => (-32_768, 32_767),
            Format::Fiff(FiffType::Float) => (-16_777_216, 16_777_216),
            Format::Fiff(_) => (i32::MIN, i32::MAX),
            // The default value format, float32 (see `ChannelSpec::xdf`).
            Format::Xdf => XdfFormat::F32.limits(),
            f if f.is_bdf() => (-8_388_608, 8_388_607),
            _ => (-32_768, 32_767),
        }
    }

    /// Extension of the file written to: `"edf"`, `"bdf"`, `"vhdr"`, `"set"`, `"gdf"`,
    /// `"fif"`, `"xdf"` or `"bvrh"`.
    pub fn extension(self) -> &'static str {
        match self {
            Format::Fiff(_) => "fif",
            Format::Xdf => "xdf",
            Format::Gdf(_) => "gdf",
            Format::BrainVision(..) => "vhdr",
            Format::Bvrf(_) => "bvrh",
            Format::Eeglab(..) => "set",
            f if f.is_bdf() => "bdf",
            _ => "edf",
        }
    }

    /// Every file [`write()`] writes for `path`: `path` itself, plus BrainVision's or BVRF's
    /// data and marker files or EEGLAB's `.fdt` beside it.
    pub fn files(self, path: &std::path::Path) -> Vec<std::path::PathBuf> {
        match self {
            Format::BrainVision(..) => vec![
                path.to_owned(),
                path.with_extension("eeg"),
                path.with_extension("vmrk"),
            ],
            Format::Bvrf(_) => vec![
                path.to_owned(),
                path.with_extension("bvrd"),
                path.with_extension("bvrm"),
            ],
            Format::Eeglab(Storage::Fdt, _) => vec![path.to_owned(), path.with_extension("fdt")],
            _ => vec![path.to_owned()],
        }
    }
}

/// Default [`Spec::spike_every`]: about one spike per channel every 50 s at 2048 Hz.
pub const SPIKE_EVERY: u64 = 100_000;

/// How far EEG channels reach from zero, in uV: within MNE's default EEG scale (20 uV per
/// half channel spacing), so traces show at Kymo's default zoom.
pub const EEG_AMPLITUDE: f64 = 15.0;

/// One data signal. Numbers are written with Rust's shortest round-trip formatting, so they
/// must fit their 8-character header field exactly as given.
#[derive(Clone, Debug, PartialEq)]
pub struct ChannelSpec {
    pub label: String,
    pub transducer: String,
    /// Physical dimension, e.g. `"uV"`.
    pub dimension: String,
    /// May exceed `physical_max`, which makes the gain negative.
    pub physical_min: f64,
    pub physical_max: f64,
    pub digital_min: i32,
    pub digital_max: i32,
    pub prefilter: String,
    /// Samples per data record; the rate is this over the record duration.
    pub samples_per_record: u32,
    /// A trigger channel: pulses of codes 1 to 255 instead of the EEG-like signal (see
    /// [`ChannelSpec::trigger`]).
    pub trigger: bool,
    /// How far the signal (sine, ramp and noise) reaches from the middle of the digital
    /// range, in physical units; at most 60% of the half-range. Spikes still reach the
    /// digital limits. Unused for trigger channels.
    pub amplitude: f64,
    /// The channel's type where the format stores one (EEGLAB's `chanlocs.type`, MNE's type
    /// names); empty writes none.
    pub kind: String,
    /// GDF: the sample type, `None` for int16. Its [limits](Stored::limits) bound the
    /// digital range.
    pub stored: Option<Stored>,
}

impl ChannelSpec {
    /// An EEG channel with the usual full-range scaling of the format:
    /// 0.1 uV per bit for EDF, about 0.03125 uV per bit for BDF (as BioSemi). Its signal
    /// stays within [`EEG_AMPLITUDE`] of zero, as real EEG does.
    ///
    /// BrainVision channels get 0.1 µV per step and no offset: int16 over its whole range,
    /// uint16 from 0 (so its signal sits at 3276.75 µV), int32 and float32 over BDF's range.
    /// BVRF channels as BrainVision's (float64 over BDF's range too).
    /// EEGLAB channels too, stored as the physical values. GDF channels are int16 at 0.1 µV
    /// per step (see [`ChannelSpec::gdf`] for the other types).
    pub fn eeg(label: impl Into<String>, samples_per_record: u32, format: Format) -> Self {
        if format.is_gdf() {
            return ChannelSpec::gdf(label, samples_per_record, Stored::I16);
        }
        if format.is_fiff() {
            return ChannelSpec::fiff(label, "eeg", samples_per_record, format);
        }
        if format.is_xdf() {
            return ChannelSpec::xdf(
                label,
                "EEG",
                "microvolts",
                samples_per_record,
                XdfFormat::F32,
            );
        }
        let (digital_min, digital_max, physical_min, physical_max) = match format {
            Format::BrainVision(_, Binary::Int16) | Format::Bvrf(BvrfData::Int16) => {
                (-32_768, 32_767, -3276.8, 3276.7)
            }
            Format::BrainVision(_, Binary::UInt16) => (0, 65_535, 0.0, 6553.5),
            Format::BrainVision(..) | Format::Eeglab(..) | Format::Bvrf(_) => {
                (-8_388_608, 8_388_607, -838_860.8, 838_860.7)
            }
            f if f.is_bdf() => (-8_388_608, 8_388_607, -262_144.0, 262_143.0),
            _ => (-32_768, 32_767, -3276.8, 3276.7),
        };
        ChannelSpec {
            label: label.into(),
            transducer: "AgAgCl electrode".into(),
            dimension: if !format.is_edf() { "\u{b5}V" } else { "uV" }.into(),
            physical_min,
            physical_max,
            digital_min,
            digital_max,
            prefilter: "HP:0.1Hz LP:100Hz".into(),
            samples_per_record,
            trigger: false,
            amplitude: EEG_AMPLITUDE,
            kind: String::new(),
            stored: None,
        }
    }

    /// A FIFF channel of MNE's type `kind` ("eeg", "mag", "grad", "eog", ...) in SI units,
    /// over the digital range of `format`'s buffer type: 0.1 µV per step for voltages (so
    /// its signal reaches [`EEG_AMPLITUDE`] µV, 120 µV for EOG, 400 µV for ECG and EMG),
    /// 0.1 fT for magnetometers and MEG references (800 fT), 0.1 fT/cm for gradiometers
    /// (300 fT/cm); other types in volts.
    pub fn fiff(
        label: impl Into<String>,
        kind: &str,
        samples_per_record: u32,
        format: Format,
    ) -> Self {
        let (lo, hi) = format.digital_limits();
        let (dimension, gain, amplitude) = match kind {
            "mag" | "ref_meg" => ("T", 1e-16, 8e-13),
            "grad" => ("T/m", 1e-14, 3e-11),
            "eog" => ("V", 1e-7, 120e-6),
            "ecg" | "emg" => ("V", 1e-7, 400e-6),
            _ => ("V", 1e-7, EEG_AMPLITUDE * 1e-6),
        };
        ChannelSpec {
            label: label.into(),
            transducer: String::new(),
            dimension: dimension.into(),
            physical_min: f64::from(lo) * gain,
            physical_max: f64::from(hi) * gain,
            digital_min: lo,
            digital_max: hi,
            prefilter: String::new(),
            samples_per_record,
            trigger: false,
            amplitude,
            kind: kind.into(),
            stored: None,
        }
    }

    /// An XDF channel of type `kind` with `unit`, over the whole range of `format`
    /// ([`XdfFormat::limits`]): a step of 1 for integer formats, 1/16 for float ones (so
    /// float32 stores every value exactly). Its signal reaches [`EEG_AMPLITUDE`] (in
    /// `unit`) from zero.
    pub fn xdf(
        label: impl Into<String>,
        kind: &str,
        unit: &str,
        samples_per_record: u32,
        format: XdfFormat,
    ) -> Self {
        let (lo, hi) = format.limits();
        let gain = if format.is_float() { 0.0625 } else { 1.0 };
        ChannelSpec {
            label: label.into(),
            transducer: String::new(),
            dimension: unit.into(),
            physical_min: f64::from(lo) * gain,
            physical_max: f64::from(hi) * gain,
            digital_min: lo,
            digital_max: hi,
            prefilter: String::new(),
            samples_per_record,
            trigger: false,
            amplitude: EEG_AMPLITUDE,
            kind: kind.into(),
            stored: None,
        }
    }

    /// A GDF EEG channel of sample type `stored`, over the type's whole
    /// [range](Stored::limits), with physical values centred on 0 µV (so unsigned types get
    /// an offset): about 0.25 µV per step for 8-bit types, 0.1 µV for 16-bit ones and
    /// float32, 1/32 µV for 24-bit ones and 2^-13 µV for wider ones.
    pub fn gdf(label: impl Into<String>, samples_per_record: u32, stored: Stored) -> Self {
        let (lo, hi) = stored.limits();
        let gain = match stored {
            Stored::I8 | Stored::U8 => 0.25,
            Stored::I16 | Stored::U16 | Stored::F32 => 0.1,
            Stored::I24 | Stored::U24 => 0.031_25,
            _ => 1.0 / 8192.0,
        };
        let middle = (f64::from(lo) + f64::from(hi)) / 2.0;
        let middle = if lo < 0 { 0.0 } else { middle.floor() };
        ChannelSpec {
            label: label.into(),
            transducer: "AgAgCl electrode".into(),
            dimension: "uV".into(),
            physical_min: (f64::from(lo) - middle) * gain,
            physical_max: (f64::from(hi) - middle) * gain,
            digital_min: lo,
            digital_max: hi,
            prefilter: "HP:0.1Hz LP:100Hz".into(),
            samples_per_record,
            trigger: false,
            amplitude: EEG_AMPLITUDE,
            kind: String::new(),
            stored: Some(stored),
        }
    }

    /// A trigger channel with unit scaling (physical = digital), named as readers expect:
    /// "Status" for BDF, "TRIGGER" for EDF.
    ///
    /// Every half second holds, depending on a hash: nothing, a pulse of one code (1 to 255,
    /// one sample to an eighth of a second long), or two codes back to back without a 0
    /// between them. The first pulse may start at sample 0. In BDF, bits 16 to 23 carry
    /// status bits as BioSemi's do: bit 20 always set, bit 16 toggling every 1.5 s, so the
    /// raw value is never 0 and only its low 16 bits hold the codes.
    ///
    /// BrainVision has no trigger channels (its events are markers); there this is a channel
    /// of unit "Boolean" over codes 0 to 65535 (32767 for int16), which reads as misc.
    /// EEGLAB stores the codes 0 to 65535 as they are, with the type "STIM".
    pub fn trigger(samples_per_record: u32, format: Format) -> Self {
        let (digital_min, digital_max) = match format.digital_limits() {
            (lo, hi) if !format.is_edf() => (lo.max(0), hi.min(65_535)),
            limits => limits,
        };
        ChannelSpec {
            label: if format.is_bdf() { "Status" } else { "TRIGGER" }.into(),
            transducer: "Triggers and Status".into(),
            dimension: "Boolean".into(),
            physical_min: f64::from(digital_min),
            physical_max: f64::from(digital_max),
            digital_min,
            digital_max,
            prefilter: "No filtering".into(),
            samples_per_record,
            trigger: true,
            amplitude: 0.0,
            kind: if format.is_eeglab() { "STIM" } else { "" }.into(),
            stored: None,
        }
    }

    /// `physical = digital * gain + offset`, as EDF defines it.
    pub fn gain(&self) -> f64 {
        (self.physical_max - self.physical_min)
            / (f64::from(self.digital_max) - f64::from(self.digital_min))
    }

    pub fn offset(&self) -> f64 {
        self.physical_min - self.gain() * f64::from(self.digital_min)
    }
}

/// A gap before a data record (EDF+D/BDF+D only).
#[derive(Clone, Debug, PartialEq)]
pub struct GapSpec {
    /// The record the gap precedes, `1..n_records`.
    pub before_record: u64,
    /// Length of the gap in seconds, `> 0`.
    pub seconds: f64,
}

/// One TAL (time-stamped annotation list) entry: one or more texts sharing onset and duration.
#[derive(Clone, Debug, PartialEq)]
pub struct Tal {
    /// Seconds since the start date/time of the file (not since the first record).
    pub onset: f64,
    /// `None` writes no duration field.
    pub duration: Option<f64>,
    /// UTF-8 texts; each becomes one annotation.
    pub texts: Vec<String>,
}

/// Errors of the generator.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid spec: {0}")]
    Invalid(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// A complete description of a synthetic file.
#[derive(Clone, Debug, PartialEq)]
pub struct Spec {
    pub format: Format,
    /// Start date and time; the year must be in 1985..=2084 (EDF's two-digit year).
    pub start: jiff::civil::DateTime,
    /// Local patient identification. EDF+ expects four subfields, e.g. `"X X X X"`.
    pub patient: String,
    /// Local recording identification. For EDF+/BDF+ the writer prepends
    /// `"Startdate dd-MMM-yyyy "`, so this holds the remaining subfields, e.g. `"X X X"`.
    pub recording: String,
    /// Seconds; written as is, so it must fit 8 characters.
    pub record_duration: f64,
    pub n_records: u64,
    /// EDF+/BDF+: onset of the first record after the start time, in seconds (e.g. a
    /// sub-second start). Must be 0 for plain EDF/BDF.
    pub first_onset: f64,
    pub channels: Vec<ChannelSpec>,
    /// Sorted by record; EDF+D/BDF+D only.
    pub gaps: Vec<GapSpec>,
    /// EDF+/BDF+ only. Each goes into the record whose time span holds its onset (the first or
    /// last record if outside all of them, the preceding record if in a gap).
    pub annotations: Vec<Tal>,
    /// Seeds the noise and spike positions.
    pub seed: u64,
    /// One sample in this many is a spike, on average; 0 for none. The default,
    /// [`SPIKE_EVERY`], keeps them rare on screen; tests that hunt peaks set it lower.
    pub spike_every: u64,
    /// FIFF's own options; ignored for other formats.
    pub fiff: FiffOptions,
    /// XDF's own options; ignored for other formats.
    pub xdf: XdfOptions,
    /// BVRF's own options; ignored for other formats.
    pub bvrf: BvrfOptions,
}

impl Spec {
    /// A spec with default identification, start 2024-01-02 03:04:05, no gaps, no annotations.
    pub fn new(
        format: Format,
        channels: Vec<ChannelSpec>,
        record_duration: f64,
        n_records: u64,
    ) -> Self {
        Spec {
            format,
            start: jiff::civil::date(2024, 1, 2).at(3, 4, 5, 0),
            patient: "X X X X".into(),
            recording: if format.is_plus() {
                "X X X".into()
            } else {
                "kymo-testgen".into()
            },
            record_duration,
            n_records,
            first_onset: 0.0,
            channels,
            gaps: Vec::new(),
            annotations: Vec::new(),
            seed: 0,
            spike_every: SPIKE_EVERY,
            fiff: FiffOptions::default(),
            xdf: XdfOptions::default(),
            bvrf: BvrfOptions::default(),
        }
    }

    /// Checks everything the writer needs; [`write()`] calls it.
    pub fn validate(&self) -> Result<(), Error> {
        header::validate(self)
    }

    /// Sampling rate of a data channel in Hz.
    pub fn rate(&self, channel: usize) -> f64 {
        f64::from(self.channels[channel].samples_per_record) / self.record_duration
    }

    /// Onset of a record in seconds since the start time, as written in its time-keeping TAL.
    pub fn record_onset(&self, record: u64) -> f64 {
        let gaps: f64 = self
            .gaps
            .iter()
            .take_while(|g| g.before_record <= record)
            .fold(0.0, |sum, g| sum + g.seconds);
        onset(self.first_onset, record, self.record_duration, gaps)
    }

    /// Index of the first sample of `record` in `channel`'s sample grid.
    ///
    /// The grid starts at the first record. Without gaps, record `r` starts at
    /// `r * samples_per_record`. In EDF+D/BDF+D it starts at
    /// `round((onset(r) - onset(0)) * samples_per_record / record_duration)`: record onsets are
    /// rounded to the nearest sample, and samples between records are gaps.
    pub fn record_start(&self, channel: usize, record: u64) -> u64 {
        let spr = self.channels[channel].samples_per_record;
        if self.format.is_discontinuous() || self.format.is_fiff() {
            let elapsed = self.record_onset(record) - self.record_onset(0);
            grid_start(elapsed, spr, self.record_duration)
        } else {
            record * u64::from(spr)
        }
    }

    /// Length of `channel`'s sample grid, gaps included.
    pub fn len(&self, channel: usize) -> u64 {
        match self.n_records {
            0 => 0,
            n => {
                self.record_start(channel, n - 1)
                    + u64::from(self.channels[channel].samples_per_record)
            }
        }
    }

    /// The sample ranges of `channel` that hold data: sorted, disjoint and not touching.
    pub fn segments(&self, channel: usize) -> Vec<Range<u64>> {
        let spr = u64::from(self.channels[channel].samples_per_record);
        let mut segments: Vec<Range<u64>> = Vec::new();
        let mut starts = std::iter::once(0)
            .chain(self.gaps.iter().map(|g| g.before_record))
            .chain(std::iter::once(self.n_records))
            .peekable();
        while let (Some(first), Some(&next)) = (starts.next(), starts.peek()) {
            if first < next {
                let end = self.record_start(channel, next - 1) + spr;
                let segment = self.record_start(channel, first)..end;
                // A gap shorter than half a sample rounds away.
                match segments.last_mut() {
                    Some(last) if last.end == segment.start => last.end = segment.end,
                    _ => segments.push(segment),
                }
            }
        }
        segments
    }

    /// Every file [`write()`] writes for `path`: [`Format::files`], and for a split FIFF
    /// recording each of its parts.
    pub fn files(&self, path: &std::path::Path) -> Vec<std::path::PathBuf> {
        if self.format.is_fiff() {
            fiff::files(self, path)
        } else {
            self.format.files(path)
        }
    }

    /// The truth function: digital value of `channel` at grid sample `i`.
    ///
    /// Defined for every `i`; in gaps no value is stored.
    pub fn digital(&self, channel: usize, i: u64) -> i32 {
        let signal = signal::ChannelSignal::new(self, channel);
        signal.value(i, signal.sine(i % signal.sine_period))
    }

    /// The trigger code (the low 16 bits) of a trigger channel at `i`; `None` for other
    /// channels.
    pub fn trigger_code(&self, channel: usize, i: u64) -> Option<i32> {
        signal::ChannelSignal::new(self, channel).code(i)
    }

    /// Whether the truth function has a spike at `i` (a value at the digital min or max).
    pub fn is_spike(&self, channel: usize, i: u64) -> bool {
        signal::ChannelSignal::new(self, channel).spike(i).is_some()
    }

    /// `digital(channel, i) * gain + offset`, in f64; without the offset for formats that
    /// have none ([`Format::has_offset`]).
    /// FIFF stores the gain in float32 (range × cal), so its physical values use that.
    pub fn physical(&self, channel: usize, i: u64) -> f64 {
        let c = &self.channels[channel];
        let offset = if self.format.has_offset() {
            c.offset()
        } else {
            0.0
        };
        let gain = if self.format.is_fiff() {
            f64::from(c.gain() as f32)
        } else {
            c.gain()
        };
        f64::from(self.digital(channel, i)) * gain + offset
    }

    /// EDF/BDF: size of the header in bytes, 256 per signal plus 256, annotation signal
    /// included.
    pub fn header_bytes(&self) -> u64 {
        256 * (self.signal_count() as u64 + 1)
    }

    /// EDF/BDF: size of one data record in bytes. An error for other formats.
    pub fn record_bytes(&self) -> Result<u64, Error> {
        Ok(write::Layout::new(self)?.record_bytes as u64)
    }

    /// Size of the file in bytes; for BrainVision, EEGLAB and BVRF, of the samples; for a split
    /// FIFF recording, of all parts written as `rec.fif` (see [`fiff::file_size_at`]).
    pub fn file_size(&self) -> Result<u64, Error> {
        if self.format.is_gdf() {
            return gdf::file_size(self);
        }
        if self.format.is_fiff() {
            return fiff::file_size(self);
        }
        if self.format.is_xdf() {
            return xdf::file_size(self);
        }
        if !self.format.is_edf() {
            self.validate()?;
            let samples = self.len(0) * self.channels.len() as u64;
            return Ok(samples * self.format.bytes_per_sample() as u64);
        }
        Ok(self.header_bytes() + self.n_records * self.record_bytes()?)
    }

    /// Number of signals in the file: the data channels plus the annotation signal, if any.
    pub fn signal_count(&self) -> usize {
        self.channels.len() + usize::from(self.format.is_plus())
    }

    /// Samples per record of the annotation signal (EDF+/BDF+), sized to fit the largest TAL
    /// list of any record.
    pub fn annotation_samples_per_record(&self) -> Result<Option<u32>, Error> {
        Ok(write::Layout::new(self)?.annotation_spr)
    }

    /// A named preset (see [`PRESETS`]).
    pub fn preset(name: &str) -> Option<Spec> {
        presets::get(name)
    }
}

/// Names and descriptions of the presets.
pub const PRESETS: &[(&str, &str)] = presets::LIST;

fn onset(first_onset: f64, record: u64, record_duration: f64, gaps: f64) -> f64 {
    first_onset + record as f64 * record_duration + gaps
}

fn grid_start(elapsed: f64, samples_per_record: u32, record_duration: f64) -> u64 {
    (elapsed * f64::from(samples_per_record) / record_duration).round() as u64
}

mod presets {
    use super::{
        Binary, BvrfData, BvrfScaling, ChannelSpec, EEG_AMPLITUDE, FiffSplit, FiffType, Format,
        GapSpec, GdfVersion, Layout, MarkerValues, Orientation, Spec, SplitNaming, Storage, Stored,
        Tal, XdfBreak, XdfFormat, XdfMarkers, XdfStamps, XdfStream,
    };

    pub const LIST: &[(&str, &str)] = &[
        (
            "edf-64ch-1h",
            "EDF, 64 EEG channels at 1000 Hz, 1 h (461 MB)",
        ),
        (
            "edf-306ch-1h",
            "EDF, 306 EEG channels at 1000 Hz, 1 h, standing in for a 306-channel MEG \
             recording (2 GB)",
        ),
        (
            "bdf-256ch-8h",
            "BDF, 256 EEG channels at 2048 Hz, 8 h (45 GB)",
        ),
        (
            "fif-306ch-1h",
            "FIFF, float32, 102 magnetometers and 204 gradiometers (Neuromag's triplets) at \
             1000 Hz, 1 h (4 GB)",
        ),
        (
            "edf+d-mixed",
            "EDF+D, 7 channels at 256/512/64 Hz, 10 min with 3 gaps, annotations, \
             huge offset, negative gain and a trigger channel",
        ),
        (
            "bdf+c-small",
            "BDF+C, 5 channels at 512 Hz, 1 min, annotations, negative gain and a Status \
             channel",
        ),
        (
            "edf+c-annotations",
            "EDF+C, 32 EEG channels at 256 Hz, 1 h, 10,000 annotations of 8 descriptions, \
             overlapping, a fifth of them events (59 MB)",
        ),
        (
            "edf+c-kinds",
            "EDF+C, a channel of every type EDF+ labels and units give (EEG, sEEG, ECoG, \
             DBS, EOG, ECG, EMG, Resp, Bio, MEG mag and grad, a trigger) at 256 Hz, 1 min",
        ),
        (
            "bv-int16-multiplexed",
            "BrainVision, multiplexed int16, 8 channels (EEG, EOG, ECG, misc) at 500 Hz, \
             2 min, markers",
        ),
        (
            "bv-int32-vectorized",
            "BrainVision, vectorized int32, 8 channels at 1000 Hz, 1 min, a large DC \
             channel, markers",
        ),
        (
            "bv-float32-multiplexed",
            "BrainVision, multiplexed float32, 4 channels at 256 Hz, 1 min, units with the \
             Greek mu, markers",
        ),
        (
            "bvrf-int16",
            "BVRF, int16, 9 channels (EEG, EOG, ECG, a photo sensor with a linear coefficient, \
             a temperature with an offset) at 500 Hz, 2 min, markers and start-end pairs",
        ),
        (
            "bvrf-float64",
            "BVRF, float64, 8 channels at 1000 Hz, 1 min, a large DC channel and a GSR sensor \
             whose coefficients aren't linear, markers",
        ),
        (
            "bvrf-participants",
            "BVRF, float32, 3 participants with 4 channels each (EEG, EOG), interleaved, and \
             2 channels of all of them at 500 Hz, 1 min, markers for each participant, each \
             participant's channel and all of them",
        ),
        (
            "eeglab-fdt",
            "EEGLAB, samples in a .fdt, 8 channels at 256 Hz (types EEG, EOG, ECG, resp, one \
             MNE lacks, one without), 1 min, string and numeric events, durations, a boundary",
        ),
        (
            "eeglab-set-single",
            "EEGLAB, float32 samples inside the .set, fields as variables, 4 channels at \
             500 Hz, 1 min, events",
        ),
        (
            "eeglab-set-double",
            "EEGLAB, float64 samples inside the .set, 8 channels at 1000 Hz, 1 min, a 1 V DC \
             channel and a stim channel, events",
        ),
        (
            "gdf1-float64",
            "GDF 1.25, float64, 7 channels at 256/128 Hz in records of 0.25 s, 1 min, \
             labels with types (EEG:Fz, EOG:L), a 1 V DC channel, a STATUS channel, events",
        ),
        (
            "gdf2-int32",
            "GDF 2.20, int32, 8 channels at 500 Hz (EEG, ECG in mV, SpO2 in %, a trigger), \
             1 min, events with durations",
        ),
        (
            "gdf2-mixed",
            "GDF 2.20 with a tag-length-value header, a channel of each of the 12 sample \
             types at 256 or 128 Hz, 1 min, events with durations (MNE reads neither)",
        ),
        (
            "fif-float32-meg",
            "FIFF, float32, 12 channels at 600 Hz (magnetometers, gradiometers, EEG, EOG, \
             ECG, a 5 V trigger line, STI 014), 1 min from first_samp 25800, annotations \
             (some on channels), two bad channels",
        ),
        (
            "fif-split",
            "FIFF, int16, 7 channels at 1000 Hz (EEG, STI 014 and a trigger line it \
             combines), 90 s in 3 split parts (Neuromag's names), a skip, annotations, a bad \
             channel",
        ),
        (
            "fif-int16-skips",
            "FIFF, DAU_PACK16, 8 channels at 1000 Hz, 2 min, an initial skip and two skips \
             in the data, a UTC offset",
        ),
        (
            "fif-int32-varying",
            "FIFF, int32, 6 channels at 512 Hz with names longer than 15 characters, \
             1 min in buffers of varying length",
        ),
        (
            "fif-double-dc",
            "FIFF, float64, 4 channels at 250 Hz, 1 min, a 1 V DC channel and STI 014",
        ),
        (
            "fif-mixed",
            "FIFF, buffers of short, int, float and double in turn, 5 channels at 400 Hz, \
             30 s",
        ),
        (
            "xdf-float32",
            "XDF, float32, 8 EEG channels at 500 Hz in microvolts, 1 min, a string marker \
             stream, the file's date and time, clock offsets and boundary chunks",
        ),
        (
            "xdf-mixed",
            "XDF, a stream of each value format (int8 to double64) at 52 to 256 Hz, starting \
             at different times, timestamps on all, some or the first sample of each chunk, \
             chunks of varying length, labels two streams share, an undescribed stream, a \
             large DC channel, 90 s, string markers",
        ),
        (
            "xdf-events",
            "XDF, float64 EEG with a Trigger channel at 250 Hz, a regular int32 Markers \
             stream at 100 Hz (stim), numeric and string marker streams (one at a nominal \
             rate), an irregular two-channel stream (channels on its own time stamps), 1 min",
        ),
        (
            "xdf-clocks",
            "XDF, 8 EEG channels at 500 Hz and an accelerometer at 52 Hz on drifting clocks \
             with a reset, jittered time stamps, rates off nominal, breaks of 3 and 4 s (the \
             second with the rate changing), string markers, 2 min",
        ),
        (
            "xdf-irregular",
            "XDF, 4 EEG channels at 250 Hz, an irregular three-channel eye tracker (about \
             300 Hz, jittered, pauses of 8 and 3 s) and a 100 Hz stream that can drop samples \
             (a pause of 2.5 s), on drifting clocks, string markers, 2 min",
        ),
        (
            "xdf-eyetracker",
            "XDF, an irregular three-channel eye tracker at about 1000 Hz (jittered, pauses of \
             12, 30 and 5 s), string markers, 1 h",
        ),
    ];

    pub fn get(name: &str) -> Option<Spec> {
        let spec = match name {
            "edf-64ch-1h" => eeg(Format::Edf, 64, 1000, 1.0, 3600),
            "edf-306ch-1h" => eeg(Format::Edf, 306, 1000, 1.0, 3600),
            "bdf-256ch-8h" => eeg(Format::Bdf, 256, 2048, 1.0, 8 * 3600),
            "fif-306ch-1h" => fif_306ch_1h(),
            "edf+d-mixed" => edf_d_mixed(),
            "bdf+c-small" => bdf_c_small(),
            "edf+c-annotations" => edf_c_annotations(),
            "edf+c-kinds" => edf_c_kinds(),
            "bv-int16-multiplexed" => bv_int16_multiplexed(),
            "bv-int32-vectorized" => bv_int32_vectorized(),
            "bv-float32-multiplexed" => bv_float32_multiplexed(),
            "bvrf-int16" => bvrf_int16(),
            "bvrf-float64" => bvrf_float64(),
            "bvrf-participants" => bvrf_participants(),
            "eeglab-fdt" => eeglab_fdt(),
            "eeglab-set-single" => eeglab_set_single(),
            "eeglab-set-double" => eeglab_set_double(),
            "gdf1-float64" => gdf1_float64(),
            "gdf2-int32" => gdf2_int32(),
            "gdf2-mixed" => gdf2_mixed(),
            "fif-float32-meg" => fif_float32_meg(),
            "fif-split" => fif_split(),
            "fif-int16-skips" => fif_int16_skips(),
            "fif-int32-varying" => fif_int32_varying(),
            "fif-double-dc" => fif_double_dc(),
            "fif-mixed" => fif_mixed(),
            "xdf-float32" => xdf_float32(),
            "xdf-mixed" => xdf_mixed(),
            "xdf-events" => xdf_events(),
            "xdf-clocks" => xdf_clocks(),
            "xdf-irregular" => xdf_irregular(),
            "xdf-eyetracker" => xdf_eyetracker(),
            _ => return None,
        };
        Some(spec)
    }

    fn eeg(format: Format, n: usize, spr: u32, record_duration: f64, records: u64) -> Spec {
        let channels = (1..=n)
            .map(|i| ChannelSpec::eeg(format!("EEG {i:03}"), spr, format))
            .collect();
        Spec::new(format, channels, record_duration, records)
    }

    /// A Neuromag recording's 102 sensor sites, each a magnetometer ("MEG 0111") and two
    /// planar gradiometers ("MEG 0112", "MEG 0113"), in 1 s buffers.
    fn fif_306ch_1h() -> Spec {
        let f = Format::Fiff(FiffType::Float);
        let channels = (1..=102)
            .flat_map(|site| {
                [(1, "mag"), (2, "grad"), (3, "grad")].map(|(k, kind)| {
                    ChannelSpec::fiff(format!("MEG {:03}{k}", 10 + site), kind, 1000, f)
                })
            })
            .collect();
        Spec::new(f, channels, 1.0, 3600)
    }

    fn edf_d_mixed() -> Spec {
        let f = Format::EdfPlusD;
        let mut channels: Vec<ChannelSpec> = ["Fz", "Cz", "Pz", "Oz"]
            .iter()
            .map(|l| ChannelSpec::eeg(format!("EEG {l}"), 128, f))
            .collect();
        channels.push(ChannelSpec {
            // A large DC offset: 1e6 uV with 0.0153 uV per bit.
            label: "EEG DC".into(),
            physical_min: 999_500.0,
            physical_max: 1_000_500.0,
            ..ChannelSpec::eeg("", 256, f)
        });
        channels.push(ChannelSpec {
            label: "Resp".into(),
            transducer: "thermistor".into(),
            dimension: "mV".into(),
            physical_min: 5.0,
            physical_max: -5.0,
            // A respiration channel (EDF+'s "Resp" type): MNE's default scale (1 V per
            // spacing) shows it flat, Auto Scale fits it.
            amplitude: EEG_AMPLITUDE / 1000.0,
            ..ChannelSpec::eeg("", 32, f)
        });
        channels.push(ChannelSpec::trigger(256, f));
        let mut spec = Spec::new(f, channels, 0.5, 1200);
        spec.first_onset = 0.25;
        spec.gaps = vec![
            GapSpec {
                before_record: 200,
                seconds: 1.5,
            },
            GapSpec {
                before_record: 500,
                seconds: 10.0,
            },
            GapSpec {
                before_record: 900,
                seconds: 0.25,
            },
        ];
        spec.annotations = (0..80)
            .map(|k| {
                let onset = 0.25 + f64::from(k) * 7.375;
                match k % 4 {
                    0 => Tal {
                        onset,
                        duration: None,
                        texts: vec![format!("event {k}")],
                    },
                    1 => Tal {
                        onset,
                        duration: Some(2.5),
                        texts: vec!["sleep stage N2".into()],
                    },
                    2 => Tal {
                        onset,
                        duration: Some(0.0),
                        texts: vec!["eyes closed".into(), "Elektrodenprüfung µV".into()],
                    },
                    _ => Tal {
                        onset,
                        duration: Some(0.125),
                        texts: vec![format!("stim {}", k % 7)],
                    },
                }
            })
            .collect();
        spec.seed = 7;
        spec
    }

    /// Many annotations, for the annotation editing benchmark: one every 0.36 s, shifted
    /// a little, up to 4.2 s long, so many overlap.
    fn edf_c_annotations() -> Spec {
        const DESCRIPTIONS: [&str; 8] = [
            "BAD",
            "blink",
            "saccade",
            "spindle",
            "K-complex",
            "arousal",
            "T0",
            "T1",
        ];
        let mut spec = eeg(Format::EdfPlusC, 32, 256, 1.0, 3600);
        spec.annotations = (0..10_000u32)
            .map(|k| Tal {
                onset: f64::from(k) * 0.36 + f64::from(k * 37 % 30) / 100.0,
                duration: (k % 5 != 0).then(|| f64::from(k % 7) * 0.7),
                texts: vec![DESCRIPTIONS[k as usize % DESCRIPTIONS.len()].into()],
            })
            .collect();
        spec
    }

    fn bdf_c_small() -> Spec {
        let f = Format::BdfPlusC;
        let mut channels: Vec<ChannelSpec> = ["A1", "A2", "A3"]
            .iter()
            .map(|l| ChannelSpec::eeg(*l, 512, f))
            .collect();
        channels.push(ChannelSpec {
            label: "EXG1".into(),
            physical_min: 262_143.0,
            physical_max: -262_144.0,
            ..ChannelSpec::eeg("", 512, f)
        });
        channels.push(ChannelSpec::trigger(512, f));
        let mut spec = Spec::new(f, channels, 1.0, 60);
        spec.annotations = (0..12)
            .map(|k| Tal {
                onset: f64::from(k) * 5.0 + 0.5,
                duration: (k % 2 == 1).then_some(1.0),
                texts: vec![format!("trigger {k}")],
            })
            .collect();
        spec.seed = 3;
        spec
    }

    /// A channel of each type an EDF+ label's type prefix or a magnetic unit gives, each
    /// within its type's default scale.
    fn edf_c_kinds() -> Spec {
        let f = Format::EdfPlusC;
        let typed = |label: &str, dimension: &str, amplitude: f64| ChannelSpec {
            label: label.into(),
            dimension: dimension.into(),
            amplitude,
            ..ChannelSpec::eeg("", 256, f)
        };
        let channels = vec![
            typed("EEG Fz", "uV", EEG_AMPLITUDE),
            typed("SEEG LA1", "uV", 80.0),
            typed("ECOG G1", "uV", 80.0),
            typed("DBS L1", "uV", 80.0),
            typed("EOG L", "uV", 120.0),
            typed("ECG", "uV", 400.0),
            typed("EMG chin", "uV", 800.0),
            typed("Resp chest", "mV", 800.0),
            typed("SaO2 finger", "%", 1.0),
            // MEG by their units: about the size of MNE's default scales (1 pT, 400 fT/cm).
            typed("MEG 0111", "fT", 800.0),
            typed("MEG 0112", "fT/cm", 300.0),
            ChannelSpec::trigger(256, f),
        ];
        let mut spec = Spec::new(f, channels, 1.0, 60);
        spec.annotations = (0..6)
            .map(|k| Tal {
                onset: f64::from(k) * 10.0 + 1.0,
                duration: Some(2.0),
                texts: vec![format!("artifact {k}")],
            })
            .collect();
        spec.seed = 8;
        spec
    }

    /// Markers every 1.5 s, on whole samples of `rate`: stimuli as Recorder writes them
    /// ("Stimulus/S  1"), responses, a bad interval with a duration, and a comment with a
    /// comma and a µ.
    fn bv_markers(seconds: u32, rate: u32) -> Vec<Tal> {
        (0..seconds * 2 / 3)
            .map(|k| {
                let onset = f64::from(k * 3 * rate / 2 + k % 5) / f64::from(rate);
                let (text, duration) = match k % 6 {
                    0 | 2 => (format!("Stimulus/S{:>3}", 1 + k % 4), None),
                    1 => (format!("Response/R{:>3}", 1 + k % 3), None),
                    3 => ("Bad Interval/Bad Min-Max".into(), Some(0.5)),
                    4 => ("Comment/eyes closed, relaxed (5 \u{b5}V)".into(), Some(0.0)),
                    _ => ("Stimulus/S 15".into(), None),
                };
                Tal {
                    onset,
                    duration,
                    texts: vec![text],
                }
            })
            .collect()
    }

    fn bv_int16_multiplexed() -> Spec {
        let f = Format::BrainVision(Orientation::Multiplexed, Binary::Int16);
        let mut channels: Vec<ChannelSpec> = ["Fp1", "Fz", "Cz", "Pz", "Oz"]
            .iter()
            .map(|l| ChannelSpec::eeg(*l, 500, f))
            .collect();
        channels.push(ChannelSpec::eeg("HEOG", 500, f));
        channels.push(ChannelSpec::eeg("ECG", 500, f));
        channels.push(ChannelSpec {
            label: "SpO2".into(),
            dimension: "%".into(),
            ..ChannelSpec::eeg("", 500, f)
        });
        let mut spec = Spec::new(f, channels, 1.0, 120);
        spec.start = jiff::civil::date(2024, 2, 8).at(11, 58, 41, 181_247_000);
        spec.annotations = bv_markers(120, 500);
        spec.seed = 5;
        spec
    }

    fn bv_int32_vectorized() -> Spec {
        let f = Format::BrainVision(Orientation::Vectorized, Binary::Int32);
        let mut channels: Vec<ChannelSpec> = (1..=7)
            .map(|i| ChannelSpec::eeg(format!("E{i}"), 1000, f))
            .collect();
        channels.push(ChannelSpec {
            // A large DC offset, 1 V at 0.1 µV per step, as DC amplifiers record.
            label: "DC, raw".into(),
            digital_min: 9_995_000,
            digital_max: 10_005_000,
            physical_min: 999_500.0,
            physical_max: 1_000_500.0,
            ..ChannelSpec::eeg("", 1000, f)
        });
        let mut spec = Spec::new(f, channels, 1.0, 60);
        spec.annotations = bv_markers(60, 1000);
        spec.seed = 6;
        spec
    }

    fn bv_float32_multiplexed() -> Spec {
        let f = Format::BrainVision(Orientation::Multiplexed, Binary::Float32);
        let channels = ["C3", "C4", "VEOG", "EMG chin"]
            .iter()
            .enumerate()
            .map(|(k, l)| ChannelSpec {
                // The Greek mu (U+03BC), which LiveAmp's PyCorder writes.
                dimension: if k < 2 { "\u{3bc}V" } else { "\u{b5}V" }.into(),
                ..ChannelSpec::eeg(*l, 256, f)
            })
            .collect();
        let mut spec = Spec::new(f, channels, 1.0, 60);
        spec.annotations = bv_markers(60, 256);
        spec.seed = 7;
        spec
    }

    /// Markers every 1.5 s as BrainVision Recorder writes them in BVRF: stimuli and
    /// responses (a code and a value), annotations, some of them start-end pairs, a comment.
    fn bvrf_markers(seconds: u32, rate: u32) -> Vec<Tal> {
        (0..seconds * 2 / 3)
            .map(|k| {
                let onset = f64::from(k * 3 * rate / 2 + k % 5) / f64::from(rate);
                let (text, duration) = match k % 6 {
                    0 | 2 => (format!("Stimulus/S{}", 1 + k % 4), None),
                    1 => (format!("Response/R{}", 1 + k % 3), None),
                    3 => ("Annotation/Eyes closed".into(), Some(0.5)),
                    4 => ("Annotation/Eye blink, \u{b5}V".into(), Some(0.0)),
                    _ => ("Reconnection".into(), None),
                };
                Tal {
                    onset,
                    duration,
                    texts: vec![text],
                }
            })
            .collect()
    }

    fn bvrf_int16() -> Spec {
        let f = Format::Bvrf(BvrfData::Int16);
        let typed = |label: &str, kind: &str| ChannelSpec {
            kind: kind.into(),
            ..ChannelSpec::eeg(label, 500, f)
        };
        let mut channels: Vec<ChannelSpec> = ["Fp1", "Fz", "Cz", "Pz", "Oz"]
            .iter()
            .map(|l| typed(l, "EEG"))
            .collect();
        channels.push(ChannelSpec {
            amplitude: 120.0,
            ..typed("HEOG", "EOG")
        });
        channels.push(ChannelSpec {
            amplitude: 400.0,
            ..typed("ECG", "ECG")
        });
        // A photo sensor on an AUX input, in mV through a coefficient of 0.001, as
        // Recorder writes them.
        channels.push(ChannelSpec {
            dimension: "mV".into(),
            prefilter: String::new(),
            ..typed("Photo_AUX1", "PHOTO")
        });
        // A temperature with an offset: 30 to 40 °C over int16.
        channels.push(ChannelSpec {
            dimension: "\u{b0}C".into(),
            physical_min: 30.0,
            physical_max: 40.0,
            amplitude: 1.0,
            prefilter: String::new(),
            ..typed("Temp", "TEMP")
        });
        let mut spec = Spec::new(f, channels, 1.0, 120);
        spec.start = jiff::civil::date(2025, 11, 27).at(12, 15, 50, 843_151_000);
        spec.annotations = bvrf_markers(120, 500);
        spec.bvrf.scaling = vec![BvrfScaling::Resolution; 7];
        spec.bvrf.scaling.push(BvrfScaling::Linear(0.001));
        spec.seed = 13;
        spec
    }

    fn bvrf_float64() -> Spec {
        let f = Format::Bvrf(BvrfData::Double);
        let mut channels: Vec<ChannelSpec> = (1..=6)
            .map(|i| ChannelSpec::eeg(format!("E{i}"), 1000, f))
            .collect();
        channels.push(ChannelSpec {
            // A large DC offset, 1 V at 0.1 µV per step, as DC amplifiers record.
            label: "DC".into(),
            digital_min: 9_995_000,
            digital_max: 10_005_000,
            physical_min: 999_500.0,
            physical_max: 1_000_500.0,
            ..ChannelSpec::eeg("", 1000, f)
        });
        // A GSR sensor: shown as its output voltage, as its coefficients aren't linear.
        channels.push(ChannelSpec {
            label: "GSR".into(),
            kind: "GSR".into(),
            dimension: "mV".into(),
            prefilter: String::new(),
            ..ChannelSpec::eeg("", 1000, f)
        });
        let mut spec = Spec::new(f, channels, 1.0, 60);
        spec.annotations = bvrf_markers(60, 1000);
        spec.bvrf.scaling = vec![BvrfScaling::Resolution; 7];
        spec.bvrf.scaling.push(BvrfScaling::Rational);
        spec.seed = 14;
        spec
    }

    fn bvrf_participants() -> Spec {
        let f = Format::Bvrf(BvrfData::Single);
        let ids = ["P1", "P2", "P3"];
        // By electrode, as Recorder writes two amplifiers' channels: each participant's
        // Fp1, then each one's Cz, and so on.
        let mut channels = Vec::new();
        let mut owners = Vec::new();
        for (label, kind) in [("Fp1", "EEG"), ("Cz", "EEG"), ("Pz", "EEG"), ("EOG", "EOG")] {
            for p in 0..ids.len() {
                channels.push(ChannelSpec {
                    kind: kind.into(),
                    amplitude: if kind == "EOG" { 120.0 } else { EEG_AMPLITUDE },
                    ..ChannelSpec::eeg(label, 500, f)
                });
                owners.push(Some(p));
            }
        }
        // A photo sensor and a GSR sensor of all participants.
        channels.push(ChannelSpec {
            kind: "PHOTO".into(),
            dimension: "mV".into(),
            prefilter: String::new(),
            ..ChannelSpec::eeg("Photo_AUX1", 500, f)
        });
        channels.push(ChannelSpec {
            kind: "GSR".into(),
            dimension: "mV".into(),
            prefilter: String::new(),
            ..ChannelSpec::eeg("GSR", 500, f)
        });
        let mut spec = Spec::new(f, channels, 1.0, 60);
        let mut annotations = bvrf_markers(60, 500);
        let mut targets = vec![super::BvrfTarget::default(); annotations.len()];
        for k in 0..36u32 {
            let p = (k % 3) as usize;
            let onset = f64::from(k * 800 + 250) / 500.0;
            let (text, duration, channel) = match k % 4 {
                0 | 2 => (format!("Response/R{}", 1 + k % 2), None, None),
                1 => ("Annotation/Eye blink".to_owned(), None, Some("Fp1")),
                _ => ("Annotation/Movement".to_owned(), Some(0.75), None),
            };
            annotations.push(Tal {
                onset,
                duration,
                texts: vec![text],
            });
            targets.push(super::BvrfTarget {
                participant: Some(p),
                channel: channel.map(str::to_owned),
            });
        }
        // Each participant's Cz at once.
        annotations.push(Tal {
            onset: 30.0,
            duration: None,
            texts: vec!["Annotation/Artifact".into()],
        });
        targets.push(super::BvrfTarget {
            participant: None,
            channel: Some("Cz".into()),
        });
        spec.annotations = annotations;
        spec.bvrf.participants = ids.map(str::to_owned).to_vec();
        spec.bvrf.channel_participants = owners;
        spec.bvrf.targets = targets;
        spec.bvrf.scaling = vec![BvrfScaling::Resolution; 12];
        spec.bvrf.scaling.push(BvrfScaling::Linear(0.001));
        spec.bvrf.scaling.push(BvrfScaling::Rational);
        spec.seed = 15;
        spec
    }

    /// Events every 1.25 s as EEGLAB writes them: stimuli and responses by name, numeric
    /// codes, some with a duration, some between samples.
    fn eeglab_events(seconds: u32, rate: u32) -> Vec<Tal> {
        (0..seconds * 4 / 5)
            .map(|k| {
                let sample = f64::from(k * 5 * rate / 4 + k % 7);
                let (text, duration) = match k % 5 {
                    0 => ("square".to_owned(), None),
                    1 => ("rt".to_owned(), Some(0.25)),
                    2 => (format!("{}", 10 + k % 3), None),
                    3 => ("S  1".to_owned(), Some(0.0)),
                    _ => ("blink µ".to_owned(), Some(0.5)),
                };
                // Every seventh between two samples, as resampled datasets have them.
                let sample = if k % 7 == 3 { sample + 0.5 } else { sample };
                Tal {
                    onset: sample / f64::from(rate),
                    duration,
                    texts: vec![text],
                }
            })
            .collect()
    }

    fn eeglab_fdt() -> Spec {
        let f = Format::Eeglab(Storage::Fdt, Layout::Struct);
        let typed = |label: &str, kind: &str, amplitude: f64| ChannelSpec {
            label: label.into(),
            kind: kind.into(),
            amplitude,
            ..ChannelSpec::eeg("", 256, f)
        };
        let channels = vec![
            typed("Fz", "EEG", EEG_AMPLITUDE),
            typed("Cz", "EEG", EEG_AMPLITUDE),
            typed("Pz", "eeg", EEG_AMPLITUDE),
            typed("Oz", "", EEG_AMPLITUDE),
            // No type: EOG from the name, as kymo reads BrainVision.
            typed("HEOG", "", 120.0),
            typed("ECG", "ECG", 400.0),
            typed("Resp", "RESP", 1e5),
            // A type MNE doesn't have: EEG, from the name.
            typed("Light", "LIGHT", EEG_AMPLITUDE),
        ];
        let mut spec = Spec::new(f, channels, 1.0, 60);
        spec.annotations = eeglab_events(60, 256);
        // A boundary, where EEGLAB removed 2 s of data (its duration).
        spec.annotations.insert(
            0,
            Tal {
                onset: 0.0,
                duration: Some(2.0),
                texts: vec!["boundary".into()],
            },
        );
        spec.seed = 9;
        spec
    }

    fn eeglab_set_single() -> Spec {
        let f = Format::Eeglab(Storage::Single, Layout::Flat);
        let channels = ["C3", "C4", "VEOG", "EMG chin"]
            .iter()
            .map(|l| ChannelSpec::eeg(*l, 500, f))
            .collect();
        let mut spec = Spec::new(f, channels, 1.0, 60);
        spec.annotations = eeglab_events(60, 500);
        spec.seed = 10;
        spec
    }

    fn eeglab_set_double() -> Spec {
        let f = Format::Eeglab(Storage::Double, Layout::Struct);
        let mut channels: Vec<ChannelSpec> = (1..=6)
            .map(|i| ChannelSpec::eeg(format!("E{i}"), 1000, f))
            .collect();
        channels.push(ChannelSpec {
            // A large DC offset, 1 V at 0.1 µV per step, as DC amplifiers record; float64
            // keeps it, and kymo keeps it through the channel's reference value.
            label: "DC".into(),
            digital_min: 9_995_000,
            digital_max: 10_005_000,
            physical_min: 999_500.0,
            physical_max: 1_000_500.0,
            ..ChannelSpec::eeg("", 1000, f)
        });
        channels.push(ChannelSpec {
            label: "Trigger".into(),
            ..ChannelSpec::trigger(1000, f)
        });
        let mut spec = Spec::new(f, channels, 1.0, 60);
        spec.annotations = eeglab_events(60, 1000);
        spec.seed = 11;
        spec
    }

    /// Events every 1.5 s on whole samples of `rate`, with GDF's codes for trial starts
    /// (768), cues (769, 770), a rejected trial (1023) and a new segment (32766), some
    /// with a duration (if `durations`).
    fn gdf_events(seconds: u32, rate: u32, durations: bool) -> Vec<Tal> {
        (0..seconds * 2 / 3)
            .map(|k| {
                let onset = f64::from(k * 3 * rate / 2 + k % 5) / f64::from(rate);
                let (code, duration) = match k % 5 {
                    0 => (768, 0.5),
                    1 => (769, 1.25),
                    2 => (770, 0.0),
                    3 => (1023, 3.0),
                    _ => (32766, 0.0),
                };
                Tal {
                    onset,
                    duration: durations.then_some(duration),
                    texts: vec![code.to_string()],
                }
            })
            .collect()
    }

    fn gdf1_float64() -> Spec {
        let f = Format::Gdf(GdfVersion::V1);
        let typed = |label: &str, spr: u32| ChannelSpec::gdf(label, spr, Stored::F64);
        let channels = vec![
            typed("EEG:Fz", 64),
            typed("EEG:Cz", 64),
            typed("EEG:Pz", 64),
            typed("EOG:L", 64),
            ChannelSpec {
                // A large DC offset, 1 V at 0.1 µV per step; float64 keeps it, and kymo
                // keeps it through the channel's reference value.
                label: "DC".into(),
                digital_min: 9_995_000,
                digital_max: 10_005_000,
                physical_min: 999_500.0,
                physical_max: 1_000_500.0,
                ..typed("", 64)
            },
            ChannelSpec {
                dimension: "mV".into(),
                amplitude: 0.4,
                physical_min: -100.0,
                physical_max: 100.0,
                ..typed("ECG", 32)
            },
            ChannelSpec {
                label: "STATUS".into(),
                stored: Some(Stored::F64),
                ..ChannelSpec::trigger(64, f)
            },
        ];
        let mut spec = Spec::new(f, channels, 0.25, 240);
        spec.annotations = gdf_events(60, 256, false);
        spec.seed = 12;
        spec
    }

    fn gdf2_int32() -> Spec {
        let f = Format::Gdf(GdfVersion::V2);
        let typed = |label: &str| ChannelSpec::gdf(label, 500, Stored::I32);
        let mut channels: Vec<ChannelSpec> = ["Fp1", "Fz", "Cz", "Pz", "Oz"]
            .iter()
            .map(|l| typed(l))
            .collect();
        channels.push(ChannelSpec {
            dimension: "mV".into(),
            amplitude: 0.4,
            physical_min: -100.0,
            physical_max: 100.0,
            ..typed("ECG")
        });
        channels.push(ChannelSpec {
            dimension: "%".into(),
            prefilter: "LP:1Hz".into(),
            amplitude: 5.0,
            physical_min: -200.0,
            physical_max: 200.0,
            ..typed("SpO2")
        });
        channels.push(ChannelSpec {
            stored: Some(Stored::I32),
            ..ChannelSpec::trigger(500, f)
        });
        let mut spec = Spec::new(f, channels, 1.0, 60);
        spec.start = jiff::civil::date(2025, 6, 30).at(23, 59, 58, 250_000_000);
        spec.annotations = gdf_events(60, 500, true);
        spec.seed = 13;
        spec
    }

    fn gdf2_mixed() -> Spec {
        let f = Format::Gdf(GdfVersion::V2Tagged);
        let mut channels: Vec<ChannelSpec> = Stored::ALL
            .iter()
            .map(|&stored| {
                // 8-bit types at half the rate.
                let spr = if stored.size() == 1 { 128 } else { 256 };
                ChannelSpec::gdf(format!("{stored:?}"), spr, stored)
            })
            .collect();
        channels.push(ChannelSpec {
            stored: Some(Stored::U16),
            ..ChannelSpec::trigger(256, f)
        });
        let mut spec = Spec::new(f, channels, 1.0, 60);
        spec.annotations = gdf_events(60, 256, true);
        spec.seed = 14;
        spec
    }

    /// STI 014, the trigger channel of Neuromag systems.
    fn sti014(spr: u32, f: Format) -> ChannelSpec {
        ChannelSpec {
            label: "STI 014".into(),
            dimension: String::new(),
            ..ChannelSpec::trigger(spr, f)
        }
    }

    fn fif_float32_meg() -> Spec {
        let f = Format::Fiff(FiffType::Float);
        let typed = |label: &str, kind: &str| ChannelSpec::fiff(label, kind, 600, f);
        let channels = vec![
            typed("MEG 0111", "mag"),
            typed("MEG 0112", "grad"),
            typed("MEG 0113", "grad"),
            typed("MEG 0121", "mag"),
            typed("EEG 001", "eeg"),
            typed("EEG 002", "eeg"),
            typed("EEG 003", "eeg"),
            typed("EEG 004", "eeg"),
            typed("EOG 061", "eog"),
            typed("ECG 063", "ecg"),
            ChannelSpec {
                // A trigger line in volts, as older Neuromag systems store them: a stim
                // channel whose calibration isn't 1, which kymo reads as misc.
                physical_min: -16_777_216.0 / 3276.8,
                physical_max: 16_777_216.0 / 3276.8,
                amplitude: 2.5,
                ..typed("STI 001", "stim")
            },
            sti014(600, f),
        ];
        let mut spec = Spec::new(f, channels, 1.0, 60);
        spec.first_onset = 43.0;
        spec.start = jiff::civil::date(2002, 12, 3).at(19, 1, 10, 500_000_000);
        spec.seed = 15;
        spec.annotations = fif_annotations(43.0, 60);
        spec.fiff.annotation_channels = (0..spec.annotations.len())
            .map(|k| match k % 4 {
                0 => vec!["EEG 001".to_owned(), "EOG 061".to_owned()],
                2 => vec!["MEG 0111".to_owned()],
                _ => Vec::new(),
            })
            .collect();
        spec.fiff.bads = vec!["EEG 003".into(), "MEG 0113".into()];
        spec
    }

    /// Annotations every 5 s from `from` (seconds after the start) for `seconds`, as MNE
    /// writes them: bad segments, a description with a colon, points in time.
    fn fif_annotations(from: f64, seconds: u32) -> Vec<Tal> {
        (0..seconds / 5)
            .map(|k| {
                let onset = from + f64::from(k) * 5.0 + 0.25;
                let (text, duration) = match k % 4 {
                    0 => ("BAD_blink", Some(0.5)),
                    1 => ("stim: 1", Some(0.0)),
                    2 => ("BAD_flux", Some(1.25)),
                    _ => ("eyes closed", None),
                };
                Tal {
                    onset,
                    duration,
                    texts: vec![text.to_owned()],
                }
            })
            .collect()
    }

    fn fif_split() -> Spec {
        let f = Format::Fiff(FiffType::Short);
        let mut channels: Vec<ChannelSpec> = ["Fz", "Cz", "Pz", "Oz", "EOG"]
            .iter()
            .map(|l| ChannelSpec::eeg(*l, 1000, f))
            .collect();
        channels.push(ChannelSpec {
            // A trigger line STI 014 combines: a stim channel without events of its own.
            label: "STI 001".into(),
            ..sti014(1000, f)
        });
        channels.push(sti014(1000, f));
        let mut spec = Spec::new(f, channels, 1.0, 90);
        spec.first_onset = 12.0;
        spec.gaps = vec![GapSpec {
            before_record: 40,
            seconds: 2.0,
        }];
        spec.fiff.split = Some(FiffSplit {
            records: 30,
            naming: SplitNaming::Neuromag,
        });
        spec.annotations = fif_annotations(12.0, 90);
        spec.fiff.bads = vec!["Oz".into()];
        spec.seed = 20;
        spec
    }

    fn fif_int16_skips() -> Spec {
        let f = Format::Fiff(FiffType::Pack16);
        let mut channels: Vec<ChannelSpec> = ["Fp1", "Fz", "Cz", "Pz", "O1", "O2"]
            .iter()
            .map(|l| ChannelSpec::eeg(*l, 1000, f))
            .collect();
        channels.push(ChannelSpec::fiff("EOG", "eog", 1000, f));
        channels.push(sti014(1000, f));
        let mut spec = Spec::new(f, channels, 1.0, 120);
        spec.first_onset = 5.0;
        spec.fiff.initial_skip = 2;
        spec.fiff.utc_offset = Some("+01:00".into());
        spec.gaps = vec![
            GapSpec {
                before_record: 30,
                seconds: 3.0,
            },
            GapSpec {
                before_record: 80,
                seconds: 1.0,
            },
        ];
        spec.seed = 16;
        spec
    }

    fn fif_int32_varying() -> Spec {
        let f = Format::Fiff(FiffType::Int);
        let typed = |label: &str, kind: &str| ChannelSpec::fiff(label, kind, 512, f);
        let channels = vec![
            typed("EEG Fp1-Reference", "eeg"),
            typed("EEG Fp2-Reference", "eeg"),
            typed("SEEG LA1", "seeg"),
            typed("ECOG grid 1 contact 1", "ecog"),
            typed("EMG chin left-right", "emg"),
            typed("ECG lead II", "ecg"),
        ];
        let mut spec = Spec::new(f, channels, 1.0, 60);
        spec.fiff.varying_buffers = true;
        spec.seed = 17;
        spec
    }

    fn fif_double_dc() -> Spec {
        let f = Format::Fiff(FiffType::Double);
        let mut channels: Vec<ChannelSpec> = ["C3", "C4"]
            .iter()
            .map(|l| ChannelSpec::eeg(*l, 250, f))
            .collect();
        channels.push(ChannelSpec {
            // A large DC offset, 1 V at 0.1 µV per step; float64 keeps it, and kymo keeps
            // it through the channel's reference value.
            label: "DC".into(),
            digital_min: 9_995_000,
            digital_max: 10_005_000,
            physical_min: 0.9995,
            physical_max: 1.0005,
            ..ChannelSpec::eeg("", 250, f)
        });
        channels.push(sti014(250, f));
        let mut spec = Spec::new(f, channels, 1.0, 60);
        spec.seed = 18;
        spec
    }

    fn fif_mixed() -> Spec {
        let f = Format::Fiff(FiffType::Mixed);
        let mut channels: Vec<ChannelSpec> = ["Fz", "Cz", "Pz"]
            .iter()
            .map(|l| ChannelSpec::eeg(*l, 200, f))
            .collect();
        channels.push(ChannelSpec::fiff("MEG 0111", "mag", 200, f));
        channels.push(sti014(200, f));
        let mut spec = Spec::new(f, channels, 0.5, 60);
        spec.seed = 19;
        spec
    }

    /// Annotations every 1.7 s from 0.3 s over `seconds`, texts from `texts` in turn.
    fn xdf_markers(seconds: f64, texts: &[&str]) -> Vec<Tal> {
        let mut tals = Vec::new();
        let mut k = 0;
        while 0.3 + k as f64 * 1.7 < seconds {
            tals.push(Tal {
                onset: 0.3 + k as f64 * 1.7,
                duration: None,
                texts: vec![texts[k % texts.len()].to_owned()],
            });
            k += 1;
        }
        tals
    }

    fn xdf_float32() -> Spec {
        let f = Format::Xdf;
        let channels = ["Fp1", "Fp2", "F3", "F4", "C3", "C4", "O1", "O2"]
            .iter()
            .map(|l| ChannelSpec::eeg(*l, 500, f))
            .collect();
        let mut spec = Spec::new(f, channels, 1.0, 60);
        let mut eeg = XdfStream::new(1, "BrainAmp", "EEG", (0..8).collect());
        eeg.chunk = 250;
        spec.annotations = xdf_markers(60.0, &["S  1", "S  2", "Stimulus & response <ok>"]);
        spec.xdf.streams = vec![eeg];
        spec.xdf.markers = vec![XdfMarkers {
            id: 2,
            name: "BrainAmp Markers".into(),
            kind: "Markers".into(),
            values: MarkerValues::Strings,
            rate: 0.0,
            annotations: 0..spec.annotations.len(),
        }];
        spec.xdf.datetime = Some("2024-01-02T03:04:05+0100".into());
        spec.xdf.clock = 52_311.25;
        spec.xdf.clock_offsets = true;
        spec.xdf.boundaries = true;
        spec.seed = 20;
        spec
    }

    fn xdf_mixed() -> Spec {
        let f = Format::Xdf;
        let ch = |label: &str, kind: &str, unit: &str, spr: u32, format: XdfFormat| {
            ChannelSpec::xdf(label, kind, unit, spr, format)
        };
        let mut channels = vec![
            ch("Fz", "EEG", "microvolts", 256, XdfFormat::I16),
            ch("Cz", "EEG", "microvolts", 256, XdfFormat::I16),
            ch("Pz", "EEG", "\u{b5}V", 256, XdfFormat::I16),
            ch("X", "accelerometer", "g", 52, XdfFormat::F32),
            ch("Y", "accelerometer", "g", 52, XdfFormat::F32),
            ch("X", "gyroscope", "dps", 52, XdfFormat::F64),
            ch("Y", "gyroscope", "dps", 52, XdfFormat::F64),
            ch("EOG", "EOG", "mV", 128, XdfFormat::I8),
            ch("EMG", "EMG", "uV", 128, XdfFormat::I8),
            ch("Resp", "RESP", "ohm", 100, XdfFormat::I32),
            ch("A", "", "", 100, XdfFormat::I32),
        ];
        channels.push(ChannelSpec {
            // A large DC offset in int64, which kymo reads relative to its first sample.
            label: "DC".into(),
            digital_min: 999_000_000,
            digital_max: 1_001_000_000,
            physical_min: 999_000_000.0,
            physical_max: 1_001_000_000.0,
            amplitude: 400.0,
            ..ch("", "MISC", "nV", 200, XdfFormat::I64)
        });
        channels.push(ChannelSpec {
            amplitude: 40.0,
            ..ch(
                "Temp",
                "temperature",
                "degrees celsius",
                200,
                XdfFormat::I64,
            )
        });
        let mut spec = Spec::new(f, channels, 1.0, 90);
        let stream = |id, name: &str, kind: &str, format, channels: Vec<usize>| XdfStream {
            format,
            ..XdfStream::new(id, name, kind, channels)
        };
        let amp = XdfStream {
            stamps: XdfStamps::Every(7),
            chunk: 40,
            varying_chunks: true,
            ..stream(3, "Amp", "EEG", XdfFormat::I16, vec![0, 1, 2])
        };
        let acc = XdfStream {
            start: 0.125,
            ..stream(1, "Muse", "ACC", XdfFormat::F32, vec![3, 4])
        };
        let gyro = XdfStream {
            start: 0.375,
            stamps: XdfStamps::All,
            chunk: 13,
            ..stream(2, "Muse", "GYRO", XdfFormat::F64, vec![5, 6])
        };
        let exg = XdfStream {
            start: 1.5,
            stamps: XdfStamps::All,
            ..stream(4, "ExG", "ExG", XdfFormat::I8, vec![7, 8])
        };
        let aux = XdfStream {
            described: false,
            start: 0.01,
            chunk: 100,
            ..stream(6, "Aux", "AUX", XdfFormat::I32, vec![9, 10])
        };
        let dc = XdfStream {
            start: 2.0,
            stamps: XdfStamps::Every(3),
            ..stream(5, "DC", "MISC", XdfFormat::I64, vec![11, 12])
        };
        spec.xdf.streams = vec![amp, acc, gyro, exg, aux, dc];
        spec.annotations = xdf_markers(90.0, &["start", "Stimulus/S  1", "end"]);
        spec.xdf.markers = vec![XdfMarkers {
            id: 9,
            name: "Markers".into(),
            kind: "Markers".into(),
            values: MarkerValues::Strings,
            rate: 0.0,
            annotations: 0..spec.annotations.len(),
        }];
        spec.xdf.clock = 1_000_000.0;
        spec.seed = 21;
        spec
    }

    fn xdf_events() -> Spec {
        let f = Format::Xdf;
        let mut channels: Vec<ChannelSpec> = ["C3", "Cz", "C4"]
            .iter()
            .map(|l| ChannelSpec::xdf(*l, "EEG", "microvolts", 250, XdfFormat::F64))
            .collect();
        channels.push(ChannelSpec {
            label: "STETriggerIn".into(),
            kind: "Trigger".into(),
            dimension: "integer".into(),
            ..ChannelSpec::trigger(250, f)
        });
        channels.push(ChannelSpec {
            label: "Code".into(),
            kind: String::new(),
            ..ChannelSpec::trigger(100, f)
        });
        let mut spec = Spec::new(f, channels, 1.0, 60);
        let eeg = XdfStream {
            format: XdfFormat::F64,
            ..XdfStream::new(1, "LiveAmp", "EEG", vec![0, 1, 2, 3])
        };
        let codes = XdfStream {
            format: XdfFormat::I32,
            start: 0.04,
            ..XdfStream::new(2, "Codes", "Markers", vec![4])
        };
        spec.xdf.streams = vec![eeg, codes];
        let mut annotations = xdf_markers(60.0, &["3", "17", "-4"]);
        let numeric = annotations.len();
        annotations.extend(xdf_markers(60.0, &["1.5", "1e-04", "-2.0", "1e-05"]));
        let floats = annotations.len();
        annotations.extend(xdf_markers(60.0, &["10.0,20.5", "nan,1.0"]));
        let pairs = annotations.len();
        annotations.extend(xdf_markers(60.0, &["onset", "offset"]));
        spec.annotations = annotations;
        let markers = |id, name: &str, kind: &str, values, rate, annotations| XdfMarkers {
            id,
            name: name.into(),
            kind: kind.into(),
            values,
            rate,
            annotations,
        };
        spec.xdf.markers = vec![
            markers(
                3,
                "Ints",
                "StickMvmt",
                MarkerValues::Numeric(XdfFormat::I16, 1),
                0.0,
                0..numeric,
            ),
            markers(
                4,
                "Floats",
                "Events",
                MarkerValues::Numeric(XdfFormat::F32, 1),
                0.0,
                numeric..floats,
            ),
            markers(
                5,
                "Mouse",
                "Position",
                MarkerValues::Numeric(XdfFormat::F32, 2),
                0.0,
                floats..pairs,
            ),
            markers(
                6,
                "Stimuli",
                "Markers",
                MarkerValues::Strings,
                10.0,
                pairs..spec.annotations.len(),
            ),
        ];
        spec.xdf.clock = 77.5;
        spec.seed = 22;
        spec
    }

    fn xdf_clocks() -> Spec {
        let f = Format::Xdf;
        let mut channels: Vec<ChannelSpec> = ["Fz", "Cz", "Pz", "Oz", "C3", "C4", "P3", "P4"]
            .iter()
            .map(|l| ChannelSpec::eeg(*l, 500, f))
            .collect();
        for axis in ["X", "Y", "Z"] {
            channels.push(ChannelSpec {
                amplitude: 1000.0,
                ..ChannelSpec::xdf(axis, "accelerometer", "NA", 52, XdfFormat::I16)
            });
        }
        let mut spec = Spec::new(f, channels, 1.0, 120);
        let eeg = XdfStream {
            chunk: 50,
            rate_error: 2e-5,
            jitter: 5e-4,
            breaks: vec![XdfBreak {
                before: 20_000,
                seconds: 3.0,
                rate_error: 2e-5,
            }],
            ..XdfStream::new(1, "Amp", "EEG", (0..8).collect())
        };
        let acc = XdfStream {
            format: XdfFormat::I16,
            start: 0.3,
            stamps: XdfStamps::All,
            chunk: 13,
            rate_error: -3e-3,
            jitter: 2e-3,
            breaks: vec![XdfBreak {
                before: 1500,
                // A break (more than 1 s and 2 samples) where pyxdf's default (500
                // samples, 9.6 s at 52 Hz) has none.
                seconds: 4.0,
                rate_error: 1e-4,
            }],
            ..XdfStream::new(2, "Wristband", "ACC", vec![8, 9, 10])
        };
        spec.xdf.streams = vec![eeg, acc];
        spec.annotations = xdf_markers(120.0, &["S  1", "S  2"]);
        spec.xdf.markers = vec![XdfMarkers {
            id: 3,
            name: "Markers".into(),
            kind: "Markers".into(),
            values: MarkerValues::Strings,
            rate: 0.0,
            annotations: 0..spec.annotations.len(),
        }];
        spec.xdf.clock = 52_000.0;
        spec.xdf.clock_offsets = true;
        spec.xdf.clock_offset = -0.75;
        spec.xdf.clock_drift = 3e-6;
        spec.xdf.clock_reset = Some((70.0, 4000.0));
        spec.seed = 23;
        spec
    }

    /// String markers every few seconds over `duration`, as `xdf_markers` writes them, in a
    /// stream of id `id`.
    fn string_markers(spec: &mut Spec, id: u32, duration: f64) {
        spec.annotations = xdf_markers(duration, &["S  1", "S  2"]);
        spec.xdf.markers = vec![XdfMarkers {
            id,
            name: "Markers".into(),
            kind: "Markers".into(),
            values: MarkerValues::Strings,
            rate: 0.0,
            annotations: 0..spec.annotations.len(),
        }];
    }

    /// Gaze position (pixels, unscaled) and pupil size channels of an eye tracker at about
    /// `rate` Hz.
    fn gaze_channels(rate: u32) -> Vec<ChannelSpec> {
        [("GazeX", "pixels"), ("GazeY", "pixels"), ("Pupil", "NA")]
            .iter()
            .map(|&(label, unit)| ChannelSpec {
                amplitude: 400.0,
                ..ChannelSpec::xdf(label, "gaze", unit, rate, XdfFormat::F32)
            })
            .collect()
    }

    fn xdf_irregular() -> Spec {
        let f = Format::Xdf;
        let mut channels: Vec<ChannelSpec> = ["Fz", "Cz", "Pz", "Oz"]
            .iter()
            .map(|l| ChannelSpec::eeg(*l, 250, f))
            .collect();
        channels.extend(gaze_channels(300));
        for axis in ["Fx", "Fy"] {
            channels.push(ChannelSpec {
                amplitude: 50.0,
                ..ChannelSpec::xdf(axis, "force", "NA", 100, XdfFormat::F32)
            });
        }
        let mut spec = Spec::new(f, channels, 1.0, 120);
        let eeg = XdfStream {
            chunk: 25,
            ..XdfStream::new(1, "Amp", "EEG", vec![0, 1, 2, 3])
        };
        // Pauses longer than 1 s and 500 median steps (1.7 s).
        let gaze = XdfStream {
            stamps: XdfStamps::All,
            chunk: 20,
            start: 0.2,
            jitter: 1.5e-3,
            irregular: true,
            breaks: vec![
                XdfBreak {
                    before: 9000,
                    seconds: 8.0,
                    rate_error: 0.0,
                },
                XdfBreak {
                    before: 24_000,
                    seconds: 3.0,
                    rate_error: 1e-3,
                },
            ],
            ..XdfStream::new(2, "Tracker", "Gaze", vec![4, 5, 6])
        };
        let force = XdfStream {
            stamps: XdfStamps::All,
            chunk: 10,
            start: 0.5,
            jitter: 1e-3,
            can_drop: true,
            breaks: vec![XdfBreak {
                before: 5000,
                seconds: 2.5,
                rate_error: 0.0,
            }],
            ..XdfStream::new(3, "Plate", "Force", vec![7, 8])
        };
        spec.xdf.streams = vec![eeg, gaze, force];
        string_markers(&mut spec, 4, 120.0);
        spec.xdf.clock = 9_000.0;
        spec.xdf.clock_offsets = true;
        spec.xdf.clock_offset = 0.4;
        spec.xdf.clock_drift = -2e-6;
        spec.seed = 24;
        spec
    }

    fn xdf_eyetracker() -> Spec {
        let mut spec = Spec::new(Format::Xdf, gaze_channels(1000), 1.0, 3600);
        let breaks = [(600_000, 12.0), (1_800_000, 30.0), (2_900_000, 5.0)]
            .map(|(before, seconds)| XdfBreak {
                before,
                seconds,
                rate_error: 0.0,
            })
            .to_vec();
        spec.xdf.streams = vec![XdfStream {
            stamps: XdfStamps::All,
            chunk: 32,
            jitter: 4e-4,
            irregular: true,
            breaks,
            ..XdfStream::new(1, "Tracker", "Gaze", vec![0, 1, 2])
        }];
        string_markers(&mut spec, 2, 3600.0);
        spec.seed = 25;
        spec
    }
}
