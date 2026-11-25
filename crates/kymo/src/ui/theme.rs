//! Kymo's light and dark looks: egui's, with text, controls and the selection given the
//! contrast of the system's own apps (modeled on macOS's semantic colors), the selection in
//! the system's accent color, and glyphs unhinted on macOS, as macOS draws them (their
//! weight comes from the font, see [`crate::ui::fonts`]).
//!
//! Text has at least WCAG's 4.5:1 against the panels, weak text too (egui's default weak
//! text, its normal text at 60 % opacity, had less than 3:1). Controls get a border, so that
//! checkboxes and radio buttons stand out from the panel. The trace area and the trace
//! colors are not changed. Windows and menus cast their shadow as the system's do: on all
//! four sides, a little lower than the window, so weakest at the top.

use eframe::egui::{self, Color32, Rgba, Stroke, Visuals};

/// The accent color where the system tells none: macOS's default blue.
pub const DEFAULT_ACCENT: Color32 = Color32::from_rgb(0, 122, 255);

/// Contrast that text needs against its background (WCAG's AA for normal text).
pub const TEXT_CONTRAST: f32 = 4.5;

/// The padding inside every window and dialog, on all four sides, in points (egui's is 6).
pub const WINDOW_MARGIN: egui::Margin = egui::Margin::same(16);

/// The padding of a window's title bar, in points (egui's window margin): the title bar
/// stays as tall as it was, only the window's content gets [`WINDOW_MARGIN`].
pub const TITLE_MARGIN: egui::Margin = egui::Margin::same(6);

/// The frame of a window's title bar: the window's, with [`TITLE_MARGIN`].
pub fn title_frame(ctx: &egui::Context) -> egui::Frame {
    egui::Frame::window(&ctx.global_style()).inner_margin(TITLE_MARGIN)
}

/// A modal dialog: a window with a title bar (but no close button, its buttons answer it)
/// over a backdrop, `width` wide inside its [`WINDOW_MARGIN`]. It looks as the windows do:
/// the title bar keeps [`TITLE_MARGIN`] and a line under it, the content gets the window's
/// padding.
pub fn modal<T>(
    ctx: &egui::Context,
    id: egui::Id,
    title: &str,
    width: f32,
    add_contents: impl FnOnce(&mut egui::Ui) -> T,
) -> egui::ModalResponse<T> {
    let style = ctx.global_style();
    // The margins are applied below, so the title bar can reach the window's edge.
    let frame = egui::Frame::window(&style).inner_margin(0);
    let margin = WINDOW_MARGIN;
    egui::Modal::new(id).frame(frame).show(ctx, |ui| {
        ui.set_width(width + f32::from(margin.left) + f32::from(margin.right));
        ui.spacing_mut().item_spacing.y = 0.0;
        let bar = egui::Frame::NONE.inner_margin(TITLE_MARGIN).show(ui, |ui| {
            ui.vertical_centered(|ui| ui.label(super::window_title(title)));
        });
        let y = bar.response.rect.bottom();
        let x = bar.response.rect.x_range();
        ui.painter().hline(x, y, style.visuals.window_stroke());
        egui::Frame::NONE
            .inner_margin(margin)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = style.spacing.item_spacing;
                add_contents(ui)
            })
            .inner
    })
}

/// Sets the light and the dark look on `ctx`, the selection in `accent` (the system's, or
/// [`DEFAULT_ACCENT`]).
pub fn apply(ctx: &egui::Context, accent: Option<Color32>) {
    ctx.all_styles_mut(|style| style.spacing.window_margin = WINDOW_MARGIN);
    ctx.set_visuals_of(egui::Theme::Light, light(accent));
    ctx.set_visuals_of(egui::Theme::Dark, dark(accent));
}

/// The light look.
pub fn light(accent: Option<Color32>) -> Visuals {
    let mut v = Visuals::light();
    let text = Color32::from_gray(30);
    let strong = Color32::BLACK;
    let w = &mut v.widgets;
    w.noninteractive.fg_stroke.color = text;
    w.noninteractive.bg_stroke.color = Color32::from_gray(200);
    // White controls with a border, as the system's, on the light gray panel.
    w.inactive.weak_bg_fill = Color32::WHITE;
    w.inactive.bg_fill = Color32::WHITE;
    w.inactive.bg_stroke = Stroke::new(1.0, Color32::from_gray(160));
    w.inactive.fg_stroke.color = text;
    w.hovered.weak_bg_fill = Color32::from_gray(236);
    w.hovered.bg_fill = Color32::from_gray(236);
    w.hovered.bg_stroke.color = Color32::from_gray(120);
    w.hovered.fg_stroke.color = strong;
    w.active.weak_bg_fill = Color32::from_gray(214);
    w.active.bg_fill = Color32::from_gray(214);
    w.active.fg_stroke.color = strong;
    w.open.fg_stroke.color = strong;
    // 4.9:1 on the panel, 4.4:1 on a hovered control.
    v.weak_text_color = Some(Color32::from_gray(108));
    v.error_fg_color = Color32::from_rgb(200, 20, 20);
    v.warn_fg_color = Color32::from_rgb(180, 80, 0);
    v.selection = selection(accent.unwrap_or(DEFAULT_ACCENT), false, v.panel_fill);
    v.hyperlink_color = v.selection.stroke.color;
    v.text_options.font_hinting = FONT_HINTING;
    shadows(&mut v, 40);
    v
}

