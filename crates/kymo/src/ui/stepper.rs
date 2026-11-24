//! A number field with a stepper beside it, as macOS has (`NSStepper`): the value is typed,
//! or stepped with the arrows beside the field or the up and down arrow keys. Nothing is
//! dragged, so the pointer never covers the value.

use std::ops::RangeInclusive;

use eframe::egui::{
    self, Align2, Id, Key, Modifiers, Rect, Response, Sense, Shape, Stroke, TextEdit, Ui,
    accesskit, pos2, vec2,
};

/// The field's width, in points.
const FIELD_WIDTH: f32 = 64.0;
/// The stepper's width, in points.
const STEPPER_WIDTH: f32 = 14.0;
/// Between the field and the stepper, in points.
const STEPPER_GAP: f32 = 3.0;
/// How long an arrow is held before it repeats, and then between steps, in seconds.
const REPEAT_DELAY: f64 = 0.5;
const REPEAT_INTERVAL: f64 = 0.08;
/// Decimals the field shows at most.
const DECIMALS: usize = 3;

/// A field for `value` within `range`, followed by a stepper whose arrows move it to the
/// next multiple of `step` up or down, then `unit`. The field takes typed text when Return is
/// pressed or it loses focus (Esc drops it); a comma counts as a decimal point. Accessible as
/// a spin button labeled `label`. Returns the field's response, changed when the value did.
pub fn stepper(
    ui: &mut Ui,
    value: &mut f64,
    range: RangeInclusive<f64>,
    step: f64,
    unit: &str,
    label: &str,
) -> Response {
    let old = *value;
    let clamp = |v: f64| v.clamp(*range.start(), *range.end());
    let id = ui.make_persistent_id(("stepper", label));
    let mut response = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = STEPPER_GAP;
            let focused = ui.memory(|m| m.has_focus(id));
            // Arrow keys step while the field has focus, instead of moving its cursor.
            let keys = if focused {
                ui.input_mut(|i| {
                    i.count_and_consume_key(Modifiers::NONE, Key::ArrowUp) as f64
                        - i.count_and_consume_key(Modifiers::NONE, Key::ArrowDown) as f64
                })
            } else {
                0.0
            };
            if keys != 0.0 {
                *value = clamp(step_by(*value, step, keys));
                ui.data_mut(|d| d.insert_temp(id, format_number(*value)));
                // Before the field has had focus for a frame, egui would also move focus
                // with them.
                ui.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
            }
            // A screen reader's (or a test's) requests.
            ui.input(|i| {
                use accesskit::{Action, ActionData};
                for request in i.accesskit_action_requests(id, Action::SetValue) {
                    if let Some(ActionData::NumericValue(v)) = request.data
                        && !v.is_nan()
                    {
                        *value = clamp(v);
                    }
                }
                let change = i.num_accesskit_action_requests(id, Action::Increment) as f64
                    - i.num_accesskit_action_requests(id, Action::Decrement) as f64;
                if change != 0.0 {
                    *value = clamp(step_by(*value, step, change));
                }
            });

            let mut text = if focused {
                ui.data(|d| d.get_temp::<String>(id))
                    .unwrap_or_else(|| format_number(*value))
            } else {
                format_number(*value)
            };
            let field = ui.add(
                TextEdit::singleline(&mut text)
                    .id(id)
                    .desired_width(FIELD_WIDTH)
                    .align(Align2::RIGHT_CENTER),
            );
            if field.has_focus() {
                ui.data_mut(|d| d.insert_temp(id, text.clone()));
            } else {
                ui.data_mut(|d| d.remove_temp::<String>(id));
            }
            if field.lost_focus()
                && !ui.input(|i| i.key_pressed(Key::Escape))
                && let Some(typed) = parse_number(&text)
            {
                *value = clamp(typed);
            }

            let arrows = arrows(ui, id);
            if arrows != 0.0 {
                *value = clamp(step_by(*value, step, arrows));
                if field.has_focus() {
                    ui.data_mut(|d| d.insert_temp(id, format_number(*value)));
                }
            }
            ui.label(unit);
            field
        })
        .inner;
    let value = *value;
    ui.ctx().accesskit_node_builder(id, |node| {
        use accesskit::Action;
        node.set_role(accesskit::Role::SpinButton);
        node.set_label(label);
        node.set_numeric_value(value);
        node.set_min_numeric_value(*range.start());
        node.set_max_numeric_value(*range.end());
        node.set_numeric_value_step(step);
        node.add_action(Action::SetValue);
        if value < *range.end() {
            node.add_action(Action::Increment);
        }
        if value > *range.start() {
            node.add_action(Action::Decrement);
        }
    });
    if value != old {
        response.mark_changed();
    }
    response
}

