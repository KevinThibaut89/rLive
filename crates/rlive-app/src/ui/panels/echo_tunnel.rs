//! Echo's tunnel: each repeat of the two delay lines is a ring, shrinking towards the centre,
//! with a fixed 1/8th-note grid of dots down the middle. Dragging in the tunnel's left or right
//! half sets that line's time.

use super::{PanelCtx, PanelDef};
use crate::ui::theme::*;
use egui::{CornerRadius, Pos2, Rect, Sense, Stroke, Vec2};
use rlive_core::ParamId;
use rlive_devices::devices::b6::echo;

pub const PANEL: PanelDef = PanelDef {
    name: "echo_tunnel",
    size: Vec2::new(220.0, 110.0),
    views: echo::TUNNEL_VIEW_LEN,
    draw,
};

/// How many beats the tunnel shows, outside edge to centre.
const TUNNEL_BEATS: f32 = 4.0;
const RINGS: usize = 16;

/// Where a repeat this many beats down the tunnel is, as a fraction of the way to the centre.
fn ring_radius(beats: f32) -> f32 {
    (1.0 - beats / TUNNEL_BEATS).clamp(0.0, 1.0)
}

fn draw(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    let response = ui.interact(
        rect,
        ui.scope_id().with(("echo_tunnel", panel.device.id)),
        Sense::drag(),
    );
    let mut times = [0.0f32; echo::TUNNEL_VIEW_LEN];
    let have = panel
        .view
        .is_some_and(|v| v.read(echo::TUNNEL_VIEW, &mut times));
    let feedback = panel.value(ParamId(echo::FEEDBACK)).unwrap_or(0.5) as f32;

    if response.dragged() {
        if let Some(pos) = response.interact_pointer_pos() {
            let right = pos.x > rect.center().x;
            let time = if right { echo::TIME_R } else { echo::TIME_L };
            let current = panel.value(ParamId(time)).unwrap_or(300.0) as f32;
            let change = 1.0 - response.drag_delta().y * 0.01;
            panel.set(ParamId(time), (current * change).clamp(1.0, 5_000.0) as f64);
        }
    }

    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(2), pal().knob_track);
    let center = rect.center();
    let max_radius = rect.height().min(rect.width()) * 0.5 - 4.0;
    let line = |p: &egui::Painter, beats: f32, level: f32, colour: egui::Color32| {
        let r = ring_radius(beats) * max_radius;
        if r > 0.5 {
            p.circle_stroke(center, r, Stroke::new(1.0, colour.gamma_multiply(level)));
        }
    };

    // The fixed 1/8th-note dots.
    for k in 0..=(TUNNEL_BEATS * 2.0) as i32 {
        let r = ring_radius(k as f32 * 0.5) * max_radius;
        p.circle_filled(Pos2::new(center.x, center.y - r), 1.2, pal().text);
    }
    if have {
        let (left, right) = (times[0].max(1e-3), times[1].max(1e-3));
        let mut level = 1.0;
        for k in 1..=RINGS {
            let (l_beats, r_beats) = (left * k as f32, right * k as f32);
            if l_beats.min(r_beats) > TUNNEL_BEATS {
                break;
            }
            line(p, l_beats, level, pal().accent);
            if (l_beats - r_beats).abs() > 1e-3 {
                line(p, r_beats, level, pal().play_green);
            }
            level = (level * feedback).max(0.15);
        }
    }
    p.rect_stroke(
        Rect::from_center_size(center, rect.size()),
        CornerRadius::same(2),
        Stroke::new(1.0, pal().separator),
        egui::StrokeKind::Inside,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeats_move_towards_the_centre_as_they_go() {
        assert_eq!(ring_radius(0.0), 1.0);
        assert!(ring_radius(1.0) > ring_radius(2.0));
        assert_eq!(ring_radius(10.0), 0.0);
    }
}
