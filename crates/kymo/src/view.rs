//! What the trace area shows and how it moves, without any GUI.
//!
//! Times are seconds from the recording start and positions are points (logical pixels)
//! within the trace area, all in f64. Every change is clamped to the recording.

use std::collections::{BTreeSet, HashMap};
use std::ops::Range;

use kymo_core::tile::{Direction, SignalVersion, TileKey, View, prefetch, samples_in};
use kymo_core::{ChannelId, ChannelKind, Quantity, Recording, StreamContent, Unit};

use crate::action::{Action, Gesture};
use crate::types::{self, TypeSettings};

/// Seconds shown at first and after [`Action::DefaultWindow`], unless set otherwise.
pub const DEFAULT_SPAN: f64 = 20.0;
/// Channel rows shown at first, as MNE's `n_channels`.
pub const DEFAULT_ROWS: usize = 20;
/// The shortest window, in samples of the fastest grid.
pub const MIN_SPAN_SAMPLES: f64 = 10.0;
/// Time magnification per [`Action::ZoomIn`].
pub const ZOOM_STEP: f64 = 2.0;
/// Trace height factor per [`Action::ScaleUp`]: 2^(1/4), so four steps double it.
pub const SCALE_STEP: f64 = 1.189_207_115_002_721;
/// Factor on the number of rows per [`Action::MoreChannels`].
pub const ROWS_STEP: f64 = 1.25;

/// Size of the trace area in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Area {
    pub width: f64,
    pub height: f64,
}

/// What a channel's unit measures, as far as sharing an amplitude goes: a quantity kymo
/// scales (µV and mV alike are volts), or else the label itself ("°C", "bpm", "").
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum UnitKey {
    Quantity(Quantity),
    Label(String),
}

impl UnitKey {
    pub fn of(unit: &Unit) -> UnitKey {
        match unit.quantity() {
            Some(quantity) => UnitKey::Quantity(quantity),
            None => UnitKey::Label(unit.label.trim().to_owned()),
        }
    }
}

/// Channels that share an amplitude per channel spacing: those of one type whose units
/// share a [`UnitKey`], or one channel of a type scaled each channel on its own.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ScaleGroup {
    Shared(ChannelKind, UnitKey),
    Channel(ChannelId),
}

/// The amplitude channels of `kind` in units of `unit` start at, SI units: the type's
/// ([`types::start`]) in its own quantity, or for a type without one; 1 otherwise (an EEG
/// channel in °C, or in a unit kymo doesn't know, isn't drawn as microvolts).
fn group_default(kind: &ChannelKind, unit: &UnitKey) -> f64 {
    let start = types::start(kind);
    match (start.quantity, unit) {
        (None, _) => start.amplitude,
        (Some(own), UnitKey::Quantity(q)) if own == *q => start.amplitude,
        _ => 1.0,
    }
}

/// Per channel, the index of the recording's stream it belongs to; all `None` without
/// streams.
fn stream_of(recording: &Recording) -> Vec<Option<usize>> {
    let mut of = vec![None; recording.channels().len()];
    for (s, stream) in recording.streams().iter().enumerate() {
        if let StreamContent::Channels(channels) = &stream.content {
            for c in channels {
                of[c.0 as usize] = Some(s);
            }
        }
    }
    of
}

/// Every channel in the order: the streams in file order, each by type (see
/// [`types::rank`], the types kymo doesn't know in the order their first channel has in the
/// file), a type's channels in file order.
fn sorted(stream_of: &[Option<usize>], kinds: &[ChannelKind]) -> Vec<ChannelId> {
    let mut first_seen = HashMap::new();
    for (c, kind) in kinds.iter().enumerate() {
        first_seen.entry(kind.clone()).or_insert(c);
    }
    let mut order: Vec<ChannelId> = (0..kinds.len() as u32).map(ChannelId).collect();
    order.sort_unstable_by_key(|c| {
        let c = c.0 as usize;
        (stream_of[c], types::rank(&kinds[c], &first_seen), c)
    });
    order
}

