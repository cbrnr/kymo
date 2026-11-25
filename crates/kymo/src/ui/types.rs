//! What sets channel types' settings: in the sidebar a type's swatch, which sets its color,
//! and its menu ("⋯"), which sets its DC removal and scaling, and the Channel Types window,
//! which lists every type kymo knows, those set and the open recording's.

use eframe::egui::containers::menu::{MenuConfig, SubMenuButton};
use eframe::egui::{
    self, Align, Button, Checkbox, CursorIcon, Layout, Popup, PopupCloseBehavior, Response,
    RichText, Sense, TextEdit, Ui, Vec2,
};
use kymo_core::{ChannelKind, KNOWN_KINDS};

use crate::document::plural;

use crate::types::{TypeSet, TypeSettings};
use crate::ui::color::{Choice, picker, swatch, swatch_button};
use crate::ui::menus::check_item;
use crate::ui::palette::type_color;
use crate::ui::{Overlay, theme, window_title};

/// The window's title, and the button that opens it.
pub const CHANNEL_TYPES: &str = "Channel Types";

/// What opens the window from a type's popup.
pub const ALL_TYPES: &str = "All Types\u{2026}";

/// What a type's DC removal checkbox is named, for screen readers.
pub fn remove_dc_label(kind: &ChannelKind) -> String {
    format!("Remove DC of {}", kind.name())
}

/// What a type's scaling checkbox is named, for screen readers.
pub fn own_scale_label(kind: &ChannelKind) -> String {
    format!("Scale Each {} Channel", kind.name())
}

/// A swatch of `size` in `kind`'s color, named "Color of {label}" for screen readers (`label`
/// names the row, as "EEG channels in Amp"): a click opens a color picker with Reset, as the
/// legend's swatches do. The type's other settings are in its [`type_menu`].
pub fn type_swatch(
    ui: &mut Ui,
    kind: &ChannelKind,
    label: &str,
    types: &mut TypeSettings,
    size: Vec2,
) {
    let color = type_color(kind, types, ui.visuals());
    let picked = types.color(kind).is_some();
    match swatch(ui, &format!("Color of {label}"), color, picked, size) {
        Some(Choice::Picked(c)) => types.set_color(kind, Some(c)),
        Some(Choice::Reset) => types.set_color(kind, None),
        None => {}
    }
}

/// How a "⋯" button's dots are drawn when the pointer isn't on it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Dots {
    /// Not at all (a channel's row, neither hovered nor selected).
    Hidden,
    Weak,
    /// In the text color: something in the menu differs from its start.
    Marked,
}

/// A square "⋯" button `height` high, named `label` for screen readers, with `hover` as its
/// tooltip, its dots drawn as `dots` says unless the pointer is on it or its menu is open
/// (then strong). The caller opens its menu.
pub fn dots_button(ui: &mut Ui, label: String, hover: String, dots: Dots, height: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(height), Sense::click());
    label_for(ui, &response, label);
    let open = Popup::is_id_open(ui.ctx(), Popup::default_response_id(&response));
    let visuals = ui.visuals();
    let color = if response.hovered() || response.is_pointer_button_down_on() || open {
        Some(visuals.strong_text_color())
    } else {
        match dots {
            Dots::Hidden => None,
            Dots::Weak => Some(visuals.weak_text_color()),
            Dots::Marked => Some(visuals.text_color()),
        }
    };
    if response.hovered() || open {
        let fill = visuals.widgets.hovered.weak_bg_fill;
        ui.painter().rect_filled(rect.shrink(1.0), 3.0, fill);
    }
    if let Some(color) = color {
        let radius = (height / 14.0).max(1.0);
        for dx in [-1.0, 0.0, 1.0] {
            let center = rect.center() + Vec2::new(dx * radius * 3.2, 0.0);
            ui.painter().circle_filled(center, radius, color);
        }
    }
    response
        .on_hover_text(hover)
        .on_hover_cursor(CursorIcon::PointingHand)
}

/// Shows a menu's contents, `add_contents`, in a scroll area that doesn't scroll, as it
/// takes the scrolling a text field in it asks for to keep its cursor in view (New Type…,
/// Other Types). Otherwise the channel list the menu opens from would scroll: its button
/// out of view, the menu would close.
fn contained(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui)) {
    egui::ScrollArea::neither()
        .id_salt("kymo menu")
        .show(ui, add_contents);
}

