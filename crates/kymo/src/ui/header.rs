//! The sidebar: the file's header and its channels.

use std::collections::{BTreeSet, HashMap};

use eframe::egui::{
    self, Align, Checkbox, Color32, CursorIcon, FontId, Frame, Id, Label, Layout, Margin, Rect,
    Response, RichText, ScrollArea, Sense, TextStyle, Ui, UiBuilder, WidgetText,
    scroll_area::ScrollBarVisibility, vec2,
};
use kymo_core::{ChannelId, ChannelKind, GridId, Stream, StreamContent, Unit};

use crate::action::Action;
use crate::annotations::{AnnotationColors, Entry, Legend};
use crate::document::{Document, Summary, format_rate, grid_rate, plural};
use crate::keymap::{Keymap, Platform};
use crate::types::TypeSettings;
use crate::ui::axis::{Clock, format_time};
use crate::ui::color::{Choice, swatch};
use crate::ui::palette::{annotation_color, type_color};
use crate::ui::readout::display_unit;
use crate::ui::types::{
    Dots, Retype, SCALE_WITH_TYPE, Targets, change_type_items, channel_menu, mark_label, type_menu,
    type_swatch,
};
use crate::ui::{elide_middle, scrollbar};
use crate::view::ViewState;

/// Runs `add_contents` in exactly the width `ui` offers, clipping whatever is wider. A panel
/// grows to fit wider contents, and while its edge is dragged the difference stays unpainted
/// (black), so panel contents must never be wider than the panel. Text that should shorten
/// with "…" instead opts in with `Label::truncate`.
pub fn within_width<R>(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    let rect = ui.max_rect();
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    child.shrink_clip_rect(rect);
    let inner = add_contents(&mut child);
    ui.advance_cursor_after_rect(rect);
    inner
}

/// What the sidebar's annotation part edits: which descriptions are shown, their picked
/// colors, and in annotation mode the description of new annotations.
pub struct AnnotationPanel<'a> {
    pub legend: &'a mut Legend,
    pub colors: &'a mut AnnotationColors,
    /// The description field, in annotation mode.
    pub field: Option<DescriptionField<'a>>,
}

/// What the description field above the legend edits.
pub enum DescriptionField<'a> {
    /// The description new annotations get.
    New(&'a mut String),
    /// The selected annotation's, `current`, as typed into `draft`; the caller applies the
    /// draft once the field gives up the keyboard, unless by Esc.
    Selected {
        current: &'a str,
        draft: &'a mut String,
    },
}

/// The description field's id, to know whether it has the keyboard.
pub fn description_field_id() -> egui::Id {
    egui::Id::unique("kymo description field")
}

/// Rows of the annotation legend shown before it scrolls.
const LEGEND_ROWS: f32 = 8.0;

/// What the sidebar shows and doesn't change.
pub struct HeaderSource<'a> {
    /// This opening of the recording (the viewer's serial): the lists start at their top
    /// for each.
    pub recording: u64,
    pub document: &'a Document,
    pub summary: &'a Summary,
    /// For the shortcuts in tooltips.
    pub platform: Platform,
}

/// File header, channels by type, and the annotation legend at the bottom. The channel
/// list shows and hides channels and marks them bad in `view`, in their types' colors, and
/// its types' swatches and menus set `types` (see [`type_swatch`] and [`type_menu`]); the
/// legend shows and hides annotations by description in `annotations.legend`, and picks
/// their colors in `annotations.colors`. In annotation mode, `annotations.field` edits the
/// description new annotations get, or the selected annotation's, above the legend, which
/// then shows even without annotations. The Format row's value is a button that asks for
/// the Recording Info window. Returns what was asked of the viewer.
pub fn header_panel(
    ui: &mut Ui,
    source: HeaderSource<'_>,
    view: &mut ViewState,
    types: &mut TypeSettings,
    annotations: AnnotationPanel<'_>,
) -> HeaderResponse {
    let HeaderSource {
        recording,
        document,
        summary,
        platform,
    } = source;
    let AnnotationPanel {
        legend,
        colors: annotation_colors,
        field,
    } = annotations;
    let mut relabel = None;
    let mut info = false;
    ui.add_space(6.0);
    ui.add(Label::new(RichText::new(document.name()).heading()).truncate());
    ui.add(
        Label::new(
            RichText::new(document.path.display().to_string())
                .small()
                .weak(),
        )
        .truncate(),
    );
    ui.add_space(6.0);

    egui::Grid::new("summary")
        .num_columns(2)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            // Keys at their full width; values, in the last column, shorten to what is left.
            for row in summary.sidebar_rows() {
                let key = Label::new(RichText::new(row.key).weak()).extend();
                let value = if row.value.contains('\n') {
                    // A line each (the rates), each shortened on its own, the key at the top.
                    ui.vertical(|ui| ui.add(key));
                    ui.vertical(|ui| {
                        for line in row.value.lines() {
                            ui.add(Label::new(line).truncate());
                        }
                    })
                    .response
                } else if row.info {
                    ui.add(key);
                    let value = info_value(ui, &row.value, platform);
                    info |= value.clicked();
                    value
                } else {
                    ui.add(key);
                    ui.add(Label::new(row.value).truncate())
                };
                if let Some(hover) = row.hover {
                    value.on_hover_text(hover);
                }
                ui.end_row();
            }
        });

    if !summary.warnings.is_empty() {
        ui.add_space(6.0);
        warning_line(ui, &summary.warnings);
    }

    ui.add_space(6.0);
    let rule = ui.separator().rect.center().y;
    // The legend at the bottom, so the channel list fills the space above it, leaving it
    // at least a few rows.
    let stride = ui.spacing().interact_size.y + ui.spacing().item_spacing.y;
    let room = ui.available_height() - LIST_MIN_ROWS * stride;
    let mut legend_rect = None;
    if !legend.is_empty() || field.is_some() {
        let panel = egui::Panel::bottom("legend")
            .resizable(false)
            .frame(Frame::NONE.inner_margin(Margin::symmetric(0, 6)))
            .show(ui, |ui| {
                if let Some(field) = field {
                    relabel = description_row(ui, field, legend);
                }
                if !legend.is_empty() {
                    legend_list(
                        ui,
                        recording,
                        document.recording.streams(),
                        legend,
                        annotation_colors,
                        room,
                    );
                }
            });
        legend_rect = Some(panel.response.rect);
    }
    let mut marks = None;
    let all_types = channel_list(
        ui,
        (recording, platform),
        document,
        (view, &mut marks),
        types,
        rule,
    );
    // On top of both lists, as each would take the drag near the line between them.
    if let Some(rect) = legend_rect {
        let folded = ui
            .data(|d| d.get_temp::<bool>(legend_folded_id(recording)))
            .unwrap_or(false);
        if !legend.is_empty() && !folded {
            legend_edge(ui, recording, rect);
        }
        if legend.by_stream() {
            fold_all_legend(ui, recording, legend, rect);
        }
    }
    HeaderResponse {
        relabel,
        info,
        all_types,
        marks,
    }
}

/// What the sidebar asks of the viewer.
pub struct HeaderResponse {
    /// A description picked from the description field's menu for the selected annotation.
    pub relabel: Option<String>,
    /// Whether the Recording Info window was asked for.
    pub info: bool,
    /// Whether a type's menu asked for the Channel Types window.
    pub all_types: bool,
    /// Channels to mark bad (`true`) or good, which the viewer does so it can be undone.
    pub marks: Marks,
}

/// Channels to mark bad (`true`) or good, if any.
pub type Marks = Option<(Vec<ChannelId>, bool)>;

/// The Format row's value: `text` as a button that opens Recording Info (named "Recording
/// Info" for screen readers, with `text` as its value, and the shortcut on `platform` on
/// hover). It reads as text until hovered, when it brightens on a background as
/// [`text_button`]'s; the background reaches past the text into the gaps around it, so the
/// format lines up with the other values.
fn info_value(ui: &mut Ui, text: &str, platform: Platform) -> Response {
    let padding = ui.spacing().button_padding;
    let room = (ui.available_width() - padding.x).max(0.0);
    let galley = egui::WidgetText::from(text).into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        room,
        TextStyle::Body,
    );
    let (rect, response) = ui.allocate_exact_size(galley.size(), Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = ui.visuals();
        let hovered = response.hovered() || response.has_focus();
        let text_color = if hovered {
            let widget = &visuals.widgets.hovered;
            ui.painter().rect(
                rect.expand2(padding),
                crate::ui::TEXT_BUTTON_RADIUS,
                widget.weak_bg_fill,
                widget.bg_stroke,
                egui::StrokeKind::Inside,
            );
            visuals.strong_text_color()
        } else {
            visuals.text_color()
        };
        let top = rect.center().y - galley.size().y / 2.0;
        ui.painter()
            .galley(egui::pos2(rect.left(), top), galley, text_color);
    }
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), INFO_LABEL));
    let value = text.to_owned();
    ui.ctx()
        .accesskit_node_builder(response.id, |node| node.set_value(value));
    let shortcut = Keymap::chord(Action::RecordingInfo)
        .map(|c| format!(" ({})", c.display(platform)))
        .unwrap_or_default();
    response
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text(format!("{INFO_LABEL}{shortcut}"))
}

/// What the Format row's button is named, for screen readers and its tooltip.
pub const INFO_LABEL: &str = "Recording Info";

/// Rows the channel list keeps when the legend is dragged taller.
const LIST_MIN_ROWS: f32 = 3.0;

/// Half the height of the band around the line between the channel list and the legend
/// that drags it, in points.
const LEGEND_EDGE_GRAB: f32 = 3.0;

