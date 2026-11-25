//! The minimap: a strip below the time axis, in place of the time scrollbar, over the
//! whole recording. It shows the annotations, the stretches without samples, and where the
//! window on screen lies. A press centers the window there and dragging on moves it, a
//! press on the window moves it as grabbed; the top edge drags to resize the strip, and a
//! right-click opens its menu.
//!
//! Every mark spans the strip's height, so the annotations and gaps are one row of
//! pixels, a texture made only when they change (the annotations, the legend, the width,
//! the theme); a frame draws it and the window's rectangle.

use std::ops::Range;

use eframe::egui::{
    self, Color32, ColorImage, CursorIcon, Rangef, Rect, Sense, Stroke, StrokeKind, TextureHandle,
    TextureOptions, Ui, pos2,
};
use kymo_core::{AnnotationStore, Recording};

use crate::annotations::{self, AnnotationColor, Legend, Marks};
use crate::ui::palette::annotation_color;

/// The lowest the strip gets, in points, and the highest, as a fraction of the trace
/// region.
pub const MIN_HEIGHT: f32 = 24.0;
pub const MAX_FRACTION: f32 = 0.25;
/// Its height at first and after Reset Height, in points: the lowest, so it takes as little
/// room from the traces as it can until it is made taller.
pub const DEFAULT_HEIGHT: f32 = MIN_HEIGHT;
/// The narrowest the window's rectangle gets, in points, so it stays visible and grabbable
/// over long recordings.
pub const MIN_WINDOW: f32 = 6.0;
/// How far from the top edge it can be grabbed, in points.
const EDGE_GRAB: f32 = 3.0;
/// Opacity of annotation bands; higher than on the traces, as nothing lies behind them.
const BAND_ALPHA: f32 = 0.5;
/// Opacity of the shade over what lies outside the window, and under the pointer or while
/// the window is dragged; the width of the window's outline, in points.
const SHADE_ALPHA: f32 = 0.55;
const SHADE_HOT_ALPHA: f32 = 0.45;
const WINDOW_STROKE: f32 = 1.5;
/// Opacity of the grid's color over stretches without samples.
const GAP_ALPHA: f32 = 0.5;
/// How near an annotation's mark the pointer must be, in points, for the status line to
/// name it.
const POINTER_SLACK: f32 = 2.0;
/// The context menu's items.
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
/// window moves.
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
/// grab of half the span centers the window on `t`.
pub fn start_for(t: f64, grab: f64, span: f64, duration: f64) -> f64 {
    let start = t - grab;
    if start.is_nan() {
        return 0.0;
    }
    start.clamp(0.0, (duration - span).max(0.0))
}

/// The pixel columns of a strip `width_px` physical pixels wide over a recording `duration`
/// seconds long that hold no sample of any grid, as runs from left to right. A column with
/// any sample in it is not one, so gaps narrower than a column don't show.
pub fn gaps(recording: &Recording, duration: f64, width_px: u32) -> Vec<Range<u32>> {
    let width = f64::from(width_px);
    let mut covered = vec![false; width_px as usize];
    for grid in recording.grids() {
        let period = if grid.rate() > 0.0 {
            1.0 / grid.rate()
        } else {
            0.0
        };
        for segment in grid.segments() {
            if segment.is_empty() {
                continue;
            }
            let first = grid.time_of(segment.start);
            let last = grid.time_of(segment.end - 1) + period;
            let x0 = x_at(first, duration, width).floor().max(0.0);
            if x0 >= width {
                continue;
            }
            // A sample takes at least its column.
            let x1 = x_at(last, duration, width).ceil().min(width).max(x0 + 1.0);
            for c in &mut covered[x0 as usize..x1 as usize] {
                *c = true;
            }
        }
    }
    let mut runs: Vec<Range<u32>> = Vec::new();
    for (column, _) in covered.iter().enumerate().filter(|(_, c)| !**c) {
        let column = column as u32;
        match runs.last_mut() {
            Some(run) if run.end == column => run.end += 1,
            _ => runs.push(column..column + 1),
        }
    }
    runs
}

/// The strip's pixel columns, `width_px` of them, back to front: the `background`, the
/// `gap` color over `gaps`, the annotations' bands (translucent, in their order) and lines
/// (solid) in their colors from `color_of`. Every mark spans the strip's height, so this
/// row is all it shows of them.
pub fn pixels(
    marks: &Marks,
    gaps: &[Range<u32>],
    width_px: u32,
    background: Color32,
    gap: Color32,
    color_of: impl Fn(AnnotationColor) -> Color32,
) -> Vec<Color32> {
    let mut row = vec![background; width_px as usize];
    let mut paint = |columns: &Range<u32>, color: Color32| {
        let end = columns.end.min(width_px) as usize;
        for pixel in &mut row[(columns.start as usize).min(end)..end] {
            *pixel = pixel.blend(color);
        }
    };
    for columns in gaps {
        paint(columns, gap);
    }
    for band in &marks.bands {
        paint(
            &band.columns,
            color_of(band.color).gamma_multiply(BAND_ALPHA),
        );
    }
    for line in &marks.lines {
        paint(&line.columns, color_of(line.color));
    }
    row
}

/// What the minimap asked of the viewer in a frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Shown {
    /// A new start for the window, in seconds.
    pub start: Option<f64>,
    /// A new height, in points, before [`height`] limits it.
    pub height: Option<f32>,
    /// Whether Hide Minimap was chosen.
    pub hide: bool,
    /// The time under the pointer, and the annotations' descriptions there.
    pub pointer: Option<(f64, Vec<String>)>,
}