/// What a type's or a channel's menu asked for.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MenuChoice {
    /// The Channel Types window.
    pub all_types: bool,
    /// A change of the channels' types.
    pub retype: Option<Retype>,
    /// The channels back to their type's amplitude (a channel's menu only).
    pub rejoin: bool,
    /// The channels marked bad (`true`) or good (a channel's menu only).
    pub bad: Option<bool>,
    /// The type's channels selected (a type's menu only).
    pub select: bool,
}

/// A change of channels' types picked in a [`change_type_menu`].
#[derive(Clone, Debug, PartialEq)]
pub enum Retype {
    To(ChannelKind),
    /// Back to the types the file gives them.
    FromFile,
}

/// The channels a [`change_type_menu`] (or a [`channel_menu`]) changes, as it needs them.
pub struct Targets<'a> {
    /// Their types now.
    pub kinds: &'a [ChannelKind],
    /// The type the file gives them where they share one, else `None`.
    pub file_kind: Option<ChannelKind>,
    /// Whether any type differs from the file's.
    pub retyped: bool,
    /// Whether any was scaled on its own though its type shares an amplitude, for a
    /// channel menu's Scale With Type.
    pub scaled_alone: bool,
    /// How many are bad, for a channel menu's Mark as Bad and Mark as Good.
    pub bad: usize,
}

/// What the Change Type To menu is called, and its items.
pub const CHANGE_TYPE: &str = "Change Type To";
pub const OTHER_TYPES: &str = "Other Types";
pub const NEW_TYPE: &str = "New Type\u{2026}";
pub const FROM_FILE: &str = "From File";
/// The channel menu's item that returns channels scaled on their own to their type's
/// amplitude.
pub const SCALE_WITH_TYPE: &str = "Scale With Type";

/// The channel menu's item that marks `count` of a menu's `total` channels bad (or good):
/// "Mark as Bad" for a menu of one channel, else "Mark 3 Channels as Bad".
pub fn mark_label(count: usize, total: usize, bad: bool) -> String {
    let state = if bad { "Bad" } else { "Good" };
    match (total, count) {
        (1, _) => format!("Mark as {state}"),
        (_, 1) => format!("Mark 1 Channel as {state}"),
        _ => format!("Mark {count} Channels as {state}"),
    }
}

/// A Change Type To submenu for the channels `targets`: the recording's types (`recording`,
/// stim left out), checked where every target has it; Other Types, every other type kymo
/// knows or something was set for (stim left out), alphabetically, with a filter field
/// above; New Type…, a field to name one (a known spelling, as "EKG", is the known type;
/// stim can't be named); and From File, enabled while a type differs from the file's (with
/// the file's type where the targets share one). Naming a type kymo doesn't know keeps it
/// in `types`, so Other Types offers it from then on.
pub fn change_type_menu(
    ui: &mut Ui,
    targets: &Targets<'_>,
    recording: &[ChannelKind],
    types: &mut TypeSettings,
) -> Option<Retype> {
    let mut picked = None;
    ui.menu_button(CHANGE_TYPE, |ui| {
        picked = change_type_items(ui, targets, recording, types);
    });
    picked
}

/// The items of a [`change_type_menu`], in a menu that is open.
pub fn change_type_items(
    ui: &mut Ui,
    targets: &Targets<'_>,
    recording: &[ChannelKind],
    types: &mut TypeSettings,
) -> Option<Retype> {
    let mut picked = None;
    for kind in recording.iter().filter(|k| !k.is_stim()) {
        let all = targets.kinds.iter().all(|k| k == kind);
        if check_item(ui, true, all, true, kind.name(), None).clicked() && !all {
            picked = Some(Retype::To(kind.clone()));
        }
    }
    ui.separator();
    let config = MenuConfig::new().close_behavior(PopupCloseBehavior::CloseOnClickOutside);
    SubMenuButton::new(OTHER_TYPES)
        .config(config.clone())
        .ui(ui, |ui| {
            if let Some(kind) = other_types(ui, recording, types) {
                picked = Some(Retype::To(kind));
                ui.close();
            }
        });
    SubMenuButton::new(NEW_TYPE).config(config).ui(ui, |ui| {
        if let Some(kind) = new_type(ui) {
            if kind.known().is_none() && !types.stored(&kind) {
                types.insert(TypeSet {
                    name: kind.name().to_owned(),
                    ..TypeSet::default()
                });
            }
            picked = Some(Retype::To(kind));
            ui.close();
        }
    });
    ui.separator();
    let text = match &targets.file_kind {
        Some(kind) => format!("{FROM_FILE} ({})", kind.name()),
        None => FROM_FILE.to_owned(),
    };
    if ui.add_enabled(targets.retyped, Button::new(text)).clicked() {
        picked = Some(Retype::FromFile);
    }
    picked
}