/// The viewer's display state for one recording.
#[derive(Clone, Debug)]
pub struct ViewState {
    duration: f64,
    min_span: f64,
    /// Every channel, top to bottom; a stream's channels stay together, and within it a
    /// type's.
    order: Vec<ChannelId>,
    /// Per channel (by id): its place in `order`.
    place: Vec<usize>,
    /// Per channel (by id): the recording's stream it belongs to (its index there), if the
    /// recording has streams.
    stream_of: Vec<Option<usize>>,
    /// The rows, top to bottom: the shown channels in `order`.
    rows_order: Vec<ChannelId>,
    /// Per channel (by id): whether it is hidden.
    hidden: Vec<bool>,
    /// Per channel (by id): its type, its unit's factor to SI and what its unit measures.
    kinds: Vec<ChannelKind>,
    /// Per channel (by id): its type as the file gives it.
    file_kinds: Vec<ChannelKind>,
    /// Counts the changes of channels' types, for [`kinds_revision`](Self::kinds_revision).
    kinds_revision: u64,
    /// The channels selected in the channel list, and the one a range selection starts
    /// from (the last clicked).
    selected: BTreeSet<ChannelId>,
    anchor: Option<ChannelId>,
    to_si: Vec<Option<f64>>,
    units: Vec<UnitKey>,
    start: f64,
    span: f64,
    /// Seconds shown at first and after [`Action::DefaultWindow`].
    default_span: f64,
    /// The first row on screen; fractional while scrolling smoothly.
    top: f64,
    /// Rows on screen as last chosen; fewer while fewer channels are shown.
    rows: usize,
    /// Amplitude per channel spacing (SI units) of the scale groups Auto Scale fitted, before
    /// `scale`; the others have their defaults ([`group_default`]).
    fitted: HashMap<ScaleGroup, f64>,
    /// The factor on every amplitude from scaling up and down.
    scale: f64,
    /// The channels scaled on their own by hand (see [`scale_channels`](Self::scale_channels)),
    /// each a scale group of its own whatever its type's setting.
    alone: BTreeSet<ChannelId>,
    /// What scrolling the number of rows left over from rounding to whole rows, as a factor
    /// on the rows shown (see [`Gesture::Rows`]).
    rows_residual: f64,
    dc: bool,
    types: TypeSettings,
    bad: BTreeSet<ChannelId>,
    direction: Direction,
    generation: u64,
    requested: Option<View>,
    /// The DC windows last asked for, while DC removal is on.
    dc_requested: Option<Vec<(ChannelId, Range<u64>)>>,
}

impl ViewState {
    /// [`DEFAULT_SPAN`] from the start (or the whole recording if shorter), the first
    /// [`DEFAULT_ROWS`] channels, default amplitudes, DC removal on, the channels the file
    /// marks as bad (FIFF) bad.
    pub fn new(recording: &Recording) -> Self {
        Self::with_default_span(recording, DEFAULT_SPAN)
    }

    /// As [`new`](Self::new), showing `seconds` at first and after
    /// [`Action::DefaultWindow`].
    pub fn with_default_span(recording: &Recording, seconds: f64) -> Self {
        let channels = recording.channels();
        let rate = recording.sfreq();
        let stream_of = stream_of(recording);
        let kinds: Vec<ChannelKind> = channels.iter().map(|c| c.kind.clone()).collect();
        let order = sorted(&stream_of, &kinds);
        let mut place = vec![0; channels.len()];
        for (i, c) in order.iter().enumerate() {
            place[c.0 as usize] = i;
        }
        let mut state = ViewState {
            duration: recording.duration().max(0.0),
            min_span: if rate > 0.0 {
                MIN_SPAN_SAMPLES / rate
            } else {
                0.0
            },
            rows_order: order.clone(),
            order,
            place,
            stream_of,
            hidden: vec![false; channels.len()],
            file_kinds: kinds.clone(),
            kinds,
            kinds_revision: 0,
            selected: BTreeSet::new(),
            anchor: None,
            to_si: channels.iter().map(|c| c.unit.to_si).collect(),
            units: channels.iter().map(|c| UnitKey::of(&c.unit)).collect(),
            start: 0.0,
            span: DEFAULT_SPAN,
            default_span: DEFAULT_SPAN,
            top: 0.0,
            rows: DEFAULT_ROWS,
            fitted: HashMap::new(),
            scale: 1.0,
            alone: BTreeSet::new(),
            rows_residual: 1.0,
            dc: true,
            types: TypeSettings::default(),
            bad: recording.bad_channels().iter().copied().collect(),
            direction: Direction::Forward,
            generation: 0,
            requested: None,
            dc_requested: None,
        };
        state.set_default_span(seconds);
        state.span = state.default_span;
        state.clamp();
        state
    }

    pub fn duration(&self) -> f64 {
        self.duration
    }

    /// Seconds shown.
    pub fn time(&self) -> Range<f64> {
        self.start..self.start + self.span
    }

    pub fn start(&self) -> f64 {
        self.start
    }

    pub fn span(&self) -> f64 {
        self.span
    }

    /// The first row on screen, fractional while scrolling.
    pub fn top(&self) -> f64 {
        self.top
    }

    /// All channel rows, on screen or not: the shown channels.
    pub fn row_count(&self) -> usize {
        self.rows_order.len()
    }

    /// Rows that fit on screen: as many as last chosen, at most all, at least 1.
    pub fn rows(&self) -> usize {
        self.rows.min(self.rows_order.len()).max(1)
    }

    /// Indices of the rows on screen, including partly visible ones.
    pub fn visible_rows(&self) -> Range<usize> {
        let n = self.rows_order.len();
        let first = (self.top.floor() as usize).min(n);
        let last = ((self.top + self.rows() as f64).ceil() as usize).min(n);
        first..last.max(first)
    }

