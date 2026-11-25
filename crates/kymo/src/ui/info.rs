//! The Recording Info window: everything `kymo --info` prints, with a button that copies it.

use std::path::Path;

use eframe::egui::{self, Align, Label, Layout, RichText, TextStyle};

use crate::action::Action;
use crate::document::{GridSummary, InfoRow, Summary, info_text};
use crate::ui::{Overlay, theme, window_title};

/// What the Copy button is named, also for tests.
pub const COPY: &str = "Copy";

/// The Recording Info window, while `open`: the file's name and path, then the rows of
/// [`Summary::info_rows`], keys at the left (capitalized, grayed; a format field's name as
/// its key: "Stream 1", "Patient") and values wrapping in the width left, and below them a
/// table of each grid's channels ([`grid_table`]) in place of its rows. Copy puts
/// `--info`'s text on the clipboard.
pub fn info_window(ctx: &egui::Context, open: &mut bool, path: &Path, summary: &Summary) {
    if !*open {
        // Its grids folded again when it next opens.
        ctx.data_mut(|d| {
            for i in 0..summary.grids.len() {
                d.remove_temp::<bool>(unfolded_id(i));
            }
        });
        return;
    }
    let rows = summary.info_rows();
    let style = ctx.global_style();
    let font = TextStyle::Body.resolve(&style);
    let color = style.visuals.text_color();
    let key_width = ctx.fonts_mut(|f| {
        rows.iter()
            .filter(|row| row.grid.is_none())
            .map(|row| {
                f.layout_no_wrap(key_text(row), font.clone(), color)
                    .size()
                    .x
            })
            .fold(0.0, f32::max)
    });
    Overlay::RecordingInfo.center_on_open(ctx, *open);
    let title = window_title(
        ctx,
        Overlay::RecordingInfo,
        Action::RecordingInfo.label().trim_end_matches('…'),
    );
    egui::Window::new(title)
        .id(Overlay::RecordingInfo.id())
        .title_frame(theme::title_frame(ctx))
        .open(open)
        .collapsible(false)
        // As wide as dragged, and as tall as its rows up to most of the screen, where they
        // scroll: a window's own scroll area (`vscroll`) keeps it at a fixed height.
        .resizable([true, false])
        .default_width(640.0)
        .show(ctx, |ui| {
            let max_height = ui.ctx().content_rect().height() * 0.7;
            egui::ScrollArea::vertical()
                .max_height(max_height)
                .auto_shrink([false, true])
                .show(ui, |ui| rows_ui(ui, path, summary, key_width));
            ui.add_space(6.0);
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if ui.button(COPY).clicked() {
                    ui.ctx().copy_text(info_text(path, summary));
                }
            });
        });
}

/// The window's rows: the file's name and path, then [`Summary::info_rows`] but the grids',
/// with the keys in a column `key_width` wide, then the grids' tables.
fn rows_ui(ui: &mut egui::Ui, path: &Path, summary: &Summary, key_width: f32) {
    let name = path.file_name().unwrap_or(path.as_os_str());
    ui.label(RichText::new(name.to_string_lossy()).strong());
    ui.add(Label::new(RichText::new(path.display().to_string()).small().weak()).wrap());
    ui.add_space(6.0);
    let gap = 12.0;
    for row in summary.info_rows() {
        if row.grid.is_some() {
            continue;
        }
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(key_width, 0.0),
                Layout::top_down(Align::Min),
                |ui| {
                    ui.set_min_width(key_width);
                    ui.add(Label::new(RichText::new(key_text(&row)).weak()).extend());
                },
            );
            ui.add_space(gap - ui.spacing().item_spacing.x);
            ui.add(Label::new(unbroken(&row.value)).wrap());
        });
    }
    for (i, grid) in summary.grids.iter().enumerate() {
        ui.add_space(6.0);
        grid_table(ui, i, grid);
    }
}