/// The Other Types submenu's contents: a filter field and the types it matches.
fn other_types(
    ui: &mut Ui,
    recording: &[ChannelKind],
    types: &TypeSettings,
) -> Option<ChannelKind> {
    let id = ui.scope_id().with("kymo other types filter");
    let mut filter: String = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    let field = ui.add(
        TextEdit::singleline(&mut filter)
            .hint_text("Filter")
            .desired_width(OTHER_TYPES_WIDTH),
    );
    label_for(ui, &field, "Filter Other Types".to_owned());
    if !ui.memory(|m| m.focused().is_some()) {
        field.request_focus();
    }
    let others: Vec<ChannelKind> = listed_types(types, &[])
        .into_iter()
        .filter(|k| !k.is_stim() && !recording.contains(k) && matches(k, &filter))
        .collect();
    let mut picked = None;
    egui::ScrollArea::vertical()
        .max_height(ui.ctx().content_rect().height() * 0.4)
        .show(ui, |ui| {
            ui.set_min_width(OTHER_TYPES_WIDTH);
            if others.is_empty() {
                ui.label(RichText::new("No channel types match.").weak());
            }
            for kind in &others {
                if ui.button(kind.name()).clicked() {
                    picked = Some(kind.clone());
                }
            }
        });
    if picked.is_some() {
        filter.clear();
    }
    ui.data_mut(|d| d.insert_temp(id, filter));
    picked
}

/// The width of the Other Types and New Type… fields.
const OTHER_TYPES_WIDTH: f32 = 160.0;

/// The New Type… submenu's contents: a field to name the type, Enter or Change taking it,
/// and below what the name stands for (a known type spelled otherwise) or why it can't be
/// taken (empty, or stim).
fn new_type(ui: &mut Ui) -> Option<ChannelKind> {
    let id = ui.scope_id().with("kymo new type name");
    let mut name: String = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    let field = ui.add(
        TextEdit::singleline(&mut name)
            .hint_text("Name")
            .desired_width(OTHER_TYPES_WIDTH),
    );
    label_for(ui, &field, "New Type Name".to_owned());
    if !ui.memory(|m| m.focused().is_some()) {
        field.request_focus();
    }
    let kind = ChannelKind::named(&name);
    let valid = !name.trim().is_empty() && !kind.is_stim();
    let note = if name.trim().is_empty() {
        String::new()
    } else if kind.is_stim() {
        "Stim channels come from the file.".to_owned()
    } else if kind.name() != name.trim() {
        format!("As {}", kind.name())
    } else {
        String::new()
    };
    if !note.is_empty() {
        ui.label(RichText::new(note).weak().small());
    }
    // The field gives up the keyboard on Enter, which `lost_focus` shows only a frame later.
    let entered =
        (field.has_focus() || field.lost_focus()) && ui.input(|i| i.key_pressed(egui::Key::Enter));
    let change = ui.add_enabled(valid, Button::new("Change"));
    let picked = (valid && (entered || change.clicked())).then_some(kind);
    if picked.is_some() {
        name.clear();
    }
    ui.data_mut(|d| d.insert_temp(id, name));
    picked
}

