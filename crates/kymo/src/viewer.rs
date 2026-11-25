//! An open recording on screen: the sidebar, the trace area with channel names, time axis
//! and scrollbars, and the status line.

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use eframe::egui::{
    self, Align, Align2, Color32, CursorIcon, Event, FontId, Pos2, Rangef, Rect, Sense, Stroke,
    StrokeKind, TextFormat, TextStyle, Ui, pos2, text::LayoutJob, vec2,
};
use kymo_core::service::TileService;
use kymo_core::stim::EventOptions;
use kymo_core::tile::query;
use kymo_core::{AnnotationData, AnnotationId, ChannelId, ChannelKind, GridId, Origin};
use kymo_core::{annotation_file, channel_file};

use crate::action::{Action, Gesture};
use crate::annotations::{self, Legend, Marks};
use crate::annotations::{AnnotationColor, AnnotationColors};
use crate::document::{Document, Summary, format_duration};
use crate::history::{self, Change, History};
use crate::keymap::{Keymap, Platform};
use crate::measure::{self, Measurement, Span};
use crate::render::{Frame, Scene};
use crate::scale;
use crate::scale_bar::{ScaleBar, reading_unit, scale_bars};
use crate::stim_annotations::{StimAnnotations, Update};
use crate::types::TypeSettings;
use crate::ui::axis::{self, Clock};
use crate::ui::gpu::Gpu;
use crate::ui::header::{
    AnnotationPanel, DescriptionField, HeaderSource, description_field_id, header_panel,
    within_width,
};
use crate::ui::minimap::{self, Minimap};
use crate::ui::palette::{annotation_color, bad_color, type_color};
use crate::ui::{elide_middle, readout, scrollbar, status};
use crate::view::{Area, ViewState};

/// Narrowest the sidebar can be dragged.
const SIDEBAR_MIN_WIDTH: f32 = 200.0;
/// Widest the sidebar can be dragged, as a fraction of the window.
const SIDEBAR_MAX_FRACTION: f32 = 0.5;
/// Widest the channel names column gets by itself, as a fraction of the trace region and
/// in ems (the body font's size; "NEDE_FlightParams_0" fits, "POR X - (Point of regard)"
/// doesn't): longer names are shortened in the middle.
const NAMES_MAX_FRACTION: f32 = 0.3;
const NAMES_MAX_EMS: f32 = 10.0;
/// Narrowest and widest the names column can be dragged, in ems (or as narrow as the names
/// fit, where that is less) and as a fraction of the trace region.
const NAMES_DRAG_MIN_EMS: f32 = 3.0;
const NAMES_DRAG_MAX_FRACTION: f32 = 0.5;
/// How far from the boundary between names and traces it can be grabbed, in points.
const NAMES_EDGE_GRAB: f32 = 3.0;
/// Padding around channel names, in points.
const NAME_PADDING: f32 = 6.0;
/// Length of the axis' tick marks, in points.
const TICK: f32 = 4.0;
/// Room between axis labels beyond the widest label, in points.
const LABEL_GAP: f32 = 24.0;
/// Opacity of annotation bands.
const BAND_ALPHA: f32 = 0.2;
/// Room left of an annotation label, and between labels, in points.
const ANNOTATION_LABEL_PAD: f32 = 3.0;
const ANNOTATION_LABEL_GAP: f32 = 8.0;
/// The plate behind an annotation label: its room beside the text, in points (less than
/// [`ANNOTATION_LABEL_PAD`], so the onset's line shows), and its opacity.
const LABEL_PLATE_MARGIN: f32 = 2.0;
const LABEL_PLATE_ALPHA: f32 = 0.85;
/// The label of the status line's DC removal chip.
pub const DC_CHIP: &str = "DC";
/// A scale bar: its distance from the traces' right edge, its end caps' width, and the gap
/// to its label, in points.
const SCALE_BAR_INSET: f32 = 10.0;
const SCALE_BAR_CAP: f32 = 6.0;
const SCALE_BAR_GAP: f32 = 4.0;
/// How near an annotation's mark the pointer must be, in points, for the status line to
/// name it; 1-pixel lines are hard to hit exactly.
const POINTER_SLACK: f32 = 2.0;
/// The share of the tile cache that tiles beyond the screen may take.
const PREFETCH_SHARE: usize = 2;
/// How far a drag must reach sideways, in points, to create, move or resize an annotation.
const MIN_DRAG: f32 = 3.0;
/// How near an edge of the selected annotation, in points, a drag resizes it.
const EDGE_GRIP: f32 = 4.0;
/// How opaque the measurement's band is.
const MEASURE_ALPHA: f32 = 0.12;
/// Between the measurement's label and its line, in points.
const MEASURE_LABEL_GAP: f32 = 4.0;
/// Below the annotation labels, the measurement's label's top, in points.
const MEASURE_LABEL_TOP: f32 = 4.0;
/// Between the parts of a line of the measurement's label, beyond a space, in points.
const MEASURE_LABEL_SPACE: f32 = 6.0;
/// The room beside the text of the measurement's label, in points.
const MEASURE_LABEL_MARGIN: f32 = 4.0;
/// The radius of the dots where the measurement's ends cross its trace, in points.
const MEASURE_DOT: f32 = 2.5;
/// Width of the selected annotation's outline, in points.
const OUTLINE_WIDTH: f32 = 2.0;
/// The description new annotations start with: MNE treats descriptions starting with
/// "BAD" as bad segments.
pub const DEFAULT_DESCRIPTION: &str = "BAD";

/// What a drag in annotation mode holds of its annotation.
#[derive(Clone, Copy, Debug)]
enum Hold {
    /// All of it, to move it by as much as the pointer moved.
    Body,
    /// The edge that was at `edge`, to move it by as much as the pointer moved while the
    /// one at `anchor` stays; past it, they swap. Creating holds a new annotation's end.
    Edge { anchor: f64, edge: f64 },
}

/// An annotation being created, moved or resized by a drag.
#[derive(Clone, Debug)]
struct Drag {
    id: AnnotationId,
    /// Its content before the drag; `None` when creating it.
    before: Option<AnnotationData>,
    /// Its content but for the times the drag sets.
    base: AnnotationData,
    /// The time the drag started at.
    press: f64,
    hold: Hold,
}

impl Drag {
    /// The annotation after the pointer moved by `delta` seconds, within a recording
    /// `duration` seconds long: moved annotations keep their duration.
    fn at(&self, delta: f64, duration: f64) -> AnnotationData {
        let (onset, length) = match self.hold {
            Hold::Body => {
                let length = self.base.duration;
                let latest = (duration - length).max(0.0);
                ((self.base.onset + delta).clamp(0.0, latest), length)
            }
            Hold::Edge { anchor, edge } => {
                let edge = (edge + delta).clamp(0.0, duration);
                (anchor.min(edge), (edge - anchor).abs())
            }
        };
        AnnotationData {
            onset,
            duration: length,
            ..self.base.clone()
        }
    }
}

/// What the pointer is over.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Hover {
    /// The channel row under the pointer, over the traces or the names.
    pub channel: Option<ChannelId>,
    /// The time under the pointer, over the traces.
    pub time: Option<f64>,
}

/// Where the trace area and the channel names were at the last frame, in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub traces: Rect,
    pub names: Rect,
    /// The scrollbars for time (below the axis, unless the minimap is there) and rows
    /// (right of the traces).
    pub time_bar: Option<Rect>,
    pub row_bar: Rect,
    /// The minimap, below the axis, where it is shown.
    pub minimap: Option<Rect>,
}

/// The status line's texts, on the left and the right.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatusLine {
    /// The selected annotation's color, in a swatch before `left`, which then starts with
    /// its description.
    pub swatch: Option<AnnotationColor>,
    pub left: String,
    /// The overview's build progress, or its error.
    pub right: String,
    /// The window's start and end, a click away from Go to Time, and the same with both at
    /// the recording's end: the widest it gets at these decimals, whose width it keeps.
    pub range: String,
    pub widest_range: String,
}

/// What [`Viewer::show`]'s controls asked of the app.
#[derive(Debug, Default)]
pub struct Shown {
    /// An action the app performs, as it changes a setting: the DC chip's
    /// [`Action::ToggleDc`], the range's [`Action::GoToTime`], the format's
    /// [`Action::RecordingInfo`].
    pub action: Option<Action>,
    /// Whether a type's popup asked for the Channel Types window.
    pub channel_types: bool,
    /// The type settings, if a type's popup changed them (they apply to the viewer at
    /// once; the app keeps them).
    pub types: Option<TypeSettings>,
    /// The minimap's height, if it was resized (it applies to the viewer at once; the app
    /// keeps it).
    pub minimap_height: Option<f32>,
    /// Whether the minimap shows signals, if its menu changed it (as the height).
    pub minimap_signals: Option<bool>,
}

