//! What the user can set in the Settings window, kept between runs in eframe's storage.

use std::ops::RangeInclusive;

use eframe::Storage;
use eframe::egui::{self, Color32};
use kymo_core::ChannelKind;
use kymo_core::tile::DEFAULT_BUDGET;

use crate::annotations::{AnnotationColors, Rgb};
use crate::render::DEFAULT_GPU_BUDGET;
use crate::types::{TypeSet, TypeSettings};
use crate::ui::minimap;
use crate::view::DEFAULT_SPAN;

/// The megabytes the tile cache may take, as Settings offers them.
pub const TILE_CACHE_MB: [u32; 6] = [128, 256, 512, 1024, 2048, 4096];
/// The megabytes of GPU memory the trace renderer may take for tiles, as Settings offers
/// them.
pub const GPU_MEMORY_MB: [u32; 5] = [128, 256, 512, 1024, 2048];
/// The disk cache turned off: overviews are kept in memory, nothing is written to disk.
pub const OFF: u32 = 0;
/// The disk cache for this session only: every cache stays until Kymo quits.
pub const SESSION: u32 = u32::MAX - 1;
/// The disk cache's limit that keeps every cache: no limit.
pub const UNLIMITED: u32 = u32::MAX;
/// The megabytes the disk cache may hold, as Settings offers them, after [`OFF`] and
/// [`SESSION`], which aren't sizes, and up to [`UNLIMITED`].
pub const DISK_CACHE_MB: [u32; 7] = [OFF, SESSION, 1024, 2048, 5120, 10240, UNLIMITED];
/// Seconds the default window may span.
pub const DEFAULT_WINDOW_S: RangeInclusive<f64> = 1.0..=3600.0;

const TILE_CACHE_KEY: &str = "kymo.tile_cache_mb";
const GPU_MEMORY_KEY: &str = "kymo.gpu_memory_mb";
const DISK_CACHE_KEY: &str = "kymo.disk_cache_mb";
const DEFAULT_WINDOW_KEY: &str = "kymo.default_window_s";
const ANTIALIAS_KEY: &str = "kymo.antialias";
const SCALE_BARS_KEY: &str = "kymo.scale_bars";
const MINIMAP_KEY: &str = "kymo.minimap";
const MINIMAP_HEIGHT_KEY: &str = "kymo.minimap_height";
const MINIMAP_SIGNALS_KEY: &str = "kymo.minimap_signals";
const REMOVE_DC_KEY: &str = "kymo.remove_dc";
const AUTO_SCALE_KEY: &str = "kymo.auto_scale_on_open";
const THEME_KEY: &str = "kymo.theme";
const SYSTEM_FONT_KEY: &str = "kymo.system_font";
const ANIMATIONS_KEY: &str = "kymo.animations";
const ANNOTATION_COLORS_KEY: &str = "kymo.annotation_colors";
const CHANNEL_TYPES_KEY: &str = "kymo.channel_types";
const CHECK_UPDATES_KEY: &str = "kymo.check_updates";

/// The types kymo had before types were names, by MNE's names, whose settings were kept
/// under a key each: `kymo.color.eeg`, `kymo.remove_dc.eeg`, `kymo.own_scale.ref_meg`.
const OLD_TYPES: [&str; 14] = [
    "eeg", "eog", "ecg", "emg", "stim", "misc", "mag", "grad", "ref_meg", "seeg", "ecog", "dbs",
    "resp", "bio",
];

/// The keys an old type's color, DC removal and scaling each channel on its own were kept
/// under.
fn old_type_keys(name: &str) -> [String; 3] {
    ["color", "remove_dc", "own_scale"].map(|setting| format!("kymo.{setting}.{name}"))
}

/// Light or dark: the system's choice, or one of them whatever the system uses. On
/// Windows, the system's choice is the default app mode (not the default Windows mode,
/// which is for the taskbar and Start), as for other apps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

impl Theme {
    pub const ALL: [Theme; 3] = [Theme::System, Theme::Light, Theme::Dark];

