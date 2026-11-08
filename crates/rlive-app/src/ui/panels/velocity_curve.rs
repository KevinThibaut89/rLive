//! Velocity's curve display: input across, output up, with the Random band shaded around it.

use super::{PanelCtx, PanelDef};
use crate::ui::theme::*;
use egui::{Pos2, Rect, Shape, Stroke, StrokeKind, Vec2};
use rlive_core::ParamId;
use rlive_devices::devices::b1::velocity::{self, CurveSettings};

pub const PANEL: PanelDef = PanelDef {
    name: "velocity_curve",
    size: Vec2::new(120.0, 120.0),
    views: 0,
    draw,
};

fn draw(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    let p = ui.painter();
    p.rect_filled(rect, 1.0, pal().field_bg);
    p.rect_stroke(
        rect,
        1.0,
        Stroke::new(1.0, pal().separator),
        StrokeKind::Inside,
    );
    let v = |id: u32, fallback: f32| panel.value(ParamId(id)).map_or(fallback, |x| x as f32);
    let settings = CurveSettings {
        lowest: v(velocity::LOWEST, 0.0),
        range: v(velocity::RANGE, 127.0),
        out_low: v(velocity::OUT_LOW, 0.0),
        out_hi: v(velocity::OUT_HI, 127.0),
        drive: v(velocity::DRIVE, 0.0) * 0.01,
        compand: v(velocity::COMPAND, 0.0) * 0.01,
    };
    let random = v(velocity::RANDOM, 0.0);
    let point = |x: f32, y: f32| to_screen(rect, x, y);
    if random > 0.0 {
        let (low, high) = (
            settings.out_low.min(settings.out_hi),
            settings.out_low.max(settings.out_hi),
        );
        let top: Vec<Pos2> = (0..=127)
            .map(|x| x as f32)
            .map(|x| point(x, (settings.curve(x) + random).clamp(low, high)))
            .collect();
        let bottom: Vec<Pos2> = (0..=127)
            .rev()
            .map(|x| x as f32)
            .map(|x| point(x, (settings.curve(x) - random).clamp(low, high)))
            .collect();
        let band = top.into_iter().chain(bottom).collect();
        p.add(Shape::convex_polygon(
            band,
            pal().raised.gamma_multiply(0.6),
            Stroke::NONE,
        ));
    }
    let line: Vec<Pos2> = (0..=127)
        .map(|x| x as f32)
        .map(|x| point(x, settings.curve(x)))
        .collect();
    p.add(Shape::line(line, Stroke::new(1.5, pal().accent)));
}

fn to_screen(rect: Rect, x: f32, y: f32) -> Pos2 {
    Pos2::new(
        rect.left() + x / 127.0 * rect.width(),
        rect.bottom() - y / 127.0 * rect.height(),
    )
}
