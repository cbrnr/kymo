//! The Settings window.

use eframe::egui::{self, Ui};

use crate::disk_cache::{DiskCache, format_bytes};
use crate::settings::{
    DEFAULT_WINDOW_S, DISK_CACHE_MB, GPU_MEMORY_MB, OFF, SESSION, Settings, TILE_CACHE_MB, Theme,
    UNLIMITED,
};
use crate::ui::fonts::SYSTEM_FONT_NAME;
use crate::ui::minimap;
use crate::ui::stepper::stepper;
use crate::ui::types::CHANNEL_TYPES;
use crate::ui::{Overlay, theme, window_title};

/// What the Channel Types row's button is labeled.
pub const EDIT_TYPES: &str = "Edit\u{2026}";

/// What the Disk Cache row's button is labeled.
pub const CLEAR_CACHE: &str = "Clear";

/// The Settings window, while `open`. Its Channel Types row's button opens the Channel
/// Types window (`channel_types`); its Disk Cache row, where there is a cache, sets its
/// limit, shows `cache` and sets `clear_cache` when its button is clicked, and with the
/// cache off warns of the memory overviews take: `overview` bytes for the open recording,
/// if there is one. Returns whether a setting changed; the caller applies it at once.
pub fn settings_window(
    ctx: &egui::Context,
    open: &mut bool,
    settings: &mut Settings,
    channel_types: &mut bool,
    cache: &DiskCache,
    overview: Option<u64>,
    clear_cache: &mut bool,
) -> bool {
    let before = settings.clone();
    let defaults = Settings::default();
    Overlay::Settings.center_on_open(ctx, *open);
    egui::Window::new(window_title(ctx, Overlay::Settings, "Settings"))
        .id(Overlay::Settings.id())
        .title_frame(theme::title_frame(ctx))
        .open(open)
        .collapsible(false)
        .resizable(false)
        // The window's width counts its frame too.
        .min_width(
            content_width(ctx)
                + egui::Frame::window(&ctx.global_style())
                    .total_margin()
                    .sum()
                    .x,
        )
        .show(ctx, |ui| {
            // Within most of the screen, scrolling beyond.
            let max_height = ui.ctx().content_rect().height() * 0.8;
            egui::ScrollArea::vertical()
                .max_height(max_height)
                .min_scrolled_height(max_height)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    egui::Grid::new("settings")
                        .num_columns(2)
                        .spacing([GAP, 6.0])
                        .show(ui, |ui| {
                            // How it looks, then how a recording opens, then memory.
                            ui.label("Theme");
                            ui.horizontal(|ui| {
                                for theme in Theme::ALL {
                                    ui.radio_value(&mut settings.theme, theme, theme.label());
                                }
                            });
                            ui.end_row();
                            hint(ui, "System follows the system's light or dark mode.");
                            if let Some(name) = SYSTEM_FONT_NAME {
                                ui.label("Font");
                                ui.checkbox(&mut settings.system_font, "System Font");
                                ui.end_row();
                                hint(ui, &format!("Use the system font ({name}) for all text."));
                            }
                            ui.label("Animations");
                            ui.checkbox(&mut settings.animations, "Animate Changes");
                            ui.end_row();
                            hint(ui, "Show changes to the traces with smooth motion.");
                            let label = ui.label(CHANNEL_TYPES);
                            if ui.button(EDIT_TYPES).labelled_by(label.id).clicked() {
                                *channel_types = true;
                            }
                            ui.end_row();
                            hint(ui, "Color, DC removal, and scaling of each channel type.");
                            let label = ui.label("Default Window");
                            stepper(
                                ui,
                                &mut settings.default_window_s,
                                DEFAULT_WINDOW_S,
                                1.0,
                                "s",
                                "Default Window",
                            )
                            .labelled_by(label.id);
                            ui.end_row();
                            hint(ui, "Seconds shown when a recording opens.");
                            ui.label("Scale");
                            ui.checkbox(&mut settings.auto_scale_on_open, "Automatic on Opening");
                            ui.end_row();
                            hint(ui, "Fit the amplitudes to the first window of a recording.");
                            ui.label("Minimap");
                            ui.checkbox(&mut settings.minimap_signals, minimap::SHOW_SIGNALS);
                            ui.end_row();
                            hint(
                                ui,
                                "A coarse picture of each channel under the annotations.",
                            );
                            budget_row(
                                ui,
                                "Tile Cache",
                                "Memory for recently viewed data.",
                                &mut settings.tile_cache_mb,
                                &TILE_CACHE_MB,
                                defaults.tile_cache_mb,
                                megabytes_text,
                            );
                            budget_row(
                                ui,
                                "GPU Memory",
                                "Graphics memory for recently viewed data.",
                                &mut settings.gpu_memory_mb,
                                &GPU_MEMORY_MB,
                                defaults.gpu_memory_mb,
                                megabytes_text,
                            );
                            if cache.exists() {
                                let label = ui.label("Disk Cache");
                                // Its limit, then what it holds.
                                ui.horizontal(|ui| {
                                    limit_menu(
                                        ui,
                                        label.id,
                                        &mut settings.disk_cache_mb,
                                        &DISK_CACHE_MB,
                                        defaults.disk_cache_mb,
                                        disk_cache_text,
                                    );
                                    let button = ui.add_enabled(
                                        cache.clearable(),
                                        egui::Button::new(CLEAR_CACHE),
                                    );
                                    if button.labelled_by(label.id).clicked() {
                                        *clear_cache = true;
                                    }
                                    ui.label(cache.text());
                                });
                                ui.end_row();
                                match settings.disk_cache_mb {
                                    OFF => warning(ui, &disk_cache_off_warning(overview)),
                                    SESSION => hint(ui, DISK_CACHE_SESSION_HINT),
                                    _ => hint(ui, DISK_CACHE_HINT),
                                }
                            }
                            ui.label("Updates");
                            ui.checkbox(&mut settings.check_updates, "Check at Startup");
                            ui.end_row();
                            hint(ui, "Look for a newer version on GitHub once a day.");
                        });
                });
            ui.add_space(6.0);
            if ui.button("Restore Defaults").clicked() {
                // What the View menu sets stays as it is, and the minimap's height, set
                // where it is.
                *settings = Settings {
                    antialias: settings.antialias,
                    scale_bars: settings.scale_bars,
                    minimap: settings.minimap,
                    minimap_height: settings.minimap_height,
                    remove_dc: settings.remove_dc,
                    ..Settings::default()
                };
            }
        });
    *settings = settings.clone().clamped();
    *settings != before
}

