//! The menu bar drawn by egui (Linux, where there is no native one), the shortcut overview
//! and the About window. Native menus are in `native_menu`.

use eframe::egui::accesskit::Role;
use eframe::egui::{
    self, Align, Atom, Button, IdSalt, Label, Layout, Response, RichText, Sense, Shape, StrokeKind,
    TextStyle, Ui, UiBuilder, Vec2, WidgetInfo, pos2, vec2,
};

use crate::action::Action;
use crate::keymap::Platform;
use crate::menu::{Item, SystemItem, menu_bar};
use crate::shortcuts::overview;
use crate::ui::{Overlay, theme, window_title};

/// What a menu shows for an action: whether it can run now, and its checkmark if it has
/// one.
pub trait MenuState {
    fn enabled(&self, action: Action) -> bool;
    /// `Some(on)` for checkable items.
    fn checked(&self, action: Action) -> Option<bool>;
    /// The labels of File → Open Recent, newest first (see [`crate::recent::Recent::labels`]).
    fn recent(&self) -> Vec<String>;
}

/// The menu bar of `platform` drawn by egui. Returns the action of a clicked item.
pub fn egui_menu_bar(ui: &mut Ui, platform: Platform, state: &dyn MenuState) -> Option<Action> {
    let mut clicked = None;
    egui::MenuBar::new().ui(ui, |ui| {
        for menu in menu_bar(platform) {
            ui.menu_button(menu.title, |ui| {
                menu_items(ui, &menu.items, platform, state, &mut clicked);
            });
        }
    });
    clicked
}

/// Draws `items` into an open menu; `clicked` gets the action of a clicked item.
fn menu_items(
    ui: &mut Ui,
    items: &[Item],
    platform: Platform,
    state: &dyn MenuState,
    clicked: &mut Option<Action>,
) {
    for item in items {
        match item {
            Item::Action {
                action,
                label,
                shortcut,
                ..
            } => {
                let shortcut = shortcut.map(|chord| chord.display(platform));
                let enabled = state.enabled(*action);
                let response = match state.checked(*action) {
                    Some(on) => check_item(ui, enabled, on, action.radio(), label, shortcut),
                    None => {
                        let mut button = Button::new(*label);
                        if let Some(shortcut) = shortcut {
                            button = button.shortcut_text(shortcut);
                        }
                        ui.add_enabled(enabled, button)
                    }
                };
                if response.clicked() {
                    *clicked = Some(*action);
                    ui.close();
                }
            }
            // Only macOS has system items, and its menus are native.
            Item::System(SystemItem::About) => {
                if ui.button(SystemItem::About.label()).clicked() {
                    *clicked = Some(Action::About);
                    ui.close();
                }
            }
            Item::System(_) => {}
            Item::Separator => {
                ui.separator();
            }
            Item::Submenu { title, items } => {
                ui.menu_button(*title, |ui| menu_items(ui, items, platform, state, clicked));
            }
            Item::Recent => {
                ui.menu_button(RECENT_TITLE, |ui| {
                    let labels = state.recent();
                    for (index, label) in labels.iter().enumerate() {
                        if ui.button(label).clicked() {
                            *clicked = Some(Action::OpenRecent(index as u8));
                            ui.close();
                        }
                    }
                    if !labels.is_empty() {
                        ui.separator();
                    }
                    let clear = Action::ClearRecent;
                    if ui
                        .add_enabled(state.enabled(clear), Button::new(clear.label()))
                        .clicked()
                    {
                        *clicked = Some(clear);
                        ui.close();
                    }
                });
            }
        }
    }
}

/// A menu item with a checkbox (or a radio button, if `radio`), `on` or not, and its
/// shortcut: a button, so it highlights as the other items do, with the box drawn into room
/// left at its start (egui's selectable buttons only change their background, and its
/// checkboxes and radio buttons have no shortcut).
pub(crate) fn check_item(
    ui: &mut Ui,
    enabled: bool,
    on: bool,
    radio: bool,
    label: &str,
    shortcut: Option<String>,
) -> Response {
    let icon = IdSalt::new("check");
    let size = ui.spacing().icon_width;
    let mut button = Button::new((Atom::custom(icon, Vec2::splat(size)), label));
    if let Some(shortcut) = shortcut {
        button = button.shortcut_text(shortcut);
    }
    let atoms = ui.add_enabled_ui(enabled, |ui| button.atom_ui(ui)).inner;
    let response = atoms.response.clone();
    if let Some(rect) = atoms.rect(icon) {
        let (small, big) = ui.spacing().icon_rectangles(rect);
        // The app's look, as menus take the outline off inactive widgets.
        let style = ui.ctx().global_style();
        let widgets = &style.visuals.widgets;
        let visuals = if !enabled {
            &widgets.noninteractive
        } else if response.hovered() {
            &widgets.hovered
        } else {
            &widgets.inactive
        };
        let painter = ui.painter();
        if radio {
            // As egui's radio buttons.
            painter.circle(
                big.center(),
                big.width() / 2.0,
                visuals.bg_fill,
                visuals.bg_stroke,
            );
            if on {
                painter.circle_filled(small.center(), small.width() / 3.0, visuals.fg_stroke.color);
            }
        } else {
            // As egui's checkboxes: rounding at most 30 % of the width, so it stays a square.
            painter.rect(
                big,
                visuals.corner_radius.at_most((0.3 * size).round() as u8),
                visuals.bg_fill,
                visuals.bg_stroke,
                StrokeKind::Inside,
            );
            if on {
                painter.add(Shape::line(
                    vec![
                        pos2(small.left(), small.center().y),
                        pos2(small.center().x, small.bottom()),
                        pos2(small.right(), small.top()),
                    ],
                    visuals.fg_stroke,
                ));
            }
        }
    }
    let role = if radio {
        Role::RadioButton
    } else {
        Role::CheckBox
    };
    response.widget_info(|| WidgetInfo::selected(role, enabled, on, label));
    response
}

