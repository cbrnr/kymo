//! The minimap: a strip below the time axis, in place of the time scrollbar, over the
//! whole recording. It shows the annotations, where the window on screen lies and, with
//! signals shown, a row per shown channel: how far its signal spreads over each column, so
//! artifacts stand out bright and flat stretches dark, gaps empty. A press centers the window
//! there (in time, and with signals in rows) and dragging on moves it, a press on the window
//! moves it as grabbed; the top edge
//! drags to resize the strip, and a right-click opens its menu.
//!
//! Everything it shows is a texture made only when it changes, and none of it is read per
//! frame: the annotations that span the strip's height are one row of pixels (the
//! annotations, the legend, the width or the theme changed); the signals are each grid's
//! [`Spread`], read once on a thread of its own once the grid's pyramid is complete, and
//! a texture of the rows (the width, the rows, the strip's height or the theme changed); the
//! annotations of channels are a texture of their rows. A frame draws those and the window's
//! rectangle.

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use eframe::egui::{
    self, Color32, ColorImage, CursorIcon, Rangef, Rect, Sense, Stroke, StrokeKind, TextureHandle,
    TextureOptions, Ui, pos2,
};
use kymo_core::pyramid::Spread;
use kymo_core::service::TileService;
use kymo_core::{AnnotationStore, ChannelId, Recording};

use crate::annotations::{self, AnnotationColor, Legend, Mark, Marks, Span};
use crate::ui::menus::check_item;
use crate::ui::palette::annotation_color;
use crate::view::ViewState;

/// The lowest the strip gets, in points, and the highest, as a fraction of the trace
/// region.
pub const MIN_HEIGHT: f32 = 24.0;
pub const MAX_FRACTION: f32 = 0.25;
/// Its height at first and after Reset Height, in points: the lowest, so it takes as little
/// room from the traces as it can until it is made taller.
pub const DEFAULT_HEIGHT: f32 = MIN_HEIGHT;
/// The narrowest (and, with signals, the lowest) the window's rectangle gets, in points, so
/// it stays visible and grabbable over long recordings and many channels.
pub const MIN_WINDOW: f32 = 6.0;
/// How far from the top edge it can be grabbed, in points.
const EDGE_GRAB: f32 = 3.0;
/// Opacity of annotation bands; higher than on the traces, as only the signals lie behind
/// them.
const BAND_ALPHA: f32 = 0.5;
/// Opacity of the shade over what lies outside the window, and under the pointer or while
/// the window is dragged; the width of the window's outline, in points.
const SHADE_ALPHA: f32 = 0.55;
const SHADE_HOT_ALPHA: f32 = 0.45;
const WINDOW_STROKE: f32 = 1.5;
/// How near an annotation's mark the pointer must be, in points, for the status line to
/// name it.
const POINTER_SLACK: f32 = 2.0;
/// A column's spread over its channel's median, in octaves, from which it is the darkest
/// and up to which it gets brighter: a quarter of the median and eight times it.
const LEVEL_LOW: f32 = -2.0;
const LEVEL_HIGH: f32 = 3.0;
/// How far from the strip's background toward the text's color the darkest and the
/// brightest signal are drawn; where there are no samples, the background shows.
const DARKEST: f32 = 0.12;
const BRIGHTEST: f32 = 0.75;
/// How long a new width of the strip must hold, in seconds, before the signals' rows are
/// made for it (while the window is resized, the old ones are stretched).
const SETTLE: f64 = 0.25;
/// The context menu's items.
pub const SHOW_SIGNALS: &str = "Show Signals";
pub const RESET_HEIGHT: &str = "Reset Height";
pub const HIDE: &str = "Hide Minimap";

/// The height of the strip for `wanted` points in a trace region `region` points tall:
/// within [`MIN_HEIGHT`] and [`MAX_FRACTION`] of the region (the least wins where the
/// region is too short for both).
pub fn height(wanted: f32, region: f32) -> f32 {
    let wanted = if wanted.is_nan() {
        DEFAULT_HEIGHT
    } else {
        wanted
    };
    wanted.min(region * MAX_FRACTION).max(MIN_HEIGHT)
}

/// Where time `t` lies on a strip `width` wide over a recording `duration` seconds long, from
/// its left edge.
pub fn x_at(t: f64, duration: f64, width: f64) -> f64 {
    if duration > 0.0 {
        t / duration * width
    } else {
        0.0
    }
}