/// The line between the channel list and the legend (`panel`'s top): dragging it sets the
/// legend's height while the recording is open (from one row to all of them, leaving the
/// channel list [`LIST_MIN_ROWS`]), a double click puts it back (up to [`LEGEND_ROWS`] rows).
fn legend_edge(ui: &mut Ui, recording: u64, panel: egui::Rect) {
    let y = panel.top();
    let band =
        egui::Rect::from_x_y_ranges(panel.x_range(), y - LEGEND_EDGE_GRAB..=y + LEGEND_EDGE_GRAB);
    let id = Id::unique(("kymo legend edge", recording));
    let response = ui.interact(band, id, Sense::click_and_drag());
    if response.hovered() || response.dragged() {
        ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
        let stroke = ui.visuals().widgets.hovered.fg_stroke;
        ui.painter().hline(panel.x_range(), y, stroke);
    }
    let key = legend_height_id(recording);
    if response.double_clicked() {
        ui.data_mut(|d| d.remove::<f32>(key));
    } else if response.dragged() {
        let shown: f32 = ui
            .data(|d| d.get_temp(legend_shown_height_id(recording)))
            .unwrap_or(0.0);
        // Up makes it taller.
        let height = shown - response.drag_delta().y;
        ui.data_mut(|d| d.insert_temp(key, height));
        ui.ctx().request_repaint();
    }
}

/// The legend's rows' height as dragged, for `recording`; none: [`LEGEND_ROWS`] rows.
fn legend_height_id(recording: u64) -> Id {
    Id::unique(("kymo legend height", recording))
}

/// The legend's rows' height as last shown, for `recording`.
fn legend_shown_height_id(recording: u64) -> Id {
    Id::unique(("kymo legend shown height", recording))
}

/// Whether the legend is folded to its "Annotations" row, for `recording`.
fn legend_folded_id(recording: u64) -> Id {
    Id::unique(("kymo legend folded", recording))
}

/// Widest a warning tooltip gets before its text wraps, in points.
const WARNING_TOOLTIP_WIDTH: f32 = 360.0;

/// The reader's warnings as one line, how many ("1 Warning") in the warning color; hovering
/// it shows them in full.
fn warning_line(ui: &mut Ui, warnings: &[String]) {
    let count = format!("{} Warning{}", warnings.len(), plural(warnings.len()));
    let color = ui.visuals().warn_fg_color;
    ui.add(
        Label::new(RichText::new(count).color(color))
            .selectable(false)
            .sense(Sense::hover()),
    )
    .on_hover_cursor(CursorIcon::Help)
    .on_hover_ui(|ui| {
        ui.set_max_width(WARNING_TOOLTIP_WIDTH);
        for warning in warnings {
            ui.add(Label::new(sentence(warning)).wrap());
        }
    });
}

/// `text` with its first letter upper case, as a sentence starts; readers write warnings
/// in lower case, for `kymo --info`'s "warning" lines.
fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Rows of descriptions the description menu shows before it scrolls.
const DESCRIPTION_MENU_ROWS: f32 = 12.0;

/// The description field: the description of new annotations, or the selected one's, with
/// a menu of the legend's descriptions to pick one from. Returns a description picked for
/// the selected annotation.
fn description_row(ui: &mut Ui, field: DescriptionField<'_>, legend: &Legend) -> Option<String> {
    let row_height = ui.spacing().interact_size.y;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), row_height), Sense::hover());
    let mut row = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::right_to_left(Align::Center)),
    );
    let (label, accessible, text, current) = match field {
        DescriptionField::New(text) => ("New", "Description of new annotations", text, None),
        DescriptionField::Selected { current, draft } => (
            "Selected",
            "Description of the selected annotation",
            draft,
            Some(current),
        ),
    };
    let mut picked = None;
    // The menu first, at the right, so the field takes the rest.
    // Each once, though several streams may have it.
    let mut descriptions: Vec<String> = Vec::new();
    for e in legend.entries() {
        if !descriptions.contains(&e.description) {
            descriptions.push(e.description.clone());
        }
    }
    let menu = row.add_enabled_ui(!descriptions.is_empty(), |ui| {
        egui::ComboBox::from_id_salt("descriptions")
            .selected_text("")
            .width(0.0)
            .height(DESCRIPTION_MENU_ROWS * row_height)
            .show_ui(ui, |ui| {
                for d in descriptions {
                    let chosen = *text == d;
                    if ui.selectable_label(chosen, &d).clicked() {
                        picked = Some(d);
                    }
                }
            })
            .response
    });
    accessible_label(&row, &menu.inner, "Descriptions".to_owned());
    row.with_layout(Layout::left_to_right(Align::Center), |ui| {
        ui.add(Label::new(RichText::new(label).weak()).extend());
        let id = description_field_id();
        let field = ui.add(
            egui::TextEdit::singleline(text)
                .id(id)
                .hint_text("Description")
                .desired_width(f32::INFINITY),
        );
        accessible_label(ui, &field, accessible.to_owned());
    });
    ui.add_space(4.0);
    match (picked, current) {
        (Some(d), None) => {
            *text = d;
            None
        }
        (Some(d), Some(_)) => {
            *text = d.clone();
            Some(d)
        }
        (None, _) => None,
    }
}

/// A row for all annotations, as a stream's row in the channel list: a chevron that folds the
/// rest of the legend away (as does a click on "Annotations"; kept while the recording is
/// open), a checkbox (mixed while some descriptions are shown), and how many there are; then,
/// a level in, a row per description: a checkbox, its color (a click picks another, kept in
/// `picked`), the description and how many annotations have it. Counts are right-aligned,
/// and rows start where the channel list's start. Grouped by stream (see
/// [`Legend::by_stream`]), each stream of `streams` with annotations gets a row a level in
/// (see [`legend_stream_row`]) and its descriptions' rows below it, a level further;
/// annotations without a stream come last, under "Added". Scrolls beyond [`LEGEND_ROWS`]
/// rows.
fn legend_list(
    ui: &mut Ui,
    recording: u64,
    streams: &[Stream],
    legend: &mut Legend,
    picked: &mut AnnotationColors,
    room: f32,
) {
    let (shown, all) = legend.shown_count();
    let row_height = ui.spacing().interact_size.y;
    let folded_key = legend_folded_id(recording);
    let folded: bool = ui.data(|d| d.get_temp(folded_key)).unwrap_or(false);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), row_height), Sense::hover());
    let mut row = legend_row(ui, rect);
    row.add(Label::new(RichText::new(legend.total().to_string()).weak()).extend());
    let toggle_root = row
        .with_layout(Layout::left_to_right(Align::Center), |ui| {
            let name = "Annotations";
            let arrow = disclosure(ui, !folded, name, row_height);
            let mut checked = shown == all;
            let checkbox =
                Checkbox::without_text(&mut checked).indeterminate(0 < shown && shown < all);
            let response = ui.add(checkbox);
            accessible_label(ui, &response, format!("Show {name}"));
            let response = visibility_hint(response, checked, name);
            if response.changed() {
                legend.set_all_shown(checked);
            }
            let label = Label::new(RichText::new(name).strong())
                .sense(Sense::click())
                .selectable(false)
                .truncate();
            let response = ui.add(label);
            arrow.clicked() || response.clicked()
        })
        .inner;
    if toggle_root {
        ui.data_mut(|d| d.insert_temp(folded_key, !folded));
    }
    if folded {
        return;
    }

    let grouped = legend.by_stream();
    let closed_key = legend_closed_id(recording);
    let mut closed: Vec<Option<usize>> = ui.data(|d| d.get_temp(closed_key)).unwrap_or_default();
    let group_name = |stream: Option<usize>| {
        stream
            .and_then(|s| streams.get(s))
            .map_or_else(|| "Added".to_owned(), stream_name)
    };
    let mut rows: Vec<LegendRow> = Vec::new();
    // Rows with every stream open: the legend keeps their height, so collapsing a stream
    // leaves room at the bottom rather than moving the legend (and the row clicked) down.
    let mut all_rows = legend.entries().count();
    if grouped {
        all_rows += legend.streams().len();
        for stream in legend.streams() {
            rows.push(LegendRow::Stream(stream));
            if !closed.contains(&stream) {
                rows.extend(
                    legend
                        .entries()
                        .filter(|e| e.stream == stream)
                        .map(|e| LegendRow::Entry(e.clone())),
                );
            }
        }
    } else {
        rows.extend(legend.entries().map(|e| LegendRow::Entry(e.clone())));
    }
    // A description is a level in from "Annotations", and grouped, from its stream, which
    // is a level in itself; in line with the channel list's rows.
    let gap = LIST_GAP;
    let depth = if grouped { 2.0 } else { 1.0 };
    let indent = CHEVRON_WIDTH + gap + depth * LEVEL;
    let mut toggled = None;
    // As tall as dragged (see `legend_edge`), else as its rows up to LEGEND_ROWS; from one
    // row to all of them, and within `room`, less what the panel has above them and its
    // margins. Exactly so tall: the panel holding it would otherwise keep its first height.
    let gap = ui.spacing().item_spacing.y;
    let rows_height = |n: f32| n * (row_height + gap) - gap;
    let tallest = rows_height(all_rows as f32);
    let dragged: Option<f32> = ui.data(|d| d.get_temp(legend_height_id(recording)));
    let above = ui.min_rect().height() + gap + 12.0;
    let height = dragged
        .unwrap_or_else(|| rows_height((all_rows as f32).min(LEGEND_ROWS)))
        .min(room - above)
        .min(tallest)
        .max(row_height.min(tallest));
    ui.data_mut(|d| d.insert_temp(legend_shown_height_id(recording), height));
    from_top(ui, "legend", recording, ScrollArea::vertical())
        .id_salt("legend")
        .min_scrolled_height(height)
        .max_height(height)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (i, item) in rows.into_iter().enumerate() {
                let (rect, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), row_height), Sense::hover());
                // Striped as the channel list, "Annotations" its first row.
                if i % 2 == 0 {
                    stripe(ui, rect);
                }
                let mut row = legend_row(ui, rect);
                let entry = match item {
                    LegendRow::Stream(stream) => {
                        let open = !closed.contains(&stream);
                        let kind = stream.and_then(|s| streams.get(s)).map(|s| s.kind.trim());
                        let group = LegendGroup {
                            stream,
                            name: group_name(stream),
                            kind: kind.unwrap_or(""),
                            open,
                        };
                        if legend_stream_row(&mut row, legend, group) {
                            toggled = Some(stream);
                        }
                        continue;
                    }
                    LegendRow::Entry(entry) => entry,
                };
                let Entry {
                    stream,
                    description,
                    color,
                    count,
                    shown,
                    ..
                } = entry;
                // In a group, the description's stream tells rows of one description apart.
                let named = if grouped {
                    format!("{description} in {}", group_name(stream))
                } else {
                    description.clone()
                };
                // The count first, at the right, so a long description truncates.
                row.add(Label::new(RichText::new(count.to_string()).weak()).extend());
                row.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add_space(indent);
                    let mut checked = shown;
                    let response = ui.add(Checkbox::without_text(&mut checked));
                    accessible_label(ui, &response, format!("Show {named}"));
                    let response = visibility_hint(response, checked, &named);
                    if response.changed() {
                        legend.set_shown(stream, &description, checked);
                    }
                    let side = ui.text_style_height(&TextStyle::Body) * 0.7;
                    let shown_color = annotation_color(color, ui.visuals());
                    let label = format!("Color of {named}");
                    let is_picked = picked.contains_key(&description);
                    match swatch(ui, &label, shown_color, is_picked, vec2(side, side)) {
                        Some(Choice::Picked(c)) => {
                            picked.insert(description.clone(), [c.r(), c.g(), c.b()]);
                            legend.apply_colors(picked);
                        }
                        Some(Choice::Reset) => {
                            picked.remove(&description);
                            legend.apply_colors(picked);
                        }
                        None => {}
                    }
                    let text = if shown {
                        RichText::new(&description)
                    } else {
                        RichText::new(&description).weak()
                    };
                    ui.add(Label::new(text).truncate());
                });
            }
        });
    if let Some(stream) = toggled {
        let open = !closed.contains(&stream);
        if ui.input(|i| i.modifiers.alt) {
            // Every stream as this one goes.
            closed = if open { legend.streams() } else { Vec::new() };
        } else if open {
            closed.push(stream);
        } else {
            closed.retain(|&c| c != stream);
        }
        ui.data_mut(|d| d.insert_temp(closed_key, closed));
    }
}

