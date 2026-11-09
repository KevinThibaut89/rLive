//! Collision's resonator display: an X-Y pad for Decay against Material or Radius, the object it
//! models, the partials it rings at, and the copy button between resonators.

use super::{PanelCtx, PanelDef};
use crate::ui::theme::{self, *};
use egui::{Align2, CornerRadius, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use rlive_core::ParamId;
use rlive_devices::devices::b12::collision as co;
use rlive_devices::devices::b12::common::MAX_MODES;

pub const RES_1: PanelDef = PanelDef {
    name: "collision_res_1",
    size: Vec2::new(320.0, 120.0),
    views: co::VIEW_LEN,
    draw: draw_res_1,
};

pub const RES_2: PanelDef = PanelDef {
    name: "collision_res_2",
    size: Vec2::new(320.0, 120.0),
    views: co::VIEW_LEN,
    draw: draw_res_2,
};

fn draw_res_1(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    draw(ui, panel, 0);
}

fn draw_res_2(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    draw(ui, panel, 1);
}

fn draw(ui: &mut egui::Ui, panel: &mut PanelCtx, slot: usize) {
    let rect = ui.max_rect();
    let base = co::RES_BASE[slot];
    let other = co::RES_BASE[1 - slot];
    let kind = (panel
        .value(ParamId(base + co::R_TYPE))
        .unwrap_or(0.0)
        .round() as usize)
        .min(co::RESONANCE_TYPES.len() - 1);
    let tubular = matches!(kind, co::PIPE | co::TUBE);
    let y_param = ParamId(
        base + if tubular {
            co::R_RADIUS
        } else {
            co::R_MATERIAL
        },
    );

    let pad = Rect::from_min_max(rect.min, Pos2::new(rect.left() + 132.0, rect.bottom()));
    let response = ui.interact(
        pad,
        ui.scope_id().with(("collision_xy", slot, panel.device.id)),
        Sense::drag(),
    );
    if let (Some(x_info), Some(y_info)) = (
        panel.param(ParamId(base + co::R_DECAY)),
        panel.param(y_param),
    ) {
        if response.dragged() {
            if let Some(p) = response.interact_pointer_pos() {
                let nx = ((p.x - pad.left()) / pad.width()).clamp(0.0, 1.0) as f64;
                let ny = (1.0 - (p.y - pad.top()) / pad.height()).clamp(0.0, 1.0) as f64;
                panel.set(ParamId(base + co::R_DECAY), x_info.from_normalized(nx));
                panel.set(y_param, y_info.from_normalized(ny));
            }
        }
        let p = ui.painter();
        p.rect_filled(pad, CornerRadius::same(2), pal().knob_track);
        p.rect_stroke(
            pad,
            CornerRadius::same(2),
            Stroke::new(1.0, pal().separator),
            StrokeKind::Inside,
        );
        let x =
            x_info.to_normalized(panel.value(ParamId(base + co::R_DECAY)).unwrap_or(0.0)) as f32;
        let y = y_info.to_normalized(panel.value(y_param).unwrap_or(0.0)) as f32;
        p.circle_filled(
            Pos2::new(
                pad.left() + x * pad.width(),
                pad.bottom() - y * pad.height(),
            ),
            4.0,
            pal().accent,
        );
        p.text(
            pad.left_bottom() + Vec2::new(3.0, -2.0),
            Align2::LEFT_BOTTOM,
            "Decay",
            theme::font(9.0),
            pal().text_dim,
        );
        p.text(
            pad.left_top() + Vec2::new(3.0, 2.0),
            Align2::LEFT_TOP,
            if tubular { "Radius" } else { "Material" },
            theme::font(9.0),
            pal().text_dim,
        );
    }

    let split = rect.left() + 140.0;
    let right = Rect::from_min_max(Pos2::new(split, rect.top()), rect.right_bottom());
    let object = Rect::from_min_size(right.min, Vec2::new(right.width(), 58.0));
    draw_object(ui, object, kind);

    let bars = Rect::from_min_max(
        Pos2::new(split, object.bottom() + 4.0),
        Pos2::new(right.right(), rect.bottom() - 20.0),
    );
    let p = ui.painter();
    p.rect_filled(bars, CornerRadius::same(2), pal().knob_track);
    if !tubular {
        let quality = panel
            .value(ParamId(base + co::R_QUALITY))
            .unwrap_or(2.0)
            .round() as usize;
        let count = co::QUALITY_MODES[quality.min(co::QUALITY_MODES.len() - 1)];
        let ratio = panel.value(ParamId(base + co::R_RATIO)).unwrap_or(1.0) as f32;
        let hit = panel.value(ParamId(base + co::R_HIT)).unwrap_or(0.0) as f32 * 0.01;
        let mut table = [co::Mode::default(); MAX_MODES];
        co::modes(kind, count, ratio, &mut table);
        let span = table[..count]
            .iter()
            .fold(2.0f32, |m, t| m.max(t.ratio))
            .log2();
        for mode in &table[..count] {
            let x = bars.left() + bars.width() * mode.ratio.log2().max(0.0) / span;
            let level = co::shape(kind, *mode, hit).abs() * mode.ratio.powf(-0.4);
            p.line_segment(
                [
                    Pos2::new(x, bars.bottom()),
                    Pos2::new(x, bars.bottom() - level * bars.height()),
                ],
                Stroke::new(1.0, pal().accent),
            );
        }
    }

    // The copy button: "1 > 2" on Resonator 1, "2 > 1" on Resonator 2.
    let label = if slot == 0 { "1 > 2" } else { "2 > 1" };
    let button = Rect::from_min_size(
        Pos2::new(split, rect.bottom() - 16.0),
        Vec2::new(40.0, 15.0),
    );
    let response = ui.interact(
        button,
        ui.scope_id()
            .with(("collision_copy", slot, panel.device.id)),
        Sense::click(),
    );
    if response.clicked() {
        for offset in 0..co::R_PARAMS {
            if let Some(value) = panel.value(ParamId(base + offset)) {
                panel.set(ParamId(other + offset), value);
            }
        }
    }
    let p = ui.painter();
    p.rect_filled(
        button,
        CornerRadius::same(2),
        if response.hovered() {
            pal().raised_hover
        } else {
            pal().raised
        },
    );
    p.text(
        button.center(),
        Align2::CENTER_CENTER,
        label,
        theme::font(10.0),
        pal().text,
    );

    let mut view = [0.0f32; co::VIEW_LEN];
    let live = panel.view.is_some_and(|v| v.read(co::VIEW, &mut view));
    let voices = if live { view[0] } else { 0.0 };
    p.text(
        Pos2::new(right.right(), rect.bottom() - 8.0),
        Align2::RIGHT_CENTER,
        format!("{} voices", voices as i32),
        theme::font(9.0),
        pal().text_dim,
    );
}

fn draw_object(ui: &egui::Ui, rect: Rect, kind: usize) {
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(2), pal().knob_track);
    p.text(
        rect.left_top() + Vec2::new(4.0, 2.0),
        Align2::LEFT_TOP,
        co::RESONANCE_TYPES[kind],
        theme::font(10.0),
        pal().text_dim,
    );
    let stroke = Stroke::new(1.5, pal().text);
    let c = rect.center() + Vec2::new(0.0, 8.0);
    match kind {
        co::BEAM | co::MARIMBA => {
            let bar = Rect::from_center_size(c, Vec2::new(rect.width() * 0.7, 12.0));
            p.rect_stroke(bar, CornerRadius::same(1), stroke, StrokeKind::Inside);
        }
        co::STRING => {
            p.line_segment(
                [
                    c - Vec2::new(rect.width() * 0.4, 0.0),
                    c + Vec2::new(rect.width() * 0.4, 0.0),
                ],
                stroke,
            );
        }
        co::MEMBRANE | co::PLATE => {
            let plate = Rect::from_center_size(c, Vec2::new(rect.width() * 0.4, 30.0));
            p.rect_stroke(plate, CornerRadius::same(1), stroke, StrokeKind::Inside);
        }
        _ => {
            let tube = Rect::from_center_size(c, Vec2::new(rect.width() * 0.7, 22.0));
            p.rect_stroke(tube, CornerRadius::same(8), stroke, StrokeKind::Inside);
        }
    }
}
