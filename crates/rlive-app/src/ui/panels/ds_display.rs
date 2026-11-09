//! The display of a Drum Synth: its decay, and the upper half is a preview button (manual
//! p. 831).

use super::{PanelCtx, PanelDef};
use crate::ui::theme::*;
use egui::{Align2, Pos2, Rect, Sense, Shape, Stroke, Vec2};

pub const PANEL: PanelDef = PanelDef {
    name: "ds_display",
    size: Vec2::new(150.0, 80.0),
    views: 0,
    draw,
};

fn draw(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    let upper = Rect::from_min_max(rect.min, Pos2::new(rect.right(), rect.center().y));
    let resp = ui.interact(
        upper,
        ui.scope_id().with(("ds_preview", panel.device.id)),
        Sense::click(),
    );
    if resp.clicked() {
        panel.preview_note();
    }
    let p = ui.painter();
    p.rect_filled(rect, 2.0, shade(0x121212));
    let decay = panel
        .runtime_param("Decay")
        .map_or(0.3, |id| panel.value(id).unwrap_or(300.0) as f32 * 0.001);
    let inner = rect.shrink(4.0);
    let points: Vec<Pos2> = (0..80)
        .map(|n| {
            let t = n as f32 / 79.0;
            let seconds = t * 1.5;
            let level = (-seconds / decay.max(0.01) * 6.9).exp();
            Pos2::new(
                inner.left() + inner.width() * t,
                inner.bottom() - level * inner.height(),
            )
        })
        .collect();
    p.add(Shape::line(points, Stroke::new(1.5, pal().accent)));
    p.text(
        upper.center(),
        Align2::CENTER_CENTER,
        "Preview",
        font(9.0),
        if resp.hovered() {
            pal().text
        } else {
            pal().text_dim
        },
    );
}