    /// The row a channel is shown in, `None` if it is hidden.
    pub fn row_of(&self, channel: ChannelId) -> Option<usize> {
        if !self.is_shown(channel) {
            return None;
        }
        // Rows are in the channels' order.
        let place = self.place[channel.0 as usize];
        self.rows_order
            .binary_search_by_key(&place, |c| self.place[c.0 as usize])
            .ok()
    }

    /// Every channel, shown or not, top to bottom.
    pub fn order(&self) -> &[ChannelId] {
        &self.order
    }

    /// The channels of all rows, on screen or not, top to bottom: the shown channels.
    pub fn row_channels(&self) -> &[ChannelId] {
        &self.rows_order
    }

    /// The channels on screen, top to bottom.
    pub fn visible_channels(&self) -> &[ChannelId] {
        &self.rows_order[self.visible_rows()]
    }

    /// Amplitude per channel spacing for a channel type's channels in its own quantity, in
    /// SI units (V for EEG), unless the type is scaled each channel on its own; for a type
    /// without a quantity of its own, of its channels without a unit.
    pub fn amplitude(&self, kind: &ChannelKind) -> f64 {
        let unit = match types::start(kind).quantity {
            Some(quantity) => UnitKey::Quantity(quantity),
            None => UnitKey::Label(String::new()),
        };
        self.group_amplitude(&ScaleGroup::Shared(kind.clone(), unit))
    }

    /// Amplitude per channel spacing for a scale group, in SI units.
    pub fn group_amplitude(&self, group: &ScaleGroup) -> f64 {
        let base = self
            .fitted
            .get(group)
            .copied()
            .unwrap_or_else(|| match group {
                ScaleGroup::Shared(kind, unit) => group_default(kind, unit),
                ScaleGroup::Channel(c) => group_default(self.kind_of(*c), self.unit_of(*c)),
            });
        base * self.scale
    }

    /// Amplitude per channel spacing for one channel, in its own physical unit. A unit
    /// without a known SI factor counts as SI.
    pub fn channel_amplitude(&self, channel: ChannelId) -> f64 {
        self.group_amplitude(&self.scale_group(channel)) / self.to_si(channel)
    }

    /// The channels `channel` shares its amplitude with: its own group if it was scaled on
    /// its own or its type is scaled each channel on its own, else those of its type whose
    /// units measure the same.
    pub fn scale_group(&self, channel: ChannelId) -> ScaleGroup {
        let kind = self.kind_of(channel);
        if self.alone.contains(&channel) || self.types.own_scale(kind) {
            ScaleGroup::Channel(channel)
        } else {
            ScaleGroup::Shared(kind.clone(), self.unit_of(channel).clone())
        }
    }

    /// Makes `channels` `factor` times taller, each on its own: a channel that shares its
    /// type's amplitude leaves the group for one of its own, starting where the group's is.
    pub fn scale_channels(&mut self, channels: &[ChannelId], factor: f64) {
        if !(factor.is_finite() && factor > 0.0) {
            return;
        }
        for &c in channels {
            if c.0 as usize >= self.kinds.len() {
                continue;
            }
            let base = self.group_amplitude(&self.scale_group(c)) / self.scale;
            self.alone.insert(c);
            self.fitted.insert(ScaleGroup::Channel(c), base / factor);
        }
    }

    /// The channels a scaling over `channel` scales: the selection if `channel` is in it,
    /// else `channel` alone.
    pub fn scale_targets(&self, channel: ChannelId) -> Vec<ChannelId> {
        if self.is_selected(channel) {
            self.selected()
        } else {
            vec![channel]
        }
    }

    /// Whether `channel` was scaled on its own though its type shares an amplitude: the
    /// channel list marks it, and [`rejoin`](Self::rejoin) returns it to its group.
    pub fn scaled_alone(&self, channel: ChannelId) -> bool {
        self.alone.contains(&channel) && !self.types.own_scale(self.kind_of(channel))
    }

    /// Returns `channels` to their type's amplitude. A channel of a type scaled each channel
    /// on its own keeps its amplitude.
    pub fn rejoin(&mut self, channels: &[ChannelId]) {
        for &c in channels {
            if self.alone.remove(&c) && !self.types.own_scale(self.kind_of(c)) {
                self.fitted.remove(&ScaleGroup::Channel(c));
            }
        }
    }

    fn unit_of(&self, channel: ChannelId) -> &UnitKey {
        const NONE: &UnitKey = &UnitKey::Label(String::new());
        self.units.get(channel.0 as usize).unwrap_or(NONE)
    }

    /// The factor from a channel's physical unit to SI; 1 for a unit without a known one.
    pub fn to_si(&self, channel: ChannelId) -> f64 {
        let i = channel.0 as usize;
        self.to_si.get(i).copied().flatten().unwrap_or(1.0)
    }