pub struct Viewer {
    document: Document,
    summary: Summary,
    /// Always `Some` until dropped; see [`Drop`].
    service: Option<TileService>,
    view: ViewState,
    /// Names this recording for the renderer.
    serial: u64,
    hover: Hover,
    status: StatusLine,
    /// Width of the trace area in physical pixels at the last frame.
    width_px: u32,
    /// Widest channel name, measured once per font size (and again after
    /// [`Viewer::forget_text_widths`]).
    names_width: Option<(f32, f32)>,
    /// The names column's width as dragged, in points, while the recording is open; `None`
    /// fits the names (up to [`NAMES_MAX_EMS`]).
    names_dragged: Option<f32>,
    /// The width channel names get in the names column, set each frame.
    name_room: f32,
    layout: Option<Layout>,
    /// Tiles the last frame wanted that weren't cached.
    missing: usize,
    legend: Legend,
    /// The annotation marks of the last frame.
    marks: Marks,
    /// The annotations under the pointer, as the status line shows them.
    pointed: String,
    /// The stim channels' annotations, for recordings that have stim channels.
    stim: Option<StimAnnotations>,
    /// Whether traces are anti-aliased.
    antialias: bool,
    /// Whether scale bars are drawn, and those of the last frame with where they were.
    show_scale_bars: bool,
    scale_bars: Vec<(ScaleBar, Rect)>,
    /// Whether the minimap is shown, its height in points as set, whether it shows signals,
    /// and its state.
    show_minimap: bool,
    minimap_height: f32,
    minimap_signals: bool,
    minimap: Minimap,
    /// The time under the pointer over the minimap, the channel of its row (with signals),
    /// and the annotations there as the status line shows them.
    minimap_pointer: Option<(f64, Option<ChannelId>, String)>,
    /// Whether [`Action::AutoScale`] waits for a complete frame.
    auto_scale: bool,
    /// What was set per channel type (colors here; the view has the rest).
    types: TypeSettings,
    /// Whether annotation mode is on: dragging creates annotations, clicking selects.
    editing: bool,
    /// The selected annotation, only in annotation mode; never a stim annotation or a
    /// hidden one.
    selected: Option<AnnotationId>,
    /// Where the selection's outline was drawn at the last frame, in points.
    outline: Option<Rect>,
    /// The annotation a drag is creating, moving or resizing.
    drag: Option<Drag>,
    /// The description of annotations created from now on.
    description: String,
    /// The selected annotation's description as the field edits it.
    draft: Option<String>,
    /// Whether the description field had the keyboard at the last frame.
    typing: bool,
    history: History,
    /// Whether Merge Annotations has anything to merge, at the store's revision and the
    /// legend's shown revision it was found at.
    can_merge: Cell<Option<(u64, u64, bool)>>,
    /// Where the annotations were saved last.
    saved_to: Option<std::path::PathBuf>,
    /// Where the channels were saved or loaded from last.
    channels_path: Option<std::path::PathBuf>,
    /// The measurement on screen, and the one a ⇧-drag is making (shown once it reaches
    /// [`MIN_DRAG`]).
    measurement: Option<Measurement>,
    measuring: Option<Measurement>,
    /// The measurement's label's width at the last frame, to place it beside the band.
    measure_label_width: f32,
    /// What the status line suggests while ⇧ is held over the traces.
    shift_hint: Option<String>,
    /// While ⇧ is held over the traces, the channel and time a ⇧-drag would start at.
    measure_start: Option<(ChannelId, f64)>,
}

impl Viewer {
    /// A recording on screen with its tile service, showing `default_span` seconds at
    /// first, with stim annotations by `stim`.
    pub fn new(
        document: Document,
        service: TileService,
        default_span: f64,
        stim: EventOptions,
    ) -> Viewer {
        static SERIAL: AtomicU64 = AtomicU64::new(1);
        let has_stim = document
            .recording
            .channels()
            .iter()
            .any(|c| c.kind == ChannelKind::STIM && c.events);
        Viewer {
            stim: has_stim.then(|| StimAnnotations::new(stim)),
            summary: document.summary(),
            legend: Legend::new(
                &document.annotations,
                annotations::groups_streams(&document.recording, &document.annotations),
            ),
            view: ViewState::with_default_span(&document.recording, default_span),
            document,
            service: Some(service),
            serial: SERIAL.fetch_add(1, Ordering::Relaxed),
            hover: Hover::default(),
            status: StatusLine::default(),
            width_px: 0,
            names_width: None,
            names_dragged: None,
            name_room: 0.0,
            layout: None,
            missing: 0,
            marks: Marks::default(),
            pointed: String::new(),
            antialias: false,
            show_scale_bars: true,
            scale_bars: Vec::new(),
            show_minimap: false,
            minimap_height: minimap::DEFAULT_HEIGHT,
            minimap_signals: true,
            minimap: Minimap::default(),
            minimap_pointer: None,
            auto_scale: false,
            types: TypeSettings::default(),
            editing: false,
            selected: None,
            outline: None,
            drag: None,
            description: DEFAULT_DESCRIPTION.to_owned(),
            draft: None,
            typing: false,
            saved_to: None,
            channels_path: None,
            measurement: None,
            measuring: None,
            measure_label_width: 0.0,
            shift_hint: None,
            measure_start: None,
            history: History::default(),
            can_merge: Cell::new(None),
        }
    }

    pub fn document(&self) -> &Document {
        &self.document
    }

    /// What the sidebar and the Recording Info window show of the recording.
    pub fn summary(&self) -> &Summary {
        &self.summary
    }

    pub fn view(&self) -> &ViewState {
        &self.view
    }

    /// Moves the window to start at `seconds` from the start of the recording, as far as
    /// the recording goes.
    pub fn go_to(&mut self, seconds: f64) {
        self.view.pan(seconds - self.view.start());
    }

    pub fn hover(&self) -> Hover {
        self.hover
    }

    /// The annotations' descriptions, colors and whether they are shown.
    pub fn legend(&self) -> &Legend {
        &self.legend
    }

    /// The annotation marks of the last frame, in physical pixels of the trace area.
    pub fn marks(&self) -> &Marks {
        &self.marks
    }

    pub fn status(&self) -> &StatusLine {
        &self.status
    }

    /// Whether annotation mode is on.
    pub fn editing(&self) -> bool {
        self.editing
    }

    /// The selected annotation.
    pub fn selected(&self) -> Option<AnnotationId> {
        self.selected
    }

    /// Where the selected annotation's outline was drawn at the last frame, in points.
    pub fn outline(&self) -> Option<Rect> {
        self.outline
    }

    /// The description new annotations get.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Whether the annotations differ from the opened or last saved ones.
    pub fn edited(&self) -> bool {
        self.history.edited()
    }

    /// Writes the file's and the viewer's annotations as an events table, or with
    /// `shown_only` those of the descriptions shown, stim annotations included (see
    /// [`kymo_core::annotation_file`]).
    pub fn write_annotations(
        &self,
        out: impl std::io::Write,
        shown_only: bool,
    ) -> std::io::Result<()> {
        let annotations = self
            .document
            .annotations
            .iter()
            .map(|a| &a.data)
            .filter(|a| {
                if shown_only {
                    self.legend.shows(a)
                } else {
                    a.origin != Origin::Stim
                }
            });
        let context = annotation_file::Context::of(&self.document.recording);
        annotation_file::write_table(out, &context, annotations)
    }

    /// Marks the annotations as saved, to `path`: the title loses "(edited)", and the next
    /// save suggests it.
    pub fn mark_saved(&mut self, path: std::path::PathBuf) {
        self.history.mark_saved();
        self.saved_to = Some(path);
    }

    /// Where to suggest saving the annotations: where they were saved last, or
    /// `<recording>-annot.tsv` next to the recording.
    pub fn save_path(&self) -> std::path::PathBuf {
        self.saved_to.clone().unwrap_or_else(|| {
            let path = &self.document.path;
            let stem = path
                .file_stem()
                .map_or("annotations".into(), |s| s.to_string_lossy());
            path.with_file_name(format!("{stem}-annot.tsv"))
        })
    }

    /// Writes every channel's type (as shown) and bad mark as a BIDS `channels.tsv` (see
    /// [`kymo_core::channel_file`]).
    pub fn write_channels(&self, out: impl std::io::Write) -> std::io::Result<()> {
        let recording = &self.document.recording;
        let kinds: Vec<ChannelKind> = (0..recording.channels().len())
            .map(|i| self.view.kind_of(ChannelId(i as u32)).clone())
            .collect();
        let entries = channel_file::entries(recording, &kinds, |c| self.view.is_bad(c));
        channel_file::write(out, entries)
    }

    /// Where to suggest saving the channels: where they were saved or loaded from last, or
    /// next to the recording, named as BIDS names it (`sub-01_task-rest_eeg.edf` gives
    /// `sub-01_task-rest_channels.tsv`, any other `<recording>_channels.tsv`).
    pub fn channels_path(&self) -> std::path::PathBuf {
        if let Some(path) = &self.channels_path {
            return path.clone();
        }
        let path = &self.document.path;
        let stem = path
            .file_stem()
            .map_or("recording".into(), |s| s.to_string_lossy());
        let base = match stem.rsplit_once('_') {
            Some((entities, suffix))
                if stem.starts_with("sub-")
                    && BIDS_SUFFIXES.contains(&suffix.to_ascii_lowercase().as_str()) =>
            {
                entities
            }
            _ => &stem,
        };
        path.with_file_name(format!("{base}_channels.tsv"))
    }

    /// Remembers `path` as where the channels were saved or loaded from.
    pub fn set_channels_path(&mut self, path: std::path::PathBuf) {
        self.channels_path = Some(path);
    }

    /// Takes the types and bad marks of a loaded `channels.tsv` (see
    /// [`ViewState::apply_kinds`]); how many changes to stim were left out.
    pub fn load_channels(&mut self, loaded: &channel_file::Loaded) -> usize {
        let (_, refused) = self.view.apply_kinds(&loaded.kinds);
        for &(channel, bad) in &loaded.bad {
            self.view.set_bad(channel, bad);
        }
        refused
    }

    /// Adds `annotations` (read from a file) as one step, as they are: no merging.
    pub fn add_annotations(&mut self, annotations: Vec<AnnotationData>) {
        let store = &mut self.document.annotations;
        let changes = annotations
            .into_iter()
            .map(|a| (store.new_id(), Some(a)))
            .collect();
        // Read files hold valid annotations only.
        let _ = self.history.edit(store, changes);
        self.legend.sync(&self.document.annotations);
    }

    /// The annotation edits that can be undone and redone.
    pub fn history(&self) -> &History {
        &self.history
    }

    /// Whether [`Action::MergeAnnotations`] has anything to merge; found again only after
    /// the annotations changed or a description was shown or hidden.
    pub fn can_merge(&self) -> bool {
        let key = (
            self.document.annotations.revision(),
            self.legend.shown_revision(),
        );
        if let Some((store, legend, can)) = self.can_merge.get()
            && (store, legend) == key
        {
            return can;
        }
        let can = !self.merged_shown().is_empty();
        self.can_merge.set(Some((key.0, key.1, can)));
        can
    }

