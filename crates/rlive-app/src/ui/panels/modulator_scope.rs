//! The small oscilloscope and trigger LED every modulator has.

use super::{PanelCtx, PanelDef};
use crate::ui::theme::*;
use egui::{Pos2, Rect, Shape, Stroke, Vec2};
use rlive_core::SCOPE_VALUES;
use rlive_devices::devices::b2::common::{LED, SCOPE, VIEW_LEN};

pub const PANEL: PanelDef = PanelDef {
    name: "modulator_scope",
    size: Vec2::new(170.0, 90.0),
    views: VIEW_LEN,
    draw,
};

fn draw(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    let p = ui.painter();
    p.rect_filled(rect, 2.0, shade(0x121212));
    // The centre line, which Offset moves the wave away from.
    p.line_segment(
        [
            Pos2::new(rect.left(), rect.center().y),
            Pos2::new(rect.right(), rect.center().y),
        ],
        Stroke::new(1.0, pal().separator),
    );
    let mut values = [0.0f32; SCOPE_VALUES];
    if !panel.view.is_some_and(|v| v.read(SCOPE, &mut values)) {
        return;
    }
    let inner = rect.shrink(3.0);
    let points: Vec<Pos2> = values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            Pos2::new(
                inner.left() + inner.width() * i as f32 / (SCOPE_VALUES - 1) as f32,
                inner.center().y - v.clamp(-1.0, 1.0) * inner.height() * 0.5,
            )
        })
        .collect();
    p.add(Shape::line(points, Stroke::new(1.5, pal().accent)));
    let mut led = [0.0f32];
    if panel.view.is_some_and(|v| v.read(LED, &mut led)) && led[0] > 0.0 {
        let c = Rect::from_center_size(rect.right_top() + Vec2::new(-7.0, 7.0), Vec2::splat(6.0));
        p.circle_filled(c.center(), 3.0, pal().accent.gamma_multiply(led[0]));
    }
}