    /// Sets the amplitude per channel spacing (SI units) of each scale group given; the
    /// others, and values that aren't positive and finite, stay as they are.
    pub fn set_amplitudes(&mut self, amplitudes: HashMap<ScaleGroup, f64>) {
        for (group, amplitude) in amplitudes {
            if amplitude.is_finite() && amplitude > 0.0 {
                self.fitted.insert(group, amplitude / self.scale);
            }
        }
    }

    /// The channel's type (misc for a channel not in the recording).
    pub fn kind_of(&self, channel: ChannelId) -> &ChannelKind {
        self.kinds
            .get(channel.0 as usize)
            .unwrap_or(&ChannelKind::MISC)
    }

    /// The recording's channel types, each once, in the order.
    pub fn kinds(&self) -> Vec<ChannelKind> {
        let mut kinds: Vec<ChannelKind> = Vec::new();
        for &c in &self.order {
            let kind = self.kind_of(c);
            if !kinds.contains(kind) {
                kinds.push(kind.clone());
            }
        }
        kinds
    }

    /// A channel's type as the file gives it (misc for a channel not in the recording).
    pub fn file_kind(&self, channel: ChannelId) -> &ChannelKind {
        self.file_kinds
            .get(channel.0 as usize)
            .unwrap_or(&ChannelKind::MISC)
    }

    /// Whether a channel's type differs from the file's.
    pub fn retyped(&self, channel: ChannelId) -> bool {
        self.kind_of(channel) != self.file_kind(channel)
    }

    /// Counts the changes of channels' types: it differs after any change.
    pub fn kinds_revision(&self) -> u64 {
        self.kinds_revision
    }

    /// Gives `channels` the type `kind`, never stim (only [`kinds_from_file`] makes a channel
    /// stim again, as only the readers make one). The order follows: each changed channel
    /// moves to its new type's channels in its stream, by its place in the file. The first
    /// row on screen stays first unless it changed. Returns whether a type changed.
    ///
    /// [`kinds_from_file`]: Self::kinds_from_file
    pub fn set_kind(&mut self, channels: &[ChannelId], kind: &ChannelKind) -> bool {
        if kind.is_stim() {
            return false;
        }
        let changes: Vec<_> = channels.iter().map(|&c| (c, kind.clone())).collect();
        self.retype(changes)
    }

    /// Returns `channels` to the types the file gives them, as [`set_kind`](Self::set_kind).
    pub fn kinds_from_file(&mut self, channels: &[ChannelId]) -> bool {
        let changes: Vec<_> = channels
            .iter()
            .map(|&c| (c, self.file_kind(c).clone()))
            .collect();
        self.retype(changes)
    }

    /// Gives each channel of `changes` its type, as a loaded `channels.tsv` says, in one
    /// change: stim only where the file gives the channel stim (as only the readers make a
    /// channel stim). Returns whether a type changed, and how many changes to stim were
    /// left out.
    pub fn apply_kinds(&mut self, changes: &[(ChannelId, ChannelKind)]) -> (bool, usize) {
        let (taken, refused): (Vec<_>, Vec<_>) = changes
            .iter()
            .cloned()
            .partition(|(c, kind)| !kind.is_stim() || self.file_kind(*c).is_stim());
        (self.retype(taken), refused.len())
    }

    fn retype(&mut self, changes: Vec<(ChannelId, ChannelKind)>) -> bool {
        let mut changed = BTreeSet::new();
        for (c, kind) in changes {
            if let Some(old) = self.kinds.get_mut(c.0 as usize)
                && *old != kind
            {
                *old = kind;
                changed.insert(c);
            }
        }
        if changed.is_empty() {
            return false;
        }
        self.kinds_revision += 1;
        // The first channel on screen that keeps its type stays where it is.
        let anchor = self.rows_order[self.visible_rows()]
            .iter()
            .copied()
            .find(|c| !changed.contains(c));
        let fraction = self.top - self.top.floor();
        self.order = sorted(&self.stream_of, &self.kinds);
        for (i, c) in self.order.iter().enumerate() {
            self.place[c.0 as usize] = i;
        }
        self.rows_order = self.shown_in_order();
        if let Some(row) = anchor.and_then(|c| self.row_of(c)) {
            self.top = row as f64 + fraction;
        }
        self.clamp();
        true
    }

    /// Whether a channel is selected in the channel list.
    pub fn is_selected(&self, channel: ChannelId) -> bool {
        self.selected.contains(&channel)
    }

    /// The selected channels, in the order.
    pub fn selected(&self) -> Vec<ChannelId> {
        self.order
            .iter()
            .copied()
            .filter(|c| self.selected.contains(c))
            .collect()
    }

    /// Selects `channel` alone.
    pub fn select(&mut self, channel: ChannelId) {
        self.selected = BTreeSet::from([channel]);
        self.anchor = Some(channel);
    }

