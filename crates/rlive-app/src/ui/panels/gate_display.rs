//! Gate's display: input and output history, with draggable Threshold and Return lines.

use super::{PanelCtx, PanelDef};
use crate::ui::theme::*;
use egui::{CornerRadius, Pos2, Rect, Sense, Shape, Stroke, Vec2};
use rlive_core::ParamId;
use rlive_devices::devices::b3::gate;
use std::collections::VecDeque;

pub const PANEL: PanelDef = PanelDef {
    name: "gate_display",
    size: Vec2::new(150.0, 100.0),
    views: gate::VIEW_LEN,
    draw,
};

const FLOOR_DB: f32 = -70.0;
const HISTORY: usize = 240;
fn to_db(linear: f32) -> f32 {
    rlive_core::gain_to_db(linear).max(FLOOR_DB)
}

fn db_to_y(rect: Rect, db: f32) -> f32 {
    let t = ((db - FLOOR_DB) / (6.0 - FLOOR_DB)).clamp(0.0, 1.0);
    rect.bottom() - t * rect.height()
}

fn y_to_db(rect: Rect, y: f32) -> f32 {
    let t = ((rect.bottom() - y) / rect.height()).clamp(0.0, 1.0);
    FLOOR_DB + t * (6.0 - FLOOR_DB)
}

#[derive(Clone, Default)]
struct State {
    /// Newest last: input, output and key level, in dB.
    history: VecDeque<[f32; 3]>,
    dragging: Option<u32>,
}

fn draw(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    let response = ui.interact(
        rect,
        ui.scope_id().with(("gate_display", panel.device.id)),
        Sense::click_and_drag(),
    );
    let id = ui.scope_id().with(("gate_state", panel.device.id));
    let mut state: State = ui.ctx().data_mut(|d| d.get_temp(id).unwrap_or_default());

    let mut values = [0.0f32; gate::VIEW_LEN];
    if panel
        .view
        .is_some_and(|v| v.read(gate::LEVELS, &mut values))
    {
        state
            .history
            .push_back([to_db(values[0]), to_db(values[1]), to_db(values[3])]);
        if state.history.len() > HISTORY {
            state.history.pop_front();
        }
    }

    let threshold = panel.value(ParamId(gate::THRESHOLD)).unwrap_or(-40.0) as f32;
    let ret = panel.value(ParamId(gate::RETURN)).unwrap_or(3.0) as f32;
    let close = threshold - ret;

    if response.drag_started() {
        if let Some(pos) = response.interact_pointer_pos() {
            let to_threshold = (pos.y - db_to_y(rect, threshold)).abs();
            let to_close = (pos.y - db_to_y(rect, close)).abs();
            state.dragging = Some(if to_close < to_threshold {
                gate::RETURN
            } else {
                gate::THRESHOLD
            });
        }
    }
    if response.dragged() {
        if let (Some(which), Some(pos)) = (state.dragging, response.interact_pointer_pos()) {
            let db = y_to_db(rect, pos.y);
            if which == gate::THRESHOLD {
                panel.set(ParamId(gate::THRESHOLD), db.clamp(-70.0, 6.0) as f64);
            } else {
                panel.set(
                    ParamId(gate::RETURN),
                    (threshold - db).clamp(0.0, 24.0) as f64,
                );
            }
        }
    }
    if response.drag_stopped() {
        state.dragging = None;
    }

    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(2), pal().knob_track);
    let width = rect.width() / (HISTORY - 1) as f32;
    let offset = HISTORY.saturating_sub(state.history.len());
    let points = |pick: fn(&[f32; 3]) -> f32| -> Vec<Pos2> {
        state
            .history
            .iter()
            .enumerate()
            .map(|(i, v)| {
                Pos2::new(
                    rect.left() + (i + offset) as f32 * width,
                    db_to_y(rect, pick(v)),
                )
            })
            .collect()
    };
    let listening = panel
        .value(ParamId(gate::SIDECHAIN + 3))
        .is_some_and(|v| v >= 0.5);
    // While listening, the display shows the sidechain signal, in green.
    let (level, colour): (fn(&[f32; 3]) -> f32, _) = if listening {
        (|v| v[2], pal().play_green)
    } else {
        (|v| v[0], pal().text_dim)
    };
    p.add(Shape::line(points(level), Stroke::new(1.0, colour)));
    p.add(Shape::line(points(|v| v[1]), Stroke::new(1.0, pal().text)));
    for (db, colour) in [(threshold, pal().solo_blue), (close, pal().accent)] {
        let y = db_to_y(rect, db);
        p.line_segment(
            [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
            Stroke::new(1.0, colour),
        );
    }
    ui.ctx().data_mut(|d| d.insert_temp(id, state));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dragging_a_line_reads_back_the_level_it_was_drawn_at() {
        let rect = Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(150.0, 100.0));
        for db in [-60.0, -30.0, 0.0] {
            assert!((y_to_db(rect, db_to_y(rect, db)) - db).abs() < 1e-3);
        }
    }
}
