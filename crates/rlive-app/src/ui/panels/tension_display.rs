//! Tension's string display: the string moving, with exciter, damper and pickup marked on it.

use super::{PanelCtx, PanelDef};
use crate::ui::theme::{self, *};
use egui::{Align2, CornerRadius, Pos2, Stroke, Vec2};
use rlive_core::ParamId;
use rlive_devices::devices::b12::tension as te;

pub const PANEL: PanelDef = PanelDef {
    name: "tension_display",
    size: Vec2::new(320.0, 80.0),
    views: te::VIEW_LEN,
    draw,
};

fn draw(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(2), pal().knob_track);

    let mut view = [0.0f32; te::VIEW_LEN];
    let live = panel.view.is_some_and(|v| v.read(te::VIEW, &mut view));
    let plot = rect.shrink2(Vec2::new(8.0, 14.0));
    let mid = plot.center().y;

    let position_of = |ctx: &PanelCtx, id: u32| {
        let percent = ctx.value(ParamId(id)).unwrap_or(0.0) as f32 * 0.01;
        plot.left() + plot.width() * percent
    };
    let positions = [
        (position_of(panel, te::EX_POSITION), pal().accent, "Exciter"),
        (position_of(panel, te::D_POSITION), pal().arm_red, "Damper"),
        (
            position_of(panel, te::P_POSITION),
            pal().solo_blue,
            "Pickup",
        ),
    ];

    if live {
        let mut points = Vec::with_capacity(te::SHAPE_POINTS);
        for (i, v) in view[4..].iter().enumerate() {
            let t = (i as f32 + 0.5) / te::SHAPE_POINTS as f32;
            points.push(Pos2::new(
                plot.left() + t * plot.width(),
                mid - v * plot.height() * 0.45,
            ));
        }
        p.add(egui::Shape::line(points, Stroke::new(1.5, pal().text)));
    } else {
        p.line_segment(
            [Pos2::new(plot.left(), mid), Pos2::new(plot.right(), mid)],
            Stroke::new(1.5, pal().text),
        );
    }
    for (row, (x, color, name)) in positions.into_iter().enumerate() {
        p.line_segment(
            [Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())],
            Stroke::new(1.0, color),
        );
        p.text(
            Pos2::new(x + 2.0, rect.top() + 2.0 + 10.0 * row as f32),
            Align2::LEFT_TOP,
            name,
            theme::font(8.5),
            color,
        );
    }
    let exciter = panel.value(ParamId(te::EX_TYPE)).unwrap_or(0.0).round() as usize;
    p.text(
        rect.left_bottom() + Vec2::new(4.0, -2.0),
        Align2::LEFT_BOTTOM,
        te::EXCITERS[exciter.min(te::EXCITERS.len() - 1)],
        theme::font(9.0),
        pal().text_dim,
    );
    // The MPE activity LEDs: pressure, slide and per-note bend.
    for (i, label) in ["P", "S", "B"].into_iter().enumerate() {
        let lit = live && view[te::VIEW_ACTIVITY + i] > 0.0;
        let centre = rect.right_top() + Vec2::new(-40.0 + 14.0 * i as f32, 9.0);
        p.circle_filled(centre, 3.0, if lit { pal().accent } else { pal().raised });
        p.text(
            centre + Vec2::new(0.0, 8.0),
            Align2::CENTER_TOP,
            label,
            theme::font(8.0),
            pal().text_dim,
        );
    }
    if live {
        p.text(
            rect.right_bottom() + Vec2::new(-4.0, -2.0),
            Align2::RIGHT_BOTTOM,
            format!("{} voices", view[0] as i32),
            theme::font(9.0),
            pal().text_dim,
        );
    }
}
