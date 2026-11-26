//! A plain scrollbar over a range that isn't laid-out content: the recording's time, or its
//! channel rows. The thumb shows the visible part of the whole; dragging it moves the view,
//! clicking the track beside it moves by a page.

use std::ops::Range;

use eframe::egui::{self, Rangef, Rect, Sense, Ui};

/// The shortest thumb, in points, so it stays grabbable over long recordings.
pub const MIN_THUMB: f32 = 20.0;
/// Thickness of the bar in points.
pub const THICKNESS: f32 = 10.0;
/// The thumb's width across the bar at rest and under the pointer or dragged, in points.
const REST_WIDTH: f32 = 3.0;
const HOT_WIDTH: f32 = THICKNESS - 4.0;
/// How long the thumb takes to widen or narrow, in seconds.
const HOT_SECONDS: f32 = 0.1;

/// Where the thumb lies on `track` for `visible` out of `0..total`.
pub fn thumb(track: Rangef, total: f64, visible: &Range<f64>) -> Rangef {
    let length = track.span().max(0.0);
    let len = visible.end - visible.start;
    if !(total > 0.0 && len < total && len.is_finite() && total.is_finite()) {
        return track;
    }
    let size = ((len / total) as f32 * length).clamp(MIN_THUMB.min(length), length);
    let room = length - size;
    let at = (visible.start / (total - len)).clamp(0.0, 1.0) as f32 * room;
    Rangef::new(track.min + at, track.min + at + size)
}

/// The start of the visible range that puts the thumb's start at `thumb_start`; the
/// inverse of [`thumb`] for a visible range of length `len`.
pub fn start_at(track: Rangef, total: f64, len: f64, thumb_start: f32) -> f64 {
    let size = thumb(track, total, &(0.0..len)).span();
    let room = track.span() - size;
    if !(room > 0.0 && total > len) {
        return 0.0;
    }
    let fraction = f64::from(((thumb_start - track.min) / room).clamp(0.0, 1.0));
    fraction * (total - len)
}

/// A scrollbar in `rect` for `visible` out of `0..total`, horizontal or vertical. Returns
/// the new start of the visible range when the user moves it.
pub fn show(
    ui: &mut Ui,
    id: egui::Id,
    rect: Rect,
    vertical: bool,
    total: f64,
    visible: Range<f64>,
) -> Option<f64> {
    let response = ui.interact(rect, id, Sense::click_and_drag());
    let along = |p: egui::Pos2| if vertical { p.y } else { p.x };
    let track = if vertical {
        rect.y_range()
    } else {
        rect.x_range()
    };
    let len = visible.end - visible.start;
    let current = thumb(track, total, &visible);

    let mut start = None;
    // Where on the thumb it was grabbed, kept while dragging.
    let grab_id = id.with("grab");
    if response.drag_started()
        && let Some(p) = response.interact_pointer_pos()
    {
        let at = along(p);
        let grab = if current.contains(at) {
            at - current.min
        } else {
            current.span() / 2.0
        };
        ui.data_mut(|d| d.insert_temp(grab_id, grab));
    }
    if response.dragged()
        && let Some(p) = response.interact_pointer_pos()
    {
        let grab = ui.data(|d| d.get_temp::<f32>(grab_id)).unwrap_or(0.0);
        start = Some(start_at(track, total, len, along(p) - grab));
    } else if response.clicked()
        && let Some(p) = response.interact_pointer_pos()
    {
        let at = along(p);
        if at < current.min {
            start = Some((visible.start - len).max(0.0));
        } else if at > current.max {
            start = Some((visible.start + len).min((total - len).max(0.0)));
        }
    }

    if ui.is_rect_visible(rect) && total > len {
        let visuals = ui.visuals();
        // The weak text's color: as the system's scroll bars, it stands out on the trace
        // area and the panels, in both themes, and more so under the pointer, where the
        // thumb also widens (quickly, not at once). The whole bar takes the pointer either
        // way, so the thin thumb is as easy to grab.
        let weak = visuals.weak_text_color();
        let hot = ui.ctx().animate_bool_with_time(
            id.with("hot"),
            response.dragged() || response.hovered(),
            HOT_SECONDS,
        );
        let color = weak.gamma_multiply(egui::lerp(0.6..=1.0, hot));
        let width = egui::lerp(REST_WIDTH..=HOT_WIDTH, hot);
        let across = |range: Rangef| Rangef::point(range.center()).expand(width / 2.0);
        let at = start.unwrap_or(visible.start);
        let shown = thumb(track, total, &(at..at + len));
        let thumb_rect = if vertical {
            Rect::from_x_y_ranges(across(rect.x_range()), shown)
        } else {
            Rect::from_x_y_ranges(shown, across(rect.y_range()))
        };
        ui.painter().rect_filled(thumb_rect, width / 2.0, color);
    }
    start
}