/// The minimap's state: what it draws, made when it changes, and a press holding the
/// window.
#[derive(Default)]
pub struct Minimap {
    /// The annotations' marks, and the store's revision, the legend's shown and colors
    /// revisions, the duration and the width in physical pixels they were made for.
    marks: Marks,
    marks_for: Option<(u64, u64, u64, u64, u32)>,
    /// The columns without samples, and the width they were found for.
    gaps: Vec<Range<u32>>,
    gaps_for: Option<u32>,
    /// The row of pixels made of both, and the theme's colors it was made in.
    texture: Option<TextureHandle>,
    texture_for: Option<(bool, Color32, Color32)>,
    /// While a press on the strip holds the window: the time it holds, after the window's
    /// start, in seconds.
    grab: Option<f64>,
}

/// What the minimap shows and acts on.
pub struct Input<'a> {
    pub recording: &'a Recording,
    pub store: &'a AnnotationStore,
    pub legend: &'a Legend,
    /// The recording's length and the window's times, in seconds.
    pub duration: f64,
    pub window: Range<f64>,
}

impl Minimap {
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

        let response = ui.interact(rect, id, Sense::click_and_drag());
        let primary = ui.input(|i| i.pointer.primary_down());
        if response.is_pointer_button_down_on() && primary {
            let pointer = response.interact_pointer_pos();
            if self.grab.is_none()
                && let Some(press) = ui.input(|i| i.pointer.press_origin())
            {
                self.grab = Some(if window.contains(press.x) {
                    time_of(press.x) - input.window.start
                } else {
                    span / 2.0
                });
            }
            if let (Some(grab), Some(p)) = (self.grab, pointer) {
                let start = start_for(time_of(p.x), grab, span, duration);
                if start != input.window.start {
                    shown.start = Some(start);
                }
            }
        } else {
            self.grab = None;
        }
        response.context_menu(|ui| {
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
            let slack = (POINTER_SLACK * ppp).round() as u32;
            let x = f64::from((p.x - rect.left()) * ppp);
            let under = annotations::overview_under(
                input.store,
                input.legend,
                duration,
                width_px,
                x,
                slack,
            );
            let descriptions = under
                .into_iter()
                .map(|a| a.data.description.clone())
                .collect();
            shown.pointer = Some((time_of(p.x), descriptions));
        }

        self.update(ui, input, width_px);
        if ui.is_rect_visible(rect) {
            let hot = response.dragged() || response.hovered() && !resizing;
            self.paint(ui, rect, window, hot, resizing);
        }
        shown
    }

    /// Makes the marks, gaps and their texture again where what they show changed.
    fn update(&mut self, ui: &Ui, input: &Input<'_>, width_px: u32) {
        let mut changed = false;
        let marks_for = (
            input.store.revision(),
            input.legend.shown_revision(),
            input.legend.colors_revision(),
            input.duration.to_bits(),
            width_px,
        );
        if self.marks_for != Some(marks_for) {
            self.marks =
                annotations::overview_marks(input.store, input.legend, input.duration, width_px);
            self.marks_for = Some(marks_for);
            changed = true;
        }
        if self.gaps_for != Some(width_px) {
            self.gaps = gaps(input.recording, input.duration, width_px);
            self.gaps_for = Some(width_px);
            changed = true;
        }
        let visuals = ui.visuals();
        let background = visuals.extreme_bg_color;
        let gap = visuals
            .widgets
            .noninteractive
            .bg_stroke
            .color
            .gamma_multiply(GAP_ALPHA);
        let texture_for = (visuals.dark_mode, background, gap);
        if self.texture_for != Some(texture_for) {
            self.texture_for = Some(texture_for);
            changed = true;
        }
        if changed || self.texture.is_none() {
            let row = pixels(&self.marks, &self.gaps, width_px, background, gap, |c| {
                annotation_color(c, visuals)
            });
            let image = ColorImage::new([width_px as usize, 1], row);
            match &mut self.texture {
                Some(texture) => texture.set(image, TextureOptions::NEAREST),
                None => {
                    self.texture = Some(ui.ctx().load_texture(
                        "minimap",
                        image,
                        TextureOptions::NEAREST,
                    ));
                }
            }
        }
    }

    fn paint(&self, ui: &Ui, rect: Rect, window: Rangef, hot: bool, resizing: bool) {
        let visuals = ui.visuals();
        let ppp = ui.pixels_per_point();
        let painter = ui.painter_at(rect.expand2(egui::vec2(0.0, 1.0)));
        if let Some(texture) = &self.texture {
            let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
            painter.image(texture.id(), rect, uv, Color32::WHITE);
        }
        let grid = visuals.widgets.noninteractive.bg_stroke.color;

        // The top edge: the grid's line, under the window's outline.
        let top = rect.top() + 0.5 / ppp;
        painter.hline(rect.x_range(), top, Stroke::new(1.0 / ppp, grid));

        // The window: what lies outside it dimmed, so it stands out however dense the
        // annotations are, and outlined in the selection's color over the strip's height,
        // in whole pixels.
        let shade = visuals.extreme_bg_color.gamma_multiply(if hot {
            SHADE_HOT_ALPHA
        } else {
            SHADE_ALPHA
        });
        let left = Rect::from_x_y_ranges(rect.left()..=window.min, rect.y_range());
        let right = Rect::from_x_y_ranges(window.max..=rect.right(), rect.y_range());
        painter.rect_filled(left, 0.0, shade);
        painter.rect_filled(right, 0.0, shade);
        let width = (WINDOW_STROKE * ppp).round().max(1.0) / ppp;
        painter.rect_stroke(
            Rect::from_x_y_ranges(window, rect.y_range()),
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
