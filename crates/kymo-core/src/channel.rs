//! Per-channel metadata as stored in the file.

use std::sync::Arc;

use crate::GridId;

/// Index of a channel in [`Recording::channels`](crate::Recording::channels).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChannelId(pub u32);

/// A channel's type: a name, as the file gives it or as typed by hand. Names that differ only
/// in case are one type ([`key`](Self::key)); a name kymo knows ([`KNOWN_KINDS`], also by its
/// other spellings) is shown as kymo spells it, any other as first given. The one type with
/// a behavior of its own is [`STIM`](Self::STIM): the readers make a channel stim when its
/// values are codes.
#[derive(Clone)]
pub struct ChannelKind(Repr);

#[derive(Clone)]
enum Repr {
    Known(&'static KnownKind),
    Custom { name: Arc<str>, key: Arc<str> },
}

/// A type kymo knows: how it is shown, its other spellings and its name in MNE.
#[derive(Debug, PartialEq, Eq)]
pub struct KnownKind {
    /// As shown, e.g. `"MEG Mag"`.
    pub name: &'static str,
    /// The lowercase name settings are kept under: MNE's name of the type where MNE has it
    /// (`"ref_meg"`), the shown name in lowercase otherwise (`"heog"`).
    pub key: &'static str,
    /// Other spellings of the type, in lowercase: BIDS' (`"megmag"`), EDF+'s (`"temp"`) and
    /// others (`"ekg"`).
    pub aliases: &'static [&'static str],
    /// MNE's channel type for it: its own, or the nearest (`"eog"` for HEOG, `"bio"` for
    /// SaO2, `"misc"` where MNE has none).
    pub mne: &'static str,
}

const fn known(
    name: &'static str,
    key: &'static str,
    aliases: &'static [&'static str],
    mne: &'static str,
) -> KnownKind {
    KnownKind {
        name,
        key,
        aliases,
        mne,
    }
}

const EEG: KnownKind = known("EEG", "eeg", &[], "eeg");
const MAG: KnownKind = known("MEG Mag", "mag", &["megmag"], "mag");
const GRAD: KnownKind = known("MEG Grad", "grad", &["meggradplanar"], "grad");
const REF_MEG: KnownKind = known(
    "MEG Ref",
    "ref_meg",
    &["megrefmag", "megrefgradaxial", "megrefgradplanar"],
    "ref_meg",
);
const EOG: KnownKind = known("EOG", "eog", &[], "eog");
const HEOG: KnownKind = known("HEOG", "heog", &[], "eog");
const VEOG: KnownKind = known("VEOG", "veog", &[], "eog");
const ECG: KnownKind = known("ECG", "ecg", &["ekg"], "ecg");
const EMG: KnownKind = known("EMG", "emg", &[], "emg");
const SEEG: KnownKind = known("sEEG", "seeg", &[], "seeg");
const ECOG: KnownKind = known("ECoG", "ecog", &[], "ecog");
const DBS: KnownKind = known("DBS", "dbs", &[], "dbs");
const RESP: KnownKind = known("Resp", "resp", &[], "resp");
const BIO: KnownKind = known("Bio", "bio", &[], "bio");
const MISC: KnownKind = known("Misc", "misc", &[], "misc");
const STIM: KnownKind = known("Stim", "stim", &[], "stim");

