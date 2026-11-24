//! Trace colors per channel type and annotation colors, for light and dark themes.

use eframe::egui::{Color32, Visuals};

use crate::annotations::{AnnotationColor, COLORS};
use crate::types::TypeSettings;
use kymo_core::ChannelKind;

/// The color of a channel type's traces and names: the one picked for it, used as it is
/// in every theme, or the default ([`trace_color`]).
pub fn type_color(kind: &ChannelKind, types: &TypeSettings, visuals: &Visuals) -> Color32 {
    types
        .color(kind)
        .unwrap_or_else(|| trace_color(kind, visuals))
}

/// The default color of a channel type's traces and names. EEG, misc and every other type
/// are gray, a little lighter than the text in light themes and darker in dark ones
/// ([`GRAY_TRACES`]), so that dense traces don't weigh more than the colored ones; seven types get a hue each, all at one lightness and
/// chroma per theme (OKLCH L 0.56, C 0.15 against white; L 0.66, C 0.13, lighter, against
/// the dark theme's near black), so none stands out more than another: MEG green
/// (magnetometers; gradiometers darker, or lighter in dark themes; references grayer),
/// EOG blue (HEOG and VEOG alike), ECG red, EMG violet, intracranial orange (sEEG; ECoG
/// darker, or lighter; DBS grayer), respiration magenta and stim amber. In the sidebar's
/// order (EEG, MEG, EOG, ECG, EMG, intracranial, respiration; stim last), neighbors stay
/// apart for color vision deficiencies (OKLab ΔE ≥ 8 under simulated protanopia and
/// deuteranopia) and for normal vision (ΔE ≥ 15), and every hue keeps 3:1 contrast with
/// the background. There are no more hues to give: any other at that lightness and chroma
/// is within ΔE 13 of one of them, so other types stay gray unless one is picked.
pub fn trace_color(kind: &ChannelKind, visuals: &Visuals) -> Color32 {
    let hex = |light: u32, dark: u32| {
        let rgb = if visuals.dark_mode { dark } else { light };
        let [_, r, g, b] = rgb.to_be_bytes();
        Color32::from_rgb(r, g, b)
    };
    match kind.key() {
        "mag" => hex(0x2b8a36, 0x5aa75e),
        "grad" => hex(0x046f1c, 0x76c479),
        "ref_meg" => hex(0x489476, 0x539277),
        "eog" | "heog" | "veog" => hex(0x4170cb, 0x6591e1),
        "ecg" => hex(0xbc4945, 0xd67069),
        "emg" => hex(0x8959ba, 0xa57cd2),
        "seeg" => hex(0xaa5f04, 0xcb7d34),
        "ecog" => hex(0x864900, 0xea9952),
        "dbs" => hex(0xa67943, 0xa17b4e),
        "resp" => hex(0xaf4a88, 0xc96fa4),
        "stim" => hex(0x8e7002, 0xb18e15),
        _ => {
            let (light, dark) = GRAY_TRACES;
            Color32::from_gray(if visuals.dark_mode { dark } else { light })
        }
    }
}

/// The gray of channel types without a hue, in light and dark themes: egui's text color
/// before Kymo set its own (8:1 against white, 5.9:1 against the dark theme's trace area).
pub const GRAY_TRACES: (u8, u8) = (80, 140);

/// The color of bad channels' traces and names, whatever their type: [`GRAY_TRACES`] at
/// 36 % opacity, faded well behind the traces of every type.
pub fn bad_color(visuals: &Visuals) -> Color32 {
    let (light, dark) = GRAY_TRACES;
    Color32::from_gray(if visuals.dark_mode { dark } else { light }).gamma_multiply(0.36)
}

/// Matplotlib's default color cycle ("tab10"), which MNE's browsers use for annotations.
const ANNOTATION_COLORS: [Color32; COLORS] = [
    Color32::from_rgb(31, 119, 180),
    Color32::from_rgb(255, 127, 14),
    Color32::from_rgb(44, 160, 44),
    Color32::from_rgb(214, 39, 40),
    Color32::from_rgb(148, 103, 189),
    Color32::from_rgb(140, 86, 75),
    Color32::from_rgb(227, 119, 194),
    Color32::from_rgb(127, 127, 127),
    Color32::from_rgb(188, 189, 34),
    Color32::from_rgb(23, 190, 207),
];

/// The color of an annotation color: a palette color (its index taken modulo the number
/// of colors), lighter in dark themes, or a picked one as it is.
pub fn annotation_color(color: AnnotationColor, visuals: &Visuals) -> Color32 {
    match color {
        AnnotationColor::Palette(index) => {
            let color = ANNOTATION_COLORS[index % COLORS];
            if visuals.dark_mode {
                color.lerp_to_gamma(Color32::WHITE, 0.25)
            } else {
                color
            }
        }
        AnnotationColor::Custom([r, g, b]) => Color32::from_rgb(r, g, b),
    }
}
