//! Go to Time…: a small window with a field for a time from the start of the recording.

use eframe::egui::text::{CCursor, CCursorRange};
use eframe::egui::{self, Key, TextEdit};

use super::axis::parse_time;
use super::{Overlay, theme, window_title};
use crate::action::Action;

/// What the window's field holds between frames.
#[derive(Clone, Debug, Default)]
pub struct GoToField {
    pub text: String,
    /// Whether the last text tried wasn't a time.
    pub invalid: bool,
    /// Whether the field takes the keyboard, with its text selected, at the next frame.
    pub focus: bool,
}

impl GoToField {
    /// A field holding `text` (the current time), selected so typing replaces it.
    pub fn new(text: String) -> GoToField {
        GoToField {
            text,
            invalid: false,
            focus: true,
        }
    }
}

/// How times can be written, for the field's hint.
const EXAMPLES: &str = "90, 1:30 or 1:02:03.5";

/// How times can be written, in a hint below the field.
const DESCRIPTION: &str =
    "Seconds (90 or 12.5), minutes:seconds (1:30), or hours:minutes:seconds (1:02:03.5).";

/// Width of the window's content: the field's, and the description's below it.
const WIDTH: f32 = 300.0;

/// The Go to Time window, while `open`. Returns the time asked for, in seconds from the
/// start of the recording, once Enter or Go is pressed with a time in the field (which
/// closes the window); Esc in the field closes it too.
pub fn go_to_window(ctx: &egui::Context, open: &mut bool, field: &mut GoToField) -> Option<f64> {
    let mut asked = None;
    let mut close = false;
    let width = WIDTH
        + egui::Frame::window(&ctx.global_style())
            .total_margin()
            .sum()
            .x;
    Overlay::GoToTime.center_on_open(ctx, *open);
    let title = window_title(
        ctx,
        Overlay::GoToTime,
        Action::GoToTime.label().trim_end_matches('…'),
    );
    egui::Window::new(title)
        .id(Overlay::GoToTime.id())
        .title_frame(theme::title_frame(ctx))
        .open(open)
        .collapsible(false)
        .resizable(false)
        // Exactly as wide as the content plus the frame; egui's default width is wider.
        .default_width(width)
        .min_width(width)
        .max_width(width)
        .show(ctx, |ui| {
            ui.set_width(WIDTH);
            ui.label("Time from the Start of the Recording:");
            let output = TextEdit::singleline(&mut field.text)
                .hint_text(EXAMPLES)
                .desired_width(f32::INFINITY)
                .show(ui);
            let response = output.response;
            if field.focus {
                field.focus = false;
                let mut state = output.state;
                let all =
                    CCursorRange::two(CCursor::new(0), CCursor::new(field.text.chars().count()));
                state.cursor.set_char_range(Some(all));
                state.store(ui.ctx(), response.id);
                response.request_focus();
            }
            if response.changed() {
                field.invalid = false;
            }
            let (enter, escape) =
                ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
            close = response.lost_focus() && escape;
            ui.add(super::hint(DESCRIPTION));
            if field.invalid {
                let color = ui.visuals().error_fg_color;
                ui.label(super::hint_text("Not a time.").color(color));
            }
            ui.add_space(4.0);
            // At the right, in a row of its own height (a layout of its own would take the
            // window's whole height).
            let row = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
            let layout = egui::Layout::right_to_left(egui::Align::Center);
            let go = ui
                .allocate_ui_with_layout(row, layout, |ui| ui.button("Go"))
                .inner
                .clicked();
            if go || response.lost_focus() && enter {
                match parse_time(&field.text) {
                    Some(t) => asked = Some(t),
                    None => {
                        field.invalid = true;
                        field.focus = true;
                    }
                }
            }
        });
    if asked.is_some() || close {
        *open = false;
    }
    asked
}
