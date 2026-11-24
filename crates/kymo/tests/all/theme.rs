//! The light and dark looks: text, weak text and selected text readable on what they sit
//! on, whatever the accent color, and controls outlined.

use eframe::egui::{Color32, Visuals};
use kymo::ui::theme::{self, TEXT_CONTRAST, contrast};
use proptest::prelude::*;

fn looks(accent: Option<Color32>) -> [Visuals; 2] {
    [theme::light(accent), theme::dark(accent)]
}

#[test]
fn text_and_weak_text_read_on_panels_and_windows() {
    for v in looks(None) {
        for background in [v.panel_fill, v.window_fill, v.extreme_bg_color] {
            for (what, color) in [
                ("text", v.text_color()),
                ("weak text", v.weak_text_color()),
                ("button text", v.widgets.inactive.fg_stroke.color),
                ("strong text", v.strong_text_color()),
            ] {
                let ratio = contrast(color, background);
                assert!(
                    ratio >= TEXT_CONTRAST,
                    "{what} {color:?} on {background:?}: {ratio:.2} (dark: {})",
                    v.dark_mode
                );
            }
        }
        // Weak text stays weaker than text.
        assert!(
            contrast(v.weak_text_color(), v.panel_fill) < contrast(v.text_color(), v.panel_fill)
        );
    }
}

#[test]
fn controls_are_outlined() {
    for v in looks(None) {
        let stroke = v.widgets.inactive.bg_stroke;
        assert!(stroke.width >= 1.0, "dark: {}", v.dark_mode);
        assert!(
            contrast(stroke.color, v.panel_fill) >= 1.8,
            "dark: {}",
            v.dark_mode
        );
    }
}

#[test]
fn the_selection_follows_the_accent() {
    let red = Color32::from_rgb(230, 40, 50);
    for (v, default) in looks(Some(red)).into_iter().zip(looks(None)) {
        assert_ne!(v.selection.bg_fill, default.selection.bg_fill);
        let fill = v.selection.bg_fill;
        // A tint of the accent: redder than it is green or blue.
        assert!(fill.r() > fill.g() && fill.r() > fill.b(), "{fill:?}");
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Selected controls' text reads on the selection, and the selection stands apart from
    /// the panel, for any accent.
    #[test]
    fn selected_text_reads_for_any_accent(r: u8, g: u8, b: u8) {
        for v in looks(Some(Color32::from_rgb(r, g, b))) {
            let ratio = contrast(v.selection.stroke.color, v.selection.bg_fill);
            prop_assert!(ratio >= TEXT_CONTRAST, "{ratio:.2} dark: {}", v.dark_mode);
            prop_assert!(contrast(v.text_color(), v.selection.bg_fill) >= 3.0);
        }
    }
}