/// The types kymo knows, in the order the sidebar lists them where a stream has several:
/// EEG, MEG, EOG, ECG, the other body and brain signals, then MNE's other types and EDF+'s
/// type prefixes; misc and stim come last, after any type kymo doesn't know.
pub static KNOWN_KINDS: [&KnownKind; 46] = [
    &EEG,
    &MAG,
    &GRAD,
    &REF_MEG,
    &EOG,
    &HEOG,
    &VEOG,
    &ECG,
    &EMG,
    &SEEG,
    &ECOG,
    &DBS,
    &RESP,
    &BIO,
    &known("SaO2", "sao2", &[], "bio"),
    &known("PPG", "ppg", &[], "bio"),
    &known("GSR", "gsr", &[], "gsr"),
    &known("Temperature", "temperature", &["temp"], "temperature"),
    &known("Eye Gaze", "eyegaze", &[], "eyegaze"),
    &known("Pupil", "pupil", &[], "pupil"),
    &known("HbO", "hbo", &[], "hbo"),
    &known("HbR", "hbr", &[], "hbr"),
    &known(
        "fNIRS CW Amplitude",
        "fnirs_cw_amplitude",
        &["nirscwamplitude"],
        "fnirs_cw_amplitude",
    ),
    &known(
        "fNIRS FD AC Amplitude",
        "fnirs_fd_ac_amplitude",
        &[],
        "fnirs_fd_ac_amplitude",
    ),
    &known("fNIRS FD Phase", "fnirs_fd_phase", &[], "fnirs_fd_phase"),
    &known("fNIRS OD", "fnirs_od", &[], "fnirs_od"),
    &known(
        "fNIRS TD Gated Amplitude",
        "fnirs_td_gated_amplitude",
        &[],
        "fnirs_td_gated_amplitude",
    ),
    &known(
        "fNIRS TD Moments Intensity",
        "fnirs_td_moments_intensity",
        &[],
        "fnirs_td_moments_intensity",
    ),
    &known(
        "fNIRS TD Moments Mean",
        "fnirs_td_moments_mean",
        &[],
        "fnirs_td_moments_mean",
    ),
    &known(
        "fNIRS TD Moments Variance",
        "fnirs_td_moments_variance",
        &[],
        "fnirs_td_moments_variance",
    ),
    &known("CSD", "csd", &[], "csd"),
    &known("cHPI", "chpi", &[], "chpi"),
    &known("EXCI", "exci", &[], "exci"),
    &known("IAS", "ias", &[], "ias"),
    &known("SYST", "syst", &[], "syst"),
    &known("Dipole", "dipole", &[], "dipole"),
    &known("GOF", "gof", &[], "gof"),
    &known("Whitened", "whitened", &[], "whitened"),
    &known("ERG", "erg", &[], "misc"),
    &known("MCG", "mcg", &[], "misc"),
    &known("EP", "ep", &[], "misc"),
    &known("Light", "light", &[], "misc"),
    &known("Sound", "sound", &[], "misc"),
    &known("Event", "event", &[], "misc"),
    &MISC,
    &STIM,
];

impl ChannelKind {
    pub const EEG: ChannelKind = ChannelKind(Repr::Known(&EEG));
    /// MEG magnetometer.
    pub const MAG: ChannelKind = ChannelKind(Repr::Known(&MAG));
    /// MEG planar gradiometer.
    pub const GRAD: ChannelKind = ChannelKind(Repr::Known(&GRAD));
    /// MEG reference sensor (magnetometer or gradiometer).
    pub const REF_MEG: ChannelKind = ChannelKind(Repr::Known(&REF_MEG));
    pub const EOG: ChannelKind = ChannelKind(Repr::Known(&EOG));
    pub const HEOG: ChannelKind = ChannelKind(Repr::Known(&HEOG));
    pub const VEOG: ChannelKind = ChannelKind(Repr::Known(&VEOG));
    pub const ECG: ChannelKind = ChannelKind(Repr::Known(&ECG));
    pub const EMG: ChannelKind = ChannelKind(Repr::Known(&EMG));
    /// Stereo-EEG (depth electrodes).
    pub const SEEG: ChannelKind = ChannelKind(Repr::Known(&SEEG));
    /// Electrocorticography.
    pub const ECOG: ChannelKind = ChannelKind(Repr::Known(&ECOG));
    /// Deep brain stimulation electrodes.
    pub const DBS: ChannelKind = ChannelKind(Repr::Known(&DBS));
    /// Respiration.
    pub const RESP: ChannelKind = ChannelKind(Repr::Known(&RESP));
    /// Other biological signals (MNE's "bio": oximetry, plethysmography, ...).
    pub const BIO: ChannelKind = ChannelKind(Repr::Known(&BIO));
    /// Channels of no particular type.
    pub const MISC: ChannelKind = ChannelKind(Repr::Known(&MISC));
    /// Channels whose values are codes, whose changes are events.
    pub const STIM: ChannelKind = ChannelKind(Repr::Known(&STIM));

    /// The type named `name`, ignoring surrounding whitespace: the known type with that
    /// name, key or other spelling, in any case ("EEG", "eeg", "MEGMAG", "Temp"), or a type
    /// of that name; Misc for an empty name.
    pub fn named(name: &str) -> ChannelKind {
        let name = name.trim();
        if name.is_empty() {
            return ChannelKind::MISC;
        }
        let key = name.to_lowercase();
        match KnownKind::find(&key) {
            Some(known) => ChannelKind(Repr::Known(known)),
            None => ChannelKind(Repr::Custom {
                name: name.into(),
                key: key.into(),
            }),
        }
    }