/// The time at `x` from the left edge of a strip `width` wide over a recording `duration`
/// seconds long; the inverse of [`x_at`].
pub fn time_at(x: f64, duration: f64, width: f64) -> f64 {
    if width > 0.0 {
        x / width * duration
    } else {
        0.0
    }
}

/// Where the window `time` lies on `track` (its ends on physical pixels, `ppp` to a point)
/// over a recording `duration` seconds long: at least [`MIN_WINDOW`] wide (or as wide as the
/// track, if narrower), widened around its middle, kept on the track, and its ends on
/// physical pixels. Its width comes from the window's length alone, so it keeps it as the
/// window moves. The same places the window's rows on the strip's height, `duration` rows.
pub fn window_on(track: Rangef, duration: f64, time: &Range<f64>, ppp: f32) -> Rangef {
    let ppp = if ppp > 0.0 { ppp } else { 1.0 };
    let snap = |x: f32| (x * ppp).round() / ppp;
    let width = track.span().max(0.0);
    let len = x_at(time.end - time.start, duration, f64::from(width)) as f32;
    let len = snap(len.max(MIN_WINDOW)).min(width);
    let middle = track.min + x_at((time.start + time.end) / 2.0, duration, f64::from(width)) as f32;
    let start = snap(middle - len / 2.0).clamp(track.min, track.max - len);
    Rangef::new(start, start + len)
}

/// The start of a window `span` seconds long, within a recording `duration` seconds long,
/// that holds the time `grab` after its start at time `t`, as far as the recording allows. A
/// grab of half the span centers the window on `t`. The same gives the first row.
pub fn start_for(t: f64, grab: f64, span: f64, duration: f64) -> f64 {
    let start = t - grab;
    if start.is_nan() {
        return 0.0;
    }
    start.clamp(0.0, (duration - span).max(0.0))
}

/// The strip's pixel columns, `width_px` of them, back to front: the `background`, the
/// annotations' bands (translucent, in their order) and lines (solid) in their colors from
/// `color_of`. Every mark spans the strip's height, so this row is all it shows of them.
pub fn pixels(
    marks: &Marks,
    width_px: u32,
    background: Color32,
    color_of: impl Fn(AnnotationColor) -> Color32,
) -> Vec<Color32> {
    let mut row = vec![background; width_px as usize];
    let mut paint = |columns: &Range<u32>, color: Color32| {
        let end = columns.end.min(width_px) as usize;
        for pixel in &mut row[(columns.start as usize).min(end)..end] {
            *pixel = pixel.blend(color);
        }
    };
    for band in marks.bands.iter().filter(|m| m.span == Span::All) {
        paint(
            &band.columns,
            color_of(band.color).gamma_multiply(BAND_ALPHA),
        );
    }
    for line in marks.lines.iter().filter(|m| m.span == Span::All) {
        paint(&line.columns, color_of(line.color));
    }
    row
}

/// How bright each column of a channel's spread is drawn, from its `columns` (NaN where it
/// has no samples): a column's spread over the channel's median spread, in octaves, from
/// [`LEVEL_LOW`] (0, the darkest) to [`LEVEL_HIGH`] (1); NaN stays. A channel whose median
/// is 0 (flat mostly, as a stim channel) is 1 where it spreads at all.
pub fn levels(columns: &[f32]) -> Vec<f32> {
    let mut present: Vec<f32> = columns.iter().copied().filter(|v| !v.is_nan()).collect();
    if present.is_empty() {
        return columns.to_vec();
    }
    let middle = present.len() / 2;
    let median = *present.select_nth_unstable_by(middle, f32::total_cmp).1;
    columns
        .iter()
        .map(|&v| {
            if v.is_nan() {
                f32::NAN
            } else if median > 0.0 {
                let octaves = (v / median).log2();
                ((octaves - LEVEL_LOW) / (LEVEL_HIGH - LEVEL_LOW)).clamp(0.0, 1.0)
            } else if v > 0.0 {
                1.0
            } else {
                0.0
            }
        })
        .collect()
}

/// The texture rows for `rows` channel rows on a strip `height_px` physical pixels high:
/// one per row, or one per pixel where there are more rows.
pub fn texture_rows(rows: usize, height_px: u32) -> usize {
    rows.min(height_px.max(1) as usize)
}