/// A "⋯" button `height` high at the end of a type's row, named "Options for {label}" for
/// screen readers: gray, or in the text color while `kind`'s DC removal or scaling differs
/// from its start. A click opens a menu (no title, as it opens at the type's row): Remove DC
/// and Scale Each Channel as checkable items (neither for stim channels), Select (its
/// `channels` channels, in the channel list), Change Type To (see [`change_type_menu`]; its
/// channels are `targets`), Reset, enabled while anything differs from the type's start (its
/// color too; a custom type keeps its entry), and All Types…, which asks for the Channel
/// Types window.
pub fn type_menu(
    ui: &mut Ui,
    kind: &ChannelKind,
    label: &str,
    types: &mut TypeSettings,
    (targets, recording): (&Targets<'_>, &[ChannelKind]),
    channels: usize,
    height: f32,
) -> MenuChoice {
    let name = kind.name();
    let dots = if types.options_changed(kind) {
        Dots::Marked
    } else {
        Dots::Weak
    };
    let response = dots_button(
        ui,
        format!("Options for {label}"),
        format!("Options for {name}"),
        dots,
        height,
    );
    let mut choice = MenuChoice::default();
    Popup::menu(&response).show(|ui| {
        contained(ui, |ui| {
            if !kind.is_stim() {
                let (remove_dc, own_scale) = (types.remove_dc(kind), types.own_scale(kind));
                let item = check_item(ui, true, remove_dc, false, REMOVE_DC, None);
                label_for(ui, &item, remove_dc_label(kind));
                if item.clicked() {
                    types.set_remove_dc(kind, !remove_dc);
                }
                let item = check_item(ui, true, own_scale, false, OWN_SCALE, None);
                label_for(ui, &item, own_scale_label(kind));
                if item.clicked() {
                    types.set_own_scale(kind, !own_scale);
                }
                ui.separator();
            }
            let select = format!("Select {channels} {name} Channel{}", plural(channels));
            if ui.button(select).clicked() {
                choice.select = true;
            }
            ui.separator();
            choice.retype = change_type_menu(ui, targets, recording, types);
            ui.separator();
            let reset = ui.add_enabled(types.changed(kind), Button::new(format!("Reset {name}")));
            if reset.clicked() {
                types.reset(kind);
            }
            if ui.button(ALL_TYPES).clicked() {
                choice.all_types = true;
            }
        })
    });
    choice
}

/// A "⋯" button `height` high at the end of a channel's row, named "Options for {label}"
/// for screen readers, its dots drawn as `dots` says: a menu for that channel alone (what
/// acts on several is in the channel list's selection bar; no title, as it opens at the
/// channel's row), with Mark as Bad or Mark as Good, Change Type To (see
/// [`change_type_menu`]) and Scale With Type, enabled while it is scaled on its own.
pub fn channel_menu(
    ui: &mut Ui,
    label: &str,
    types: &mut TypeSettings,
    (targets, recording): (&Targets<'_>, &[ChannelKind]),
    dots: Dots,
    height: f32,
) -> MenuChoice {
    let response = dots_button(
        ui,
        format!("Options for {label}"),
        format!("Options for {label}"),
        dots,
        height,
    );
    let mut choice = MenuChoice::default();
    Popup::menu(&response).show(|ui| {
        contained(ui, |ui| {
            let total = targets.kinds.len();
            for (count, bad) in [(total - targets.bad, true), (targets.bad, false)] {
                if count > 0 && ui.button(mark_label(count, total, bad)).clicked() {
                    choice.bad = Some(bad);
                }
            }
            ui.separator();
            choice.retype = change_type_menu(ui, targets, recording, types);
            ui.separator();
            if ui
                .add_enabled(targets.scaled_alone, Button::new(SCALE_WITH_TYPE))
                .clicked()
            {
                choice.rejoin = true;
            }
        })
    });
    choice
}

/// The checkboxes' text, also the window's column headers.
const REMOVE_DC: &str = "Remove DC";
const OWN_SCALE: &str = "Scale Each Channel";

/// What the window marks types kymo knows and the others with.
const BUILT_IN: &str = "Built in";
const CUSTOM: &str = "Custom";

/// Names a widget for screen readers (and tests), whatever its text says.
fn label_for(ui: &Ui, response: &egui::Response, label: String) {
    ui.ctx()
        .accesskit_node_builder(response.id, |node| node.set_label(label));
}

/// The types the Channel Types window lists: every type kymo knows, every other type
/// something was set for, and the other types of the open recording (`recording`), each
/// once, by name in alphabetical order, ignoring case.
pub fn listed_types(types: &TypeSettings, recording: &[ChannelKind]) -> Vec<ChannelKind> {
    let mut listed: Vec<ChannelKind> = KNOWN_KINDS
        .iter()
        .map(|k| ChannelKind::named(k.name))
        .collect();
    let others = types
        .entries()
        .map(|t| ChannelKind::named(&t.name))
        .chain(recording.iter().cloned());
    for kind in others {
        if !listed.contains(&kind) {
            listed.push(kind);
        }
    }
    listed.sort_by_cached_key(|k| (k.name().to_lowercase(), k.name().to_owned()));
    listed
}

/// Whether `kind` matches the window's filter: its name or another of its spellings
/// contains `filter`, or "Built in" or "Custom" does, as the window marks it; ignoring case
/// and surrounding spaces. Everything matches an empty filter.
pub fn matches(kind: &ChannelKind, filter: &str) -> bool {
    let filter = filter.trim().to_lowercase();
    if filter.is_empty() {
        return true;
    }
    let origin = if kind.known().is_some() {
        BUILT_IN
    } else {
        CUSTOM
    };
    let aliases = kind.known().map_or(&[][..], |k| k.aliases);
    [kind.name(), kind.key(), origin]
        .into_iter()
        .chain(aliases.iter().copied())
        .any(|text| text.to_lowercase().contains(&filter))
}

/// The Channel Types window, while `open`: a filter field (`filter`), then a row per type
/// of [`listed_types`] that matches it ([`matches`]): its swatch (a click picks its color),
/// its name, Remove DC and Scale Each Channel (neither for stim channels), "Built in" or
/// "Custom" grayed, and Reset for a type kymo knows while something differs from its start,
/// Forget for another type something was set for (which removes it from the list unless
/// the recording has it). The rows scroll beyond most of the screen. Returns whether
/// a setting changed; the caller applies it at once.
pub fn channel_types_window(
    ctx: &egui::Context,
    open: &mut bool,
    types: &mut TypeSettings,
    recording: &[ChannelKind],
    filter: &mut String,
) -> bool {
    let before = types.clone();
    let listed = listed_types(types, recording);
    let columns = Columns::of(ctx, &listed);
    Overlay::ChannelTypes.center_on_open(ctx, *open);
    egui::Window::new(window_title(ctx, CHANNEL_TYPES))
        .id(Overlay::ChannelTypes.id())
        .title_frame(theme::title_frame(ctx))
        .open(open)
        .collapsible(false)
        .resizable(false)
        // The window's width counts its frame too; set from the first frame, which egui
        // would lay out narrower, widening the window a frame later.
        .min_width(
            columns.total()
                + egui::Frame::window(&ctx.global_style())
                    .total_margin()
                    .sum()
                    .x,
        )
        .show(ctx, |ui| {
            let field = ui.add(
                TextEdit::singleline(filter)
                    .hint_text("Filter")
                    .desired_width(columns.total()),
            );
            label_for(ui, &field, "Filter Channel Types".to_owned());
            ui.add_space(6.0);
            let shown: Vec<&ChannelKind> = listed.iter().filter(|k| matches(k, filter)).collect();
            let max_height = ui.ctx().content_rect().height() * 0.6;
            egui::ScrollArea::vertical()
                .max_height(max_height)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    if shown.is_empty() {
                        ui.label(RichText::new("No channel types match.").weak());
                        return;
                    }
                    type_grid(ui, &shown, types, &columns);
                });
            ui.add_space(6.0);
            ui.add(super::hint(
                "Remove DC centers a type's traces while Remove DC is on. Scale Each Channel \
                 scales each of its channels on its own, rather than all alike. A type's color \
                 in the sidebar sets them too.",
            ));
        });
    *types != before
}

