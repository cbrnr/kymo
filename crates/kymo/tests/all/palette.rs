//! Trace colors per channel type, in light and dark themes.

use eframe::egui::Color32;
use kymo::ui::palette::{GRAY_TRACES, bad_color, trace_color};
use kymo::ui::theme;
use kymo_core::ChannelKind;

/// Sum of the channel differences, ignoring alpha.
fn distance(a: Color32, b: Color32) -> u32 {
    let d = |x: u8, y: u8| u32::from(x.abs_diff(y));
    d(a.r(), b.r()) + d(a.g(), b.g()) + d(a.b(), b.b())
}

/// `color` drawn over `background`.
fn over(color: Color32, background: Color32) -> Color32 {
    let a = f32::from(color.a()) / 255.0;
    // Premultiplied: the color's channels already carry its alpha.
    let mix = |c: u8, b: u8| (f32::from(c) + f32::from(b) * (1.0 - a)).round() as u8;
    Color32::from_rgb(
        mix(color.r(), background.r()),
        mix(color.g(), background.g()),
        mix(color.b(), background.b()),
    )
}

/// sRGB channel (0 to 255) to linear light.
fn linear(c: u8) -> f64 {
    let c = f64::from(c) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// WCAG contrast ratio of two colors.
fn contrast(a: Color32, b: Color32) -> f64 {
    let luminance =
        |c: Color32| 0.2126 * linear(c.r()) + 0.7152 * linear(c.g()) + 0.0722 * linear(c.b());
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Linear RGB to OKLab.
fn oklab([r, g, b]: [f64; 3]) -> [f64; 3] {
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    [
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    ]
}

/// Machado, Oliveira and Fernandes' (2009) simulations at full severity, on linear RGB.
const PROTAN: [[f64; 3]; 3] = [
    [0.152286, 1.052583, -0.204868],
    [0.114503, 0.786281, 0.099216],
    [-0.003882, -0.048116, 1.051998],
];
const DEUTAN: [[f64; 3]; 3] = [
    [0.367322, 0.860646, -0.227968],
    [0.280085, 0.672501, 0.047413],
    [-0.011820, 0.042940, 0.968881],
];
const NORMAL: [[f64; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// OKLab distance ×100 of two colors as seen through `vision`.
fn delta_e(a: Color32, b: Color32, vision: &[[f64; 3]; 3]) -> f64 {
    let seen = |c: Color32| {
        let rgb = [linear(c.r()), linear(c.g()), linear(c.b())];
        oklab(
            vision.map(|row| (row[0] * rgb[0] + row[1] * rgb[1] + row[2] * rgb[2]).clamp(0.0, 1.0)),
        )
    };
    let (a, b) = (seen(a), seen(b));
    100.0 * ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

#[test]
fn types_are_told_apart_in_both_themes() {
    use crate::kinds::*;
    let kind = ChannelKind::named;
    for visuals in [theme::light(None), theme::dark(None)] {
        let background = visuals.extreme_bg_color;
        let text = Color32::from_gray(if visuals.dark_mode {
            GRAY_TRACES.1
        } else {
            GRAY_TRACES.0
        });
        let color = |kind: &ChannelKind| trace_color(kind, &visuals);
        // EEG, misc and every type without a hue of its own in gray.
        for k in [
            Eeg,
            Misc,
            Bio,
            kind("Temperature"),
            kind("Gaze"),
            kind("SaO2"),
        ] {
            assert_eq!(color(&k), text, "{k:?}");
        }
        // HEOG and VEOG as EOG.
        assert_eq!(color(&kind("HEOG")), color(&Eog));
        assert_eq!(color(&kind("VEOG")), color(&Eog));
        let colored = [
            Mag, Grad, RefMeg, Eog, Ecg, Emg, Seeg, Ecog, Dbs, Resp, Stim,
        ];
        let bad = over(bad_color(&visuals), background);
        for k in &colored {
            let c = color(k);
            assert!(contrast(c, background) >= 3.0, "{k:?} on the background");
            assert!(distance(c, text) >= 60, "{k:?} looks like text");
            assert!(distance(c, bad) >= 60, "{k:?} looks bad");
        }
        // One hue per family, in the sidebar's order: neighbors apart for normal vision and
        // with protanopia or deuteranopia.
        let hues = [Mag, Eog, Ecg, Emg, Seeg, Resp, Stim];
        for pair in hues.windows(2) {
            let (a, b) = (color(&pair[0]), color(&pair[1]));
            let normal = delta_e(a, b, &NORMAL);
            assert!(normal >= 15.0, "{pair:?}: ΔE {normal:.1}");
            let deficient = delta_e(a, b, &PROTAN).min(delta_e(a, b, &DEUTAN));
            assert!(
                deficient >= 8.0,
                "{pair:?}: ΔE {deficient:.1} with a deficiency"
            );
        }
        // Within a family, shades told apart.
        for pair in [[Mag, Grad], [Grad, RefMeg], [Seeg, Ecog], [Ecog, Dbs]] {
            let normal = delta_e(color(&pair[0]), color(&pair[1]), &NORMAL);
            assert!(normal >= 8.0, "{pair:?}: ΔE {normal:.1}");
        }
        assert!(distance(text, bad) >= 60, "bad looks like text");
        assert!(distance(bad, background) >= 60, "bad disappears");
    }
}