/// The texture row that channel row `row` of `rows` lies in, of `texture` rows.
pub fn texture_row(row: usize, rows: usize, texture: usize) -> usize {
    row * texture / rows.max(1)
}

/// The pixels of the signals' rows, `texture` rows of `width` (row-major), from each
/// channel row's levels (`None` while its spread isn't read): each the brightest level of
/// its rows, so no artifact disappears, from `dark` (0) to `bright` (1); `background` where
/// none of its rows has samples.
pub fn signal_pixels(
    levels: &[Option<&[f32]>],
    width: usize,
    texture: usize,
    background: Color32,
    (dark, bright): (Color32, Color32),
) -> Vec<Color32> {
    let mut brightest = vec![f32::NAN; width * texture];
    for (row, of_row) in levels.iter().enumerate() {
        let Some(of_row) = of_row else {
            continue;
        };
        let at = texture_row(row, levels.len(), texture);
        let line = &mut brightest[at * width..(at + 1) * width];
        for (b, &v) in line.iter_mut().zip(*of_row) {
            if b.is_nan() || v > *b {
                *b = v;
            }
        }
    }
    brightest
        .into_iter()
        .map(|v| {
            if v.is_nan() {
                background
            } else {
                dark.lerp_to_gamma(bright, v)
            }
        })
        .collect()
}

/// The pixels of the annotations tied to channels, `texture` rows of `width` (row-major)
/// for `rows` channel rows: in each, the marks over its rows, back to front, bands
/// (translucent, those of a color merged, so overlapping ones of its rows blend once) and
/// lines (solid), by color; transparent elsewhere. Marks over the strip's height are left
/// out (see [`pixels`]).
pub fn row_pixels(
    marks: &Marks,
    rows: usize,
    width: usize,
    texture: usize,
    color_of: impl Fn(AnnotationColor) -> Color32,
) -> Vec<Color32> {
    let mut out = vec![Color32::TRANSPARENT; width * texture];
    let mut by_row: BTreeMap<(usize, bool, AnnotationColor), Vec<Range<usize>>> = BTreeMap::new();
    for (marks, line) in [(&marks.bands, false), (&marks.lines, true)] {
        for m in marks {
            if let Span::Row(row) = m.span
                && row < rows
            {
                let at = texture_row(row, rows, texture);
                let columns = m.columns.start as usize..(m.columns.end as usize).min(width);
                by_row.entry((at, line, m.color)).or_default().push(columns);
            }
        }
    }
    // Bands come before lines in each row: `false` sorts first.
    for ((at, line, color), mut columns) in by_row {
        let color = if line {
            color_of(color)
        } else {
            color_of(color).gamma_multiply(BAND_ALPHA)
        };
        columns.sort_unstable_by_key(|c| c.start);
        let pixels = &mut out[at * width..(at + 1) * width];
        let mut done = 0;
        for c in columns {
            for pixel in &mut pixels[c.start.max(done).min(c.end)..c.end] {
                *pixel = pixel.blend(color);
            }
            done = done.max(c.end);
        }
    }
    out
}

/// What the minimap asked of the viewer in a frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Shown {
    /// A new start for the window, in seconds.
    pub start: Option<f64>,
    /// A new first row on screen, with signals shown.
    pub top: Option<f64>,
    /// A new height, in points, before [`height`] limits it.
    pub height: Option<f32>,
    /// Whether Show Signals was chosen, to show them (`true`) or not.
    pub signals: Option<bool>,
    /// Whether Hide Minimap was chosen.
    pub hide: bool,
    /// What lies under the pointer.
    pub pointer: Option<Pointer>,
}

/// What lies under the pointer over the minimap.
#[derive(Clone, Debug, PartialEq)]
pub struct Pointer {
    pub time: f64,
    /// The channel of the row, with signals shown.
    pub channel: Option<ChannelId>,
    /// The descriptions of the annotations there.
    pub under: Vec<String>,
}

/// Each grid's spread, read on a thread of its own once its pyramid is complete.
#[derive(Default)]
struct Spreads {
    /// Per grid: whether its read has started.
    asked: Vec<bool>,
    read: Vec<Spread>,
    channel: Option<(mpsc::Sender<Spread>, mpsc::Receiver<Spread>)>,
}