/// Names of the channel tables' columns, also for tests; the last three only where a
/// channel of the grid has a value for them.
pub const COLUMNS: [&str; 7] = [
    "#",
    "Name",
    "Type",
    "Unit",
    "Physical Range",
    "Prefilter",
    "Transducer",
];

/// Grid `index`'s channels as the file describes them, under a header that unfolds them
/// ("Grid 2 · iViewNG_HMD (Gaze)", its stream named where the recording has several): the
/// grid's rate, samples and channels, then a striped table, a row per channel
/// ([`COLUMNS`]), which scrolls sideways where it is wider than the window. Folded whenever
/// the window opens: whether it is unfolded is kept only while the window is open, and not
/// in egui's memory, which kymo saves.
fn grid_table(ui: &mut egui::Ui, index: usize, grid: &GridSummary) {
    let unfolded = unfolded_id(index);
    let open = ui.data(|d| d.get_temp::<bool>(unfolded)).unwrap_or(false);
    let mut title = format!("Grid {}", index + 1);
    if let Some(stream) = &grid.stream {
        title += &format!(" \u{b7} {stream}");
    }
    let response = egui::CollapsingHeader::new(RichText::new(title).strong())
        .id_salt(("kymo info grid", index))
        .open(Some(open))
        .show(ui, |ui| {
            ui.add(Label::new(RichText::new(unbroken(&grid.describe())).weak()).wrap());
            let range = grid.channels.iter().any(|c| c.range.is_some());
            let prefilter = grid.channels.iter().any(|c| !c.prefilter.is_empty());
            let transducer = grid.channels.iter().any(|c| !c.transducer.is_empty());
            let shown = [true, true, true, true, range, prefilter, transducer];
            egui::ScrollArea::horizontal()
                .id_salt(("kymo info channels", index))
                .show(ui, |ui| {
                    egui::Grid::new(("kymo info table", index))
                        .striped(true)
                        .spacing([12.0, 2.0])
                        .show(ui, |ui| {
                            for (name, _) in COLUMNS.iter().zip(shown).filter(|(_, s)| *s) {
                                ui.add(Label::new(RichText::new(*name).weak()).extend());
                            }
                            ui.end_row();
                            for c in &grid.channels {
                                let range = c.range.map_or(String::new(), |(min, max)| {
                                    format!("{} \u{2026} {}", number(min), number(max))
                                });
                                let cells = [
                                    c.number.to_string(),
                                    c.name.clone(),
                                    c.kind.clone(),
                                    c.unit.clone(),
                                    range,
                                    c.prefilter.clone(),
                                    c.transducer.clone(),
                                ];
                                for (cell, _) in cells.into_iter().zip(shown).filter(|(_, s)| *s) {
                                    ui.add(Label::new(cell).extend());
                                }
                                ui.end_row();
                            }
                        });
                });
        });
    if response.header_response.clicked() {
        ui.data_mut(|d| d.insert_temp(unfolded, !open));
    }
}

/// Whether grid `index`'s table is unfolded, while the window is open.
fn unfolded_id(index: usize) -> egui::Id {
    egui::Id::unique(("kymo info grid unfolded", index))
}

/// `value` as written, or in scientific notation where that is long (a float format's
/// range: "3.4028234663852886e38").
fn number(value: f64) -> String {
    let plain = value.to_string();
    if plain.len() > 12 {
        format!("{value:e}")
    } else {
        plain
    }
}

/// A row's key as shown: its format field's name, or its key, capitalized.
fn key_text(row: &InfoRow) -> String {
    capitalized(row.field.as_deref().unwrap_or(&row.key))
}

/// `text` with a non-breaking space before each "Hz", so a rate's unit stays on its line
/// ("60 Hz" in a stream's line).
fn unbroken(text: &str) -> String {
    text.replace(" Hz", "\u{a0}Hz")
}

/// `key` with its first letter in upper case ("grid 1" → "Grid 1", "stream 1" →
/// "Stream 1").
fn capitalized(key: &str) -> String {
    let mut chars = key.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}