/// The background of every other row of a list, the row in `rect` and half the gaps beside
/// it.
fn stripe(ui: &Ui, rect: egui::Rect) {
    let stripe = rect.expand2(vec2(0.0, ui.spacing().item_spacing.y / 2.0));
    ui.painter()
        .rect_filled(stripe, 0.0, ui.visuals().faint_bg_color);
}

/// A row of the legend in `rect`, laid out from the right (its count first, so a long
/// description truncates), with the channel list's gaps.
fn legend_row(ui: &mut Ui, rect: egui::Rect) -> Ui {
    let mut row = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::right_to_left(Align::Center)),
    );
    row.spacing_mut().item_spacing.x = LIST_GAP;
    row
}

/// Where the legend keeps the streams whose rows are folded, for `recording`.
fn legend_closed_id(recording: u64) -> Id {
    Id::unique(("kymo closed annotation streams", recording))
}

/// The legend's [`fold_all`] button, on the edge of its panel at `panel`'s top.
fn fold_all_legend(ui: &mut Ui, recording: u64, legend: &Legend, panel: egui::Rect) {
    let key = legend_closed_id(recording);
    let closed: Vec<Option<usize>> = ui.data(|d| d.get_temp(key)).unwrap_or_default();
    let streams = legend.streams();
    let any_open = streams.iter().any(|s| !closed.contains(s));
    if fold_all(ui, "Annotations", panel, panel.top(), any_open) {
        let closed = if any_open { streams } else { Vec::new() };
        ui.data_mut(|d| d.insert_temp(key, closed));
    }
}

/// While the pointer is over a list's `area` (or the button), a button that folds all of
/// its groups (`any_open`) or unfolds them: two small chevrons at the right end of the
/// rule at `y` above the list, so it takes no room ("Collapse All" or "Expand All" on
/// hover, and with `what` for screen readers: "Collapse All Channels"). Returns whether it
/// was clicked.
fn fold_all(ui: &mut Ui, what: &str, area: egui::Rect, y: f32, any_open: bool) -> bool {
    let size = vec2(FOLD_ALL_WIDTH, 12.0);
    let rect = egui::Rect::from_center_size(egui::pos2(area.right() - size.x / 2.0, y), size);
    if !ui.rect_contains_pointer(area.union(rect)) {
        return false;
    }
    let verb = if any_open { "Collapse" } else { "Expand" };
    let id = Id::unique(("kymo fold all", what));
    let response = ui.interact(rect, id, Sense::click());
    accessible_label(ui, &response, format!("{verb} All {what}"));
    let color = if response.hovered() {
        ui.visuals().strong_text_color()
    } else {
        ui.visuals().weak_text_color()
    };
    let painter = ui.painter().with_clip_rect(rect.expand(1.0));
    // The rule's line gives way to the button.
    painter.rect_filled(rect, 2.0, ui.visuals().panel_fill);
    // Two chevrons pointing up (fold) or down (unfold).
    let s = 3.0;
    let tip = if any_open { -s / 2.0 } else { s / 2.0 };
    for dy in [-2.5, 2.5] {
        let c = rect.center() + vec2(0.0, dy);
        let points = vec![c + vec2(-s, -tip), c + vec2(0.0, tip), c + vec2(s, -tip)];
        painter.add(egui::Shape::line(points, egui::Stroke::new(1.5, color)));
    }
    let response = response
        .on_hover_text(format!("{verb} All"))
        .on_hover_cursor(CursorIcon::PointingHand);
    response.clicked()
}

/// Width of the button that folds or unfolds all groups, in points.
const FOLD_ALL_WIDTH: f32 = 16.0;

/// How far each level of a list moves its rows' checkboxes in, in points: under a type, a
/// stream or "Annotations".
const LEVEL: f32 = 10.0;

/// A row of the annotation legend.
enum LegendRow {
    /// A stream's row (`None`: annotations without a stream, "Added").
    Stream(Option<usize>),
    Entry(Entry),
}

/// A stream of the annotation legend, as its row shows it.
struct LegendGroup<'a> {
    stream: Option<usize>,
    name: String,
    /// The stream's type, empty for "Added" or a stream without one.
    kind: &'a str,
    open: bool,
}

/// A stream's row in the annotation legend: a chevron that shows or hides its
/// descriptions' rows, a checkbox for all of them (mixed while some are shown), its name
/// (a click also shows or hides the rows; the name and type on hover), and at the right
/// how many annotations it has. Returns whether the rows are to be shown or hidden.
fn legend_stream_row(row: &mut Ui, legend: &mut Legend, group: LegendGroup<'_>) -> bool {
    let LegendGroup {
        stream,
        name,
        kind,
        open,
    } = group;
    let (shown, all) = legend.stream_shown(stream);
    let row_height = row.max_rect().height();
    row.add(Label::new(RichText::new(legend.stream_total(stream).to_string()).weak()).extend());
    row.with_layout(Layout::left_to_right(Align::Center), |ui| {
        // A level in from "Annotations".
        ui.add_space(LEVEL);
        let arrow = disclosure(ui, open, &name, row_height);

        let mut checked = shown == all;
        let checkbox = Checkbox::without_text(&mut checked).indeterminate(0 < shown && shown < all);
        let response = ui.add(checkbox);
        accessible_label(ui, &response, format!("Show {name}"));
        let response = visibility_hint(response, checked, &name);
        if response.changed() {
            legend.set_stream_shown(stream, checked);
        }

        let label = Label::new(RichText::new(&name).strong())
            .sense(Sense::click())
            .selectable(false)
            .truncate();
        let mut response = ui.add(label);
        if !kind.is_empty() {
            response = response.on_hover_text(format!("{name}\n{kind}"));
        }
        arrow.clicked() || response.clicked()
    })
    .inner
}

/// Narrowest the list's names get, in ems (the body font's size), before columns drop out;
/// narrower names are shortened in the middle.
const LIST_MIN_NAME_EMS: f32 = 6.0;

/// Narrowest the list's units get, in ems, before the names narrow: units kymo doesn't know
/// can be long ("normalized_signed").
const LIST_MIN_UNIT_EMS: f32 = 4.0;

/// Gap between the channel list's columns, in points.
const LIST_GAP: f32 = 8.0;

/// Width of a stream row's disclosure chevron, in points.
const CHEVRON_WIDTH: f32 = 10.0;

/// Widths of the channel list's text columns, fitting every channel's entry, so they stay
/// put while the list scrolls (only the rows in view are drawn).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Columns {
    name: f32,
    unit: f32,
}

/// Has the channel list's column widths measured again, as the fonts changed.
pub fn forget_text_widths(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove_by_type::<Measured>());
}

/// Column widths with the font and scale they were measured for.
#[derive(Clone, Debug, PartialEq)]
struct Measured {
    font: FontId,
    pixels_per_point: f32,
    columns: Columns,
}