impl Spreads {
    /// Starts reading the spreads of the grids whose pyramids are complete, and takes those
    /// read since the last frame.
    fn update(&mut self, ctx: &egui::Context, service: &TileService) {
        let recording = service.recording();
        let (sender, receiver) = self.channel.get_or_insert_with(mpsc::channel);
        let grids = recording.grids().len();
        self.asked.resize(grids, false);
        for (g, asked) in self.asked.iter_mut().enumerate() {
            let Some(pyramid) = service.pyramid(kymo_core::GridId(g as u32)) else {
                continue;
            };
            if *asked || !pyramid.is_complete() {
                continue;
            }
            *asked = true;
            let (recording, pyramid): (Arc<Recording>, _) =
                (Arc::clone(recording), pyramid.clone());
            let (sender, ctx) = (sender.clone(), ctx.clone());
            // Without a thread, the grid's row stays empty.
            let _ = std::thread::Builder::new()
                .name("kymo-minimap".into())
                .spawn(move || {
                    if let Ok(Some(spread)) = Spread::read(&recording, &pyramid) {
                        let _ = sender.send(spread);
                        ctx.request_repaint();
                    }
                });
        }
        while let Ok(read) = receiver.try_recv() {
            self.read.push(read);
        }
    }
}

/// The minimap's state: what it draws, made when it changes, and a press holding the
/// window.
#[derive(Default)]
pub struct Minimap {
    /// The annotations' marks, and the store's revision, the legend's shown and colors
    /// revisions, the duration, the width in physical pixels and (with signals) the rows'
    /// revision they were made for.
    marks: Marks,
    marks_for: Option<(u64, u64, u64, u64, u32, Option<u64>)>,
    /// The row of pixels of the marks over the strip's height, and whether it lies over
    /// signals and the theme it was made for.
    texture: Option<TextureHandle>,
    texture_for: Option<(bool, bool, Color32)>,
    /// The texture of the marks of channels' rows, the marks it shows and the texture rows
    /// and theme it was made for.
    row_texture: Option<TextureHandle>,
    row_marks: Vec<Mark>,
    row_texture_for: Option<(usize, bool, Color32)>,
    row_marks_changed: bool,
    /// The rows (the shown channels, top to bottom) and how often they changed.
    rows: Vec<ChannelId>,
    rows_revision: u64,
    spreads: Spreads,
    /// Per channel (by id): how bright each column of its row is (see [`levels`]); the
    /// duration and width they were made for and how many spreads they hold, how often
    /// they changed, and a new duration and width waiting to settle since when.
    levels: Vec<Option<Box<[f32]>>>,
    levels_size: Option<(u64, u32)>,
    levels_read: usize,
    levels_revision: u64,
    size_since: Option<((u64, u32), f64)>,
    /// The signals' texture, and the levels' and rows' revisions, texture rows and theme it
    /// was made for.
    signal_texture: Option<TextureHandle>,
    signal_for: Option<(u64, u64, usize, Color32, Color32)>,
    /// While a press on the strip holds the window: the time it holds, after the window's
    /// start, in seconds, and (with signals) the row, after its first row.
    grab: Option<(f64, f64)>,
}

/// What the minimap shows and acts on.
pub struct Input<'a> {
    pub store: &'a AnnotationStore,
    pub legend: &'a Legend,
    /// The recording's length and the window's times, in seconds.
    pub duration: f64,
    pub window: Range<f64>,
    /// Where signals are shown: the view, for its rows, and the tile service, for the
    /// pyramids.
    pub signals: Option<Signals<'a>>,
}

/// What the minimap's signals need.
pub struct Signals<'a> {
    pub view: &'a ViewState,
    pub service: &'a TileService,
}

impl Minimap {
    /// How many grids' spreads have been read, for the signals' rows.
    pub fn spreads_read(&self) -> usize {
        self.spreads.read.len()
    }

