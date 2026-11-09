//! Electric's display: the fork's partials and the pickup's response at its current position.

use super::{PanelCtx, PanelDef};
use crate::ui::theme::{self, *};
use egui::{Align2, CornerRadius, Pos2, Rect, Stroke, Vec2};
use rlive_core::ParamId;
use rlive_devices::devices::b12::electric as el;

pub const PANEL: PanelDef = PanelDef {
    name: "electric_display",
    size: Vec2::new(260.0, 90.0),
    views: el::VIEW_LEN,
    draw,
};

fn draw(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    let split = rect.center().x;
    let left = Rect::from_min_max(rect.min, Pos2::new(split - 3.0, rect.bottom()));
    let right = Rect::from_min_max(Pos2::new(split + 3.0, rect.top()), rect.max);
    let p = ui.painter();
    p.rect_filled(left, CornerRadius::same(2), pal().knob_track);
    p.rect_filled(right, CornerRadius::same(2), pal().knob_track);

    let color = panel.value(ParamId(el::TINE_COLOR)).unwrap_or(50.0) as f32 * 0.01;
    let tilt = -1.2 + 1.5 * color;
    let span = (el::TINE_RATIOS[el::TINE_RATIOS.len() - 1]).log2();
    for ratio in el::TINE_RATIOS {
        let x = left.left() + 4.0 + (left.width() - 8.0) * ratio.log2() / span;
        let level = ratio.powf(tilt).min(1.0);
        p.line_segment(
            [
                Pos2::new(x, left.bottom() - 2.0),
                Pos2::new(x, left.bottom() - 2.0 - level * (left.height() - 14.0)),
            ],
            Stroke::new(2.0, pal().accent),
        );
    }
    p.text(
        left.left_top() + Vec2::new(4.0, 2.0),
        Align2::LEFT_TOP,
        "Tine",
        theme::font(9.0),
        pal().text_dim,
    );

    let kind = panel.value(ParamId(el::PICKUP_TYPE)).unwrap_or(0.0).round() as usize;
    let symmetry = panel.value(ParamId(el::SYMMETRY)).unwrap_or(50.0) as f32 * 0.01;
    let distance = panel.value(ParamId(el::DISTANCE)).unwrap_or(50.0) as f32 * 0.01;
    let d = 0.04 + 0.96 * distance.powf(1.5);
    let offset = (symmetry - 0.5) * 1.6;
    let mut points = Vec::with_capacity(right.width() as usize + 1);
    let h = right.height();
    let mut previous = f32::NAN;
    for i in 0..right.width() as usize {
        let t = i as f32 / right.width();
        let u = (t - 0.5) * 2.0;
        let y = if kind == el::PICKUP_W {
            let a = (u + offset) * 0.5;
            let v = a / (1.0 + (a / d).powi(2)).sqrt();
            (v * 2.0).clamp(-1.0, 1.0)
        } else {
            let a = u + offset;
            let flux = d * d / (a * a + d * d).sqrt();
            let dy = if previous.is_nan() {
                0.0
            } else {
                (flux - previous) * 20.0
            };
            previous = flux;
            dy.clamp(-1.0, 1.0)
        };
        points.push(Pos2::new(
            right.left() + t * right.width(),
            right.center().y - y * h * 0.4,
        ));
    }
    p.add(egui::Shape::line(points, Stroke::new(1.0, pal().accent)));
    p.text(
        right.left_top() + Vec2::new(4.0, 2.0),
        Align2::LEFT_TOP,
        if kind == el::PICKUP_W {
            "Pickup W"
        } else {
            "Pickup R"
        },
        theme::font(9.0),
        pal().text_dim,
    );
    let mut view = [0.0f32; el::VIEW_LEN];
    if panel.view.is_some_and(|v| v.read(el::VIEW, &mut view)) {
        p.text(
            right.right_top() + Vec2::new(-4.0, 2.0),
            Align2::RIGHT_TOP,
            format!("{} voices", view[0] as i32),
            theme::font(9.0),
            pal().text_dim,
        );
    }
}