/// The dark look.
pub fn dark(accent: Option<Color32>) -> Visuals {
    let mut v = Visuals::dark();
    let text = Color32::from_gray(225);
    let strong = Color32::WHITE;
    let w = &mut v.widgets;
    w.noninteractive.fg_stroke.color = text;
    w.noninteractive.bg_stroke.color = Color32::from_gray(70);
    w.inactive.bg_stroke = Stroke::new(1.0, Color32::from_gray(95));
    w.inactive.fg_stroke.color = text;
    w.hovered.fg_stroke.color = strong;
    w.active.fg_stroke.color = strong;
    w.open.fg_stroke.color = strong;
    // 6.2:1 on the panel, 5:1 on a control.
    v.weak_text_color = Some(Color32::from_gray(155));
    v.selection = selection(accent.unwrap_or(DEFAULT_ACCENT), true, v.panel_fill);
    v.hyperlink_color = v.selection.stroke.color;
    v.text_options.font_hinting = FONT_HINTING;
    shadows(&mut v, 110);
    v
}

/// Shadows around windows and menus, `alpha` black at their darkest: shifted down by less
/// than they blur, so that they show on all four sides (egui's fall to the right and below).
fn shadows(v: &mut Visuals, alpha: u8) {
    let color = Color32::from_black_alpha(alpha);
    v.window_shadow = egui::Shadow {
        offset: [0, 8],
        blur: 28,
        spread: 0,
        color,
    };
    v.popup_shadow = egui::Shadow {
        offset: [0, 4],
        blur: 14,
        spread: 0,
        color,
    };
}

/// Whether glyph outlines are hinted to the pixel grid: not on macOS, which doesn't hint
/// either (its displays have the pixels not to need it); elsewhere as egui does.
pub const FONT_HINTING: bool = !cfg!(target_os = "macos");

/// The selection in `accent`: a tint of it behind selected controls and text, which keep a
/// shade of it for their text, far enough from the tint to read. The tint is kept dark
/// enough (light enough in light mode) for white (black) to have 7:1 on it, so that a
/// shade with 4.5:1 exists for any accent, a very light or dark one too.
fn selection(accent: Color32, dark: bool, panel: Color32) -> egui::style::Selection {
    let (fill, toward) = if dark {
        let tint = mix(panel, accent, 0.45);
        (
            contrasting(tint, Color32::WHITE, Color32::BLACK, 7.0),
            Color32::WHITE,
        )
    } else {
        let tint = mix(Color32::WHITE, accent, 0.3);
        (
            contrasting(tint, Color32::BLACK, Color32::WHITE, 7.0),
            Color32::BLACK,
        )
    };
    egui::style::Selection {
        bg_fill: fill,
        stroke: Stroke::new(1.0, contrasting(accent, fill, toward, TEXT_CONTRAST)),
    }
}

/// `a` moved the fraction `t` of the way to `b`, in sRGB.
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let channel = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    Color32::from_rgb(
        channel(a.r(), b.r()),
        channel(a.g(), b.g()),
        channel(a.b(), b.b()),
    )
}

/// `color`, or the least mix of it with `toward` (black or white) that has `ratio` against
/// `background`; `toward` itself if nothing less does.
pub fn contrasting(color: Color32, background: Color32, toward: Color32, ratio: f32) -> Color32 {
    (0..=20)
        .map(|step| mix(color, toward, step as f32 / 20.0))
        .find(|&c| contrast(c, background) >= ratio)
        .unwrap_or(toward)
}

/// WCAG's contrast ratio of two opaque colors, from 1 to 21.
pub fn contrast(a: Color32, b: Color32) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Relative luminance, from linear RGB.
fn luminance(color: Color32) -> f32 {
    let c = Rgba::from(color);
    0.2126 * c.r() + 0.7152 * c.g() + 0.0722 * c.b()
}