    pub fn label(self) -> &'static str {
        match self {
            Theme::System => "System",
            Theme::Light => "Light",
            Theme::Dark => "Dark",
        }
    }

    /// How it is kept in the storage.
    fn key(self) -> &'static str {
        match self {
            Theme::System => "system",
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }

    fn parse(text: &str) -> Option<Theme> {
        Theme::ALL.into_iter().find(|t| t.key() == text.trim())
    }

    /// egui's look for it.
    pub fn preference(self) -> egui::ThemePreference {
        match self {
            Theme::System => egui::ThemePreference::System,
            Theme::Light => egui::ThemePreference::Light,
            Theme::Dark => egui::ThemePreference::Dark,
        }
    }

    /// The window's title bar for it.
    pub fn window(self) -> egui::SystemTheme {
        match self {
            Theme::System => egui::SystemTheme::SystemDefault,
            Theme::Light => egui::SystemTheme::Light,
            Theme::Dark => egui::SystemTheme::Dark,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// Budget of the tile cache, in MB (2^20 bytes).
    pub tile_cache_mb: u32,
    /// Budget for tiles on the GPU, in MB.
    pub gpu_memory_mb: u32,
    /// What the disk cache may hold, in MB: the least recently used caches of closed
    /// recordings go beyond it (see [`kymo_core::cache::trim`]). [`OFF`] keeps overviews in
    /// memory instead, [`SESSION`] keeps every cache until Kymo quits, [`UNLIMITED`] all.
    pub disk_cache_mb: u32,
    /// Seconds shown when a recording opens and after Default Window (⌘0).
    pub default_window_s: f64,
    /// Whether traces are anti-aliased.
    pub antialias: bool,
    /// Whether each channel type on screen shows a scale bar.
    pub scale_bars: bool,
    /// Whether the minimap is shown below the traces, in place of the time scrollbar.
    pub minimap: bool,
    /// The minimap's height, in points, as dragged (the window limits it further, see
    /// [`crate::ui::minimap::height`]).
    pub minimap_height: f32,
    /// Whether the minimap shows a row per shown channel under the annotations.
    pub minimap_signals: bool,
    /// Whether each trace is centered on its mean over the window (DC removal), of the types
    /// `types` has it for.
    pub remove_dc: bool,
    /// Per channel type: its color, DC removal, and scaling each channel on its own.
    pub types: TypeSettings,
    /// Whether a recording opens with Auto Scale instead of the default amplitudes.
    pub auto_scale_on_open: bool,
    pub theme: Theme,
    /// Whether text is in the system's interface font rather than egui's (see
    /// [`crate::ui::fonts`]); by default wherever there is one (macOS, Windows), as egui's,
    /// Ubuntu Light, is thin and looks foreign there.
    pub system_font: bool,
    /// Whether rows move to their new places when channels are shown or hidden, instead of
    /// jumping there.
    pub animations: bool,
    /// Whether Kymo asks GitHub for a newer release at startup, at most once a day (see
    /// [`crate::update`]).
    pub check_updates: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            tile_cache_mb: (DEFAULT_BUDGET >> 20) as u32,
            gpu_memory_mb: (DEFAULT_GPU_BUDGET >> 20) as u32,
            disk_cache_mb: 5120,
            default_window_s: DEFAULT_SPAN,
            antialias: false,
            scale_bars: true,
            minimap: true,
            minimap_height: minimap::DEFAULT_HEIGHT,
            minimap_signals: true,
            remove_dc: true,
            types: TypeSettings::default(),
            auto_scale_on_open: true,
            theme: Theme::System,
            system_font: crate::ui::fonts::SYSTEM_FONT_NAME.is_some(),
            animations: true,
            check_updates: true,
        }
    }
}

impl Settings {
    /// The settings kept in `storage`; each one missing or unreadable is the default, and
    /// each one out of range the nearest allowed value.
    pub fn load(storage: Option<&dyn Storage>) -> Settings {
        let defaults = Settings::default();
        let get = |key: &str| storage.and_then(|s| s.get_string(key));
        Settings {
            tile_cache_mb: get(TILE_CACHE_KEY)
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(defaults.tile_cache_mb),
            gpu_memory_mb: get(GPU_MEMORY_KEY)
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(defaults.gpu_memory_mb),
            disk_cache_mb: get(DISK_CACHE_KEY)
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(defaults.disk_cache_mb),
            default_window_s: get(DEFAULT_WINDOW_KEY)
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(defaults.default_window_s),
            antialias: get(ANTIALIAS_KEY)
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(defaults.antialias),
            scale_bars: get(SCALE_BARS_KEY)
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(defaults.scale_bars),
            minimap: get(MINIMAP_KEY)
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(defaults.minimap),
            minimap_height: get(MINIMAP_HEIGHT_KEY)
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(defaults.minimap_height),
            minimap_signals: get(MINIMAP_SIGNALS_KEY)
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(defaults.minimap_signals),
            remove_dc: get(REMOVE_DC_KEY)
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(defaults.remove_dc),
            types: match get(CHANNEL_TYPES_KEY) {
                Some(text) => parse_types(&text),
                None => old_types(&get),
            },
            auto_scale_on_open: get(AUTO_SCALE_KEY)
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(defaults.auto_scale_on_open),
            theme: get(THEME_KEY)
                .and_then(|v| Theme::parse(&v))
                .unwrap_or(defaults.theme),
            system_font: get(SYSTEM_FONT_KEY)
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(defaults.system_font),
            animations: get(ANIMATIONS_KEY)
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(defaults.animations),
            check_updates: get(CHECK_UPDATES_KEY)
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(defaults.check_updates),
        }
        .clamped()
    }