    /// What merging the shown annotations would do (see [`annotations::merge_shown`]).
    fn merged_shown(&self) -> Vec<annotations::Merged> {
        annotations::merge_shown(&self.document.annotations, self.legend.by_stream(), |a| {
            self.legend.shows(a)
        })
    }

    /// Merges the shown annotations of a kind that overlap or touch, as one step. A
    /// selected annotation merged into another leaves that one selected.
    fn merge_shown(&mut self) {
        let merged = self.merged_shown();
        if merged.is_empty() {
            return;
        }
        if let Some(selected) = self.selected
            && let Some(m) = merged.iter().find(|m| m.removed.contains(&selected))
        {
            self.selected = Some(m.kept);
        }
        let changes = merged
            .into_iter()
            .flat_map(|m| {
                let removed = m.removed.into_iter().map(|id| (id, None));
                std::iter::once((m.kept, Some(m.data))).chain(removed)
            })
            .collect();
        // Merged annotations are valid: they span annotations that are.
        let _ = self.history.edit(&mut self.document.annotations, changes);
        self.legend.sync(&self.document.annotations);
    }

    /// Whether the last frame had every tile it wanted, none drawn from a coarser one.
    pub fn complete(&self) -> bool {
        self.layout.is_some() && self.missing == 0
    }

    /// The layout of the last frame, once there was one with room for traces.
    pub fn layout(&self) -> Option<Layout> {
        self.layout
    }

    /// The tile service's cache, for its budget and size.
    pub fn cache(&self) -> &kymo_core::tile::TileCache {
        self.service().cache()
    }

    /// Whether the recording has stim channels, whose codes the rule annotates.
    pub fn has_stim(&self) -> bool {
        self.stim.is_some()
    }

    /// Annotates the stim codes by `options` from the next frame on.
    pub fn set_stim_options(&mut self, options: EventOptions) {
        if let Some(stim) = &mut self.stim {
            stim.set_options(options);
        }
    }

    /// Whether traces are anti-aliased from the next frame on.
    pub fn set_antialias(&mut self, on: bool) {
        self.antialias = on;
    }

    /// Whether traces are centered on their window means (DC removal), from now on.
    pub fn set_remove_dc(&mut self, on: bool) {
        self.view.set_dc_removed(on);
    }

    /// Whether scale bars are drawn from the next frame on.
    pub fn set_scale_bars(&mut self, on: bool) {
        self.show_scale_bars = on;
    }

    /// Whether the minimap is shown in place of the time scrollbar, its height in points
    /// (before the window limits it), and whether it shows signals, from the next frame on.
    pub fn set_minimap(&mut self, on: bool, height: f32, signals: bool) {
        self.show_minimap = on;
        self.minimap_height = height;
        self.minimap_signals = signals;
    }

    /// The minimap's state.
    pub fn minimap(&self) -> &Minimap {
        &self.minimap
    }

    /// The scale bars of the last frame, each with the rectangle of its bar (not its
    /// label), in points.
    pub fn scale_bars(&self) -> &[(ScaleBar, Rect)] {
        &self.scale_bars
    }

    /// Draws channel types in the colors picked for them, the others in their defaults.
    /// Which types DC removal centers and which are scaled each channel on its own, from now
    /// on.
    pub fn set_type_settings(&mut self, types: TypeSettings) {
        self.view.set_type_settings(types.clone());
        self.types = types;
    }

    /// Seconds [`Action::DefaultWindow`] shows from now on.
    pub fn set_default_span(&mut self, seconds: f64) {
        self.view.set_default_span(seconds);
    }

    /// The file the recording's pyramids live in, if they live in one.
    pub fn sidecar_path(&self) -> Option<std::path::PathBuf> {
        self.service().sidecar_path()
    }

    fn service(&self) -> &TileService {
        live(&self.service)
    }

    /// The physical value drawn at the middle of `channel`'s row: its mean over the window
    /// where DC removal centers it (the last one computed, 0 until there is one; see
    /// [`ViewState::dc_applies`]), otherwise 0. Always 0 for stim channels.
    pub fn baseline(&self, channel: ChannelId) -> f64 {
        if !self.view.dc_applies(channel) {
            return 0.0;
        }
        self.service().mean(channel).map_or(0.0, |m| m.value)
    }

    /// Applies a view action.
    /// [`Action::AutoScale`] applies at the first frame that has all its tiles, as coarser
    /// stand-ins would draw too wide an envelope; any other scale action cancels it.
    pub fn apply(&mut self, action: Action) -> bool {
        match action {
            Action::AutoScale => {
                self.auto_scale = true;
                return true;
            }
            Action::ToggleEditing => {
                self.finish_drag();
                self.editing = !self.editing;
                self.selected = None;
                return true;
            }
            Action::Undo => {
                let changed = self.history.undo(&mut self.document.annotations);
                self.after_history(changed);
                return true;
            }
            Action::Redo => {
                let changed = self.history.redo(&mut self.document.annotations);
                self.after_history(changed);
                return true;
            }
            Action::MergeAnnotations => {
                self.merge_shown();
                return true;
            }
            Action::DeleteAnnotation => {
                if let Some(id) = self.selected.take() {
                    // Fails only for an annotation that is gone, leaving nothing to do.
                    let _ = self
                        .history
                        .edit(&mut self.document.annotations, vec![(id, None)]);
                    self.legend.sync(&self.document.annotations);
                }
                return true;
            }
            // Esc first takes back a drag, then removes the measurement, then deselects the
            // annotation, then the channels.
            Action::Deselect => {
                if !self.cancel_drag()
                    && !self.clear_measurement()
                    && self.selected.take().is_none()
                {
                    self.view.clear_selection();
                }
                return true;
            }
            Action::ScaleUp
            | Action::ScaleDown
            | Action::ScaleSelectedUp
            | Action::ScaleSelectedDown
            | Action::DefaultScale => self.auto_scale = false,
            _ => {}
        }
        self.view.apply(action)
    }

    /// After an undo or redo that changed `changed` (`None` if there was nothing to do):
    /// the legend follows, and in annotation mode the first of them still there is
    /// selected.
    fn after_history(&mut self, changed: Option<Vec<AnnotationId>>) {
        let Some(changed) = changed else {
            return;
        };
        self.legend.sync(&self.document.annotations);
        self.selected = None;
        if self.editing {
            self.selected = changed.into_iter().find(|&id| self.selectable(id));
        }
    }

    /// Whether the selected annotation `id` can stay selected: there, and shown. Stim
    /// annotations never get selected (see [`annotations::pick`]), and the history never
    /// holds them.
    fn selectable(&self, id: AnnotationId) -> bool {
        self.document
            .annotations
            .get(id)
            .is_some_and(|a| self.legend.shows(&a.data))
    }

    /// Whether the description field had the keyboard at the last frame: an Esc now left
    /// it, and is not for the traces.
    pub fn was_typing(&self) -> bool {
        self.typing
    }

    /// Whether a drag is creating, moving or resizing an annotation.
    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// The measurement on screen, while a ⇧-drag makes it too.
    pub fn measurement(&self) -> Option<Measurement> {
        self.measurement
    }

    /// Whether a measurement is on screen or being made, for Esc to remove.
    pub fn measured(&self) -> bool {
        self.measurement.is_some() || self.measuring.is_some()
    }

    /// Removes the measurement, ending a ⇧-drag making one; returns whether there was one.
    fn clear_measurement(&mut self) -> bool {
        let measured = self.measured();
        self.measurement = None;
        self.measuring = None;
        measured
    }