    /// Adds `channel` to the selection or takes it out (⌘-click).
    pub fn toggle_selected(&mut self, channel: ChannelId) {
        if !self.selected.remove(&channel) {
            self.selected.insert(channel);
        }
        self.anchor = Some(channel);
    }

    /// Selects the channels of `listed` (the list's rows, in its order) from the last clicked
    /// one to `channel` (⇧-click), or `channel` alone if the last clicked isn't listed.
    pub fn select_range(&mut self, listed: &[ChannelId], channel: ChannelId) {
        let from = self
            .anchor
            .and_then(|a| listed.iter().position(|&c| c == a));
        let (Some(from), Some(to)) = (from, listed.iter().position(|&c| c == channel)) else {
            self.select(channel);
            return;
        };
        self.selected = listed[from.min(to)..=from.max(to)]
            .iter()
            .copied()
            .collect();
    }

    /// Selects the channels of a type (in `stream`, for a recording grouped by stream),
    /// hidden ones too.
    pub fn select_group(&mut self, stream: Option<usize>, kind: &ChannelKind) {
        self.selected = (0..self.order.len())
            .filter(|&c| self.in_group(c, stream, kind))
            .map(|c| ChannelId(c as u32))
            .collect();
        self.anchor = None;
    }

    /// Selects every channel, hidden ones too.
    pub fn select_all(&mut self) {
        self.selected = self.order.iter().copied().collect();
        self.anchor = None;
    }

    /// Selects the bad channels.
    pub fn select_bad(&mut self) {
        self.selected = self.bad.clone();
        self.anchor = None;
    }

    /// Clears the selection; returns whether anything was selected.
    pub fn clear_selection(&mut self) -> bool {
        self.anchor = None;
        !std::mem::take(&mut self.selected).is_empty()
    }

    pub fn dc_removed(&self) -> bool {
        self.dc
    }

    /// Turns DC removal on or off; the app's setting decides, as [`Action::ToggleDc`]
    /// changes the setting, not the view.
    pub fn set_dc_removed(&mut self, on: bool) {
        self.dc = on;
    }

    /// Whether DC removal centers `channel`: it is on, the channel's type is one it
    /// centers, and the channel isn't a stim channel.
    pub fn dc_applies(&self, channel: ChannelId) -> bool {
        self.dc && self.types.remove_dc(self.kind_of(channel))
    }

    pub fn type_settings(&self) -> &TypeSettings {
        &self.types
    }

    /// The per-type settings from now on (the app's settings decide). Amplitudes fitted to
    /// groups that no longer exist stay, for when they come back.
    pub fn set_type_settings(&mut self, types: TypeSettings) {
        self.types = types;
    }

    pub fn is_bad(&self, channel: ChannelId) -> bool {
        self.bad.contains(&channel)
    }