impl Columns {
    /// The widths for `document`'s channels in `ui`'s body font, measured once per document,
    /// font, scale, set of units shown and of channels scaled on their own (whose names the
    /// mark follows, see [`alone_mark`]), and again after [`forget_text_widths`]; `grouped`,
    /// the names within their streams (see [`list_name`]). Only channels of a type row whose
    /// channels differ in unit show theirs (see [`shared_units`]); without any, the unit
    /// column is 0 wide.
    fn of(
        ui: &Ui,
        document: &Document,
        view: &ViewState,
        grouped: bool,
        shared: &HashMap<Group, Option<String>>,
    ) -> Columns {
        let font = TextStyle::Body.resolve(ui.style());
        let pixels_per_point = ui.ctx().pixels_per_point();
        let rec = &document.recording;
        let channels = rec.channels();
        let units: BTreeSet<String> = channels
            .iter()
            .enumerate()
            .filter_map(|(c, channel)| {
                let id = ChannelId(c as u32);
                let mixed = shared.get(&view.group_of(id)).is_some_and(Option::is_none);
                mixed.then(|| display_unit(view.kind_of(id), &channel.unit).0)
            })
            .collect();
        let alone: Vec<ChannelId> = (0..channels.len() as u32)
            .map(ChannelId)
            .filter(|&c| view.scaled_alone(c))
            .collect();
        let key = Id::unique((
            "kymo channel columns",
            &document.path,
            channels.len(),
            grouped,
            &units,
            &alone,
        ));
        if let Some(m) = ui.data(|d| d.get_temp::<Measured>(key))
            && m.font == font
            && m.pixels_per_point == pixels_per_point
        {
            return m.columns;
        }
        let widest = |texts: &mut dyn Iterator<Item = String>| {
            ui.fonts_mut(|f| {
                texts
                    .map(|t| {
                        f.layout_no_wrap(t, font.clone(), Color32::PLACEHOLDER)
                            .size()
                            .x
                    })
                    .fold(0.0, f32::max)
            })
        };
        // The names the mark follows are wider by it.
        let marked = widest(
            &mut alone
                .iter()
                .map(|&c| list_name(rec.channel(c), grouped).to_owned()),
        );
        let marked = if alone.is_empty() {
            0.0
        } else {
            marked + alone_mark_width(ui)
        };
        let columns = Columns {
            name: widest(&mut channels.iter().map(|c| list_name(c, grouped).to_owned()))
                .max(marked),
            unit: widest(&mut units.into_iter()),
        };
        ui.data_mut(|d| {
            d.insert_temp(
                key,
                Measured {
                    font,
                    pixels_per_point,
                    columns,
                },
            )
        });
        columns
    }

    /// What a row of `width` shows: the unit column, and the name's and unit's widths.
    /// Everything in full where it fits; with too little room the unit narrows first, down
    /// to `min_unit`, then the name, down to `min_name`; then the unit drops out, the name
    /// widening again into the room it leaves. (A channel's type and rate are in the row it
    /// is under.) `fixed` is what every row has besides (the checkbox, the room for "Bad" and
    /// the gaps between them and the name). Without `rated` rows have no rate column.
    fn fit(&self, width: f32, fixed: f32, gap: f32, min_name: f32, min_unit: f32) -> RowColumns {
        if self.unit == 0.0 {
            return RowColumns {
                name: (width - fixed).clamp(0.0, self.name),
                unit: None,
            };
        }
        let min_name = min_name.min(self.name);
        let min_unit = min_unit.min(self.unit);
        // The room for the name and the unit.
        let room = width - fixed - gap;
        if room >= min_name + min_unit {
            let name = (room - min_unit).min(self.name);
            return RowColumns {
                name,
                unit: Some((room - name).min(self.unit)),
            };
        }
        RowColumns {
            name: (width - fixed).clamp(0.0, self.name),
            unit: None,
        }
    }
}

/// What a row of the channel list shows (see [`Columns::fit`]): the name's width, and the
/// unit's, if it shows.
#[derive(Clone, Copy, Debug, PartialEq)]
struct RowColumns {
    name: f32,
    unit: Option<f32>,
}

/// The rate of a group's channels, as its row shows it: their rate if they share one, else
/// the range ("128–512 Hz", in full "128 to 512 Hz").
struct GroupRate {
    list: String,
    full: String,
    mixed: bool,
}

fn group_rate(document: &Document, view: &ViewState, group: &Group) -> GroupRate {
    let rec = &document.recording;
    // Each of the group's grids once (a recording has few), so each rate is written once.
    let mut ids: Vec<GridId> = Vec::new();
    for &c in view.order() {
        let id = rec.channel(c).grid;
        if !ids.contains(&id) && view.group_of(c) == *group {
            ids.push(id);
        }
    }
    let grids: Vec<&kymo_core::Grid> = ids.into_iter().map(|id| rec.grid(id)).collect();
    let Some(first) = grids.first() else {
        return GroupRate {
            list: String::new(),
            full: String::new(),
            mixed: false,
        };
    };
    let rate = grid_rate(first);
    if grids[1..].iter().all(|g| grid_rate(g) == rate) {
        return GroupRate {
            list: rate.clone(),
            full: rate,
            mixed: false,
        };
    }
    let rates = grids.iter().map(|g| g.rate());
    let (low, high) = rates.fold((f64::INFINITY, 0.0f64), |(l, h), r| (l.min(r), h.max(r)));
    let number = |rate: f64| format_rate(rate).trim_end_matches(" Hz").to_owned();
    GroupRate {
        list: format!("{}\u{2013}{}", number(low), format_rate(high)),
        full: format!("{} to {}", number(low), format_rate(high)),
        mixed: true,
    }
}

/// A channel's group in the order: its stream (by its index in the recording; `None`
/// without streams) and its type (see [`ViewState::group_of`]).
type Group = (Option<usize>, ChannelKind);

/// Per type row (group), the unit its channels share as shown (see [`display_unit`]; empty
/// for none), or `None` where they differ: then each channel's row shows its own.
fn shared_units(document: &Document, view: &ViewState) -> HashMap<Group, Option<String>> {
    // With the unit the group's first channel stores: a group's channels share a type, so
    // one stored in that unit shows it as the first does, and only others need theirs.
    let mut shared: HashMap<Group, (&Unit, Option<String>)> = HashMap::new();
    for (c, channel) in document.recording.channels().iter().enumerate() {
        let id = ChannelId(c as u32);
        let shown = || display_unit(view.kind_of(id), &channel.unit).0;
        match shared.get_mut(&view.group_of(id)) {
            Some((first, unit)) => {
                if **first != channel.unit && unit.as_ref().is_some_and(|u| *u != shown()) {
                    *unit = None;
                }
            }
            None => {
                shared.insert(view.group_of(id), (&channel.unit, Some(shown())));
            }
        }
    }
    shared
        .into_iter()
        .map(|(group, (_, unit))| (group, unit))
        .collect()
}

/// One row of the channel list.
#[derive(Clone, Debug, PartialEq)]
enum Item {
    /// A stream's row (by its index in the recording), above its channels.
    Stream(usize),
    /// A type's row, above its channels, and how many levels in it is (under a stream, 1).
    Kind(Group, usize),
    /// A channel, its group, and how many levels in it is (under its type or stream, 1;
    /// under both, 2).
    Channel(ChannelId, Group, usize),
    /// A stream of the file that isn't shown, at the end.
    NotShown(usize),
}

/// A row of the channel list that shows or hides the rows below it.
#[derive(Clone, Debug, PartialEq)]
enum Fold {
    Stream(usize),
    Kind(Group),
}

/// The channel list's rows: the channels in the view's order, under their types, grouped
/// by stream if `grouped` (each stream's channels under their types in it, however many
/// there are); the rows of `closed` folds left out; then the streams that aren't shown.
fn list_items(document: &Document, view: &ViewState, grouped: bool, closed: &[Fold]) -> Vec<Item> {
    let rec = &document.recording;
    let streams = rec.streams();
    let mut items = Vec::with_capacity(view.order().len() + 3 * streams.len());
    let (mut stream_now, mut group_now) = (None, None);
    for &c in view.order() {
        let group @ (stream, _) = view.group_of(c);
        let mut depth = 0;
        if grouped && let Some(s) = stream {
            if stream_now != Some(s) {
                stream_now = Some(s);
                items.push(Item::Stream(s));
            }
            if closed.contains(&Fold::Stream(s)) {
                continue;
            }
            depth = 1;
        }
        if group_now.as_ref() != Some(&group) {
            group_now = Some(group.clone());
            items.push(Item::Kind(group.clone(), depth));
        }
        if closed.contains(&Fold::Kind(group.clone())) {
            continue;
        }
        items.push(Item::Channel(c, group, depth + 1));
    }
    items.extend(
        streams
            .iter()
            .enumerate()
            .filter(|(_, s)| matches!(s.content, StreamContent::NotShown(_)))
            .map(|(i, _)| Item::NotShown(i)),
    );
    items
}

/// How the channel list names `channel`: grouped by stream, its name within its stream
/// where that is shorter ([`Channel::in_stream`](kymo_core::Channel::in_stream)), else its
/// name.
fn list_name(channel: &kymo_core::Channel, grouped: bool) -> &str {
    match &channel.in_stream {
        Some(short) if grouped => short,
        _ => &channel.name,
    }
}

/// A stream's name as the list shows it: "Stream 3" for one without a name.
fn stream_name(stream: &Stream) -> String {
    if stream.name.trim().is_empty() {
        format!("Stream {}", stream.id)
    } else {
        stream.name.clone()
    }
}

/// A visibility checkbox's hover text: what a click does to `name` ("Hide A1", "Show EEG
/// channels"), `shown` being whether it is shown (all of it, for a group).
fn visibility_hint(response: Response, shown: bool, name: &str) -> Response {
    let verb = if shown { "Hide" } else { "Show" };
    response.on_hover_text(format!("{verb} {name}"))
}