/// The shortcut overview, while `open`: per section a row per action, the action at the
/// left and its keys or gestures right-aligned, in columns as wide as their widest entry in
/// any section, so all sections line up.
pub fn shortcuts_window(ctx: &egui::Context, open: &mut bool, platform: Platform) {
    let sections = overview(platform);
    let style = ctx.global_style();
    let font = TextStyle::Body.resolve(&style);
    let color = style.visuals.text_color();
    let widest = |text: &dyn Fn(&crate::shortcuts::Row) -> &str| {
        let rows = sections.iter().flat_map(|s| &s.rows);
        ctx.fonts_mut(|f| {
            rows.map(|row| {
                f.layout_no_wrap(text(row).to_owned(), font.clone(), color)
                    .size()
                    .x
            })
            .fold(0.0, f32::max)
        })
    };
    let padding = 6.0;
    let width = widest(&|r| r.action) + COLUMN_GAP + widest(&|r| &r.keys) + 2.0 * padding;
    // The window's own scroll area, which egui puts at the window's border, inside its
    // stroke, the frame's margins inside it; a scroll area of ours in the window would end
    // at the window's content, a margin in, and egui keeps a bar inside its parent's clip.
    // The right margin is the scroll bar's room, so the rows end where the bar starts.
    let scroll = &style.spacing.scroll;
    let room = (scroll.bar_width + scroll.bar_inner_margin).ceil();
    let margin = style.spacing.window_margin;
    // The title bar keeps the usual margins, so its close button stays in the corner.
    let title_frame = theme::title_frame(ctx);
    let frame = title_frame.inner_margin(egui::Margin {
        right: room as i8,
        ..margin
    });
    // The window's outer width: the rows, the margins and the stroke.
    let stroke = 2.0 * style.visuals.window_stroke.width;
    let outer = width + f32::from(margin.left) + room + stroke;
    Overlay::Shortcuts.center_on_open(ctx, *open);
    egui::Window::new(window_title(ctx, Action::Shortcuts.label()))
        .id(Overlay::Shortcuts.id())
        .frame(frame)
        .title_frame(title_frame)
        .open(open)
        .collapsible(false)
        .resizable([false, true])
        .vscroll(true)
        // Exactly as wide as the rows and margins. A window never shrinks by itself (egui
        // keeps the widest size it had, across runs too), so one that was ever wider (fonts
        // or scale settling, an older layout) would keep a strip right of the scroll bar.
        .min_width(outer)
        .max_width(outer)
        .default_height(480.0)
        .show(ctx, |ui| {
            let row_height = ui.spacing().interact_size.y;
            // Whatever width the window gives them (within a point or two of `width`).
            let width = ui.available_width();
            for (i, section) in sections.iter().enumerate() {
                if i > 0 {
                    ui.add_space(10.0);
                }
                ui.label(RichText::new(section.title).strong());
                for (j, row) in section.rows.iter().enumerate() {
                    let (rect, _) = ui.allocate_exact_size(vec2(width, row_height), Sense::hover());
                    if j % 2 == 1 {
                        ui.painter()
                            .rect_filled(rect, 2.0, ui.visuals().faint_bg_color);
                    }
                    let mut cells = ui.new_child(
                        UiBuilder::new()
                            .max_rect(rect.shrink2(vec2(padding, 0.0)))
                            .layout(Layout::left_to_right(Align::Center)),
                    );
                    cells.add(Label::new(row.action).extend());
                    cells.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.add(Label::new(RichText::new(&row.keys).weak()).extend());
                    });
                }
            }
        });
}

/// Room between an action and its keys in the shortcut overview, in points.
const COLUMN_GAP: f32 = 32.0;

/// The About window (Windows and Linux; macOS uses the system's About panel).
pub fn about_window(ctx: &egui::Context, open: &mut bool) {
    Overlay::About.center_on_open(ctx, *open);
    egui::Window::new(window_title(ctx, Action::About.label()))
        .id(Overlay::About.id())
        .title_frame(theme::title_frame(ctx))
        .open(open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.heading("Kymo");
                ui.label(format!("Version {}", kymo_core::version()));
                ui.add_space(6.0);
                ui.label(ABOUT);
                ui.add_space(6.0);
                ui.label(COPYRIGHT);
                ui.label(RichText::new(LICENSE).weak());
            });
        });
}

/// The title of File → Open Recent.
pub const RECENT_TITLE: &str = "Open Recent";

/// What Kymo is, for About windows and panels.
pub const ABOUT: &str = "A viewer for large EEG and MEG recordings.";
pub const COPYRIGHT: &str = "© Kymo developers";
pub const LICENSE: &str = "BSD 3-Clause License";