    /// The name as shown ("MEG Mag", or a type kymo doesn't know as first given).
    pub fn name(&self) -> &str {
        match &self.0 {
            Repr::Known(known) => known.name,
            Repr::Custom { name, .. } => name,
        }
    }

    /// The name types are compared and settings kept under: lowercase, a known type's
    /// [`KnownKind::key`].
    pub fn key(&self) -> &str {
        match &self.0 {
            Repr::Known(known) => known.key,
            Repr::Custom { key, .. } => key,
        }
    }

    /// The known type this is, if kymo knows it.
    pub fn known(&self) -> Option<&'static KnownKind> {
        match self.0 {
            Repr::Known(known) => Some(known),
            Repr::Custom { .. } => None,
        }
    }

    /// MNE's channel type for it: a known type's [`KnownKind::mne`], misc for others.
    pub fn mne_name(&self) -> &'static str {
        self.known().map_or("misc", |k| k.mne)
    }

    /// Whether it is [`STIM`](Self::STIM).
    pub fn is_stim(&self) -> bool {
        self.key() == STIM.key
    }
}

impl KnownKind {
    /// The known type with the lowercase `key` as its key, other spelling or shown name.
    fn find(key: &str) -> Option<&'static KnownKind> {
        KNOWN_KINDS
            .iter()
            .copied()
            .find(|k| k.key == key || k.aliases.contains(&key) || k.name.eq_ignore_ascii_case(key))
    }
}

impl PartialEq for ChannelKind {
    fn eq(&self, other: &Self) -> bool {
        self.key() == other.key()
    }
}

impl Eq for ChannelKind {}

impl std::hash::Hash for ChannelKind {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.key().hash(state);
    }
}

impl std::fmt::Debug for ChannelKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl std::fmt::Display for ChannelKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// Physical unit of a channel.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Unit {
    /// As stored in the file, e.g. `"uV"`.
    pub label: String,
    /// Factor to the SI unit, e.g. `1e-6` for `"uV"`; `None` if the label is not recognized.
    pub to_si: Option<f64>,
}

/// What a unit measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Quantity {
    /// V.
    Voltage,
    /// S.
    Conductance,
    /// T.
    MagneticField,
    /// T/m.
    FieldGradient,
}

/// The factor to SI and the quantity of a unit label, if it is a unit kymo scales; a unit
/// written as words as its symbol.
fn parse_unit(label: &str) -> Option<(f64, Quantity)> {
    let label = label.trim();
    // Seen in EDF files.
    if matches!(label, "UV" | "uv") {
        return Some((1e-6, Quantity::Voltage));
    }
    match word_symbol(label) {
        Some(symbol) => parse_symbol(&symbol),
        None => parse_symbol(label),
    }
}

/// [`parse_unit`] for a unit written as a symbol.
fn parse_symbol(label: &str) -> Option<(f64, Quantity)> {
    let (numerator, per) = match label.split_once('/') {
        Some((n, "m")) => (n, 1.0),
        Some((n, "cm")) => (n, 1e-2),
        Some((n, "mm")) => (n, 1e-3),
        Some(_) => return None,
        None => (label, 0.0),
    };
    let base = numerator.chars().last()?;
    let prefix = &numerator[..numerator.len() - base.len_utf8()];
    let factor = match prefix {
        "" => 1.0,
        "m" => 1e-3,
        "\u{b5}" | "\u{3bc}" | "u" => 1e-6,
        "n" => 1e-9,
        "p" => 1e-12,
        "f" => 1e-15,
        _ => return None,
    };
    match (base, per) {
        ('V', 0.0) if prefix != "p" && prefix != "f" => Some((factor, Quantity::Voltage)),
        ('S', 0.0) if prefix != "p" && prefix != "f" => Some((factor, Quantity::Conductance)),
        ('T', 0.0) => Some((factor, Quantity::MagneticField)),
        ('T', per) if per > 0.0 => Some((factor / per, Quantity::FieldGradient)),
        _ => None,
    }
}

/// SI prefixes written as words, and their symbols.
const PREFIX_WORDS: [(&str, &str); 10] = [
    ("femto", "f"),
    ("pico", "p"),
    ("nano", "n"),
    ("micro", "\u{b5}"),
    ("milli", "m"),
    ("centi", "c"),
    ("deci", "d"),
    ("kilo", "k"),
    ("mega", "M"),
    ("giga", "G"),
];