/// All channels in the view's order, under their types (see [`kind_row`]), drawing only the
/// rows in view, in columns as wide as the widest entry of any channel. Per channel:
/// whether it is shown, its name, its unit as the readout shows values (µV for EEG
/// whatever the file stores), and "Bad" for a bad channel; its type and rate are in the rows
/// above it. A click anywhere else on the row selects it (scrolling the traces to it if it
/// is off screen). A list too narrow for all of it narrows the units and names (shortened in
/// the middle) and then leaves out the unit (see [`Columns::fit`]);
/// hovering a row whose text is shortened or left out shows it all, its type and rate too,
/// as does a channel whose type's channels differ in rate. Rows under a type or stream are
/// a level in.
///
/// A recording with several streams of channels (XDF, BVRF's participants) groups them by
/// stream: a row per stream (see [`stream_row`]) above its channels, which are under their
/// types in it, as in a stream of one type, so the list always reads stream, type, channel.
/// Streams the file has but doesn't show are listed at the end, grayed, with the reason on
/// hover. Nothing of streams shows for other recordings.
///
/// A type's row has the type's swatch and menu, which set `types` (see [`type_swatch`] and
/// [`type_menu`]).
/// Returns whether one asked for the Channel Types window.
fn channel_list(
    ui: &mut Ui,
    (recording, platform): (u64, Platform),
    document: &Document,
    (view, marks): (&mut ViewState, &mut Marks),
    types: &mut TypeSettings,
    rule: f32,
) -> bool {
    let rec = &document.recording;
    let streams = rec.streams();
    let grouped = streams
        .iter()
        .filter(|s| matches!(s.content, StreamContent::Channels(_)))
        .count()
        > 1;
    let closed_key = ui.scope_id().with(("closed folds", recording));
    let mut closed: Vec<Fold> = ui.data(|d| d.get_temp(closed_key)).unwrap_or_default();
    let items = list_items(document, view, grouped, &closed);
    let mut toggled = None;
    let mut all_types = false;
    let shared = shared_units(document, view);
    let columns = Columns::of(ui, document, view, grouped, &shared);
    let row_height = ui.spacing().interact_size.y;
    let gap = LIST_GAP;
    let body = TextStyle::Body.resolve(ui.style());
    // A channel's row has no chevron.
    let level = LEVEL;
    let indent = |depth: usize| CHEVRON_WIDTH + gap + depth as f32 * level;
    // The columns are laid out for the deepest channel, and the names of the others are
    // wider by what they are less indented, so all columns after the names line up.
    let deepest = items
        .iter()
        .filter_map(|item| match item {
            Item::Channel(_, _, depth) => Some(*depth),
            _ => None,
        })
        .max()
        .unwrap_or(1);
    // The width of a row's checkbox and room for "Bad" with the gaps beside them (not the
    // indent), as the last frame laid them out (a guess before).
    let fixed_key = Id::unique("kymo channel list fixed width");
    let fixed = ui
        .data(|d| d.get_temp::<f32>(fixed_key))
        .unwrap_or(ui.spacing().interact_size.x + ui.spacing().icon_width + 3.0 * gap);
    // Ungrouped, the types whose channels differ in rate: their rows give a range.
    let mixed_rates: Vec<Group> = items
        .iter()
        .filter_map(|item| match item {
            Item::Kind(group, _) if !grouped && group_rate(document, view, group).mixed => {
                Some(group.clone())
            }
            _ => None,
        })
        .collect();
    let mut measured = None;
    // The channels' rows in the list's order, for ⇧-click to select a range of.
    let channel_rows: Vec<ChannelId> = items
        .iter()
        .filter_map(|item| match item {
            Item::Channel(id, _, _) => Some(*id),
            _ => None,
        })
        .collect();
    let recording_kinds = view.kinds();
    // The menu at each channel row's end (a square button), always given its room, so
    // nothing moves as it shows.
    let menu = row_height;
    let mut last_bottom = f32::NEG_INFINITY;
    // Below the rows, while channels are selected.
    if !view.selected().is_empty() {
        egui::Panel::bottom("selection bar")
            .resizable(false)
            .frame(Frame::NONE.inner_margin(Margin::symmetric(0, 6)))
            .show(ui, |ui| selection_bar(ui, (view, marks), types, platform));
    }
    // The traces' scrollbar (see `scrollbar::show`) in a strip at the list's right, while
    // the list scrolls, in place of egui's: the two look and behave alike. The strip lies
    // beside the rows, not over their ends, where it would take the clicks meant for the
    // types' menus. The wheel and the trackpad scroll the list as egui does; the bar
    // follows the offset the list had at the end of the last frame, and moves it at once.
    let spacing = ui.spacing().item_spacing.y;
    let content = ((row_height + spacing) * items.len() as f32 - spacing).max(0.0);
    let room = ui.available_rect_before_wrap();
    let scrolls = content > room.height();
    let offset_key = ui.scope_id().with("channel list offset");
    let offset = ui
        .data(|d| d.get_temp::<(u64, f32)>(offset_key))
        .filter(|(r, _)| *r == recording)
        .map_or(0.0, |(_, offset)| offset)
        .clamp(0.0, (content - room.height()).max(0.0));
    let mut area = from_top(ui, "channels", recording, ScrollArea::vertical())
        .id_salt("channels")
        .auto_shrink(false)
        .scroll_bar_visibility(ScrollBarVisibility::AlwaysHidden);
    let mut rows_rect = room;
    if scrolls {
        rows_rect.max.x -= scrollbar::THICKNESS;
        let strip = Rect::from_x_y_ranges(rows_rect.right()..=room.right(), room.y_range());
        let visible = f64::from(offset)..f64::from(offset + room.height());
        let id = ui.scope_id().with("channel list scrollbar");
        if let Some(start) = scrollbar::show(ui, id, strip, true, f64::from(content), visible) {
            area = area.vertical_scroll_offset(start as f32);
        }
    }
    let rows = UiBuilder::new().id_salt("channel rows").max_rect(rows_rect);
    let output = ui
        .scope_builder(rows, |ui| {
            area.show_rows(ui, row_height, items.len(), |ui, rows| {
                let fit = columns.fit(
                    ui.available_width(),
                    fixed + indent(deepest) + menu + gap,
                    gap,
                    LIST_MIN_NAME_EMS * body.size,
                    LIST_MIN_UNIT_EMS * body.size,
                );
                for i in rows {
                    let sense = match items[i] {
                        Item::Channel(..) => Sense::click(),
                        _ => Sense::hover(),
                    };
                    let (rect, row_response) =
                        ui.allocate_exact_size(vec2(ui.available_width(), row_height), sense);
                    last_bottom = rect.bottom();
                    if i % 2 == 1 {
                        stripe(ui, rect);
                    }
                    let pointed = ui.rect_contains_pointer(rect);
                    if let Item::Channel(id, _, _) = items[i]
                        && view.is_selected(id)
                    {
                        let fill = ui.visuals().selection.bg_fill.gamma_multiply(0.4);
                        ui.painter().rect_filled(rect, 0.0, fill);
                    }
                    // What is left past the row's right edge is clipped.
                    let mut row = ui.new_child(
                        UiBuilder::new()
                            .max_rect(rect)
                            .layout(Layout::left_to_right(Align::Center)),
                    );
                    row.spacing_mut().item_spacing.x = gap;
                    let ui = &mut row;
                    // A cell of exactly `width`, the label at its left, or at its right with
                    // `Layout::right_to_left`.
                    let cell = |ui: &mut Ui, width: f32, layout: Layout, label: Label| {
                        let (_, rect) = ui.allocate_space(vec2(width, row_height));
                        // A child that leaves the row's cursor after the whole cell.
                        let builder = UiBuilder::new().max_rect(rect).layout(layout);
                        ui.new_child(builder).add(label)
                    };
                    let left = Layout::left_to_right(Align::Center);

                    let (id, group, depth) = match items[i].clone() {
                        Item::Stream(s) => {
                            let open = !closed.contains(&Fold::Stream(s));
                            if stream_row(ui, document, s, open, view, row_response) {
                                toggled = Some(Fold::Stream(s));
                            }
                            continue;
                        }
                        Item::Kind(group, depth) => {
                            let fold = Fold::Kind(group.clone());
                            let in_stream = group
                                .0
                                .filter(|_| grouped)
                                .map(|s| stream_name(&streams[s]));
                            let name = kind_label(&group.1, in_stream.as_deref());
                            ui.add_space(depth as f32 * level);
                            let rate = in_stream
                                .is_none()
                                .then(|| group_rate(document, view, &group));
                            let unit = shared
                                .get(&group)
                                .cloned()
                                .flatten()
                                .filter(|u| !u.is_empty());
                            let kind = KindRow {
                                group,
                                unit,
                                label: name,
                                open: !closed.contains(&fold),
                                // Under a stream, the stream's row has the rate.
                                rate,
                            };
                            let (folded, asked) = kind_row(ui, view, kind, types, row_response);
                            if folded {
                                toggled = Some(fold);
                            }
                            all_types |= asked;
                            continue;
                        }
                        Item::NotShown(s) => {
                            not_shown_row(ui, &streams[s], row_response);
                            continue;
                        }
                        Item::Channel(id, group, depth) => (id, group, depth),
                    };
                    let channel = rec.channel(id);
                    ui.add_space(indent(depth));
                    let name_width = fit.name + (deepest - depth) as f32 * level;
                    let name = &channel.name;
                    // Under its stream, the name within it ("X" for "ACC X").
                    let listed = list_name(channel, grouped);
                    let grid = rec.grid(channel.grid);
                    let (unit, _) = display_unit(view.kind_of(id), &channel.unit);
                    // Its own unit only where its type row's channels differ in unit; otherwise
                    // the type row shows it.
                    let own_unit = shared.get(&group).is_some_and(Option::is_none);
                    let unit_width = fit.unit.filter(|_| own_unit);
                    // The mark of a channel scaled on its own follows its name, within the
                    // name's column.
                    let alone = view.scaled_alone(id);
                    let mark = if alone { alone_mark_width(ui) } else { 0.0 };
                    let short_name = elide_middle(ui.ctx(), listed, &body, name_width - mark);
                    let short_unit =
                        unit_width.and_then(|width| elide_middle(ui.ctx(), &unit, &body, width));
                    // The row's whole text where some of it is shortened or left out, or its
                    // rate isn't its type's alone.
                    let mixed = mixed_rates.contains(&group);
                    let full = (short_name.is_some()
                        || listed != name
                        || short_unit.is_some()
                        || (own_unit && unit_width.is_none())
                        || mixed)
                        .then(|| {
                            let unit = if unit.is_empty() {
                                String::new()
                            } else {
                                format!(" \u{b7} {unit}")
                            };
                            format!(
                                "{name}\n{}{unit} \u{b7} {}",
                                view.kind_of(id).name(),
                                grid_rate(grid)
                            )
                        });

                    let mut shown = view.is_shown(id);
                    let response = ui.add(Checkbox::without_text(&mut shown));
                    accessible_label(ui, &response, format!("Show {name}"));
                    let response = visibility_hint(response, shown, name);
                    if response.changed() {
                        view.set_shown(id, shown);
                    }

                    // A bad channel's name isn't grayed as its trace is: "Bad" after it says so.
                    let bad = view.is_bad(id);
                    let color = if shown {
                        type_color(view.kind_of(id), types, ui.visuals())
                    } else {
                        ui.visuals().weak_text_color()
                    };
                    let text = short_name.as_deref().unwrap_or(listed);
                    let label = Label::new(RichText::new(text).color(color))
                        .sense(Sense::click())
                        .selectable(false)
                        .extend();
                    let mut response = cell(ui, name_width, left, label);
                    if text != name {
                        accessible_text(ui, &response, name.clone());
                    }
                    if alone && alone_mark(ui, response.rect, platform, id) {
                        view.rejoin(&[id]);
                    }
                    if let Some(full) = &full {
                        response = response.on_hover_text(full);
                    }
                    // The whole row is one target: a click selects it alone (or takes the only
                    // selected row out again), ⌘-click adds or takes it out, ⇧-click selects the
                    // rows from the last clicked; each scrolls the traces to it if it is off
                    // screen.
                    if response.hovered() || row_response.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }
                    if response.clicked() || row_response.clicked() {
                        let modifiers = ui.input(|i| i.modifiers);
                        if modifiers.command {
                            view.toggle_selected(id);
                        } else if modifiers.shift {
                            view.select_range(&channel_rows, id);
                        } else if view.selected() == [id] {
                            view.clear_selection();
                        } else {
                            view.select(id);
                        }
                        view.reveal(id);
                    }

                    // "Bad" on a bad channel's row, as text, not a button (a click on the name
                    // beside the traces marks it, as does its menu); its room is taken on every
                    // row, so the columns after it line up.
                    // In the text color, whatever the channel's type.
                    let text = RichText::new("Bad")
                        .small()
                        .color(ui.visuals().text_color());
                    let width = WidgetText::from(text.clone())
                        .into_galley(
                            ui,
                            Some(egui::TextWrapMode::Extend),
                            f32::INFINITY,
                            TextStyle::Body,
                        )
                        .size()
                        .x
                        + 2.0 * ui.spacing().button_padding.x;
                    let (room, _) = ui.allocate_exact_size(vec2(width, row_height), Sense::hover());
                    if bad {
                        let response = ui
                        .put(room, Label::new(text).selectable(false))
                        .on_hover_text(
                            "A bad channel: its menu, or a click on its name beside the traces, \
                             marks it good.",
                        );
                        accessible_text(ui, &response, format!("{name} bad"));
                    }
                    // Everything but the indent and the name, without the gap before the next
                    // column; in whole points, so positions rounded to pixels don't change it
                    // from frame to frame.
                    let others =
                        ui.cursor().left() - rect.left() - indent(depth) - name_width - gap;
                    measured = Some(others.ceil());

                    // Not selectable, as the list's other labels.
                    let weak = |text: &str| {
                        Label::new(RichText::new(text).weak())
                            .selectable(false)
                            .extend()
                    };
                    // Each cell shows the row's whole text too, as the name hides the row from
                    // the pointer.
                    let mut cells = vec![row_response];
                    if let Some(width) = unit_width {
                        let response = cell(
                            ui,
                            width,
                            left,
                            weak(short_unit.as_deref().unwrap_or(&unit)),
                        );
                        if short_unit.is_some() {
                            accessible_text(ui, &response, unit.clone());
                        }
                        cells.push(response);
                    }
                    // The menu acts on its row alone, selected or not (what acts on the
                    // selection is in the bar below the list), so it shows only on hover.
                    let ids = [id];
                    let kinds = [view.kind_of(id).clone()];
                    let targets = targets_of(view, &ids, &kinds);
                    let dots = if pointed { Dots::Weak } else { Dots::Hidden };
                    let choice = ui
                        .with_layout(Layout::right_to_left(Align::Center), |ui| {
                            channel_menu(
                                ui,
                                name,
                                types,
                                (&targets, &recording_kinds),
                                dots,
                                row_height,
                            )
                        })
                        .inner;
                    retype(ui, view, &ids, choice.retype);
                    if choice.rejoin {
                        view.rejoin(&ids);
                    }
                    if let Some(bad) = choice.bad {
                        *marks = Some((vec![id], bad));
                    }
                    all_types |= choice.all_types;
                    if let Some(full) = full {
                        for response in cells {
                            response.on_hover_text(&full);
                        }
                    }
                }
            })
        })
        .inner;
    let area = output.inner_rect;
    ui.data_mut(|d| d.insert_temp(offset_key, (recording, output.state.clamped_offset().y)));
    // A click below the rows clears the selection.
    let below = ui.input(|i| {
        i.pointer.primary_clicked()
            && i.pointer
                .interact_pos()
                .is_some_and(|p| area.contains(p) && p.y > last_bottom)
    });
    if below {
        view.clear_selection();
    }
    if let Some(width) = measured
        && width != fixed
    {
        ui.data_mut(|d| d.insert_temp(fixed_key, width));
        ui.ctx().request_repaint();
    }
    // Every row that folds, with nothing folded.
    let folds: Vec<Fold> = list_items(document, view, grouped, &[])
        .into_iter()
        .filter_map(|item| match item {
            Item::Stream(s) => Some(Fold::Stream(s)),
            Item::Kind(group, _) => Some(Fold::Kind(group)),
            _ => None,
        })
        .collect();
    let any_open = folds.iter().any(|f| !closed.contains(f));
    if fold_all(ui, "Channels", area, rule, any_open) {
        closed = if any_open { folds.clone() } else { Vec::new() };
        ui.data_mut(|d| d.insert_temp(closed_key, closed.clone()));
    }
    if let Some(fold) = toggled {
        let open = !closed.contains(&fold);
        let level = |f: &Fold| matches!(f, Fold::Stream(_)) == matches!(fold, Fold::Stream(_));
        if ui.input(|i| i.modifiers.alt) {
            // Every row of its level (streams, or types) as this one goes.
            closed.retain(|f| !level(f));
            if open {
                closed.extend(folds.iter().filter(|f| level(f)).cloned());
            }
        } else if open {
            closed.push(fold);
        } else {
            closed.retain(|f| *f != fold);
        }
        ui.data_mut(|d| d.insert_temp(closed_key, closed));
    }
    all_types
}

