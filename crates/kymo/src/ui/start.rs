//! The window without a recording: the app's icon, Open…, and the recordings opened last,
//! each opened with a click (or the arrow keys and Return) and removed from the list with
//! the × that shows on hover. A recording that could not be opened shows its error in place
//! of the title.

use std::path::Path;

use eframe::egui::{
    self, Align, Button, Color32, CursorIcon, FontId, Key, Layout, Modifiers, Rect, RichText, Role,
    ScrollArea, Sense, TextStyle, TextureHandle, Ui, UiBuilder, WidgetInfo, vec2,
};

use super::{elide_middle, text_button};
use crate::recent::Recent;

/// The title without a recording.
pub const TITLE: &str = "Kymo";
/// The title when a recording could not be opened.
pub const FAILED: &str = "Could Not Open the Recording";
/// The tooltip of a recent recording's ×.
const REMOVE: &str = "Remove from List";

/// The column's widest, its icon's size, and the padding in a recent recording's row.
const WIDTH: f32 = 440.0;
const ICON: f32 = 96.0;
const ROW_PADDING: f32 = 6.0;

/// What was asked for on the start screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Picked {
    /// Open…
    Open,
    /// The recent recording at this index.
    Recent(usize),
    /// Remove the recent recording at this index from the list.
    Remove(usize),
    /// Clear the list.
    Clear,
}

/// The start screen's state between frames.
#[derive(Default)]
pub struct StartScreen {
    /// The row the arrow keys moved to, and whether they just did (to scroll to it).
    selected: Option<usize>,
    moved: bool,
    icon: Option<TextureHandle>,
    /// The column's height last frame, the whole list's included, and its head's (down to
    /// Open…), to place it.
    height: f32,
    head: f32,
}