    pub fn bad_channels(&self) -> impl Iterator<Item = ChannelId> + '_ {
        self.bad.iter().copied()
    }

    /// Whether a channel has a row (on screen or not); `false` for unknown channels.
    pub fn is_shown(&self, channel: ChannelId) -> bool {
        self.hidden.get(channel.0 as usize) == Some(&false)
    }

    /// The group a channel belongs to in the order: its stream (by its index in the
    /// recording; `None` without streams) and its type. A group's channels stay together,
    /// as do a stream's groups.
    pub fn group_of(&self, channel: ChannelId) -> (Option<usize>, ChannelKind) {
        self.group(channel.0 as usize)
    }

    /// How many channels of a group (see [`group_of`](Self::group_of)) are shown, and how
    /// many it has.
    pub fn group_shown(&self, stream: Option<usize>, kind: &ChannelKind) -> (usize, usize) {
        (0..self.order.len())
            .filter(|&c| self.in_group(c, stream, kind))
            .fold((0, 0), |(shown, all), c| {
                (shown + usize::from(!self.hidden[c]), all + 1)
            })
    }

    /// Applies a view action. Returns `false` for actions outside the view (open, quit,
    /// ...).
    pub fn apply(&mut self, action: Action) -> bool {
        let rows = self.rows() as f64;
        match action {
            Action::PanLeft => self.pan(-self.span / 4.0),
            Action::PanRight => self.pan(self.span / 4.0),
            Action::PageLeft => self.pan(-self.span),
            Action::PageRight => self.pan(self.span),
            Action::ToStart => self.pan(-self.duration),
            Action::ToEnd => self.pan(self.duration),
            // From the window's start, as Default Window keeps it, so zooming in and out
            // returns to the same window unless the end of the recording moved it.
            Action::ZoomIn => self.zoom(ZOOM_STEP, 0.0),
            Action::ZoomOut => self.zoom(1.0 / ZOOM_STEP, 0.0),
            Action::DefaultWindow => {
                self.span = self.default_span;
                self.clamp();
            }
            Action::Fit => {
                self.start = 0.0;
                self.span = self.duration;
                self.clamp();
            }
            // Row steps land on whole rows, also after smooth scrolling.
            Action::ChannelUp => self.set_top((self.top - 1.0).ceil()),
            Action::ChannelDown => self.set_top((self.top + 1.0).floor()),
            Action::ChannelPageUp => self.set_top((self.top - rows).ceil()),
            Action::ChannelPageDown => self.set_top((self.top + rows).floor()),
            Action::FirstChannels => self.set_top(0.0),
            Action::LastChannels => self.set_top(f64::INFINITY),
            Action::MoreChannels => {
                let more = (rows * ROWS_STEP).round() as usize;
                self.set_rows(more.max(self.rows() + 1));
            }
            Action::FewerChannels => {
                let fewer = (rows / ROWS_STEP).round() as usize;
                self.set_rows(fewer.min(self.rows().saturating_sub(1)));
            }
            Action::ScaleUp => self.scale(SCALE_STEP),
            Action::ScaleDown => self.scale(1.0 / SCALE_STEP),
            Action::ScaleSelectedUp => self.scale_channels(&self.selected(), SCALE_STEP),
            Action::ScaleSelectedDown => self.scale_channels(&self.selected(), 1.0 / SCALE_STEP),
            Action::SelectAllChannels => self.select_all(),
            Action::SelectBadChannels => self.select_bad(),
            Action::DeselectChannels => {
                self.clear_selection();
            }
            Action::DefaultScale => {
                self.fitted.clear();
                self.alone.clear();
                self.scale = 1.0;
            }
            Action::ToggleDc
            | Action::Open
            | Action::GoToTime
            | Action::OpenRecent(_)
            | Action::ClearRecent
            | Action::Close
            | Action::Quit
            | Action::Settings
            | Action::Shortcuts
            | Action::RecordingInfo
            | Action::About
            | Action::CheckForUpdates
            | Action::ToggleSidebar
            | Action::ToggleScaleBars
            | Action::ToggleMinimap
            | Action::ToggleAntialias
            | Action::AutoScale
            | Action::StimEveryChange
            | Action::StimFromZero
            | Action::StimIncreasing
            | Action::ToggleInitialCode
            | Action::ToggleEditing
            | Action::Undo
            | Action::Redo
            | Action::DeleteAnnotation
            | Action::MergeAnnotations
            | Action::Deselect
            | Action::LoadAnnotations
            | Action::SaveAnnotations
            | Action::ExportShownAnnotations
            | Action::LoadChannels
            | Action::SaveChannels => return false,
        }
        true
    }

    /// Applies a gesture made over a trace area of size `area`, with the pointer at
    /// `pointer_x` points from its left edge (zooming centers on the area without one).
    pub fn gesture(&mut self, gesture: Gesture, area: Area, pointer_x: Option<f64>) {
        match gesture {
            Gesture::Pan { points } => self.pan_points(points, area.width),
            Gesture::ScrollRows { points } => {
                let row = self.row_height(area.height);
                if row > 0.0 {
                    self.set_top(self.top - points / row);
                }
            }
            Gesture::Zoom { factor } => {
                let anchor = match pointer_x {
                    Some(x) if area.width > 0.0 => (x / area.width).clamp(0.0, 1.0),
                    _ => 0.5,
                };
                self.zoom(factor, anchor);
            }
            Gesture::Scale { factor } => self.scale(factor),
            Gesture::Rows { factor } => self.zoom_rows(factor),
            // Needs the channel under the pointer, which the viewer finds (see
            // [`scale_channels`](Self::scale_channels)).
            Gesture::ScaleChannel { .. } => {}
        }
    }

    /// Shows `factor` times fewer rows (above 1 magnifies them), in whole rows: what a step
    /// leaves over from rounding carries over to the next, so slow scrolling adds up.
    pub fn zoom_rows(&mut self, factor: f64) {
        if !(factor.is_finite() && factor > 0.0) {
            return;
        }
        let rows = self.rows();
        let wanted = rows as f64 * self.rows_residual / factor;
        let n = (wanted.round().max(1.0)) as usize;
        if n != rows {
            self.set_rows(n);
        }
        // At the ends (one row, or all of them) nothing carries over.
        let now = self.rows();
        self.rows_residual = if n == now && n > 1 {
            (wanted / now as f64).clamp(0.5, 2.0)
        } else {
            1.0
        };
    }

    /// Moves the window by `seconds` (positive is later).
    pub fn pan(&mut self, seconds: f64) {
        if !seconds.is_finite() || seconds == 0.0 {
            return;
        }
        self.direction = if seconds > 0.0 {
            Direction::Forward
        } else {
            Direction::Backward
        };
        self.start += seconds;
        self.clamp();
    }

    /// Moves the traces right by `points` in an area `width` points wide, as dragging does.
    pub fn pan_points(&mut self, points: f64, width: f64) {
        if width > 0.0 {
            self.pan(-points / width * self.span);
        }
    }

    /// Magnifies time by `factor` (above 1 zooms in), keeping the time at `anchor` (0 is
    /// the left edge, 1 the right) in place unless the recording's edges stop it.
    pub fn zoom(&mut self, factor: f64, anchor: f64) {
        if !(factor.is_finite() && factor > 0.0) {
            return;
        }
        let anchor = anchor.clamp(0.0, 1.0);
        let at = self.start + anchor * self.span;
        self.span /= factor;
        self.clamp_span();
        self.start = at - anchor * self.span;
        self.clamp();
    }

    /// Makes traces `factor` times taller.
    pub fn scale(&mut self, factor: f64) {
        if factor.is_finite() && factor > 0.0 {
            self.scale /= factor;
        }
    }

    /// Seconds [`Action::DefaultWindow`] shows from now on; the window shown now stays.
    /// Values that aren't positive and finite are ignored.
    pub fn set_default_span(&mut self, seconds: f64) {
        if seconds.is_finite() && seconds > 0.0 {
            self.default_span = seconds;
        }
    }

    pub fn default_span(&self) -> f64 {
        self.default_span
    }

    /// Marks a channel bad or good.
    pub fn set_bad(&mut self, channel: ChannelId, bad: bool) {
        if !bad {
            self.bad.remove(&channel);
        } else if (channel.0 as usize) < self.kinds.len() {
            self.bad.insert(channel);
        }
    }

    /// Shows `rows` rows (at least 1, at most all), keeping the top row. Asking for all
    /// rows or more keeps a larger number chosen before, for when hidden channels return.
    pub fn set_rows(&mut self, rows: usize) {
        let n = self.rows_order.len();
        self.rows = if rows >= n { self.rows.max(n) } else { rows };
        self.clamp();
    }

    /// Shows or hides a channel's row. The first row on screen stays first while it is
    /// shown; the rows on screen stay as many as chosen while there are enough.
    pub fn set_shown(&mut self, channel: ChannelId, shown: bool) {
        if let Some(hidden) = self.hidden.get_mut(channel.0 as usize) {
            *hidden = !shown;
            self.rebuild_rows();
        }
    }

    /// How many channels of a stream (by its index in the recording) are shown, and how many
    /// it has.
    pub fn stream_shown(&self, stream: usize) -> (usize, usize) {
        self.stream_of
            .iter()
            .zip(&self.hidden)
            .filter(|&(&s, _)| s == Some(stream))
            .fold((0, 0), |(shown, all), (_, &hidden)| {
                (shown + usize::from(!hidden), all + 1)
            })
    }

    /// Shows or hides every channel of a stream, as [`set_shown`](Self::set_shown).
    pub fn set_stream_shown(&mut self, stream: usize, shown: bool) {
        for (hidden, &s) in self.hidden.iter_mut().zip(&self.stream_of) {
            if s == Some(stream) {
                *hidden = !shown;
            }
        }
        self.rebuild_rows();
    }

    /// Shows or hides every channel of a group (see [`group_of`](Self::group_of)), as
    /// [`set_shown`](Self::set_shown).
    pub fn set_group_shown(&mut self, stream: Option<usize>, kind: &ChannelKind, shown: bool) {
        for c in 0..self.order.len() {
            if self.in_group(c, stream, kind) {
                self.hidden[c] = !shown;
            }
        }
        self.rebuild_rows();
    }

    /// Scrolls so that a shown channel's row is first on screen, or as close to it as the
    /// end of the list allows. Returns `false` for a hidden channel.
    pub fn scroll_to(&mut self, channel: ChannelId) -> bool {
        match self.row_of(channel) {
            Some(row) => {
                self.set_top(row as f64);
                true
            }
            None => false,
        }
    }

    /// Scrolls just far enough that a shown channel's row is wholly on screen: it becomes
    /// the first row if it was above, the last if below; nothing moves if it is on screen.
    /// Returns `false` for a hidden channel.
    pub fn reveal(&mut self, channel: ChannelId) -> bool {
        let Some(row) = self.row_of(channel) else {
            return false;
        };
        let (row, rows) = (row as f64, self.rows() as f64);
        if row < self.top {
            self.set_top(row);
        } else if row + 1.0 > self.top + rows {
            self.set_top(row + 1.0 - rows);
        }
        true
    }

    /// Scrolls so that row `top` (fractional for smooth scrolling) is first on screen.
    pub fn set_top(&mut self, top: f64) {
        if !top.is_nan() {
            self.top = top;
            self.clamp();
        }
    }

    /// The time at `x` points from the left edge of an area `width` points wide.
    pub fn time_at(&self, x: f64, width: f64) -> f64 {
        self.start + x / width * self.span
    }

    /// Points from the left edge at which time `t` lies.
    pub fn x_at(&self, t: f64, width: f64) -> f64 {
        (t - self.start) / self.span * width
    }

    /// Height of one row in an area `height` points tall.
    pub fn row_height(&self, height: f64) -> f64 {
        height / self.rows() as f64
    }

    /// The (fractional) row index at `y` points from the top.
    pub fn row_at(&self, y: f64, height: f64) -> f64 {
        self.top + y / self.row_height(height)
    }

    /// Points from the top of row `row`'s upper edge.
    pub fn y_of_row(&self, row: f64, height: f64) -> f64 {
        (row - self.top) * self.row_height(height)
    }

    /// The channel whose row is at `y` points from the top.
    pub fn channel_at(&self, y: f64, height: f64) -> Option<ChannelId> {
        let row = self.row_at(y, height);
        if row < 0.0 {
            return None;
        }
        self.rows_order.get(row.floor() as usize).copied()
    }

    /// What the tiles must cover for a trace area `width_px` physical pixels wide.
    pub fn tile_view(&self, width_px: u32) -> View {
        View {
            version: SignalVersion::RAW,
            time: self.time(),
            width_px,
            channels: self.visible_channels().to_vec(),
        }
    }

    /// The generation of the last [`request`](Self::request), so other keys can join its
    /// queue.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The tiles to ask for when the tile view changed since the last call: a new
    /// generation and the keys from [`prefetch`], looking ahead in the direction of the
    /// last pan. `None` while the view stays the same.
    pub fn request(
        &mut self,
        recording: &Recording,
        width_px: u32,
        max_bytes: usize,
    ) -> Option<(u64, Vec<TileKey>)> {
        let view = self.tile_view(width_px);
        if self.requested.as_ref() == Some(&view) {
            return None;
        }
        let keys = prefetch(recording, &view, self.direction, max_bytes);
        self.requested = Some(view);
        self.generation += 1;
        Some((self.generation, keys))
    }

    /// The windows to ask for DC means over when they changed since the last call: each
    /// visible channel's samples in view, top to bottom, except stim channels, which DC
    /// removal leaves alone. `None` while DC removal is off or nothing changed.
    pub fn dc_request(&mut self, recording: &Recording) -> Option<Vec<(ChannelId, Range<u64>)>> {
        if !self.dc {
            self.dc_requested = None;
            return None;
        }
        let time = self.time();
        let windows: Vec<_> = self
            .visible_channels()
            .iter()
            .filter(|&&c| self.dc_applies(c))
            .map(|&c| {
                let grid = recording.grid(recording.channel(c).grid);
                (c, samples_in(grid, &time))
            })
            .collect();
        if self.dc_requested.as_ref() == Some(&windows) {
            return None;
        }
        self.dc_requested = Some(windows.clone());
        Some(windows)
    }

    /// The rows after a change of what is shown, keeping the first channel on screen (or
    /// the next shown one after it) first.
    fn rebuild_rows(&mut self) {
        let row = self.top.floor();
        let anchor = self.rows_order.get(row as usize).copied();
        self.rows_order = self.shown_in_order();
        if let Some(anchor) = anchor {
            let place = self.place[anchor.0 as usize];
            let top = match self
                .rows_order
                .binary_search_by_key(&place, |c| self.place[c.0 as usize])
            {
                Ok(i) => i as f64 + (self.top - row),
                Err(i) => i as f64,
            };
            self.top = top;
        }
        self.clamp();
    }

    /// After the order changed: the places and rows follow it; the rows on screen stay.
    fn shown_in_order(&self) -> Vec<ChannelId> {
        self.order
            .iter()
            .copied()
            .filter(|&c| self.is_shown(c))
            .collect()
    }

    /// A channel's (by index) group, as [`group_of`](Self::group_of).
    fn group(&self, c: usize) -> (Option<usize>, ChannelKind) {
        (self.stream_of[c], self.kinds[c].clone())
    }

    /// Whether a channel (by index) is in the group of `stream` and `kind`.
    fn in_group(&self, c: usize, stream: Option<usize>, kind: &ChannelKind) -> bool {
        self.stream_of[c] == stream && self.kinds[c] == *kind
    }

    fn clamp_span(&mut self) {
        let shortest = self.min_span.min(self.duration);
        self.span = if self.span.is_nan() {
            self.duration
        } else {
            self.span.clamp(shortest, self.duration)
        };
    }

    fn clamp(&mut self) {
        self.clamp_span();
        let latest = self.duration - self.span;
        self.start = if self.start.is_nan() {
            0.0
        } else {
            self.start.clamp(0.0, latest.max(0.0))
        };
        self.rows = self.rows.max(1);
        let lowest = self.rows_order.len().saturating_sub(self.rows()) as f64;
        self.top = self.top.clamp(0.0, lowest);
    }
}