/// The selection bar below the channel list, while channels are selected: how many, then
/// what acts on them all: Mark (Mark as Bad for those that are good, Mark as Good for those
/// that are bad, saying how many each marks), Type (Change Type To's items) and Scale (up
/// and down, with their shortcuts on `platform` and the scroll gesture, and Scale With
/// Type), wrapped where the sidebar is narrow.
fn selection_bar(
    ui: &mut Ui,
    (view, marks): (&mut ViewState, &mut Marks),
    types: &mut TypeSettings,
    platform: Platform,
) {
    let row_height = ui.spacing().interact_size.y;
    let selected = view.selected();
    let n = selected.len();
    ui.horizontal(|ui| {
        ui.set_min_height(row_height);
        ui.label(format!("{n} channel{} selected", plural(n)));
    });
    let kinds: Vec<ChannelKind> = selected.iter().map(|&c| view.kind_of(c).clone()).collect();
    let targets = targets_of(view, &selected, &kinds);
    let recording_kinds = view.kinds();
    let (mut mark, mut picked, mut scale, mut rejoin) = (None, None, None, false);
    // Wrapped, so a narrow sidebar shows them all.
    ui.horizontal_wrapped(|ui| {
        ui.set_min_height(row_height);
        bar_menu(ui, "Mark", |ui| {
            for (count, bad) in [(n - targets.bad, true), (targets.bad, false)] {
                if count > 0 && ui.button(mark_label(count, n, bad)).clicked() {
                    mark = Some(bad);
                }
            }
        });
        bar_menu(ui, "Type", |ui| {
            picked = change_type_items(ui, &targets, &recording_kinds, types);
        });
        bar_menu(ui, "Scale", |ui| {
            for (action, label) in [
                (Action::ScaleSelectedUp, "Scale Up"),
                (Action::ScaleSelectedDown, "Scale Down"),
            ] {
                let mut button = egui::Button::new(label);
                if let Some(chord) = Keymap::chord(action) {
                    button = button.shortcut_text(chord.display(platform));
                }
                if ui.add(button).clicked() {
                    scale = Some(action);
                }
            }
            if ui
                .add_enabled(targets.scaled_alone, egui::Button::new(SCALE_WITH_TYPE))
                .clicked()
            {
                rejoin = true;
            }
            ui.separator();
            let keys = Keymap::new(platform).channel_scale_keys();
            ui.label(RichText::new(format!("Or {keys}-scroll over one of them.")).weak());
        });
    });
    if let Some(bad) = mark {
        *marks = Some((selected.clone(), bad));
    }
    retype(ui, view, &selected, picked);
    if let Some(action) = scale {
        view.apply(action);
    }
    if rejoin {
        view.rejoin(&selected);
    }
}