/// Units written as words, singular and plural, that take a prefix, and their symbols.
const UNIT_WORDS: [(&[&str], &str); 18] = [
    (&["volt", "volts"], "V"),
    (&["tesla", "teslas"], "T"),
    (&["siemens"], "S"),
    (&["ampere", "amperes", "amp", "amps"], "A"),
    (&["ohm", "ohms"], "\u{3a9}"),
    (&["second", "seconds"], "s"),
    (&["meter", "meters", "metre", "metres"], "m"),
    (&["gram", "grams"], "g"),
    (&["hertz"], "Hz"),
    (&["watt", "watts"], "W"),
    (&["joule", "joules"], "J"),
    (&["newton", "newtons"], "N"),
    (&["pascal", "pascals"], "Pa"),
    (&["liter", "liters", "litre", "litres"], "L"),
    (&["mole", "moles"], "mol"),
    (&["kelvin", "kelvins"], "K"),
    (&["radian", "radians"], "rad"),
    // For decibels.
    (&["bel", "bels"], "B"),
];

/// Units written as words that take no prefix, and their symbols.
const PLAIN_WORDS: [(&[&str], &str); 5] = [
    (&["degree celsius", "degrees celsius", "celsius"], "\u{b0}C"),
    (&["degree", "degrees"], "\u{b0}"),
    (&["percent"], "%"),
    (&["minute", "minutes"], "min"),
    (&["hour", "hours"], "h"),
];

/// The symbol of a unit written as words, ignoring case ("microvolts" as µV, as XDF streams
/// write them, "nanoseconds" as ns, "meters per second" as m/s); `None` for other labels.
fn word_symbol(label: &str) -> Option<String> {
    let label = label.trim().to_lowercase();
    match label.split_once(" per ") {
        Some((numerator, denominator)) => Some(format!(
            "{}/{}",
            one_word_symbol(numerator.trim())?,
            one_word_symbol(denominator.trim())?
        )),
        None => one_word_symbol(&label),
    }
}

/// [`word_symbol`] for a unit without "per", in lowercase.
fn one_word_symbol(word: &str) -> Option<String> {
    if let Some((_, symbol)) = PLAIN_WORDS.iter().find(|(forms, _)| forms.contains(&word)) {
        return Some((*symbol).to_owned());
    }
    let (prefix, rest) = PREFIX_WORDS
        .iter()
        .find_map(|(name, symbol)| Some((*symbol, word.strip_prefix(name)?)))
        .unwrap_or(("", word));
    let (_, symbol) = UNIT_WORDS.iter().find(|(forms, _)| forms.contains(&rest))?;
    Some(format!("{prefix}{symbol}"))
}

impl Unit {
    /// The unit of `label`, with its SI factor if it is a unit kymo scales: volts and siemens
    /// (plain or with the prefix m, µ or n), teslas (also p and f) and teslas per meter,
    /// centimeter or millimeter ("fT/cm"). The micro prefix may be the micro sign (U+00B5),
    /// the Greek mu (U+03BC, which looks the same) or "u". Surrounding whitespace is ignored.
    /// Units may also be written as words (see [`symbol`](Self::symbol)): "microvolts",
    /// "femtotesla per centimeter".
    pub fn from_label(label: impl Into<String>) -> Unit {
        let label = label.into();
        let to_si = parse_unit(&label).map(|(factor, _)| factor);
        Unit { label, to_si }
    }

    /// What the unit measures, if its label is a unit kymo scales.
    pub fn quantity(&self) -> Option<Quantity> {
        parse_unit(&self.label).map(|(_, quantity)| quantity)
    }

    /// The label as a symbol. A unit written as words, in any case, singular or plural,
    /// becomes its symbol: an SI unit (volt, tesla, siemens, ampere, ohm, second, meter,
    /// gram, hertz, watt, joule, newton, pascal, liter, mole, kelvin, radian, bel) with or
    /// without an SI prefix from femto to giga ("microvolts" as µV, "nanoseconds" as ns,
    /// "decibels" as dB), degrees (Celsius), percent, minutes and hours, and two of them
    /// joined by "per" ("meters per second" as m/s). A unit's micro prefix written as "u"
    /// or the Greek mu becomes the micro sign (U+00B5: "uV" and EDF's "UV" as µV, "us" as
    /// µs).
    /// Other labels stay as they are. Surrounding whitespace is dropped.
    pub fn symbol(&self) -> String {
        let label = self.label.trim();
        if matches!(label, "UV" | "uv") {
            return "\u{b5}V".to_owned();
        }
        if let Some(symbol) = word_symbol(label) {
            return symbol;
        }
        match label.strip_prefix(['u', '\u{3bc}']) {
            Some(rest) if parse_symbol(label).is_some() || is_unit_symbol(rest) => {
                format!("\u{b5}{rest}")
            }
            _ => label.to_owned(),
        }
    }
}

