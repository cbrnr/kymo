//! Pieces of the window: the time axis, scrollbars, the status line, the sidebar, trace
//! colors, the light and dark looks and the color picker, the egui-drawn menus and windows,
//! the Settings and Recording Info windows, the renderer inside egui, and which of the
//! windows is in front.

pub mod axis;
pub mod color;
pub mod fonts;
pub mod go_to;
pub mod gpu;
pub mod header;
pub mod info;
pub mod menus;
pub mod palette;
pub mod readout;
pub mod scrollbar;
pub mod settings;
pub mod status;
pub mod theme;
pub mod types;

use eframe::egui::{Button, Color32, Context, FontId, IntoAtoms, Label, RichText};

/// Corner radius of buttons in a line of text, as [`text_button`] makes them.
pub const TEXT_BUTTON_RADIUS: f32 = 4.0;

/// A button in a line of text, the status line's or a channel's: it reads as text until
/// hovered, or for a toggle (`selected` is `Some`) until on, when it is filled in the
/// selection color. As tall as egui's other controls, with corners of
/// [`TEXT_BUTTON_RADIUS`]. The status line's range and the channel list's Bad use it.
pub fn text_button<'a>(atoms: impl IntoAtoms<'a>, selected: Option<bool>) -> Button<'a> {
    let button = match selected {
        Some(on) => Button::selectable(on, atoms),
        None => Button::new(atoms).frame_when_inactive(false),
    };
    button.corner_radius(TEXT_BUTTON_RADIUS)
}

/// `text` shortened in the middle to fit `width` in `font`: as much of its start and end as
/// fits around "…", the start a character longer where they differ, so names that differ
/// only at their ends ("NEDE_FlightParams_0", "…_1") stay apart, as do those that differ at
/// their starts. `None` if the whole text fits; just "…" if nothing more does.
pub fn elide_middle(ctx: &Context, text: &str, font: &FontId, width: f32) -> Option<String> {
    ctx.fonts_mut(|fonts| {
        let mut fits = |s: String| {
            fonts
                .layout_no_wrap(s, font.clone(), Color32::PLACEHOLDER)
                .size()
                .x
                <= width
        };
        if fits(text.to_owned()) {
            return None;
        }
        let chars: Vec<char> = text.chars().collect();
        let n = chars.len();
        let shortened = |kept: usize| -> String {
            let tail = kept / 2;
            let head = kept - tail;
            let mut s: String = chars[..head].iter().collect();
            s.push('\u{2026}');
            s.extend(&chars[n - tail..]);
            s
        };
        // The most characters kept that still fit: wider with each one kept.
        let (mut lo, mut hi) = (0, n.saturating_sub(1));
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if fits(shortened(mid)) {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        Some(shortened(lo))
    })
}

/// Size of hints, in points: below the body text's 14, so a hint reads as secondary to the
/// label above it, as macOS and Windows write such text in dialogs.
pub const HINT_SIZE: f32 = 12.0;

/// A description below a field or setting: dimmed, smaller than the body text, upright, and
/// wrapped at the width it has. Settings and Go to Time write theirs with it.
pub fn hint(text: &str) -> Label {
    Label::new(hint_text(text).weak()).wrap()
}

/// `text` at the hints' size, for what sits with them in another color (an error below a
/// field).
pub fn hint_text(text: &str) -> RichText {
    RichText::new(text).size(HINT_SIZE)
}

/// The windows over the recording, which Esc and ⌘W close, the one in front first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Overlay {
    Settings,
    Shortcuts,
    About,
    GoToTime,
    RecordingInfo,
    ChannelTypes,
}

impl Overlay {
    pub const ALL: [Overlay; 6] = [
        Overlay::Settings,
        Overlay::Shortcuts,
        Overlay::About,
        Overlay::GoToTime,
        Overlay::RecordingInfo,
        Overlay::ChannelTypes,
    ];

    /// The window's id, which also names its layer, so the one in front can be found.
    pub fn id(self) -> eframe::egui::Id {
        eframe::egui::Id::unique(("kymo overlay", self))
    }

    /// Which of `open` is in front, from egui's layer order of the last frame; one that
    /// hasn't been drawn yet counts as behind the others.
    pub fn front(
        ctx: &eframe::egui::Context,
        open: impl IntoIterator<Item = Overlay>,
    ) -> Option<Overlay> {
        let layers: Vec<eframe::egui::Id> = ctx.memory(|m| m.layer_ids().map(|l| l.id).collect());
        open.into_iter()
            .max_by_key(|overlay| layers.iter().position(|&id| id == overlay.id()))
    }
}