/// A button named `label` with a chevron after it, which opens a menu of `add_contents`.
fn bar_menu(ui: &mut Ui, label: &str, add_contents: impl FnOnce(&mut Ui)) {
    let icon = egui::IdSalt::new("chevron");
    let button = egui::Button::new((label, egui::Atom::custom(icon, vec2(CHEVRON_WIDTH, 1.0))));
    let atoms = button.atom_ui(ui);
    if let Some(rect) = atoms.rect(icon) {
        let color = ui.visuals().text_color();
        chevron(ui.painter(), rect.center(), true, color);
    }
    egui::Popup::menu(&atoms.response).show(add_contents);
}

/// What the mark of a channel scaled on its own is named for screen readers (and tests).
pub const ALONE_LABEL: &str = "Scaled on its own";

/// The room the mark of a channel scaled on its own takes after its name, with the gap
/// before it.
fn alone_mark_width(ui: &Ui) -> f32 {
    ui.text_style_height(&TextStyle::Body) * 0.75
}

/// The mark of `channel`, scaled on its own, right after its name (`name`, the name's
/// label): a vertical arrow pointing both ways, weak, explained on hover, a button that
/// returns the channel to its type's scale. Returns whether it was clicked.
fn alone_mark(ui: &mut Ui, name: egui::Rect, platform: Platform, channel: ChannelId) -> bool {
    let width = alone_mark_width(ui);
    let height = ui.text_style_height(&TextStyle::Body) * 0.8;
    let rect = egui::Rect::from_min_size(
        egui::pos2(name.right(), name.center().y - height / 2.0),
        vec2(width, height),
    );
    let response = ui.interact(
        rect,
        Id::unique(("kymo scaled alone", channel)),
        Sense::click(),
    );
    ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_role(egui::accesskit::Role::Button);
        node.set_label(ALONE_LABEL);
    });
    let color = if response.hovered() {
        ui.visuals().strong_text_color()
    } else {
        ui.visuals().weak_text_color()
    };
    let x = rect.right() - width * 0.4;
    let (top, bottom) = (rect.top() + 1.0, rect.bottom() - 1.0);
    let head = width * 0.22;
    let stroke = egui::Stroke::new(1.2, color);
    let painter = ui.painter();
    painter.line_segment([egui::pos2(x, top), egui::pos2(x, bottom)], stroke);
    for (tip, dy) in [(top, head), (bottom, -head)] {
        painter.line_segment([egui::pos2(x, tip), egui::pos2(x - head, tip + dy)], stroke);
        painter.line_segment([egui::pos2(x, tip), egui::pos2(x + head, tip + dy)], stroke);
    }
    let default_scale = Keymap::chord(Action::DefaultScale)
        .map(|c| format!(" ({})", c.display(platform)))
        .unwrap_or_default();
    response
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text(format!(
            "{ALONE_LABEL}\n\nClick to scale it with its type (Default Scale{default_scale} does so for every channel)."
        ))
        .clicked()
}

/// A disclosure chevron centered on `center`, pointing right (closed) or down (open), as
/// outline views draw them.
fn chevron(painter: &egui::Painter, center: egui::Pos2, open: bool, color: Color32) {
    let s = 3.0;
    let points = if open {
        vec![
            center + vec2(-s, -s / 2.0),
            center + vec2(0.0, s / 2.0),
            center + vec2(s, -s / 2.0),
        ]
    } else {
        vec![
            center + vec2(-s / 2.0, -s),
            center + vec2(s / 2.0, 0.0),
            center + vec2(-s / 2.0, s),
        ]
    };
    painter.add(egui::Shape::line(points, egui::Stroke::new(1.5, color)));
}

/// A stream's row in the channel list: a disclosure arrow that shows or hides its channels'
/// rows, a checkbox that shows or hides its channels in the traces (mixed while some are
/// shown), its name (a click scrolls the traces to its first shown channel), its type, and
/// at the right its number of channels and its rate. A narrow row leaves out the type, then
/// the number and rate, then shortens the name; hovering shows it all. Returns whether the
/// arrow was clicked.
fn stream_row(
    ui: &mut Ui,
    document: &Document,
    s: usize,
    open: bool,
    view: &mut ViewState,
    row: Response,
) -> bool {
    let rec = &document.recording;
    let stream = &rec.streams()[s];
    let StreamContent::Channels(ids) = &stream.content else {
        return false;
    };
    let name = stream_name(stream);
    let grid = rec.grid(rec.channel(ids[0]).grid);
    let (shown, all) = view.stream_shown(s);
    let row_height = row.rect.height();

    let clicked = disclosure(ui, open, &name, row_height).clicked();

    let mut checked = shown == all;
    let checkbox = Checkbox::without_text(&mut checked).indeterminate(0 < shown && shown < all);
    let response = ui.add(checkbox);
    accessible_label(ui, &response, format!("Show {name}"));
    let response = visibility_hint(response, checked, &name);
    if response.changed() {
        view.set_stream_shown(s, checked);
    }

    let body = TextStyle::Body.resolve(ui.style());
    let width = |text: &str| {
        ui.fonts_mut(|f| {
            f.layout_no_wrap(text.to_owned(), body.clone(), Color32::PLACEHOLDER)
                .size()
                .x
        })
    };
    let kind = stream.kind.trim();
    let count = format!("{all} \u{b7} {}", grid_rate(grid));
    let gap = ui.spacing().item_spacing.x;
    // The room of its types' menus (square buttons), so the counts line up.
    let menu = row_height;
    let room = (ui.available_width() - menu - gap).max(0.0);
    let (name_width, kind_width, count_width) = (width(&name), width(kind), width(&count));
    let least = name_width.min(LIST_MIN_NAME_EMS * body.size);
    let beside = |kind: bool, count: bool| {
        f32::from(u8::from(kind)) * (gap + kind_width)
            + f32::from(u8::from(count)) * (gap + count_width)
    };
    let (show_kind, show_count) = [(!kind.is_empty(), true), (false, true), (false, false)]
        .into_iter()
        .find(|&(k, c)| least + beside(k, c) <= room)
        .unwrap_or((false, false));
    let name_room = (room - beside(show_kind, show_count)).clamp(0.0, name_width);
    let short = elide_middle(ui.ctx(), &name, &body, name_room);

    let channels = format!("{all} channel{}", plural(all));
    let full = [kind, &channels, &grid_rate(grid)]
        .iter()
        .filter(|t| !t.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" \u{b7} ");
    // Streams start and end at different times.
    let clock = Clock::for_end(rec.duration());
    let span = format!(
        "from {} to {}",
        format_time(grid.time_of(0), 1, clock),
        format_time(grid.time_of(grid.len()), 1, clock)
    );
    let full = format!("{name}\n{full}\n{span}");

    let (_, rect) = ui.allocate_space(vec2(name_room, row_height));
    let label = Label::new(RichText::new(short.as_deref().unwrap_or(&name)).strong())
        .sense(Sense::click())
        .selectable(false)
        .extend();
    let mut response = ui
        .new_child(
            UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::left_to_right(Align::Center)),
        )
        .add(label);
    if short.is_some() {
        accessible_text(ui, &response, name.clone());
    }
    let first_shown = view
        .order()
        .iter()
        .copied()
        .find(|c| ids.contains(c) && view.is_shown(*c));
    if let Some(first) = first_shown {
        response = response
            .on_hover_text(format!("{full}\n\nShow in Traces"))
            .on_hover_cursor(CursorIcon::PointingHand);
        if response.clicked() {
            view.scroll_to(first);
        }
    } else {
        response.on_hover_text(&full);
    }
    let weak = |text: &str| {
        Label::new(RichText::new(text).weak())
            .selectable(false)
            .extend()
    };
    // The name hides the row from the pointer: each label shows the row's whole text too.
    let mut cells = vec![row];
    if show_kind {
        cells.push(ui.add(weak(kind)));
    }
    if show_count {
        let response = ui
            .with_layout(Layout::right_to_left(Align::Center), |ui| {
                // The menu and the gap after it, which spaces aren't given.
                ui.add_space(menu + gap);
                ui.add(weak(&count))
            })
            .inner;
        cells.push(response);
    }
    for cell in cells {
        cell.on_hover_text(&full);
    }
    clicked
}

/// The size of a type's swatch in the channel list, as the legend's descriptions' are.
fn swatch_size(ui: &Ui) -> egui::Vec2 {
    let side = ui.text_style_height(&TextStyle::Body) * 0.7;
    vec2(side, side)
}

/// A fold's disclosure chevron in a row `height` high, pointing down while `open`, named
/// "Collapse {name}" or "Expand {name}" for screen readers.
fn disclosure(ui: &mut Ui, open: bool, name: &str, height: f32) -> Response {
    let (rect, arrow) = ui.allocate_exact_size(vec2(CHEVRON_WIDTH, height), Sense::click());
    let color = if arrow.hovered() {
        ui.visuals().strong_text_color()
    } else {
        ui.visuals().weak_text_color()
    };
    chevron(ui.painter(), rect.center(), open, color);
    let verb = if open { "Collapse" } else { "Expand" };
    accessible_label(ui, &arrow, format!("{verb} {name}"));
    arrow.on_hover_cursor(CursorIcon::PointingHand)
}