/// Space between the label and value columns.
const GAP: f32 = 12.0;

/// The settings' row labels, in the first column.
const LABELS: [&str; 10] = [
    "Theme",
    "Font",
    "Channel Types",
    "Default Window",
    "Scale",
    "Minimap",
    "Tile Cache",
    "GPU Memory",
    "Disk Cache",
    "Updates",
];

fn text_width(ctx: &egui::Context, text: &str) -> f32 {
    let font = egui::TextStyle::Body.resolve(&ctx.global_style());
    ctx.fonts_mut(|f| {
        f.layout_no_wrap(text.to_owned(), font, egui::Color32::WHITE)
            .size()
            .x
    })
}

/// The width of the window's content: the widest label, then [`VALUES_WIDTH`]. Set from
/// the first frame, as egui sizes a window from a first pass that gives the values column
/// less room than its widest row takes, and would widen the window a frame later (moving its
/// close button).
fn content_width(ctx: &egui::Context) -> f32 {
    let labels = LABELS
        .iter()
        .map(|label| text_width(ctx, label))
        .fold(0.0, f32::max);
    labels + GAP + VALUES_WIDTH
}

/// The values column's width: the longest hint (Scale's, about 300 points in egui's font)
/// on one line, with room for wider fonts.
const VALUES_WIDTH: f32 = 360.0;

/// What the Disk Cache row's hint says.
pub const DISK_CACHE_HINT: &str = "Overviews that make recordings open faster.";
/// What the Disk Cache row's hint says for the session only.
pub const DISK_CACHE_SESSION_HINT: &str =
    "Overviews that make recordings open faster, removed when Kymo quits.";

/// What the Disk Cache row warns of with the cache off: the memory the open recording's
/// overviews take (`overview` bytes), or about how much they take without one.
pub fn disk_cache_off_warning(overview: Option<u64>) -> String {
    let size = match overview {
        Some(bytes) => format!("{} for this recording", format_bytes(bytes)),
        None => "about 10% of each recording".to_owned(),
    };
    format!("Overviews use extra memory ({size}).")
}

/// A label, a menu of the `offered` budgets for `mb` (`default` marked as such), each shown
/// as `text` gives it, and a hint below in the menu's column.
fn budget_row(
    ui: &mut Ui,
    label: &str,
    hint: &str,
    mb: &mut u32,
    offered: &[u32],
    default: u32,
    text: fn(u32) -> String,
) {
    let label = ui.label(label);
    limit_menu(ui, label.id, mb, offered, default, text);
    ui.end_row();
    self::hint(ui, hint);
}

/// A menu of the `offered` budgets for `mb`, labeled by `label`, `default` marked as such,
/// each shown as `text` gives it.
fn limit_menu(
    ui: &mut Ui,
    label: egui::Id,
    mb: &mut u32,
    offered: &[u32],
    default: u32,
    text: fn(u32) -> String,
) {
    egui::ComboBox::from_id_salt(label)
        .selected_text(text(*mb))
        .show_ui(ui, |ui| {
            for &o in offered {
                let shown = if o == default {
                    format!("{} (Default)", text(o))
                } else {
                    text(o)
                };
                ui.selectable_value(mb, o, shown);
            }
        })
        .response
        .labelled_by(label);
}

/// The disk cache's limit as its menu shows it: "Off", "Session", "Unlimited" for no
/// limit, else as [`megabytes_text`].
pub fn disk_cache_text(mb: u32) -> String {
    match mb {
        OFF => "Off".to_owned(),
        SESSION => "Session".to_owned(),
        UNLIMITED => "Unlimited".to_owned(),
        mb => megabytes_text(mb),
    }
}

/// A budget as the menus show it: in MB below a GB, else in GB.
pub fn megabytes_text(mb: u32) -> String {
    if mb >= 1024 && mb.is_multiple_of(1024) {
        format!("{} GB", mb / 1024)
    } else if mb >= 1024 {
        format!("{:.1} GB", f64::from(mb) / 1024.0)
    } else {
        format!("{mb} MB")
    }
}

/// A hint below a row, in the value's column.
fn hint(ui: &mut Ui, text: &str) {
    ui.label("");
    ui.add(super::hint(text));
    ui.end_row();
}

/// A warning below a row, in the value's column: a hint in the warning color, as the
/// sidebar's warnings are.
fn warning(ui: &mut Ui, text: &str) {
    ui.label("");
    let color = ui.visuals().warn_fg_color;
    ui.add(egui::Label::new(super::hint_text(text).color(color)).wrap());
    ui.end_row();
}
