//! The breakpoint editor Shaper and Shaper MIDI share (manual pp. 847, 863).

use super::{PanelCtx, PanelDef};
use crate::ui::theme::*;
use egui::{Align2, Pos2, Rect, Sense, Shape as EguiShape, Stroke, StrokeKind, Vec2};
use rlive_core::ParamId;
use rlive_devices::devices::b2::breakpoints::{Point, Shape, PRESETS};
use rlive_devices::devices::b2::{shaper, shaper_midi};

pub const PANEL: PanelDef = PanelDef {
    name: "shaper_editor",
    size: Vec2::new(280.0, 130.0),
    views: 0,
    draw,
};

const BUTTON_H: f32 = 16.0;
const GRAB: f32 = 8.0;
/// The choices of the Grid menu.
const GRIDS: [u32; 9] = [2, 3, 4, 6, 8, 12, 16, 24, 32];

#[derive(Clone, Default)]
struct State {
    /// The breakpoint being dragged.
    dragging: Option<usize>,
    /// The segment's first point, while Alt bends it.
    bending: Option<usize>,
    shape: Option<Shape>,
}

fn is_midi(panel: &PanelCtx) -> bool {
    matches!(
        &panel.device.kind,
        rlive_project::DeviceKind::Builtin { type_id, .. } if type_id == "rlive.shaper_midi"
    )
}

fn pos_of(rect: Rect, p: &Point) -> Pos2 {
    Pos2::new(
        rect.left() + p.time * rect.width(),
        rect.center().y - p.value * rect.height() * 0.5,
    )
}

fn point_at(rect: Rect, pos: Pos2) -> (f32, f32) {
    (
        ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0),
        ((rect.center().y - pos.y) / (rect.height() * 0.5)).clamp(-1.0, 1.0),
    )
}

fn snap(v: f32, divisions: f32) -> f32 {
    (v * divisions).round() / divisions
}