    /// Keeps the settings in `storage`.
    pub fn save(&self, storage: &mut dyn Storage) {
        storage.set_string(TILE_CACHE_KEY, self.tile_cache_mb.to_string());
        storage.set_string(GPU_MEMORY_KEY, self.gpu_memory_mb.to_string());
        storage.set_string(DISK_CACHE_KEY, self.disk_cache_mb.to_string());
        storage.set_string(DEFAULT_WINDOW_KEY, self.default_window_s.to_string());
        storage.set_string(ANTIALIAS_KEY, self.antialias.to_string());
        storage.set_string(SCALE_BARS_KEY, self.scale_bars.to_string());
        storage.set_string(MINIMAP_KEY, self.minimap.to_string());
        storage.set_string(MINIMAP_HEIGHT_KEY, self.minimap_height.to_string());
        storage.set_string(MINIMAP_SIGNALS_KEY, self.minimap_signals.to_string());
        storage.set_string(REMOVE_DC_KEY, self.remove_dc.to_string());
        storage.set_string(CHANNEL_TYPES_KEY, format_types(&self.types));
        for name in OLD_TYPES {
            for key in old_type_keys(name) {
                storage.remove_string(&key);
            }
        }
        storage.set_string(AUTO_SCALE_KEY, self.auto_scale_on_open.to_string());
        storage.set_string(THEME_KEY, self.theme.key().to_owned());
        storage.set_string(SYSTEM_FONT_KEY, self.system_font.to_string());
        storage.set_string(ANIMATIONS_KEY, self.animations.to_string());
        storage.set_string(CHECK_UPDATES_KEY, self.check_updates.to_string());
    }

    /// Every setting within its range, each budget the nearest offered one (a disk cache that
    /// is off, for the session or without a limit stays so); NaN seconds and a minimap
    /// height that isn't a number become the default.
    pub fn clamped(self) -> Settings {
        let seconds = if self.default_window_s.is_nan() {
            DEFAULT_SPAN
        } else {
            self.default_window_s
        };
        Settings {
            tile_cache_mb: nearest(self.tile_cache_mb, &TILE_CACHE_MB),
            gpu_memory_mb: nearest(self.gpu_memory_mb, &GPU_MEMORY_MB),
            // Off, Session and no limit stay; any other size is the nearest one offered.
            disk_cache_mb: match self.disk_cache_mb {
                mb @ (OFF | SESSION | UNLIMITED) => mb,
                mb => nearest(mb, &DISK_CACHE_MB[2..DISK_CACHE_MB.len() - 1]),
            },
            default_window_s: seconds.clamp(*DEFAULT_WINDOW_S.start(), *DEFAULT_WINDOW_S.end()),
            antialias: self.antialias,
            scale_bars: self.scale_bars,
            minimap: self.minimap,
            minimap_height: if self.minimap_height.is_finite() {
                self.minimap_height.max(minimap::MIN_HEIGHT)
            } else {
                minimap::DEFAULT_HEIGHT
            },
            minimap_signals: self.minimap_signals,
            remove_dc: self.remove_dc,
            types: self.types,
            auto_scale_on_open: self.auto_scale_on_open,
            theme: self.theme,
            system_font: self.system_font,
            animations: self.animations,
            check_updates: self.check_updates,
        }
    }

    pub fn tile_cache_bytes(&self) -> usize {
        (self.tile_cache_mb as usize) << 20
    }

    pub fn gpu_memory_bytes(&self) -> usize {
        (self.gpu_memory_mb as usize) << 20
    }

    /// Whether overviews are kept in memory, with nothing written to disk.
    pub fn disk_cache_off(&self) -> bool {
        self.disk_cache_mb == OFF
    }

    /// What the disk cache may hold while Kymo runs: nothing with it off, everything for
    /// the session.
    pub fn disk_cache_bytes(&self) -> u64 {
        match self.disk_cache_mb {
            SESSION | UNLIMITED => u64::MAX,
            mb => u64::from(mb) << 20,
        }
    }

    /// What the disk cache may hold once Kymo has quit (and when it starts, after a crash):
    /// nothing if it is off or for the session, else its limit.
    pub fn disk_cache_bytes_at_exit(&self) -> u64 {
        match self.disk_cache_mb {
            SESSION => 0,
            _ => self.disk_cache_bytes(),
        }
    }
}