/// Space between the window's columns.
const GAP: f32 = 12.0;

/// Space between a swatch and its type's name.
const SWATCH_GAP: f32 = 6.0;

/// The widths of the window's columns, fitting every type listed, so they stay put as the
/// filter changes what shows.
struct Columns {
    /// A row's height, also the swatch's side.
    row: f32,
    name: f32,
    remove_dc: f32,
    own_scale: f32,
    origin: f32,
    button: f32,
}

impl Columns {
    fn of(ctx: &egui::Context, listed: &[ChannelKind]) -> Columns {
        let style = ctx.global_style();
        let font = egui::TextStyle::Body.resolve(&style);
        let width = |text: &str| {
            ctx.fonts_mut(|f| {
                f.layout_no_wrap(text.to_owned(), font.clone(), egui::Color32::PLACEHOLDER)
                    .size()
                    .x
            })
        };
        let row = ctx.fonts_mut(|f| f.row_height(&font));
        let names = listed.iter().map(|k| width(k.name())).fold(0.0, f32::max);
        let padding = 2.0 * style.spacing.button_padding.x;
        Columns {
            row,
            name: row + SWATCH_GAP + names,
            remove_dc: width(REMOVE_DC),
            own_scale: width(OWN_SCALE),
            origin: width(BUILT_IN).max(width(CUSTOM)),
            button: width("Reset").max(width("Forget")) + padding,
        }
    }