/// Whether `symbol` is the symbol of a unit that takes a prefix ("A", "s").
fn is_unit_symbol(symbol: &str) -> bool {
    UNIT_WORDS.iter().any(|(_, s)| *s == symbol)
}

/// File metadata of one channel. Display state (visible, color, scale, bad) lives elsewhere.
#[derive(Clone, Debug, PartialEq)]
pub struct Channel {
    pub name: String,
    pub kind: ChannelKind,
    pub unit: Unit,
    /// `physical = digital * gain + offset`, computed in f64. Gain may be negative.
    pub gain: f64,
    /// Can be huge for EDF/BDF.
    pub offset: f64,
    /// The grid this channel belongs to; the sampling rate comes from there.
    pub grid: GridId,
    /// Prefiltering as stored in the file.
    pub prefilter: String,
    pub transducer: String,
    /// Physical (min, max) as stored; informational. `None` if the format has none.
    pub physical_range: Option<(f64, f64)>,
    /// Digital (min, max) as stored; `None` for float formats.
    pub digital_range: Option<(f64, f64)>,
    /// For stim channels: the bits of the raw value that carry event codes, e.g. the low 16
    /// of a BDF status channel, whose upper bits are the amplifier's own status. `None`
    /// means all bits.
    pub trigger_mask: Option<u32>,
    /// For stim channels: whether its value changes are events (stim annotations); the
    /// pyramid build records changes only for those. Off for stim channels a combined one
    /// covers (FIFF's STI001 to STI016 beside STI101), which are still drawn as codes.
    pub events: bool,
    /// Its name within its stream, where that is shorter than [`name`](Self::name): in XDF,
    /// the file's label where `name` has its stream's qualifier in front (a label two
    /// streams share), or its number where the file has no label (`name` is then
    /// `"{stream name}_{n}"`); in BVRF, a participant's channel's name without its
    /// participant (`name` is `"{name} ({participant})"`). `None` elsewhere and in formats
    /// without streams.
    pub in_stream: Option<String>,
}

impl Channel {
    /// Gain and offset that turn the values in this channel's tiles into the values shown:
    /// [`gain`](Self::gain) and [`offset`](Self::offset), except for stim channels, whose
    /// tiles hold codes, shown as they are (1 and 0), as MNE shows stim channels. Those are
    /// the digital values, masked for a channel with a [`trigger_mask`](Self::trigger_mask).
    pub fn tile_scaling(&self) -> (f64, f64) {
        if self.kind == ChannelKind::STIM {
            (1.0, 0.0)
        } else {
            (self.gain, self.offset)
        }
    }

    /// The unit of the values shown from this channel's tiles: [`unit`](Self::unit)'s
    /// label, or none for stim channels, whose tiles hold codes (BioSemi files call the unit
    /// of their Status channel "Boolean").
    pub fn tile_unit(&self) -> &str {
        if self.kind == ChannelKind::STIM {
            ""
        } else {
            &self.unit.label
        }
    }

    /// A channel of kind `Misc` with unit gain, zero offset and empty metadata.
    pub fn new(name: impl Into<String>, grid: GridId) -> Self {
        Channel {
            name: name.into(),
            kind: ChannelKind::MISC,
            unit: Unit::default(),
            gain: 1.0,
            offset: 0.0,
            grid,
            prefilter: String::new(),
            transducer: String::new(),
            physical_range: None,
            digital_range: None,
            trigger_mask: None,
            events: true,
            in_stream: None,
        }
    }
}

/// The kind of a channel with a voltage unit, from its name (BrainVision's rule, also for
/// EEGLAB channels without an MNE type): EOG for names starting with
/// EOG, HEOG or VEOG, ECG for ECG or EKG, EMG for EMG; EEG otherwise.
pub(crate) fn kind_from_name(name: &str) -> ChannelKind {
    let upper = name.trim().to_ascii_uppercase();
    let starts = |prefixes: &[&str]| prefixes.iter().any(|p| upper.starts_with(p));
    if starts(&["EOG", "HEOG", "VEOG"]) {
        ChannelKind::EOG
    } else if starts(&["ECG", "EKG"]) {
        ChannelKind::ECG
    } else if starts(&["EMG"]) {
        ChannelKind::EMG
    } else {
        ChannelKind::EEG
    }
}