    /// Draws the minimap in `rect` (its left edge on a physical pixel, `width_px` pixels
    /// wide) and handles its input.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        id: egui::Id,
        rect: Rect,
        width_px: u32,
        input: &Input<'_>,
    ) -> Shown {
        let mut shown = Shown::default();
        let duration = input.duration;
        let span = input.window.end - input.window.start;
        let width = f64::from(rect.width());
        let time_of = |x: f32| time_at(f64::from(x - rect.left()), duration, width);
        let ppp = ui.pixels_per_point();
        let window = window_on(rect.x_range(), duration, &input.window, ppp);
        // With signals, the rows: all of them over the strip's height, and those on screen.
        let rows = input.signals.as_ref().map(|s| {
            let view = s.view;
            let count = view.row_count() as f64;
            let on_screen = view.top()..view.top() + view.rows() as f64;
            (view, count, on_screen)
        });
        let row_of = |y: f32, count: f64| {
            time_at(f64::from(y - rect.top()), count, f64::from(rect.height()))
        };
        let rows_window = match &rows {
            Some((_, count, on_screen)) if *count > 0.0 => {
                window_on(rect.y_range(), *count, on_screen, ppp)
            }
            _ => rect.y_range(),
        };
        let window_rect = Rect::from_x_y_ranges(window, rows_window);

        let response = ui.interact(rect, id, Sense::click_and_drag());
        let primary = ui.input(|i| i.pointer.primary_down());
        if response.is_pointer_button_down_on() && primary {
            let pointer = response.interact_pointer_pos();
            if self.grab.is_none()
                && let Some(press) = ui.input(|i| i.pointer.press_origin())
            {
                // Where the press was on the window, else the window's middle.
                let on_window = window_rect.contains(press);
                let time = if on_window {
                    time_of(press.x) - input.window.start
                } else {
                    span / 2.0
                };
                let row = match &rows {
                    Some((_, count, on_screen)) if on_window => {
                        row_of(press.y, *count) - on_screen.start
                    }
                    Some((_, _, on_screen)) => (on_screen.end - on_screen.start) / 2.0,
                    None => 0.0,
                };
                self.grab = Some((time, row));
            }
            if let (Some((grab, grab_row)), Some(p)) = (self.grab, pointer) {
                let start = start_for(time_of(p.x), grab, span, duration);
                if start != input.window.start {
                    shown.start = Some(start);
                }
                if let Some((_, count, on_screen)) = &rows {
                    let visible = on_screen.end - on_screen.start;
                    let top = start_for(row_of(p.y, *count), grab_row, visible, *count);
                    if top != on_screen.start {
                        shown.top = Some(top);
                    }
                }
            }
        } else {
            self.grab = None;
        }
        response.context_menu(|ui| {
            let on = input.signals.is_some();
            if check_item(ui, true, on, false, SHOW_SIGNALS, None).clicked() {
                shown.signals = Some(!on);
                ui.close();
            }
            if ui.button(RESET_HEIGHT).clicked() {
                shown.height = Some(DEFAULT_HEIGHT);
                ui.close();
            }
            if ui.button(HIDE).clicked() {
                shown.hide = true;
                ui.close();
            }
        });

        // The top edge, over the strip, so a press near it resizes rather than moves.
        let edge = Rect::from_x_y_ranges(
            rect.x_range(),
            rect.top() - EDGE_GRAB..=rect.top() + EDGE_GRAB,
        );
        let edge_response = ui.interact(edge, id.with("edge"), Sense::click_and_drag());
        let resizing = edge_response.hovered() || edge_response.dragged();
        if resizing {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
        }
        if edge_response.double_clicked() {
            shown.height = Some(DEFAULT_HEIGHT);
        } else if edge_response.dragged()
            && let Some(p) = edge_response.interact_pointer_pos()
        {
            shown.height = Some(rect.bottom() - p.y);
        }

        if let Some(p) = response.hover_pos().filter(|_| !resizing) {
            let channel = rows.as_ref().and_then(|(view, count, _)| {
                let row = row_of(p.y, *count).floor().max(0.0) as usize;
                let channels = view.row_channels();
                channels
                    .get(row.min(channels.len().saturating_sub(1)))
                    .copied()
            });
            let slack = (POINTER_SLACK * ppp).round() as u32;
            let x = f64::from((p.x - rect.left()) * ppp);
            let under = annotations::overview_under(
                input.store,
                input.legend,
                duration,
                width_px,
                x,
                slack,
                channel,
            );
            shown.pointer = Some(Pointer {
                time: time_of(p.x),
                channel,
                under: under
                    .into_iter()
                    .map(|a| a.data.description.clone())
                    .collect(),
            });
        }

        let height_px = (rect.height() * ppp).round().max(1.0) as u32;
        self.update(ui, input, width_px, height_px);
        if ui.is_rect_visible(rect) {
            let hot = response.dragged() || response.hovered() && !resizing;
            self.paint(
                ui,
                rect,
                window_rect,
                hot,
                resizing,
                input.signals.is_some(),
            );
        }
        shown
    }

    /// Makes the marks, the levels and their textures again where what they show changed.
    fn update(&mut self, ui: &Ui, input: &Input<'_>, width_px: u32, height_px: u32) {
        let visuals = ui.visuals();
        let background = visuals.extreme_bg_color;
        let color_of = |c| annotation_color(c, visuals);
        let signals = input.signals.as_ref();
        if let Some(s) = signals
            && s.view.row_channels() != self.rows.as_slice()
        {
            self.rows = s.view.row_channels().to_vec();
            self.rows_revision += 1;
        }

        let marks_for = (
            input.store.revision(),
            input.legend.shown_revision(),
            input.legend.colors_revision(),
            input.duration.to_bits(),
            width_px,
            signals.map(|_| self.rows_revision),
        );
        let mut marks_changed = false;
        if self.marks_for != Some(marks_for) {
            self.marks = annotations::overview_marks(
                input.store,
                input.legend,
                input.duration,
                width_px,
                signals.map(|s| s.view),
            );
            self.marks_for = Some(marks_for);
            marks_changed = true;
            let row_marks: Vec<Mark> = (self.marks.bands.iter().chain(&self.marks.lines))
                .filter(|m| m.span != Span::All)
                .cloned()
                .collect();
            if row_marks != self.row_marks {
                self.row_marks = row_marks;
                self.row_marks_changed = true;
            }
        }
        let texture_for = (signals.is_some(), visuals.dark_mode, background);
        if marks_changed || self.texture_for != Some(texture_for) {
            self.texture_for = Some(texture_for);
            // Over signals, the annotations let them show through.
            let under = if signals.is_some() {
                Color32::TRANSPARENT
            } else {
                background
            };
            let row = pixels(&self.marks, width_px, under, color_of);
            set_texture(
                ui,
                &mut self.texture,
                "minimap",
                [width_px as usize, 1],
                row,
            );
        }

        let Some(s) = signals else {
            return;
        };
        let rows = self.rows.len();
        let texture = texture_rows(rows, height_px);
        if rows > 0 {
            let row_texture_for = (texture, visuals.dark_mode, background);
            if !self.row_marks.is_empty()
                && (self.row_marks_changed || self.row_texture_for != Some(row_texture_for))
            {
                let image = row_pixels(&self.marks, rows, width_px as usize, texture, color_of);
                set_texture(
                    ui,
                    &mut self.row_texture,
                    "minimap rows",
                    [width_px as usize, texture],
                    image,
                );
                self.row_texture_for = Some(row_texture_for);
                self.row_marks_changed = false;
            }
        }

        self.spreads.update(ui.ctx(), s.service);
        self.update_levels(
            ui,
            input.duration,
            width_px,
            s.service.recording().channels().len(),
        );
        let Some((_, levels_width)) = self.levels_size else {
            return;
        };
        let text = visuals.text_color();
        let shades = (
            background.lerp_to_gamma(text, DARKEST),
            background.lerp_to_gamma(text, BRIGHTEST),
        );
        let signal_for = (
            self.levels_revision,
            self.rows_revision,
            texture,
            background,
            text,
        );
        if rows > 0 && self.signal_for != Some(signal_for) {
            let by_row: Vec<Option<&[f32]>> = self
                .rows
                .iter()
                .map(|c| self.levels.get(c.0 as usize).and_then(|l| l.as_deref()))
                .collect();
            let width = levels_width as usize;
            let image = signal_pixels(&by_row, width, texture, background, shades);
            set_texture(
                ui,
                &mut self.signal_texture,
                "minimap signals",
                [width, texture],
                image,
            );
            self.signal_for = Some(signal_for);
        }
    }

    /// Brings the levels up to the spreads read and the strip's columns, `width_px` over
    /// `duration` seconds, for a recording of `channels` channels. A new width (or
    /// duration) waits until it has held for [`SETTLE`], while the window is resized, the
    /// old levels drawn stretched meanwhile; spreads read since are added at once.
    fn update_levels(&mut self, ui: &Ui, duration: f64, width_px: u32, channels: usize) {
        let size = (duration.to_bits(), width_px);
        let now = ui.input(|i| i.time);
        if self.levels_size.is_some_and(|s| s != size) {
            match self.size_since {
                Some((s, since)) if s == size && now - since >= SETTLE => {}
                Some((s, _)) if s == size => {
                    ui.ctx()
                        .request_repaint_after(Duration::from_secs_f64(SETTLE));
                    return;
                }
                _ => {
                    self.size_since = Some((size, now));
                    ui.ctx()
                        .request_repaint_after(Duration::from_secs_f64(SETTLE));
                    return;
                }
            }
        }
        let from = if self.levels_size == Some(size) {
            self.levels_read
        } else {
            self.levels = vec![None; channels];
            0
        };
        if from == self.spreads.read.len() && self.levels_size == Some(size) {
            return;
        }
        let each = (width_px as usize).max(1);
        for spread in &self.spreads.read[from..] {
            let columns = spread.columns(duration, width_px);
            for (&channel, columns) in spread.channels().iter().zip(columns.chunks(each)) {
                if let Some(slot) = self.levels.get_mut(channel.0 as usize) {
                    *slot = Some(levels(columns).into());
                }
            }
        }
        self.levels_size = Some(size);
        self.size_since = None;
        self.levels_read = self.spreads.read.len();
        self.levels_revision += 1;
    }

    fn paint(&self, ui: &Ui, rect: Rect, window: Rect, hot: bool, resizing: bool, signals: bool) {
        let visuals = ui.visuals();
        let ppp = ui.pixels_per_point();
        let painter = ui.painter_at(rect.expand2(egui::vec2(0.0, 1.0)));
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        if signals {
            painter.rect_filled(rect, 0.0, visuals.extreme_bg_color);
            if !self.rows.is_empty()
                && let Some(texture) = &self.signal_texture
            {
                painter.image(texture.id(), rect, uv, Color32::WHITE);
            }
        }
        if let Some(texture) = &self.texture {
            painter.image(texture.id(), rect, uv, Color32::WHITE);
        }
        if signals
            && !self.rows.is_empty()
            && !self.row_marks.is_empty()
            && let Some(texture) = &self.row_texture
        {
            painter.image(texture.id(), rect, uv, Color32::WHITE);
        }
        let grid = visuals.widgets.noninteractive.bg_stroke.color;

        // The top edge: the grid's line, under the window's outline.
        let top = rect.top() + 0.5 / ppp;
        painter.hline(rect.x_range(), top, Stroke::new(1.0 / ppp, grid));

        // The window: what lies outside it dimmed, so it stands out however dense the
        // annotations are, and outlined in the selection's color, in whole pixels.
        let shade = visuals.extreme_bg_color.gamma_multiply(if hot {
            SHADE_HOT_ALPHA
        } else {
            SHADE_ALPHA
        });
        let left = Rect::from_x_y_ranges(rect.left()..=window.left(), rect.y_range());
        let right = Rect::from_x_y_ranges(window.right()..=rect.right(), rect.y_range());
        let above = Rect::from_x_y_ranges(window.x_range(), rect.top()..=window.top());
        let below = Rect::from_x_y_ranges(window.x_range(), window.bottom()..=rect.bottom());
        for r in [left, right, above, below] {
            if r.width() > 0.0 && r.height() > 0.0 {
                painter.rect_filled(r, 0.0, shade);
            }
        }
        let width = (WINDOW_STROKE * ppp).round().max(1.0) / ppp;
        painter.rect_stroke(
            window,
            2.0,
            Stroke::new(width, visuals.selection.stroke.color),
            StrokeKind::Inside,
        );

        // Highlighted while it can be dragged, over everything.
        if resizing {
            painter.hline(
                rect.x_range(),
                rect.top(),
                visuals.widgets.hovered.fg_stroke,
            );
        }
    }
}

/// Sets `texture` to `pixels`, `size` (width, height), making it the first time.
fn set_texture(
    ui: &Ui,
    texture: &mut Option<TextureHandle>,
    name: &str,
    size: [usize; 2],
    pixels: Vec<Color32>,
) {
    let image = ColorImage::new(size, pixels);
    match texture {
        Some(texture) => texture.set(image, TextureOptions::NEAREST),
        None => *texture = Some(ui.ctx().load_texture(name, image, TextureOptions::NEAREST)),
    }
}