/// `value` moved `steps` multiples of `step` up (positive) or down: to the next multiple
/// first, where it is between two.
pub fn step_by(value: f64, step: f64, steps: f64) -> f64 {
    let at = value / step;
    // Within rounding of a multiple, at it.
    let near = at.round();
    let at = if (at - near).abs() < 1e-9 { near } else { at };
    let moved = if steps > 0.0 {
        at.floor() + steps
    } else {
        at.ceil() + steps
    };
    moved * step
}

/// `value` as the field shows it: at most [`DECIMALS`] decimals, without trailing zeros.
pub fn format_number(value: f64) -> String {
    let text = format!("{value:.DECIMALS$}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" {
        "0".to_owned()
    } else {
        text.to_owned()
    }
}

/// The number typed in the field, a decimal comma as a point; `None` if there is none.
pub fn parse_number(text: &str) -> Option<f64> {
    text.trim()
        .replace(',', ".")
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
}

/// The stepper's arrows, as tall as the field: a rounded body split into an upper and a
/// lower half, each with a chevron, darker while pressed. A press steps once; held, it
/// repeats after [`REPEAT_DELAY`] while the pointer stays on it. Returns the steps: positive up, negative down.
fn arrows(ui: &mut Ui, id: Id) -> f64 {
    let height = ui.spacing().interact_size.y;
    let (rect, _) = ui.allocate_exact_size(vec2(STEPPER_WIDTH, height), Sense::hover());
    let visuals = ui.visuals().widgets.inactive;
    let radius = visuals.corner_radius;
    ui.painter().rect_filled(rect, radius, visuals.weak_bg_fill);
    let halves = [
        (
            Rect::from_min_max(rect.min, pos2(rect.max.x, rect.center().y)),
            1.0,
            "Up",
        ),
        (
            Rect::from_min_max(pos2(rect.min.x, rect.center().y), rect.max),
            -1.0,
            "Down",
        ),
    ];
    let mut steps = 0.0;
    for (half, direction, name) in halves {
        let half_id = id.with(name);
        // Dragging too: egui drops a press that only clicks once it is held longer than a
        // click (0.8 s).
        let response = ui.interact(half, half_id, Sense::click_and_drag());
        response
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), name));
        let done = ui.data(|d| d.get_temp::<i64>(half_id));
        if response.is_pointer_button_down_on() {
            let now = ui.input(|i| i.time);
            let pressed_at = ui.input(|i| i.pointer.press_start_time()).unwrap_or(now);
            let held = now - pressed_at;
            // Steps due since the press: one at once, then one per interval after the delay.
            let due = if held < REPEAT_DELAY {
                1
            } else {
                2 + ((held - REPEAT_DELAY) / REPEAT_INTERVAL) as i64
            };
            // Off the arrow, the steps due pass without stepping, as on macOS.
            let over = ui.rect_contains_pointer(half);
            let done = done.unwrap_or(0);
            if due > done {
                if over {
                    steps += direction * (due - done) as f64;
                }
                ui.data_mut(|d| d.insert_temp(half_id, due));
            }
            ui.ctx().request_repaint();
        } else {
            ui.data_mut(|d| d.remove_temp::<i64>(half_id));
            // A click without a press seen (a screen reader's, or a test's).
            if response.clicked() && done.is_none() {
                steps += direction;
            }
        }
        let style = ui.style().interact(&response);
        if response.is_pointer_button_down_on() || response.hovered() {
            let corners = if direction > 0.0 {
                egui::CornerRadius {
                    sw: 0,
                    se: 0,
                    ..radius
                }
            } else {
                egui::CornerRadius {
                    nw: 0,
                    ne: 0,
                    ..radius
                }
            };
            ui.painter().rect_filled(half, corners, style.weak_bg_fill);
        }
        chevron(ui, half, direction, style.fg_stroke);
    }
    ui.painter()
        .rect_stroke(rect, radius, visuals.bg_stroke, egui::StrokeKind::Inside);
    steps
}

/// A small chevron in the middle of `rect`, pointing up (`direction` positive) or down.
fn chevron(ui: &Ui, rect: Rect, direction: f64, stroke: Stroke) {
    let c = rect.center();
    let (w, h) = (3.0, 1.75 * direction.signum() as f32);
    let points = vec![
        pos2(c.x - w, c.y + h),
        pos2(c.x, c.y - h),
        pos2(c.x + w, c.y + h),
    ];
    ui.painter()
        .add(Shape::line(points, Stroke::new(1.25, stroke.color)));
}