    /// Ends a drag, recording it (see [`record`](Self::record)).
    fn finish_drag(&mut self) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        self.record(drag.id, drag.before);
    }

    /// Records the edit of annotation `id`, already in the store, from `before`: merges it
    /// with those of its kind it overlaps or touches (see [`annotations::merge`]), records
    /// all as one step, and selects it. An edit that changed nothing records nothing and
    /// merges nothing.
    fn record(&mut self, edited: AnnotationId, before: Option<AnnotationData>) {
        let store = &mut self.document.annotations;
        let after = store.get(edited).map(|a| a.data.clone());
        if after.is_none() || after == before {
            return;
        }
        let mut step = vec![Change {
            id: edited,
            before,
            after,
        }];
        for (id, data) in annotations::merge(store, edited, self.legend.by_stream()) {
            let before = store.get(id).map(|a| a.data.clone());
            history::set(store, id, data.clone()).expect("merged annotations are valid");
            step.push(Change {
                id,
                before,
                after: data,
            });
        }
        self.history.push(step);
        self.legend.sync(store);
        self.selected = Some(edited);
    }

    /// When the description field gives up the keyboard (Enter, a press in the traces,
    /// which selects only on release), the selected annotation gets what was typed, unless
    /// Esc took the keyboard (egui does that before any widget sees the key); while it
    /// doesn't have it, it shows the selected annotation's description.
    fn sync_draft(&mut self, ctx: &egui::Context) {
        let typing = ctx.memory(|m| m.has_focus(description_field_id()));
        let stopped = std::mem::replace(&mut self.typing, typing) && !typing;
        if typing {
            return;
        }
        let escaped = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        let typed = self.draft.take();
        if stopped
            && !escaped
            && let (Some(typed), Some(id)) = (typed, self.selected)
        {
            self.relabel(id, typed);
        }
        self.draft = self
            .selected
            .filter(|_| self.editing)
            .and_then(|id| self.document.annotations.get(id))
            .map(|a| a.data.description.clone());
    }

    /// Gives annotation `id` the description `description`, unless it is empty, as one
    /// step; it merges as any edit does, and its description is shown.
    fn relabel(&mut self, id: AnnotationId, description: String) {
        let Some(a) = self.document.annotations.get(id) else {
            return;
        };
        if description.is_empty() || a.data.description == description {
            return;
        }
        let before = a.data.clone();
        let after = AnnotationData {
            description: description.clone(),
            ..before.clone()
        };
        let stream = self.legend.stream_of(&after);
        history::set(&mut self.document.annotations, id, Some(after))
            .expect("a new description keeps it valid");
        self.legend.sync(&self.document.annotations);
        self.legend.set_shown(stream, &description, true);
        self.record(id, Some(before));
    }

    /// Puts the annotation of a drag back as it was before; returns whether there was one.
    fn cancel_drag(&mut self) -> bool {
        let Some(drag) = self.drag.take() else {
            return false;
        };
        let store = &mut self.document.annotations;
        history::set(store, drag.id, drag.before).expect("its state before the drag is valid");
        self.legend.sync(store);
        true
    }

    /// What a press `x` points from the trace area's left edge grabs of the selected
    /// annotation, on an area `width` points wide with `ppp` pixels per point.
    fn grip_at(&self, x: f64, width: f64, ppp: f32) -> Option<(AnnotationData, annotations::Grip)> {
        let a = &self.document.annotations.get(self.selected?)?.data;
        let left = self.view.x_at(a.onset, width);
        let right = self.view.x_at(a.end(), width);
        let grip = annotations::grip(left, right, x, f64::from(EDGE_GRIP), f64::from(1.0 / ppp))?;
        Some((a.clone(), grip))
    }

    /// Shows the recording in `ui`, with the sidebar if `sidebar`, its annotations in the
    /// colors picked in `annotation_colors` (which the legend changes). Returns what its
    /// controls asked of the app (see [`Shown`]).
    #[must_use]
    pub fn show(
        &mut self,
        ui: &mut Ui,
        gpu: &Gpu,
        keymap: &Keymap,
        sidebar: bool,
        annotation_colors: &mut AnnotationColors,
    ) -> Shown {
        self.sync_stim(ui.ctx());
        self.legend.apply_colors(annotation_colors);
        if let Some(id) = self.selected
            && !self.selectable(id)
        {
            self.selected = None;
        }
        let mut relabel = None;
        let mut toggle_dc = false;
        let mut go_to = false;
        let mut info = false;
        let mut channel_types = false;
        let types_before = self.types.clone();
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                if let Some(color) = self.status.swatch {
                    let side = ui.text_style_height(&TextStyle::Body) * 0.7;
                    let (rect, _) = ui.allocate_exact_size(vec2(side, side), Sense::hover());
                    ui.painter()
                        .rect_filled(rect, 2.0, annotation_color(color, ui.visuals()));
                }
                ui.label(&self.status.left);
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    // What changes the traces, always in sight and a click away.
                    let shortcut = Keymap::chord(Action::ToggleDc)
                        .map(|c| format!(" ({})", c.display(keymap.platform())))
                        .unwrap_or_default();
                    let dc =
                        status::chip(ui, self.view.dc_removed(), DC_CHIP).on_hover_text(format!(
                            "Remove DC{shortcut}: center each trace on its mean over the window"
                        ));
                    toggle_dc = dc.clicked();
                    go_to = self.range_button(ui, keymap).clicked();
                    ui.label(&self.status.right);
                });
            });
        });
        if sidebar {
            egui::Panel::left("header")
                .resizable(true)
                .default_size(300.0)
                .min_size(SIDEBAR_MIN_WIDTH)
                .max_size((ui.available_width() * SIDEBAR_MAX_FRACTION).max(SIDEBAR_MIN_WIDTH))
                .show(ui, |ui| {
                    let header = within_width(ui, |ui| {
                        header_panel(
                            ui,
                            HeaderSource {
                                recording: self.serial,
                                document: &self.document,
                                summary: &self.summary,
                                platform: keymap.platform(),
                            },
                            &mut self.view,
                            &mut self.types,
                            AnnotationPanel {
                                legend: &mut self.legend,
                                colors: annotation_colors,
                                field: self.editing.then(|| match &mut self.draft {
                                    Some(draft) => DescriptionField::Selected {
                                        current: self
                                            .selected
                                            .and_then(|id| self.document.annotations.get(id))
                                            .map_or("", |a| &a.data.description),
                                        draft,
                                    },
                                    None => DescriptionField::New(&mut self.description),
                                }),
                            },
                        )
                    });
                    relabel = header.relabel;
                    info = header.info;
                    channel_types = header.all_types;
                });
            if let Some(description) = relabel
                && let Some(id) = self.selected
            {
                self.relabel(id, description);
            }
        }
        let minimap = egui::CentralPanel::default()
            .show(ui, |ui| self.trace_region(ui, gpu, keymap))
            .inner;
        self.sync_draft(ui.ctx());

        // The status line was laid out before the traces moved; show the new texts in the
        // next frame, and only ask for one if they changed.
        let status = self.status_line();
        if status != self.status {
            self.status = status;
            ui.ctx().request_repaint();
        }
        let action = if go_to {
            Some(Action::GoToTime)
        } else if info {
            Some(Action::RecordingInfo)
        } else if minimap.hide {
            Some(Action::ToggleMinimap)
        } else {
            toggle_dc.then_some(Action::ToggleDc)
        };
        let types = (self.types != types_before).then(|| {
            self.view.set_type_settings(self.types.clone());
            self.types.clone()
        });
        Shown {
            action,
            channel_types,
            types,
            minimap_height: minimap.height,
            minimap_signals: minimap.signals,
        }
    }

    /// The window's start and end in the status line: a [`crate::ui::text_button`], as a
    /// channel's Bad is, which opens Go to Time. It keeps
    /// the width of the widest range at its decimals, so scrolling doesn't shift its
    /// neighbors (the digits are all one width).
    fn range_button(&self, ui: &mut Ui, keymap: &Keymap) -> egui::Response {
        let font = TextStyle::Body.resolve(ui.style());
        let color = ui.visuals().text_color();
        let widest = ui
            .painter()
            .layout_no_wrap(self.status.widest_range.clone(), font, color)
            .size()
            .x;
        let width = widest + 2.0 * ui.spacing().button_padding.x;
        let shortcut = Keymap::chord(Action::GoToTime)
            .map(|c| format!(" ({})", c.display(keymap.platform())))
            .unwrap_or_default();
        let span = format_duration(self.view.span());
        ui.add(crate::ui::text_button(&self.status.range, None).min_size(vec2(width, 0.0)))
            .on_hover_text(format!("{span} shown. Go to Time…{shortcut}"))
    }

    /// Brings the stim annotations up to the build and the rule, and the legend with them.
    fn sync_stim(&mut self, ctx: &egui::Context) {
        let Some(stim) = &mut self.stim else {
            return;
        };
        let service = live(&self.service);
        let rec = &self.document.recording;
        let view = &self.view;
        // A stim channel given another type has no annotations, until it is stim again.
        let changes = || -> Vec<kymo_core::stim::StimChanges> {
            (0..rec.grids().len() as u32)
                .filter_map(|grid| service.pyramid(GridId(grid)))
                .flat_map(|pyramid| pyramid.stim_changes(rec))
                .filter(|changes| view.kind_of(changes.channel).is_stim())
                .collect()
        };
        let progress = (service.progress().done, view.kinds_revision());
        let now = ctx.input(|i| i.time);
        match stim.update(&mut self.document.annotations, rec, progress, now, changes) {
            Update::Changed => self.legend.sync(&self.document.annotations),
            Update::Wait(seconds) => ctx.request_repaint_after(Duration::from_secs_f64(seconds)),
            Update::Unchanged => {}
        }
    }

    fn status_line(&self) -> StatusLine {
        let service = self.service();
        let rec = &self.document.recording;
        // Over the minimap, its time, its row's channel without a reading, and its
        // annotations.
        let (time, channel, row, pointed) = match &self.minimap_pointer {
            Some((time, row, under)) => (Some(*time), None, *row, under.clone()),
            None => (
                self.hover.time,
                self.hover.channel,
                None,
                self.pointed.clone(),
            ),
        };
        let left = time
            .map(|time| {
                let rate = rec.sfreq();
                let row = row.map(|id| (rec.channel(id).name.as_str(), None));
                let channel = channel.map(|id| {
                    let info = rec.channel(id);
                    let view = self.view.tile_view(self.width_px.max(1));
                    let (unit, factor) = reading_unit(rec, &self.view, id);
                    let reading = readout::read(rec, service.cache(), &view, id, time).map(|r| {
                        let r = r.relative_to(self.baseline(id)).scaled(factor);
                        status::reading_text(r, &unit)
                    });
                    (info.name.as_str(), reading)
                });
                status::pointer_text(
                    time,
                    status::time_decimals(rate),
                    Clock::for_end(self.view.duration()),
                    channel.or(row),
                )
            })
            .unwrap_or_default();
        let selected = self
            .selected
            .and_then(|id| self.document.annotations.get(id));
        let swatch = selected
            .and_then(|a| self.legend.of(&a.data))
            .map(|e| e.color);
        let mode = self.editing.then(|| match selected {
            Some(a) => status::selection_text(
                &a.data.description,
                a.data.onset,
                a.data.duration,
                status::time_decimals(rec.sfreq()),
                Clock::for_end(self.view.duration()),
            ),
            None => "Editing annotations".to_owned(),
        });
        let hint = self.shift_hint.clone().unwrap_or_default();
        // The measurement in full while it is made or the pointer is over its band, in
        // place of the reading and the annotations under the pointer.
        let measured = self.measurement.and_then(|m| {
            let span = m.span(rec);
            let over = |t: f64| span.start <= t && t <= span.end;
            (self.measuring.is_some() || self.hover.time.is_some_and(over))
                .then(|| self.measurement_details(m, &span))
        });
        let (left, pointed) = match measured {
            Some(details) => (details, String::new()),
            None => (left, pointed),
        };
        let left = [mode.unwrap_or_default(), hint, left, pointed]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        // The overview's build while it runs, or why it stopped; nothing once it is done.
        let right = status::build_text(&service.progress()).unwrap_or_default();
        let time = self.view.time();
        let decimals = status::range_decimals(self.view.span());
        let clock = Clock::for_end(self.view.duration());
        let end = self.view.duration();
        StatusLine {
            swatch,
            left,
            right,
            range: status::range_text(time.start, time.end, decimals, clock),
            widest_range: status::range_text(end, end, decimals, clock),
        }
    }

    /// Channel names, traces, the time axis, both scrollbars or the row scrollbar and the
    /// minimap. Returns what the minimap asked for, its height already limited.
    fn trace_region(&mut self, ui: &mut Ui, gpu: &Gpu, keymap: &Keymap) -> minimap::Shown {
        let full = ui.available_rect_before_wrap();
        let id = egui::Id::unique("kymo traces");
        let small = TextStyle::Small.resolve(ui.style());
        let body = TextStyle::Body.resolve(ui.style());
        let bar = scrollbar::THICKNESS;
        let fitted =
            self.names_width(ui, &body).min(NAMES_MAX_EMS * body.size) + 2.0 * NAME_PADDING;
        let names_width = match self.names_dragged {
            Some(width) => width
                .max((NAMES_DRAG_MIN_EMS * body.size + 2.0 * NAME_PADDING).min(fitted))
                .min(full.width() * NAMES_DRAG_MAX_FRACTION),
            None => fitted.min(full.width() * NAMES_MAX_FRACTION),
        };
        // The room for a name: from the column's width before the trace area's edge is
        // rounded to a pixel, which would otherwise shorten the widest names in some frames
        // and not in others as the edge moves (with the sidebar's).
        self.name_room = names_width - 2.0 * NAME_PADDING;
        let axis_height = TICK + ui.fonts_mut(|f| f.row_height(&small)) + 4.0;

        // Trace area on whole physical pixels, so its columns are the renderer's.
        let ppp = ui.pixels_per_point();
        let px = |v: f32| (v * ppp).round();
        let left = px(full.left() + names_width);
        let right = px(full.right() - bar - 2.0).max(left);
        let top = px(full.top());
        let minimap_height = self
            .show_minimap
            .then(|| minimap::height(self.minimap_height, full.height()));
        let below = minimap_height.unwrap_or(bar);
        let bottom = px(full.bottom() - below - axis_height).max(top);
        let traces = Rect::from_min_max(pos2(left, top) / ppp, pos2(right, bottom) / ppp);
        let size_px = [(right - left) as u32, (bottom - top) as u32];
        let names = Rect::from_min_max(pos2(full.left(), traces.top()), traces.left_bottom());
        let axis_rect = Rect::from_min_max(
            traces.left_bottom(),
            pos2(traces.right(), traces.bottom() + axis_height),
        );
        let vbar = Rect::from_min_max(
            pos2(full.right() - bar, traces.top()),
            pos2(full.right(), traces.bottom()),
        );
        // On whole physical pixels, as the minimap's window is.
        let below = Rect::from_min_max(
            pos2(traces.left(), px(full.bottom() - below) / ppp),
            pos2(traces.right(), (full.bottom() * ppp).floor() / ppp),
        );
        let (hbar, minimap_rect) = match minimap_height {
            Some(_) => (None, Some(below)),
            None => (Some(below), None),
        };
        ui.advance_cursor_after_rect(full);
        if size_px[0] == 0 || size_px[1] == 0 {
            self.hover = Hover::default();
            self.pointed.clear();
            self.minimap_pointer = None;
            self.scale_bars.clear();
            return minimap::Shown::default();
        }
        self.layout = Some(Layout {
            traces,
            names,
            time_bar: hbar,
            row_bar: vbar,
            minimap: minimap_rect,
        });
        let area = Area {
            width: f64::from(traces.width()),
            height: f64::from(traces.height()),
        };

        self.handle_input(ui, id, keymap, traces, names, axis_rect, size_px[0]);
        self.names_edge(ui, id, names_width, traces);
        if let Some(hbar) = hbar
            && let Some(start) = scrollbar::show(
                ui,
                id.with("time"),
                hbar,
                false,
                self.view.duration(),
                self.view.time(),
            )
        {
            self.view.pan(start - self.view.start());
        }
        let mut asked = minimap::Shown::default();
        self.minimap_pointer = None;
        if let Some(rect) = minimap_rect {
            let service = live(&self.service);
            asked = self.minimap.show(
                ui,
                id.with("minimap"),
                rect,
                size_px[0],
                &minimap::Input {
                    store: &self.document.annotations,
                    legend: &self.legend,
                    duration: self.view.duration(),
                    window: self.view.time(),
                    signals: self.minimap_signals.then_some(minimap::Signals {
                        view: &self.view,
                        service,
                    }),
                },
            );
            if let Some(start) = asked.start {
                self.view.pan(start - self.view.start());
            }
            if let Some(top) = asked.top {
                self.view.set_top(top);
            }
            if let Some(signals) = asked.signals {
                self.minimap_signals = signals;
            }
            asked.height = asked.height.map(|h| minimap::height(h, full.height()));
            if let Some(height) = asked.height {
                self.minimap_height = height;
            }
            self.minimap_pointer = asked.pointer.take().map(|p| {
                (
                    p.time,
                    p.channel,
                    status::annotations_text(p.under.iter().map(String::as_str)),
                )
            });
        }
        let rows = self.view.rows() as f64;
        if let Some(top) = scrollbar::show(
            ui,
            id.with("rows"),
            vbar,
            true,
            self.view.row_count() as f64,
            self.view.top()..self.view.top() + rows,
        ) {
            self.view.set_top(top);
        }
        self.hover = self.hover_at(ui.input(|i| i.pointer.hover_pos()), traces, names);
        self.pointed = match self.hover.time {
            Some(time) => {
                let x = self.view.x_at(time, f64::from(size_px[0]));
                let slack = (POINTER_SLACK * ppp).round() as u32;
                let under = annotations::under(
                    &self.view,
                    &self.document.annotations,
                    &self.legend,
                    size_px[0],
                    x,
                    slack,
                    self.hover.channel,
                )
                .into_iter()
                .map(|a| a.data.description.as_str());
                status::annotations_text(under)
            }
            None => String::new(),
        };

        // Tiles: ask for what's missing, draw what's cached.
        self.width_px = size_px[0];
        let rec = &self.document.recording;
        let service = live(&self.service);
        let budget = service.cache().budget() / PREFETCH_SHARE;
        if let Some((generation, keys)) = self.view.request(rec, size_px[0], budget) {
            service.submit(generation, keys);
        }
        if let Some(windows) = self.view.dc_request(rec) {
            service.request_means(windows);
        }
        let visuals = ui.visuals();
        let painter = ui.painter_at(traces.union(names).union(axis_rect));
        painter.rect_filled(traces, 0.0, visuals.extreme_bg_color);
        let labels = ui.painter_at(Rect::from_x_y_ranges(full.x_range(), axis_rect.y_range()));
        let grid_color = visuals.widgets.noninteractive.bg_stroke.color;
        let label_color = visuals.weak_text_color();
        let widest = ui.fonts_mut(|f| {
            f.layout_no_wrap("00:00:00.00".into(), small.clone(), label_color)
                .size()
                .x
        });
        for tick in axis::ticks(self.view.time(), area.width, f64::from(widest + LABEL_GAP)) {
            let x = traces.left() + self.view.x_at(tick.time, area.width) as f32;
            let x = (x * ppp).round() / ppp + 0.5 / ppp;
            painter.vline(x, traces.y_range(), (1.0 / ppp, grid_color));
            painter.vline(
                x,
                axis_rect.top()..=axis_rect.top() + TICK,
                (1.0, label_color),
            );
            // Centered on the tick; at the ends, shifted inward to stay whole.
            let galley = painter.layout_no_wrap(tick.label, small.clone(), label_color);
            let width = galley.size().x;
            let left =
                (x - width / 2.0).clamp(full.left(), (full.right() - width).max(full.left()));
            labels.galley(
                pos2(left, axis_rect.top() + TICK + 1.0),
                galley,
                label_color,
            );
        }

        // Annotations behind the traces; their labels go on top, below.
        self.marks = annotations::marks(
            &self.view,
            &self.document.annotations,
            &self.legend,
            size_px[0],
            |text| {
                let width = ui.fonts_mut(|f| {
                    f.layout_no_wrap(text.to_owned(), small.clone(), Color32::PLACEHOLDER)
                        .size()
                        .x
                });
                f64::from((width + ANNOTATION_LABEL_PAD + ANNOTATION_LABEL_GAP) * ppp)
            },
        );
        let overlay = painter.with_clip_rect(traces);
        let x_of = |column: u32| traces.left() + column as f32 / ppp;
        let rect_of = |mark: &annotations::Mark| {
            let y = annotations::span_y(&self.view, mark.span, area.height);
            let top = traces.top();
            let y = Rangef::new(top + y.start as f32, top + y.end as f32);
            let x = Rangef::new(x_of(mark.columns.start), x_of(mark.columns.end));
            Rect::from_x_y_ranges(x, y)
        };
        for band in &self.marks.bands {
            let color = annotation_color(band.color, visuals).gamma_multiply(BAND_ALPHA);
            overlay.rect_filled(rect_of(band), 0.0, color);
        }
        for line in &self.marks.lines {
            overlay.rect_filled(rect_of(line), 0.0, annotation_color(line.color, visuals));
        }

        // The selection's outline, in its annotation's color.
        self.outline = self
            .selected
            .and_then(|id| self.document.annotations.get(id))
            .and_then(|a| {
                let columns = annotations::extent(&self.view, &a.data, size_px[0])?;
                let color = self.legend.of(&a.data)?.color;
                let x = Rangef::new(x_of(columns.start), x_of(columns.end));
                let mut rect = Rect::from_x_y_ranges(x, traces.y_range());
                if columns.len() == 1 {
                    // A line: the outline goes around it.
                    rect = rect.expand2(vec2(OUTLINE_WIDTH, 0.0));
                }
                overlay.rect_stroke(
                    rect,
                    0.0,
                    Stroke::new(OUTLINE_WIDTH, annotation_color(color, visuals)),
                    StrokeKind::Inside,
                );
                Some(rect)
            });

        let query = query(rec, &self.view.tile_view(size_px[0]), service.cache());
        self.missing = query.missing.len();
        if self.auto_scale && query.missing.is_empty() {
            self.view
                .set_amplitudes(scale::fit(rec, &self.view, &query, size_px[0]));
            self.auto_scale = false;
        }
        let mut scene = Scene::from_view(&self.view, size_px, rgba(visuals.text_color()));
        scene.antialias = self.antialias;
        for row in &mut scene.rows {
            row.color = rgba(self.channel_color(row.channel, visuals));
            row.baseline = self.baseline(row.channel);
        }
        let frame = Frame::new(rec, &scene, &query);
        gpu.paint(&painter, traces, frame, self.serial);
        if let Some(error) = gpu.error() {
            let galley = painter.layout(
                error.to_owned(),
                body.clone(),
                visuals.error_fg_color,
                (traces.width() - 40.0).max(100.0),
            );
            let pos = traces.center() - galley.size() / 2.0;
            painter.galley(pos, galley, visuals.error_fg_color);
        }

        // Each on a plate of the traces' background, so dense traces don't hide it; the
        // plates are narrower than the gap between labels and leave the onset's line free.
        let plate = visuals.extreme_bg_color.gamma_multiply(LABEL_PLATE_ALPHA);
        for label in &self.marks.labels {
            let color = annotation_text_color(annotation_color(label.color, visuals), visuals);
            let galley = painter.layout_no_wrap(label.text.clone(), small.clone(), color);
            let pos = pos2(
                traces.left() + label.x as f32 / ppp + ANNOTATION_LABEL_PAD,
                traces.top() + 1.0,
            );
            let rect =
                Rect::from_min_size(pos, galley.size()).expand2(vec2(LABEL_PLATE_MARGIN, 0.0));
            overlay.rect_filled(rect, 2.0, plate);
            overlay.galley(pos, galley, color);
        }

        let bars = if self.show_scale_bars {
            self.draw_scale_bars(&overlay, traces, &small, visuals)
        } else {
            Vec::new()
        };
        let label_width = self.draw_measurement(ui, id, &overlay, traces, size_px[0]);
        if let Some((channel, time)) = self.measure_start {
            let visuals = ui.visuals();
            self.draw_trace_dots(&overlay, traces, channel, &[time], size_px[0], visuals);
        }

        // Channel names, centered on their rows, shortened in the middle where too long.
        let names_painter = painter.with_clip_rect(names);
        let name_room = self.name_room;
        let height = area.height;
        for (i, &channel) in self.view.visible_rows().zip(self.view.visible_channels()) {
            let y = traces.top() + self.view.y_of_row(i as f64 + 0.5, height) as f32;
            let color = if self.hover.channel == Some(channel) && !self.view.is_bad(channel) {
                visuals.strong_text_color()
            } else {
                self.channel_color(channel, visuals)
            };
            let name = &rec.channel(channel).name;
            let short = elide_middle(ui.ctx(), name, &body, name_room);
            names_painter.text(
                pos2(names.left() + NAME_PADDING, y),
                Align2::LEFT_CENTER,
                short.as_deref().unwrap_or(name),
                body.clone(),
                color,
            );
        }
        self.scale_bars = bars;
        self.measure_label_width = label_width.unwrap_or(0.0);
        asked
    }

    /// The measurement over the traces: a shaded band between a line at either end, on
    /// whole pixels, and its label in an area of its own (text that screen readers read)
    /// at the band's top, beside its later end or, without room there, its earlier one.
    /// Returns the label's width, if it is shown.
    fn draw_measurement(
        &self,
        ui: &Ui,
        id: egui::Id,
        painter: &egui::Painter,
        traces: Rect,
        width_px: u32,
    ) -> Option<f32> {
        let measurement = self.measurement?;
        let rec = &self.document.recording;
        let span = measurement.span(rec);
        let width = f64::from(traces.width());
        let ppp = ui.pixels_per_point();
        let x = |t: f64| {
            let x = traces.left() + self.view.x_at(t, width) as f32;
            (x * ppp).round() / ppp
        };
        let (left, right) = (x(span.start), x(span.end));
        if right < traces.left() || left > traces.right() {
            return None;
        }
        let visuals = ui.visuals();
        let color = visuals.strong_text_color();
        let band = Rect::from_x_y_ranges(left..=right, traces.y_range());
        painter.rect_filled(band, 0.0, color.gamma_multiply(MEASURE_ALPHA));
        for x in [left, right] {
            let line = Rect::from_x_y_ranges(x..=x + 1.0 / ppp, traces.y_range());
            painter.rect_filled(line, 0.0, color);
        }

        let ends = [span.start, span.end];
        self.draw_trace_dots(
            painter,
            traces,
            measurement.channel,
            &ends,
            width_px,
            visuals,
        );

        // Two lines: the interval and its frequency at a glance, then the channel and its
        // peak-to-peak amplitude in the band; the rest is in the status line.
        let body = TextStyle::Body.resolve(ui.style());
        let small = TextStyle::Small.resolve(ui.style());
        let mut interval = LayoutJob::default();
        let duration = measure::duration_text(&span);
        interval.append(&duration, 0.0, TextFormat::simple(body.clone(), color));
        if let Some(frequency) = measure::frequency_text(&span) {
            let format = TextFormat::simple(body, color);
            interval.append(&format!(" {frequency}"), MEASURE_LABEL_SPACE, format);
        }
        let rec = &self.document.recording;
        let (unit, factor) = reading_unit(rec, &self.view, measurement.channel);
        let version = self.view.tile_view(width_px).version;
        let peak_to_peak = self
            .band_extremes(measurement.channel, &span, version)
            .map(|(lo, hi)| (hi - lo) * factor.abs());
        let mut values = LayoutJob::default();
        let name = &rec.channel(measurement.channel).name;
        let text = visuals.text_color();
        values.append(name, 0.0, TextFormat::simple(small.clone(), text));
        let amplitude = format!(" {}", measure::amplitude_text(peak_to_peak, &unit));
        values.append(
            &amplitude,
            MEASURE_LABEL_SPACE,
            TextFormat::simple(small.clone(), color),
        );

        let top = traces.top() + ui.fonts_mut(|f| f.row_height(&small)) + MEASURE_LABEL_TOP;
        let room = self.measure_label_width + MEASURE_LABEL_GAP;
        let (pivot, at) = if right + room <= traces.right() {
            (Align2::LEFT_TOP, right + MEASURE_LABEL_GAP)
        } else if left - room >= traces.left() {
            (Align2::RIGHT_TOP, left - MEASURE_LABEL_GAP)
        } else {
            (Align2::RIGHT_TOP, right - MEASURE_LABEL_GAP)
        };
        let plate = visuals.extreme_bg_color.gamma_multiply(LABEL_PLATE_ALPHA);
        let shown = egui::Area::new(id.with("measurement"))
            .pivot(pivot)
            .fixed_pos(pos2(at, top))
            .constrain_to(traces)
            .interactable(false)
            .accessible_name(self.measurement_details(measurement, &span))
            .show(ui.ctx(), |ui| {
                egui::Frame::new()
                    .fill(plate)
                    .corner_radius(2.0)
                    .inner_margin(vec2(MEASURE_LABEL_MARGIN, 2.0))
                    .show(ui, |ui| {
                        // Never wrapped: the area lays out in last frame's size, which a
                        // longer text would otherwise wrap to.
                        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                        ui.style_mut().interaction.selectable_labels = false;
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.label(interval);
                        ui.label(values);
                    });
            });
        Some(shown.response.rect.width())
    }

    /// Dots where `channel`'s trace is at `times`: a measurement's ends, or where one would
    /// start while ⇧ is held. Where the row is on screen and the times read values (not
    /// envelopes).
    fn draw_trace_dots(
        &self,
        painter: &egui::Painter,
        traces: Rect,
        channel: ChannelId,
        times: &[f64],
        width_px: u32,
        visuals: &egui::Visuals,
    ) {
        let Some(row) = self.view.row_of(channel) else {
            return;
        };
        if !self.view.visible_rows().contains(&row) {
            return;
        }
        let rec = &self.document.recording;
        let height = f64::from(traces.height());
        let row_height = self.view.row_height(height);
        let center = self.view.y_of_row(row as f64 + 0.5, height);
        let amplitude = self.view.channel_amplitude(channel);
        if !(amplitude > 0.0 && amplitude.is_finite()) {
            return;
        }
        let per_unit = row_height / (2.0 * amplitude);
        let baseline = self.baseline(channel);
        // Stim traces are clipped to their rows.
        let rows = if self.view.kind_of(channel).is_stim() {
            let top = traces.top() + (center - row_height / 2.0) as f32;
            Rangef::new(top, top + row_height as f32)
        } else {
            traces.y_range()
        };
        let view = self.view.tile_view(width_px);
        let width = f64::from(traces.width());
        for &t in times {
            let Some(readout::Reading::Value(v)) =
                readout::read(rec, self.service().cache(), &view, channel, t)
            else {
                continue;
            };
            let y = traces.top() + (center - (v - baseline) * per_unit) as f32;
            let x = traces.left() + self.view.x_at(t, width) as f32;
            if rows.contains(y) && traces.x_range().contains(x) {
                let at = pos2(x, y);
                painter.circle(
                    at,
                    MEASURE_DOT,
                    visuals.strong_text_color(),
                    Stroke::new(1.0, visuals.extreme_bg_color),
                );
            }
        }
    }

    /// The smallest and largest physical value of `channel` in the band, exact; `None`
    /// while tiles are missing (asked for here) or where no sample has a value.
    fn band_extremes(
        &self,
        channel: ChannelId,
        span: &Span,
        version: kymo_core::tile::SignalVersion,
    ) -> Option<(f64, f64)> {
        let service = self.service();
        let rec = &self.document.recording;
        match readout::extremes(rec, service.cache(), version, channel, span.inside.clone()) {
            readout::Extremes::Ready(extremes) => extremes,
            readout::Extremes::Missing(keys) => {
                // Behind the view's own tiles; again each frame, as a new view's request
                // drops what is still queued.
                service.submit(self.view.generation(), keys);
                None
            }
        }
    }

    /// The measurement in full for the status line and screen readers: its channel, the
    /// ends' times, the interval with its samples, and the readings at both ends as the
    /// status line reads them (measured from the window's mean with DC removal, in the
    /// scale bar's unit) with their difference.
    fn measurement_details(&self, m: Measurement, span: &Span) -> String {
        let rec = &self.document.recording;
        let view = self.view.tile_view(self.width_px.max(1));
        let (unit, factor) = reading_unit(rec, &self.view, m.channel);
        let baseline = self.baseline(m.channel);
        let read = |t: f64| {
            readout::read(rec, self.service().cache(), &view, m.channel, t)
                .map(|r| r.relative_to(baseline).scaled(factor))
        };
        measure::details_text(
            &rec.channel(m.channel).name,
            span,
            Clock::for_end(self.view.duration()),
            &unit,
            [read(span.start), read(span.end)],
        )
    }

    /// The scale bars, at the right edge of `traces`: a line one physical pixel wide in its
    /// type's color with end caps, on whole pixels so it is sharp and symmetric, on a halo
    /// of the background one pixel wide all around, so the traces under it don't blur it;
    /// its label to its left on a plate, as annotation labels have. Rows lower than the
    /// label leave them out, as they would cover their neighbors. Returns them, each with
    /// its bar's rectangle (the caps' width and the line's length, halo not included).
    fn draw_scale_bars(
        &self,
        painter: &egui::Painter,
        traces: Rect,
        font: &FontId,
        visuals: &egui::Visuals,
    ) -> Vec<(ScaleBar, Rect)> {
        let height = f64::from(traces.height());
        let row_height = self.view.row_height(height) as f32;
        if row_height < painter.ctx().fonts_mut(|f| f.row_height(font)) {
            return Vec::new();
        }
        let mut drawn = Vec::new();
        let ppp = painter.ctx().pixels_per_point();
        let background = visuals.extreme_bg_color;
        let plate = background.gamma_multiply(LABEL_PLATE_ALPHA);
        // In physical pixels: the line's column, and the caps' reach to either side of it.
        let column = ((traces.right() - SCALE_BAR_INSET) * ppp).round();
        let reach = (SCALE_BAR_CAP / 2.0 * ppp).round().max(1.0);
        let points = |x0: f32, y0: f32, x1: f32, y1: f32| {
            Rect::from_min_max(pos2(x0, y0) / ppp, pos2(x1, y1) / ppp)
        };
        for bar in scale_bars(&self.document.recording, &self.view) {
            let middle = traces.top() + bar.middle as f32 * row_height;
            let half = bar.fraction as f32 * row_height / 2.0;
            let first = ((middle - half) * ppp).round();
            let last = ((middle + half) * ppp).round().max(first + 2.0);
            let pieces = [
                points(column, first, column + 1.0, last),
                points(column - reach, first, column + 1.0 + reach, first + 1.0),
                points(column - reach, last - 1.0, column + 1.0 + reach, last),
            ];
            let halo = 1.0 / ppp;
            for piece in pieces {
                painter.rect_filled(piece.expand(halo), 0.0, background);
            }
            let color = type_color(&bar.kind, &self.types, visuals);
            for piece in pieces {
                painter.rect_filled(piece, 0.0, color);
            }
            let rect = pieces[1].union(pieces[2]);
            let galley = painter.layout_no_wrap(bar.label(), font.clone(), color);
            let pos = pos2(
                rect.left() - halo - SCALE_BAR_GAP - galley.size().x,
                middle - galley.size().y / 2.0,
            );
            let label =
                Rect::from_min_size(pos, galley.size()).expand2(vec2(LABEL_PLATE_MARGIN, 0.0));
            painter.rect_filled(label, 2.0, plate);
            painter.galley(pos, galley, color);
            drawn.push((bar, rect));
        }
        drawn
    }

    /// The color of a channel's trace and name: its type's, or grayed out if it is bad.
    fn channel_color(&self, channel: ChannelId, visuals: &egui::Visuals) -> Color32 {
        if self.view.is_bad(channel) {
            bad_color(visuals)
        } else {
            type_color(self.view.kind_of(channel), &self.types, visuals)
        }
    }

    /// Dragging, scrolling and pinching over the trace region, and clicks on names. The
    /// trace area is `width_px` physical pixels wide.
    #[expect(clippy::too_many_arguments)]
    fn handle_input(
        &mut self,
        ui: &Ui,
        id: egui::Id,
        keymap: &Keymap,
        traces: Rect,
        names: Rect,
        axis_rect: Rect,
        width_px: u32,
    ) {
        let area = Area {
            width: f64::from(traces.width()),
            height: f64::from(traces.height()),
        };
        let response = ui.interact(traces, id.with("drag"), Sense::click_and_drag());
        // A press in the traces takes the keyboard from a text field (the description),
        // so keys act on the traces again: ⌫ deletes the annotation just made.
        if response.is_pointer_button_down_on() {
            ui.memory_mut(|m| m.stop_text_input());
        }
        // ⇧ alone measures (its dot, hint and crosshair too); with other keys it means
        // something else, ⌥⇧⌘ scaling the channel under the pointer.
        let shift = ui.input(|i| {
            let m = i.modifiers;
            m.shift && !(m.alt || m.ctrl || m.mac_cmd || m.command)
        });
        let channel_keys =
            ui.input(|i| i.modifiers.shift && i.modifiers.alt && keymap.command(i.modifiers));
        if response.drag_started()
            && shift
            && let Some(origin) = ui.input(|i| i.pointer.press_origin())
        {
            self.finish_drag();
            self.measurement = None;
            self.measuring = self
                .channel_near(f64::from(origin.y - traces.top()), area.height)
                .map(|channel| {
                    let press = self.time_at_x(f64::from(origin.x - traces.left()), area.width);
                    Measurement {
                        channel,
                        press,
                        reach: press,
                    }
                });
        }
        // Any click removes the measurement (and in annotation mode selects as ever).
        if response.clicked() {
            self.measurement = None;
        }
        if let Some(making) = &mut self.measuring {
            if response.dragged()
                && let Some(p) = response.interact_pointer_pos()
            {
                let x = f64::from(p.x - traces.left());
                let duration = self.view.duration();
                making.reach = self.view.time_at(x, area.width).clamp(0.0, duration);
                let reach = x - self.view.x_at(making.press, area.width);
                self.measurement = (reach.abs() >= f64::from(MIN_DRAG)).then_some(*making);
            }
            if response.drag_stopped() {
                self.measuring = None;
            }
        } else if self.editing {
            self.edit_input(ui, &response, traces, width_px);
        } else if response.dragged() {
            self.view
                .pan_points(f64::from(response.drag_delta().x), area.width);
        }
        let over_traces = response.hovered() || self.measuring.is_some();
        if self.measuring.is_some() || shift && response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
        }
        // Where a ⇧-drag would start: the sample nearest the pointer on the channel it
        // would measure.
        self.measure_start = (shift && response.hovered() && self.measuring.is_none())
            .then(|| response.hover_pos())
            .flatten()
            .and_then(|p| {
                let channel = self.channel_near(f64::from(p.y - traces.top()), area.height)?;
                let rec = &self.document.recording;
                let grid = rec.grid(rec.channel(channel).grid);
                let time = self.time_at_x(f64::from(p.x - traces.left()), area.width);
                Some((channel, measure::snap(grid, time)))
            });
        let scaled = (channel_keys && response.hovered() && self.measuring.is_none())
            .then(|| response.hover_pos())
            .flatten()
            .and_then(|p| self.channel_near(f64::from(p.y - traces.top()), area.height));
        self.shift_hint = if let Some(channel) = scaled {
            let what = match self.view.scale_targets(channel).as_slice() {
                [one] => format!("{} alone", self.document.recording.channel(*one).name),
                many => format!("the {} selected channels", many.len()),
            };
            Some(format!(
                "{}-scroll to scale {what}",
                keymap.channel_scale_keys()
            ))
        } else {
            (shift && over_traces && self.measuring.is_none()).then(|| {
                match keymap.platform() {
                    Platform::Mac => "\u{21e7}-drag to measure",
                    Platform::Windows | Platform::Linux => "Shift-drag to measure",
                }
                .to_owned()
            })
        };
        // A click on a name marks the channel bad or good.
        let names_response = ui.interact(names, id.with("names"), Sense::click());
        let channel_at = |p: Pos2| {
            self.view
                .channel_at(f64::from(p.y - traces.top()), area.height)
        };
        let pressed = names_response.interact_pointer_pos().and_then(channel_at);
        let hovered = names_response.hover_pos().and_then(channel_at);
        if names_response.clicked()
            && let Some(channel) = pressed
        {
            self.view.toggle_bad(channel);
        }
        if let Some(channel) = hovered {
            let mark = if self.view.is_bad(channel) {
                "Mark as Good"
            } else {
                "Mark as Bad"
            };
            // A shortened name in full above it.
            let name = &self.document.recording.channel(channel).name;
            let font = TextStyle::Body.resolve(ui.style());
            let tip = match elide_middle(ui.ctx(), name, &font, self.name_room) {
                Some(_) => format!("{name}\n\n{mark}"),
                None => mark.to_owned(),
            };
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            // At the pointer: the names' area is the whole column.
            names_response.on_hover_text_at_pointer(tip);
        }

        let region = traces.union(names).union(axis_rect);
        let pointer = ui.input(|i| i.pointer.hover_pos());
        if !pointer.is_some_and(|p| region.contains(p)) || !ui.rect_contains_pointer(region) {
            return;
        }
        let pointer_x = pointer
            .filter(|p| traces.x_range().contains(p.x))
            .map(|p| f64::from(p.x - traces.left()));
        // The channel a ⌥⇧⌘-scroll scales: the row under the pointer, beside the traces too.
        let pointer_channel =
            pointer.and_then(|p| self.channel_near(f64::from(p.y - traces.top()), area.height));
        let (events, modifiers) = ui.input(|i| (i.events.clone(), i.modifiers));
        for event in events {
            let gestures = match event {
                Event::MouseWheel {
                    unit,
                    delta,
                    modifiers,
                    ..
                } => keymap.wheel(unit, delta, modifiers, traces.size()),
                Event::Zoom(factor) => vec![keymap.pinch(factor, modifiers)],
                _ => continue,
            };
            for gesture in gestures {
                match gesture {
                    Gesture::ScaleChannel { factor } => {
                        if let Some(channel) = pointer_channel {
                            let targets = self.view.scale_targets(channel);
                            self.view.scale_channels(&targets, factor);
                            self.auto_scale = false;
                        }
                    }
                    Gesture::Scale { .. } => {
                        self.view.gesture(gesture, area, pointer_x);
                        self.auto_scale = false;
                    }
                    _ => self.view.gesture(gesture, area, pointer_x),
                }
            }
        }
    }

    /// Annotation mode's gestures on the traces: a drag on the selected annotation moves
    /// it, or near an edge resizes it; elsewhere it creates an annotation of all channels
    /// with the current description. A click selects the annotation under the pointer, the
    /// next one there on the selected one (see [`annotations::pick`]), or, away from them,
    /// deselects.
    fn edit_input(&mut self, ui: &Ui, response: &egui::Response, traces: Rect, width_px: u32) {
        let width = f64::from(traces.width());
        let duration = self.view.duration();
        let ppp = ui.pixels_per_point();
        let x_of = |p: Pos2| f64::from(p.x - traces.left());
        if response.drag_started()
            && let Some(origin) = ui.input(|i| i.pointer.press_origin())
        {
            self.finish_drag();
            // The window lies within the recording.
            let press = self.view.time_at(x_of(origin), width);
            self.drag = match self.grip_at(x_of(origin), width, ppp) {
                Some((a, grip)) => Some(Drag {
                    id: self.selected.expect("a grip is on the selected annotation"),
                    hold: match grip {
                        annotations::Grip::Start => Hold::Edge {
                            anchor: a.end(),
                            edge: a.onset,
                        },
                        annotations::Grip::End => Hold::Edge {
                            anchor: a.onset,
                            edge: a.end(),
                        },
                        annotations::Grip::Body => Hold::Body,
                    },
                    before: Some(a.clone()),
                    base: a,
                    press,
                }),
                None if !self.description.is_empty() => Some(Drag {
                    id: self.document.annotations.new_id(),
                    before: None,
                    base: AnnotationData {
                        onset: press,
                        duration: 0.0,
                        description: self.description.clone(),
                        channels: Vec::new(),
                        origin: Origin::Viewer,
                        stream: None,
                    },
                    press,
                    hold: Hold::Edge {
                        anchor: press,
                        edge: press,
                    },
                }),
                None => None,
            };
        }
        if response.dragged()
            && let Some(drag) = &self.drag
            && let Some(p) = response.interact_pointer_pos()
        {
            let time = self.view.time_at(x_of(p), width).clamp(0.0, duration);
            let reach = self.view.x_at(time, width) - self.view.x_at(drag.press, width);
            // Too short a drag changes nothing, or takes back what it changed.
            let data = if reach.abs() >= f64::from(MIN_DRAG) {
                Some(drag.at(time - drag.press, duration))
            } else {
                drag.before.clone()
            };
            let store = &mut self.document.annotations;
            history::set(store, drag.id, data).expect("times within the recording are valid");
            self.legend.sync(store);
            if drag.before.is_none() && store.get(drag.id).is_some() {
                let stream = self.legend.stream_of(&drag.base);
                self.legend.set_shown(stream, &drag.base.description, true);
            }
        }
        if response.drag_stopped() {
            self.finish_drag();
        }
        if response.clicked()
            && let Some(p) = response.interact_pointer_pos()
        {
            let channel = self
                .view
                .channel_at(f64::from(p.y - traces.top()), f64::from(traces.height()));
            self.selected = annotations::pick(
                &self.view,
                &self.document.annotations,
                &self.legend,
                width_px,
                f64::from((p.x - traces.left()) * ppp),
                (POINTER_SLACK * ppp).round() as u32,
                channel,
                self.selected,
            );
        }
        // The pointer shows what a drag would do, or does.
        let cursor = match &self.drag {
            Some(Drag {
                hold: Hold::Body, ..
            }) => Some(CursorIcon::Grabbing),
            Some(Drag {
                before: Some(_), ..
            }) => Some(CursorIcon::ResizeHorizontal),
            Some(_) => Some(CursorIcon::Crosshair),
            None => response.hover_pos().map(|p| {
                match self.grip_at(x_of(p), width, ppp).map(|(_, g)| g) {
                    Some(annotations::Grip::Body) => CursorIcon::Grab,
                    Some(_) => CursorIcon::ResizeHorizontal,
                    None => CursorIcon::Crosshair,
                }
            }),
        };
        if let Some(cursor) = cursor {
            ui.ctx().set_cursor_icon(cursor);
        }
    }

    /// The time `x` points from the trace area's left edge (`width` wide), within the
    /// recording.
    fn time_at_x(&self, x: f64, width: f64) -> f64 {
        self.view.time_at(x, width).clamp(0.0, self.view.duration())
    }

    /// The channel row at `y` points from the trace area's top (`height` high), or the
    /// last row shown below the rows.
    fn channel_near(&self, y: f64, height: f64) -> Option<ChannelId> {
        self.view
            .channel_at(y.max(0.0), height)
            .or_else(|| self.view.visible_channels().last().copied())
    }

    fn hover_at(&self, pointer: Option<Pos2>, traces: Rect, names: Rect) -> Hover {
        let Some(p) = pointer else {
            return Hover::default();
        };
        let height = f64::from(traces.height());
        let channel = || self.view.channel_at(f64::from(p.y - traces.top()), height);
        if traces.contains(p) {
            Hover {
                channel: channel(),
                time: Some(
                    self.view
                        .time_at(f64::from(p.x - traces.left()), f64::from(traces.width())),
                ),
            }
        } else if names.contains(p) {
            Hover {
                channel: channel(),
                time: None,
            }
        } else {
            Hover::default()
        }
    }

    /// The boundary between the names column (`width` wide) and `traces`: dragging it sets
    /// the column's width while the recording is open, a double click fits the names again.
    /// Interacted with after the names and traces, so it is on top of both near the edge.
    fn names_edge(&mut self, ui: &Ui, id: egui::Id, width: f32, traces: Rect) {
        let x = traces.left();
        let edge =
            Rect::from_x_y_ranges(x - NAMES_EDGE_GRAB..=x + NAMES_EDGE_GRAB, traces.y_range());
        let response = ui.interact(edge, id.with("names edge"), Sense::click_and_drag());
        if response.hovered() || response.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
            // Just inside the names, as the traces paint over their own left edge.
            let stroke = ui.visuals().widgets.hovered.fg_stroke;
            ui.painter().vline(x - 1.0, traces.y_range(), stroke);
        }
        if response.double_clicked() {
            self.names_dragged = None;
        } else if response.dragged() {
            self.names_dragged = Some(width + response.drag_delta().x);
        }
    }

    /// Has the widest channel name measured again, as the fonts changed.
    pub fn forget_text_widths(&mut self) {
        self.names_width = None;
    }

    /// Width of the widest channel name in `font`.
    fn names_width(&mut self, ui: &Ui, font: &FontId) -> f32 {
        if let Some((size, width)) = self.names_width
            && size == font.size
        {
            return width;
        }
        let width = ui.fonts_mut(|f| {
            self.document
                .recording
                .channels()
                .iter()
                .map(|c| {
                    f.layout_no_wrap(c.name.clone(), font.clone(), Color32::WHITE)
                        .size()
                        .x
                })
                .fold(0.0, f32::max)
        });
        self.names_width = Some((font.size, width));
        width
    }
}