fn draw(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let whole = ui.max_rect();
    let buttons = Rect::from_min_max(
        Pos2::new(whole.left(), whole.bottom() - BUTTON_H),
        whole.right_bottom(),
    );
    let rect = Rect::from_min_max(
        whole.left_top(),
        Pos2::new(whole.right(), buttons.top() - 2.0),
    );
    let id = ui.scope_id().with(("shaper_editor", panel.device.id));
    let mut state: State = ui.ctx().data_mut(|d| d.get_temp(id).unwrap_or_default());
    let saved = panel
        .state_text()
        .and_then(Shape::parse)
        .unwrap_or_else(Shape::flat);
    let mut shape = state.shape.clone().unwrap_or(saved);
    let midi = is_midi(panel);
    let (grid_param, snap_param) = if midi {
        (shaper_midi::GRID, shaper_midi::SNAP)
    } else {
        (shaper::GRID, shaper::SNAP)
    };
    let divisions = GRIDS
        [(panel.value(ParamId(grid_param)).unwrap_or(4.0).round() as usize).min(GRIDS.len() - 1)]
        as f32;
    let snapping = panel.value(ParamId(snap_param)).unwrap_or(1.0) >= 0.5;

    let resp = ui.interact(
        rect,
        ui.scope_id().with(("shaper_area", panel.device.id)),
        Sense::click_and_drag(),
    );
    let (alt, shift, ctrl) =
        ui.input(|i| (i.modifiers.alt, i.modifiers.shift, i.modifiers.command));
    let nearest = |shape: &Shape, pos: Pos2| {
        shape
            .points
            .iter()
            .enumerate()
            .map(|(i, p)| (i, pos_of(rect, p).distance(pos)))
            .filter(|(_, d)| *d <= GRAB)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    };
    let mut changed = false;
    if resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let hit = nearest(&shape, pos);
            match hit {
                Some(i) if shift => {
                    if shape.points.len() > 2 {
                        shape.points.remove(i);
                        if shape.sustain == Some(i) {
                            shape.sustain = None;
                        }
                        changed = true;
                    }
                }
                Some(i) if ctrl && midi => {
                    shape.sustain = Some(i);
                    changed = true;
                }
                Some(_) => {}
                None if !shift => {
                    let (mut time, mut value) = point_at(rect, pos);
                    if snapping {
                        time = snap(time, divisions);
                        value = snap(value * 0.5 + 0.5, divisions) * 2.0 - 1.0;
                    }
                    let at = shape.points.partition_point(|p| p.time <= time);
                    shape.points.insert(
                        at,
                        Point {
                            time,
                            value,
                            curve: 0.0,
                        },
                    );
                    if ctrl && midi {
                        shape.sustain = Some(at);
                    }
                    changed = true;
                }
                None => {}
            }
        }
    }
    if resp.drag_started() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let hit = nearest(&shape, pos);
            if alt && hit.is_none() {
                let (time, _) = point_at(rect, pos);
                state.bending = shape
                    .points
                    .iter()
                    .rposition(|p| p.time <= time)
                    .filter(|i| i + 1 < shape.points.len());
            } else {
                state.dragging = hit;
            }
        }
    }
    if resp.dragged() {
        if let (Some(i), Some(pos)) = (state.dragging, resp.interact_pointer_pos()) {
            let (mut time, mut value) = point_at(rect, pos);
            if snapping {
                time = snap(time, divisions);
                value = snap(value * 0.5 + 0.5, divisions) * 2.0 - 1.0;
            }
            let last = shape.points.len() - 1;
            let low = if i == 0 {
                0.0
            } else {
                shape.points[i - 1].time
            };
            let high = if i == last {
                1.0
            } else {
                shape.points[i + 1].time
            };
            shape.points[i].time = if i == 0 {
                0.0
            } else if i == last {
                1.0
            } else {
                time.clamp(low, high)
            };
            shape.points[i].value = value;
            changed = true;
        } else if let Some(i) = state.bending {
            let delta = resp.drag_delta().y / rect.height() * 2.0;
            shape.points[i].curve = (shape.points[i].curve + delta).clamp(-1.0, 1.0);
            changed = true;
        }
    }
    if resp.drag_stopped() {
        state.dragging = None;
        state.bending = None;
    }

    // Clear and the six presets.
    let w = buttons.width() / (PRESETS.len() + 1) as f32;
    let device = panel.device.id;
    let button = |ui: &mut egui::Ui, n: usize, text: &str| {
        let r = Rect::from_min_size(
            Pos2::new(buttons.left() + n as f32 * w, buttons.top()),
            Vec2::new(w - 2.0, BUTTON_H),
        );
        let resp = ui.interact(
            r,
            ui.scope_id().with(("shaper_preset", device, n)),
            Sense::click(),
        );
        let p = ui.painter();
        p.rect_filled(r, 2.0, pal().field_bg);
        p.text(
            r.center(),
            Align2::CENTER_CENTER,
            text,
            font(9.0),
            pal().text,
        );
        resp.clicked()
    };
    if button(ui, 0, "Clear") {
        shape = Shape::flat();
        changed = true;
    }
    for (n, name) in PRESETS.iter().enumerate() {
        if button(ui, n + 1, name) {
            shape = Shape::preset(n);
            changed = true;
        }
    }

    if changed {
        if state.dragging.is_some() || state.bending.is_some() {
            state.shape = Some(shape.clone());
        } else {
            state.shape = None;
        }
        panel.set_state(shape.clone().normalized().to_text());
    } else if !resp.dragged() {
        state.shape = None;
    }

    let p = ui.painter();
    p.rect_filled(rect, 2.0, shade(0x121212));
    p.rect_stroke(
        rect,
        2.0,
        Stroke::new(1.0, pal().separator),
        StrokeKind::Inside,
    );
    let line = Stroke::new(1.0, pal().separator.gamma_multiply(0.6));
    for n in 1..divisions as u32 {
        let x = rect.left() + rect.width() * n as f32 / divisions;
        p.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            line,
        );
    }
    p.line_segment(
        [
            Pos2::new(rect.left(), rect.center().y),
            Pos2::new(rect.right(), rect.center().y),
        ],
        Stroke::new(1.0, pal().separator),
    );
    let curve: Vec<Pos2> = (0..240)
        .map(|n| {
            let t = n as f32 / 239.0;
            pos_of(
                rect,
                &Point {
                    time: t,
                    value: shape.value_at(t),
                    curve: 0.0,
                },
            )
        })
        .collect();
    p.add(EguiShape::line(curve, Stroke::new(1.5, pal().accent)));
    for (i, point) in shape.points.iter().enumerate() {
        let radius = if shape.sustain == Some(i) { 4.5 } else { 2.5 };
        p.circle_filled(pos_of(rect, point), radius, pal().text);
    }
    ui.ctx().data_mut(|d| d.insert_temp(id, state));
}