impl StartScreen {
    /// Shows the screen in `ui`, with `error` (a recording that could not be opened) in place
    /// of the title. `keys` lets the arrow keys and Return pick a recent recording.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        recent: &Recent,
        error: Option<&str>,
        keys: bool,
    ) -> Option<Picked> {
        let count = recent.paths().len();
        self.selected = self.selected.filter(|&i| i < count);
        let mut picked = None;
        if keys && count > 0 {
            picked = self.keys(ui, count);
        }
        let icon = self
            .icon
            .get_or_insert_with(|| {
                let icon = crate::app::icon();
                let size = [icon.width as usize, icon.height as usize];
                let image = egui::ColorImage::from_rgba_unmultiplied(size, &icon.rgba);
                ui.ctx()
                    .load_texture("kymo icon", image, egui::TextureOptions::LINEAR)
            })
            .clone();

        let area = ui.available_rect_before_wrap();
        let width = (area.width() - 32.0).clamp(0.0, WIDTH);
        // Open… a little above the middle, the list below it, so the head stays put as the
        // list changes; higher where the list wouldn't fit otherwise, and the list scrolls
        // where it still doesn't.
        let top = (area.height() * 0.45 - self.head).min(area.height() - self.height - 16.0);
        let column = Rect::from_min_max(
            egui::pos2(area.center().x - width / 2.0, area.top() + top.max(16.0)),
            egui::pos2(area.center().x + width / 2.0, area.bottom() - 16.0),
        );
        let builder = UiBuilder::new()
            .max_rect(column)
            .layout(Layout::top_down(Align::Center));
        let shown = ui.scope_builder(builder, |ui| {
            self.column(ui, &icon, recent, error, &mut picked)
        });
        self.moved = false;
        let (head, hidden) = shown.inner;
        let height = shown.response.rect.height() + hidden;
        if (height - self.height).abs() > 0.5 || (head - self.head).abs() > 0.5 {
            (self.height, self.head) = (height, head);
            ui.ctx().request_repaint();
        }
        picked
    }

    /// Forgets the row the arrow keys moved to, as the list changes order once a
    /// recording opens.
    pub fn deselect(&mut self) {
        self.selected = None;
    }

    /// ↑ and ↓ move through the `count` recent recordings, stopping at either end; Return
    /// opens the one moved to.
    fn keys(&mut self, ui: &Ui, count: usize) -> Option<Picked> {
        let (down, up, enter) = ui.ctx().input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::Enter),
            )
        });
        self.moved = down || up;
        if down {
            self.selected = Some(self.selected.map_or(0, |i| (i + 1).min(count - 1)));
        }
        if up {
            self.selected = Some(self.selected.map_or(0, |i| i.saturating_sub(1)));
        }
        if enter {
            return self.selected.map(Picked::Recent);
        }
        None
    }

    /// The column's pieces; returns its head's height, down to Open…, and how much of the
    /// list is scrolled out of sight.
    fn column(
        &mut self,
        ui: &mut Ui,
        icon: &TextureHandle,
        recent: &Recent,
        error: Option<&str>,
        picked: &mut Option<Picked>,
    ) -> (f32, f32) {
        ui.add(egui::Image::new((icon.id(), vec2(ICON, ICON))));
        ui.add_space(8.0);
        match error {
            None => {
                ui.label(RichText::new(TITLE).size(28.0).strong());
            }
            Some(error) => {
                ui.heading(FAILED);
                ui.add_space(4.0);
                let color = ui.visuals().error_fg_color;
                ui.add(egui::Label::new(RichText::new(error).color(color)).wrap());
            }
        }
        ui.add_space(32.0);
        let open = Button::new("Open…").min_size(vec2(96.0, 0.0));
        if ui.add(open).clicked() {
            *picked = Some(Picked::Open);
        }
        let head = ui.min_rect().bottom() - ui.max_rect().top();
        let mut hidden = 0.0;
        if !recent.paths().is_empty() {
            ui.add_space(36.0);
            hidden = self.recent(ui, recent, picked);
        }
        (head, hidden)
    }

    /// The heading with Clear, and a row per recent recording in a frame, scrolling within
    /// the space left; returns how much of it is out of sight.
    fn recent(&mut self, ui: &mut Ui, recent: &Recent, picked: &mut Option<Picked>) -> f32 {
        let weak = ui.visuals().weak_text_color();
        let row_height = ui.spacing().interact_size.y;
        ui.allocate_ui_with_layout(
            vec2(ui.available_width(), row_height),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.add_space(4.0);
                ui.label(RichText::new("Recent").small().strong().color(weak));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let clear = text_button(RichText::new("Clear").small(), None);
                    if ui.add(clear).clicked() {
                        *picked = Some(Picked::Clear);
                    }
                });
            },
        );
        ui.add_space(2.0);
        let visuals = ui.visuals().clone();
        let margin = 4.0;
        let space = (ui.available_height() - 2.0 * margin).max(row_height);
        egui::Frame::new()
            .fill(visuals.faint_bg_color)
            .stroke(visuals.widgets.noninteractive.bg_stroke)
            .corner_radius(6.0)
            .inner_margin(margin)
            .show(ui, |ui| {
                let scrolled = ScrollArea::vertical()
                    .max_height(space)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        let labels = recent.labels();
                        let rows = recent.paths().iter().zip(&labels).enumerate();
                        for (index, (path, label)) in rows {
                            if let Some(row) = self.row(ui, index, path, label) {
                                *picked = Some(row);
                            }
                        }
                    });
                (scrolled.content_size.y - scrolled.inner_rect.height()).max(0.0)
            })
            .inner
    }

    /// One recent recording: its name over its folder, a click opening it (the hand on hover
    /// says so), its × removing it.
    fn row(&mut self, ui: &mut Ui, index: usize, path: &Path, label: &str) -> Option<Picked> {
        let name_font = TextStyle::Body.resolve(ui.style());
        let folder_font = TextStyle::Small.resolve(ui.style());
        let height = name_font.size + folder_font.size + 3.0 * ROW_PADDING;
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
        response.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
        // A click opens it, rather than selecting it as in other lists.
        let response = response.on_hover_cursor(CursorIcon::PointingHand);
        if self.moved && self.selected == Some(index) {
            response.scroll_to_me(None);
        }
        let hovered = ui.rect_contains_pointer(rect);
        // The pointer takes over from the arrow keys once it moves.
        if hovered && ui.input(|i| i.pointer.is_moving()) {
            self.selected = None;
        }
        let visuals = ui.visuals();
        if hovered || self.selected == Some(index) {
            let fill = visuals.widgets.hovered.weak_bg_fill;
            ui.painter().rect_filled(rect, 4.0, fill);
        }

        let remove = Rect::from_center_size(
            egui::pos2(rect.right() - ROW_PADDING - 10.0, rect.center().y),
            vec2(20.0, 20.0),
        );
        let text = rect
            .shrink(ROW_PADDING)
            .with_max_x(remove.left() - ROW_PADDING);
        let name = file_name(path);
        let folder = folder(path);
        let strong = visuals.strong_text_color();
        let weak = visuals.weak_text_color();
        let painter = ui.painter_at(text);
        let paint = |at: egui::Pos2, s: &str, font: &FontId, color: Color32| {
            let shown = elide_middle(ui.ctx(), s, font, text.width()).unwrap_or_else(|| s.into());
            painter.text(at, egui::Align2::LEFT_TOP, shown, font.clone(), color);
        };
        paint(text.left_top(), &name, &name_font, strong);
        let below = text.left_top() + vec2(0.0, name_font.size + ROW_PADDING);
        paint(below, &folder, &folder_font, weak);

        let mut picked = response.clicked().then_some(Picked::Recent(index));
        if hovered {
            let button = text_button("×", None).min_size(remove.size());
            // Placed without moving the cursor, which would shift the rows below up.
            if ui.place(remove, button).on_hover_text(REMOVE).clicked() {
                picked = Some(Picked::Remove(index));
            }
        }
        picked
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// The folder `path` is in, the home folder shortened to ~ (except on Windows, which doesn't).
fn folder(path: &Path) -> String {
    let Some(parent) = path.parent() else {
        return String::new();
    };
    if !cfg!(windows)
        && let Some(home) = std::env::home_dir()
        && let Ok(rest) = parent.strip_prefix(&home)
    {
        return if rest.as_os_str().is_empty() {
            "~".into()
        } else {
            format!("~/{}", rest.display())
        };
    }
    parent.display().to_string()
}