/// The one of `offered` nearest `mb` by ratio (300 MB is nearer 256 than 512), the smaller
/// of two as near.
pub fn nearest(mb: u32, offered: &[u32]) -> u32 {
    let ratio = |o: u32| (f64::from(mb.max(1)) / f64::from(o)).log2().abs();
    offered
        .iter()
        .copied()
        .min_by(|a, b| ratio(*a).total_cmp(&ratio(*b)))
        .unwrap_or(mb)
}

/// The settings per channel type as kept: a line per type, its color (`#rrggbb`), whether DC
/// removal centers it and whether each of its channels is scaled on its own (`true` or
/// `false`), each empty where nothing was set, then its name with backslashes and line
/// breaks escaped, separated by tabs.
fn format_types(types: &TypeSettings) -> String {
    let flag = |v: Option<bool>| v.map_or_else(String::new, |v| v.to_string());
    types
        .entries()
        .map(|t| {
            let color = t
                .color
                .map_or_else(String::new, |c| format_rgb([c.r(), c.g(), c.b()]));
            format!(
                "{color}\t{}\t{}\t{}\n",
                flag(t.remove_dc),
                flag(t.own_scale),
                escape(&t.name)
            )
        })
        .collect()
}

/// The inverse of [`format_types`]; unreadable lines and values are skipped.
fn parse_types(text: &str) -> TypeSettings {
    let mut types = TypeSettings::default();
    for line in text.lines() {
        let mut fields = line.splitn(4, '\t');
        let (Some(color), Some(dc), Some(own), Some(name)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let Some(name) = unescape(name).filter(|n| !n.trim().is_empty()) else {
            continue;
        };
        types.insert(TypeSet {
            name,
            color: parse_rgb(color).map(|[r, g, b]| Color32::from_rgb(r, g, b)),
            remove_dc: dc.trim().parse().ok(),
            own_scale: own.trim().parse().ok(),
        });
    }
    types
}

/// The settings per channel type as kept before types were names, a key per setting and
/// type ([`old_type_keys`]).
fn old_types(get: &dyn Fn(&str) -> Option<String>) -> TypeSettings {
    let mut types = TypeSettings::default();
    for name in OLD_TYPES {
        let kind = ChannelKind::named(name);
        let [color, remove_dc, own_scale] = old_type_keys(name).map(|key| get(&key));
        if let Some([r, g, b]) = color.as_deref().and_then(parse_rgb) {
            types.set_color(&kind, Some(Color32::from_rgb(r, g, b)));
        }
        if let Some(on) = remove_dc.and_then(|v| v.trim().parse().ok()) {
            types.set_remove_dc(&kind, on);
        }
        if let Some(on) = own_scale.and_then(|v| v.trim().parse().ok()) {
            types.set_own_scale(&kind, on);
        }
    }
    types
}

/// The colors picked for annotation descriptions, kept in `storage`; empty without one.
/// Unreadable lines are skipped.
pub fn load_annotation_colors(storage: Option<&dyn Storage>) -> AnnotationColors {
    let Some(text) = storage.and_then(|s| s.get_string(ANNOTATION_COLORS_KEY)) else {
        return AnnotationColors::new();
    };
    text.lines()
        .filter_map(|line| {
            let (rgb, description) = line.split_once('\t')?;
            Some((unescape(description)?, parse_rgb(rgb)?))
        })
        .collect()
}

/// Keeps the colors picked for annotation descriptions in `storage`: a line per
/// description, its color, a tab and the description, with backslashes and line breaks
/// escaped.
pub fn save_annotation_colors(colors: &AnnotationColors, storage: &mut dyn Storage) {
    let text: String = colors
        .iter()
        .map(|(description, &rgb)| format!("{}\t{}\n", format_rgb(rgb), escape(description)))
        .collect();
    storage.set_string(ANNOTATION_COLORS_KEY, text);
}

/// `#rrggbb`.
fn format_rgb([r, g, b]: Rgb) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// A color as `#rrggbb`; `None` for anything else.
fn parse_rgb(text: &str) -> Option<Rgb> {
    let hex = text.trim().strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some([channel(0)?, channel(2)?, channel(4)?])
}

pub(crate) fn escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

/// The inverse of [`escape`]; `None` for a backslash before anything else.
pub(crate) fn unescape(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            out.push(match chars.next()? {
                '\\' => '\\',
                'n' => '\n',
                'r' => '\r',
                _ => return None,
            });
        } else {
            out.push(c);
        }
    }
    Some(out)
}
