//! The interface's fonts: egui's own, or the system's (Settings → Font), with macOS's font
//! for its shortcut symbols as the last fallback.

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{self, FontData, FontDefinitions, FontFamily, FontTweak};
use eframe::epaint::text::VariationCoords;

/// The system's interface font, as Settings names it, where Kymo can use it: macOS's and
/// Windows's. Linux has no one font file for it.
pub const SYSTEM_FONT_NAME: Option<&str> = if cfg!(target_os = "macos") {
    Some("SF Pro")
} else if cfg!(windows) {
    Some("Segoe UI")
} else {
    None
};

/// Sets the interface's fonts: egui's own, or with `system` the system's interface font in
/// front of them (see [`SYSTEM_FONT_NAME`]), egui's staying for glyphs it lacks, and left
/// out if it can't be read. egui puts them in use as the next frame begins.
pub fn apply_fonts(ctx: &egui::Context, system: bool) {
    let mut fonts = FontDefinitions::default();
    if let Some(bytes) = system.then(system_font).flatten() {
        let name = "System".to_owned();
        fonts.font_data.insert(
            name.clone(),
            Arc::new(FontData::from_owned(bytes).tweak(text_cut())),
        );
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, name);
    }
    add_symbol_font(&mut fonts);
    ctx.set_fonts(fonts);
}

/// SF Pro as macOS shows it at the interface's sizes. A variable font whose default
/// instance is the display cut (optical size 28), with tighter spacing and thinner strokes,
/// which looks cramped and light at 13 points: macOS uses optical size 17, the axis's
/// smallest, for every size up to 17 points, and all of Kymo's text is that small. Its
/// grade is raised to 500, as Core Text's font smoothing makes strokes heavier than egui
/// draws them: with it, text has as much ink as Core Text gives it, and the edges stay as
/// soft (grade, unlike weight, keeps the widths; a steeper coverage curve gives the ink too
/// but hardens the edges, so strokes look jagged). Segoe UI is no variable font; the tweak
/// changes nothing there.
fn text_cut() -> FontTweak {
    FontTweak {
        coords: VariationCoords::new([(b"opsz", 17.0), (b"GRAD", 500.0)]),
        ..Default::default()
    }
}

/// The system's interface font file, if Kymo uses one and it can be read. Kymo has no
/// monospaced text, so the system's monospaced font isn't needed.
fn system_font() -> Option<Vec<u8>> {
    let path = if cfg!(target_os = "macos") {
        PathBuf::from("/System/Library/Fonts/SFNS.ttf")
    } else if cfg!(windows) {
        let windows = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
        PathBuf::from(windows).join("Fonts").join("segoeui.ttf")
    } else {
        return None;
    };
    std::fs::read(path).ok()
}

/// Adds macOS's font for its shortcut symbols (⌘ ⌥ ⇧ and arrows) as the last font of each
/// family. egui's own fonts lack them, and the system font fallback eframe installs only
/// searches generic families, which lack them too; Lucida Grande is what macOS itself falls
/// back to. Elsewhere shortcuts are written as words ("Ctrl+O"), so nothing is needed.
fn add_symbol_font(fonts: &mut FontDefinitions) {
    #[cfg(target_os = "macos")]
    if let Ok(bytes) = std::fs::read("/System/Library/Fonts/LucidaGrande.ttc") {
        let name = "Lucida Grande".to_owned();
        fonts.font_data.insert(
            name.clone(),
            Arc::new(FontData::from_owned(bytes).tweak(text_cut())),
        );
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push(name.clone());
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = fonts;
}