/// What a type's row's controls name it: "EEG channels", or under a stream "EEG channels in
/// Amp", as several streams can have one (and a channel can be named as its type).
fn kind_label(kind: &ChannelKind, stream: Option<&str>) -> String {
    match stream {
        Some(stream) => format!("{} channels in {stream}", kind.name()),
        None => format!("{} channels", kind.name()),
    }
}

/// A type's row in the channel list, as [`kind_row`] shows it.
struct KindRow {
    group: Group,
    /// The unit its channels share, if they share one.
    unit: Option<String>,
    /// What its controls are named (see [`kind_label`]).
    label: String,
    open: bool,
    /// Its channels' rate, unless the row above gives it.
    rate: Option<GroupRate>,
}

/// A type's row in the channel list: a chevron that shows or hides its channels' rows, a
/// checkbox that shows or hides them in the traces (mixed while some are shown), its swatch
/// (see [`type_swatch`]), its name in its trace color (a click scrolls the traces to its
/// first shown channel), the unit its channels share, if they do (see [`shared_units`]), at
/// the right its number of channels and, unless a stream's row above gives it, their rate
/// ("2 · 512 Hz"), as a stream's row has them, and its menu at the end (see [`type_menu`]);
/// a narrow row leaves the count and rate out, then the unit, and hovering shows it all.
/// Returns whether the chevron was clicked, and whether the menu asked for the Channel
/// Types window.
fn kind_row(
    ui: &mut Ui,
    view: &mut ViewState,
    spec: KindRow,
    types: &mut TypeSettings,
    row: Response,
) -> (bool, bool) {
    let KindRow {
        group,
        unit,
        label,
        open,
        rate,
    } = spec;
    let (stream, kind) = group;
    let name = kind.name();
    let (shown, all) = view.group_shown(stream, &kind);
    let row_height = row.rect.height();
    let clicked = disclosure(ui, open, &label, row_height).clicked();

    let mut checked = shown == all;
    let checkbox = Checkbox::without_text(&mut checked).indeterminate(0 < shown && shown < all);
    let response = ui.add(checkbox);
    accessible_label(ui, &response, format!("Show {label}"));
    let response = visibility_hint(response, checked, &label);
    if response.changed() {
        view.set_group_shown(stream, &kind, checked);
    }
    type_swatch(ui, &kind, &label, types, swatch_size(ui));

    let body = TextStyle::Body.resolve(ui.style());
    let width = |text: &str| {
        ui.fonts_mut(|f| {
            f.layout_no_wrap(text.to_owned(), body.clone(), Color32::PLACEHOLDER)
                .size()
                .x
        })
    };
    let channels = format!("{all} channel{}", plural(all));
    let in_unit = unit
        .as_ref()
        .map(|u| format!(" \u{b7} {u}"))
        .unwrap_or_default();
    let (count, full) = match &rate {
        Some(rate) => (
            format!("{all} \u{b7} {}", rate.list),
            format!("{name}\n{channels}{in_unit} \u{b7} {}", rate.full),
        ),
        None => (all.to_string(), format!("{name}\n{channels}{in_unit}")),
    };
    let gap = ui.spacing().item_spacing.x;
    // The menu at the end of the row (a square button), always.
    let menu = row_height;
    let room = (ui.available_width() - menu - gap).max(0.0);
    let name_width = width(name);
    // With too little room the count goes first, then the unit.
    let unit_width = unit.as_deref().map_or(0.0, |u| gap + width(u));
    let count_width = gap + width(&count);
    let (show_unit, show_count) = [(true, true), (true, false), (false, false)]
        .into_iter()
        .find(|&(u, c)| {
            name_width + f32::from(u8::from(u)) * unit_width + f32::from(u8::from(c)) * count_width
                <= room
        })
        .unwrap_or((false, false));
    let show_unit = show_unit && unit.is_some();
    let name_room = if show_unit || show_count {
        name_width
    } else {
        room.clamp(0.0, name_width)
    };

    let color = type_color(&kind, types, ui.visuals());
    let (_, rect) = ui.allocate_space(vec2(name_room, row_height));
    let text = Label::new(RichText::new(name).color(color).strong())
        .sense(Sense::click())
        .selectable(false)
        .truncate();
    let mut response = ui
        .new_child(
            UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::left_to_right(Align::Center)),
        )
        .add(text);
    let first_shown = view
        .order()
        .iter()
        .copied()
        .find(|&c| view.group_of(c) == (stream, kind.clone()) && view.is_shown(c));
    if let Some(first) = first_shown {
        response = response
            .on_hover_text(format!("{full}\n\nShow in Traces"))
            .on_hover_cursor(CursorIcon::PointingHand);
        if response.clicked() {
            view.scroll_to(first);
        }
    } else {
        response.on_hover_text(&full);
    }
    // The name hides the row from the pointer: the unit and count show the row's whole text
    // too.
    let mut cells = vec![row];
    if let Some(unit) = unit.filter(|_| show_unit) {
        cells.push(
            ui.add(
                Label::new(RichText::new(unit).weak())
                    .selectable(false)
                    .extend(),
            ),
        );
    }
    let ids: Vec<ChannelId> = view
        .order()
        .iter()
        .copied()
        .filter(|&c| view.group_of(c) == (stream, kind.clone()))
        .collect();
    let kinds = [kind.clone()];
    let targets = targets_of(view, &ids, &kinds);
    let recording = view.kinds();
    let choice = ui
        .with_layout(Layout::right_to_left(Align::Center), |ui| {
            let choice = type_menu(
                ui,
                &kind,
                &label,
                types,
                (&targets, &recording),
                ids.len(),
                row_height,
            );
            if show_count {
                cells.push(
                    ui.add(
                        Label::new(RichText::new(count).weak())
                            .selectable(false)
                            .extend(),
                    ),
                );
            }
            choice
        })
        .inner;
    for cell in cells {
        cell.on_hover_text(&full);
    }
    retype(ui, view, &ids, choice.retype);
    if choice.select {
        view.select_group(stream, &kind);
    }
    (clicked, choice.all_types)
}

/// What a [`change_type_menu`](crate::ui::types::change_type_menu) for `ids` needs: their
/// types (`kinds`, all of them or the one they share), the file's type where they share
/// one, whether any type differs from the file's, and whether any is scaled on its own.
fn targets_of<'a>(view: &ViewState, ids: &[ChannelId], kinds: &'a [ChannelKind]) -> Targets<'a> {
    let file_kind = ids.first().map(|&c| view.file_kind(c).clone());
    let file_kind = file_kind.filter(|k| ids.iter().all(|&c| view.file_kind(c) == k));
    Targets {
        kinds,
        file_kind,
        retyped: ids.iter().any(|&c| view.retyped(c)),
        scaled_alone: ids.iter().any(|&c| view.scaled_alone(c)),
        bad: ids.iter().filter(|&&c| view.is_bad(c)).count(),
    }
}

/// Changes the types of `ids` as a menu picked, redrawing at once, as the list's rows were
/// laid out before.
fn retype(ui: &Ui, view: &mut ViewState, ids: &[ChannelId], picked: Option<Retype>) {
    let changed = match picked {
        Some(Retype::To(kind)) => view.set_kind(ids, &kind),
        Some(Retype::FromFile) => view.kinds_from_file(ids),
        None => false,
    };
    if changed {
        ui.ctx().request_repaint();
    }
}

/// A stream the file has but kymo doesn't show, grayed, in line with the streams' names;
/// hovering it says why.
fn not_shown_row(ui: &mut Ui, stream: &Stream, row: Response) {
    let StreamContent::NotShown(reason) = &stream.content else {
        return;
    };
    let name = stream_name(stream);
    let height = row.rect.height();
    // Where a stream's chevron and checkbox are.
    ui.allocate_exact_size(vec2(CHEVRON_WIDTH, height), Sense::hover());
    ui.add_visible(false, Checkbox::without_text(&mut false));
    let hover = format!("{name} isn't shown: {reason}");
    let weak = |text: &str| {
        Label::new(RichText::new(text).weak())
            .selectable(false)
            .truncate()
    };
    let mut cells = vec![row, ui.add(weak(&name))];
    let kind = stream.kind.trim();
    if !kind.is_empty() {
        cells.push(ui.add(weak(kind)));
    }
    for cell in cells {
        cell.on_hover_text(&hover);
    }
}

/// The whole text of a label that shows it shortened, for screen readers (and tests), which
/// read a label's text as its value.
fn accessible_text(ui: &Ui, response: &Response, text: String) {
    ui.ctx()
        .accesskit_node_builder(response.id, |node| node.set_value(text));
}

/// Names a widget whose text doesn't say what it is for, for screen readers (and tests).
fn accessible_label(ui: &Ui, response: &Response, label: String) {
    ui.ctx()
        .accesskit_node_builder(response.id, |node| node.set_label(label));
}

/// `area` scrolled to its top the first time it shows for `recording`. egui keeps scroll
/// offsets by id, so another recording would start where the last one was, and, as egui's
/// memory is saved, so would the first one after a restart.
fn from_top(ui: &Ui, salt: &str, recording: u64, area: ScrollArea) -> ScrollArea {
    let id = ui.scope_id().with(("scrolled for", salt));
    let first = ui.data_mut(|d| {
        let first = d.get_temp::<u64>(id) != Some(recording);
        d.insert_temp(id, recording);
        first
    });
    if first {
        area.vertical_scroll_offset(0.0)
    } else {
        area
    }
}
