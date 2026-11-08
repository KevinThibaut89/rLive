//! The Compressor's display: collapsed, a transfer curve, or an activity view.

use super::{PanelCtx, PanelDef};
use crate::ui::theme::{self, *};
use egui::{Align2, Pos2, Rect, Sense, Shape, Stroke, Vec2};
use rlive_core::ParamId;
use rlive_devices::effects::comp;
use std::collections::VecDeque;

pub const PANEL: PanelDef = PanelDef {
    name: "compressor_display",
    size: Vec2::new(150.0, 100.0),
    views: comp::VIEW_LEN,
    draw,
};

/// What the display shows.
#[derive(Clone, Copy, PartialEq, Default)]
enum Mode {
    Collapsed,
    #[default]
    Curve,
    Activity,
}

/// What the darker Activity trace shows.
#[derive(Clone, Copy, PartialEq, Default)]
enum Trace {
    #[default]
    GainReduction,
    Output,
}

#[derive(Clone, Default)]
struct State {
    mode: Mode,
    trace: Trace,
    /// Newest last: input level, output level and gain reduction, in dB.
    history: VecDeque<[f32; 3]>,
}

const HISTORY: usize = 240;
/// The lowest level the display draws, in dB.
const FLOOR_DB: f32 = -70.0;

fn to_db(linear: f32) -> f32 {
    rlive_core::gain_to_db(linear).max(FLOOR_DB)
}

fn level_y(rect: Rect, db: f32) -> f32 {
    let t = ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0);
    rect.bottom() - t * rect.height()
}

fn level_x(rect: Rect, db: f32) -> f32 {
    let t = ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0);
    rect.left() + t * rect.width()
}

fn draw(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    let id = ui.scope_id().with(("compressor_display", panel.device.id));
    let mut state: State = ui.ctx().data_mut(|d| d.get_temp(id).unwrap_or_default());

    let mut values = [0.0f32; comp::VIEW_LEN];
    if panel
        .view
        .is_some_and(|v| v.read(comp::GAIN_REDUCTION, &mut values))
    {
        state.history.push_back([
            to_db(values[3].max(values[4])),
            to_db(values[1].max(values[2])),
            values[0],
        ]);
        if state.history.len() > HISTORY {
            state.history.pop_front();
        }
    }

    // The switches run along the bottom corners.
    let switch_h = 14.0;
    let switches = Rect::from_min_max(
        Pos2::new(rect.left(), rect.bottom() - switch_h),
        rect.right_bottom(),
    );
    let tab = |ui: &mut egui::Ui, x: f32, w: f32, text: &str, on: bool| -> bool {
        let r = Rect::from_min_size(Pos2::new(x, switches.top()), Vec2::new(w, switch_h));
        let response = ui.interact(r, ui.scope_id().with(("tab", text)), Sense::click());
        let fill = if on { pal().accent } else { pal().raised };
        ui.painter().rect_filled(r, 2.0, fill);
        ui.painter().text(
            r.center(),
            Align2::CENTER_CENTER,
            text,
            theme::font(9.0),
            if on { pal().text_dark } else { pal().text_dim },
        );
        response.clicked()
    };
    let mut x = rect.left();
    for (mode, label) in [
        (Mode::Collapsed, "-"),
        (Mode::Curve, "Curve"),
        (Mode::Activity, "Act"),
    ] {
        let w = if mode == Mode::Collapsed { 16.0 } else { 34.0 };
        if tab(ui, x, w, label, state.mode == mode) {
            state.mode = mode;
        }
        x += w + 2.0;
    }
    if state.mode == Mode::Activity {
        let mut x = rect.right() - 54.0;
        for (trace, label) in [(Trace::GainReduction, "GR"), (Trace::Output, "Out")] {
            if tab(ui, x, 26.0, label, state.trace == trace) {
                state.trace = trace;
            }
            x += 28.0;
        }
    }

    let area = Rect::from_min_max(rect.min, Pos2::new(rect.right(), switches.top() - 2.0));
    ui.painter().rect_filled(area, 2.0, pal().knob_track);
    match state.mode {
        Mode::Collapsed => {}
        Mode::Curve => draw_curve(ui, area, panel),
        Mode::Activity => draw_activity(ui, area, &state),
    }
    ui.ctx().data_mut(|d| d.insert_temp(id, state));
}

fn draw_curve(ui: &mut egui::Ui, rect: Rect, panel: &PanelCtx) {
    let get = |id: u32, default: f64| panel.value(ParamId(id)).unwrap_or(default) as f32;
    let threshold = get(comp::THRESHOLD, -18.0);
    let ratio = get(comp::RATIO, 4.0);
    let knee = get(comp::KNEE, 6.0);
    let expand = panel
        .value(ParamId(comp::MODE))
        .is_some_and(|m| m.round() == comp::MODE_EXPAND);
    let p = ui.painter();
    let line = Stroke::new(1.0, pal().text_faint);
    p.line_segment(
        [
            Pos2::new(level_x(rect, threshold), rect.top()),
            Pos2::new(level_x(rect, threshold), rect.bottom()),
        ],
        Stroke::new(1.0, pal().solo_blue),
    );
    // The knee, as a pair of dotted lines.
    for edge in [threshold - knee / 2.0, threshold + knee / 2.0] {
        let x = level_x(rect, edge);
        let mut y = rect.top();
        while y < rect.bottom() {
            p.line_segment([Pos2::new(x, y), Pos2::new(x, y + 2.0)], line);
            y += 4.0;
        }
    }
    let points: Vec<Pos2> = (0..=64)
        .map(|i| {
            let input = FLOOR_DB + (-FLOOR_DB) * i as f32 / 64.0;
            let output = input + comp::curve_gain_db(input, threshold, ratio, knee, expand);
            Pos2::new(level_x(rect, input), level_y(rect, output))
        })
        .collect();
    p.add(Shape::line(points, Stroke::new(1.5, pal().accent)));
}

fn draw_activity(ui: &mut egui::Ui, rect: Rect, state: &State) {
    let p = ui.painter();
    let width = rect.width() / (HISTORY - 1) as f32;
    let x = |i: usize| rect.left() + i as f32 * width;
    let offset = HISTORY.saturating_sub(state.history.len());
    let input: Vec<Pos2> = state
        .history
        .iter()
        .enumerate()
        .map(|(i, v)| Pos2::new(x(i + offset), level_y(rect, v[0])))
        .collect();
    p.add(Shape::line(input, Stroke::new(1.0, pal().text_dim)));
    let colour = match state.trace {
        Trace::GainReduction => pal().accent,
        Trace::Output => pal().text_faint,
    };
    let value = |v: &[f32; 3]| match state.trace {
        Trace::GainReduction => rect.top() + (v[2] / 24.0).min(1.0) * rect.height(),
        Trace::Output => level_y(rect, v[1]),
    };
    let trace: Vec<Pos2> = state
        .history
        .iter()
        .enumerate()
        .map(|(i, v)| Pos2::new(x(i + offset), value(v)))
        .collect();
    p.add(Shape::line(trace, Stroke::new(1.0, colour)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_level_maps_inside_the_display() {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(70.0, 70.0));
        assert_eq!(level_x(rect, 0.0), 70.0);
        assert_eq!(level_y(rect, 0.0), 0.0);
        assert_eq!(level_x(rect, FLOOR_DB), 0.0);
        assert_eq!(level_x(rect, 20.0), 70.0);
    }

    #[test]
    fn the_curve_panel_defaults_to_the_transfer_curve() {
        assert!(matches!(Mode::default(), Mode::Curve));
        assert!(State::default().history.is_empty());
    }
}