impl Drop for Viewer {
    /// Stopping the tile service joins its threads, which may wait for a build step; that
    /// happens on a thread of its own so the window never waits.
    fn drop(&mut self) {
        if let Some(service) = self.service.take() {
            // If no thread starts, the closure and the service are dropped right here.
            let _ = std::thread::Builder::new()
                .name("kymo-close".into())
                .spawn(move || drop(service));
        }
    }
}

/// The viewer's tile service, borrowed on its own so other fields can be borrowed mutably
/// beside it. Only `drop` takes it.
fn live(service: &Option<TileService>) -> &TileService {
    service
        .as_ref()
        .expect("the tile service lives as long as the viewer")
}

/// An egui color as the renderer's premultiplied RGBA.
fn rgba(color: Color32) -> [f32; 4] {
    color.to_normalized_gamma_f32()
}

/// An annotation label's text color: its annotation's, darker on light themes, where thin
/// text in the lighter palette colors (pink, green) reads poorly on its plate; dark themes
/// lighten the palette already.
fn annotation_text_color(color: Color32, visuals: &egui::Visuals) -> Color32 {
    if visuals.dark_mode {
        color
    } else {
        color.lerp_to_gamma(Color32::BLACK, 0.4)
    }
}

/// BIDS' suffixes of recordings, which a `channels.tsv`'s name replaces.
const BIDS_SUFFIXES: [&str; 6] = ["eeg", "meg", "ieeg", "nirs", "physio", "motion"];