    fn total(&self) -> f32 {
        self.name + self.remove_dc + self.own_scale + self.origin + self.button + 4.0 * GAP
    }
}

/// The window's rows of `shown` under a row of column headers (see
/// [`channel_types_window`]).
fn type_grid(ui: &mut Ui, shown: &[&ChannelKind], types: &mut TypeSettings, columns: &Columns) {
    let fixed = |ui: &mut Ui, width: f32, align: Align, add: &mut dyn FnMut(&mut Ui)| {
        let layout = Layout::left_to_right(Align::Center).with_main_align(align);
        ui.allocate_ui_with_layout(egui::vec2(width, columns.row), layout, |ui| {
            ui.set_min_width(width);
            add(ui);
        });
    };
    egui::Grid::new("channel types")
        .num_columns(5)
        .spacing([GAP, 4.0])
        .min_col_width(0.0)
        .striped(true)
        .show(ui, |ui| {
            let header = |text: &str| RichText::new(text).weak();
            fixed(ui, columns.name, Align::Min, &mut |ui| {
                ui.label(header("Type"));
            });
            fixed(ui, columns.remove_dc, Align::Center, &mut |ui| {
                ui.label(header(REMOVE_DC));
            });
            fixed(ui, columns.own_scale, Align::Center, &mut |ui| {
                ui.label(header(OWN_SCALE));
            });
            ui.label("");
            ui.label("");
            ui.end_row();
            for &kind in shown {
                type_row(ui, kind, types, columns, &fixed);
                ui.end_row();
            }
        });
}

/// A layout helper of [`type_grid`]: a cell of a width, its contents aligned so.
type Fixed<'a> = dyn Fn(&mut Ui, f32, Align, &mut dyn FnMut(&mut Ui)) + 'a;

/// One type's row in the window.
fn type_row(
    ui: &mut Ui,
    kind: &ChannelKind,
    types: &mut TypeSettings,
    columns: &Columns,
    fixed: &Fixed<'_>,
) {
    let name = kind.name();
    fixed(ui, columns.name, Align::Min, &mut |ui| {
        ui.spacing_mut().item_spacing.x = SWATCH_GAP;
        let color = type_color(kind, types, ui.visuals());
        let size = egui::vec2(columns.row, columns.row) * 0.8;
        let response = swatch_button(ui, &format!("Color of {name}"), color, size);
        Popup::menu(&response)
            .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                if let Some(c) = picker(ui, color) {
                    types.set_color(kind, Some(c));
                }
            });
        ui.label(name);
    });
    if kind.is_stim() {
        ui.label("");
        ui.label("");
    } else {
        // Each checkbox centered under its header.
        let mut remove_dc = types.remove_dc(kind);
        let mut own_scale = types.own_scale(kind);
        for (flag, width, label) in [
            (&mut remove_dc, columns.remove_dc, remove_dc_label(kind)),
            (&mut own_scale, columns.own_scale, own_scale_label(kind)),
        ] {
            fixed(ui, width, Align::Center, &mut |ui| {
                let response = ui.add(Checkbox::without_text(flag));
                label_for(ui, &response, label.clone());
            });
        }
        if remove_dc != types.remove_dc(kind) {
            types.set_remove_dc(kind, remove_dc);
        }
        if own_scale != types.own_scale(kind) {
            types.set_own_scale(kind, own_scale);
        }
    }
    let known = kind.known().is_some();
    ui.label(RichText::new(if known { BUILT_IN } else { CUSTOM }).weak());
    fixed(ui, columns.button, Align::Min, &mut |ui| {
        if known && types.changed(kind) {
            let reset = ui.button("Reset");
            label_for(ui, &reset, format!("Reset {name}"));
            if reset.clicked() {
                types.reset(kind);
            }
        } else if !known && types.stored(kind) {
            let forget = ui.button("Forget");
            label_for(ui, &forget, format!("Forget {name}"));
            if forget.clicked() {
                types.forget(kind);
            }
        }
    });
}
