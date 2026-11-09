//! A plug-in's X-Y field: two parameters on one pad, chosen from its panel (manual p. 482).

use super::{PanelCtx, PanelDef};
use crate::ui::theme::{pal, shade};
use crate::ui::widgets::info;
use egui::{Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use rlive_core::{ParamId, ParamInfo};

pub const PANEL: PanelDef = PanelDef {
    name: "plugin_xy",
    size: Vec2::new(120.0, 118.0),
    views: 0,
    draw,
};

const CHOOSER_H: f32 = 19.0;

fn chooser(ui: &mut egui::Ui, panel: &mut PanelCtx, axis: usize, rect: Rect) {
    let current = panel.xy()[axis];
    let choices = panel.xy_choices();
    let label = current
        .and_then(|id| panel.param(ParamId(id)))
        .map_or("None".to_string(), |p| p.name.clone());
    let mut picked = None;
    let device = panel.device.id;
    let mut area = ui.new_child(
        egui::UiBuilder::new()
            .id_salt(("plugin_xy_row", device, axis))
            .max_rect(rect),
    );
    let response = egui::ComboBox::from_id_salt(("plugin_xy", device, axis))
        .width(rect.width() - 8.0)
        .selected_text(egui::RichText::new(label).size(10.0))
        .show_ui(&mut area, |ui| {
            ui.set_min_width(150.0);
            if ui.selectable_label(current.is_none(), "None").clicked() {
                picked = Some(None);
            }
            for (id, name) in &choices {
                if ui.selectable_label(current == Some(*id), name).clicked() {
                    picked = Some(Some(*id));
                }
            }
        })
        .response;
    info(
        response,
        if axis == 0 {
            "X-Y Field: X — the parameter moved left and right"
        } else {
            "X-Y Field: Y — the parameter moved up and down"
        },
    );
    if let Some(param) = picked {
        panel.set_xy(axis, param);
    }
}

fn norm(panel: &PanelCtx, p: &ParamInfo) -> f32 {
    p.to_normalized(panel.plugin_value(p)) as f32
}

fn draw(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    let pad = Rect::from_min_max(
        rect.min,
        Pos2::new(rect.right(), rect.bottom() - CHOOSER_H * 2.0 - 2.0),
    );
    let [x_id, y_id] = panel.xy();
    let x = x_id.and_then(|id| panel.param(ParamId(id)));
    let y = y_id.and_then(|id| panel.param(ParamId(id)));
    let resp = ui.interact(
        pad,
        ui.scope_id().with(("xy", panel.device.id)),
        Sense::click_and_drag(),
    );
    let painter = ui.painter();
    painter.rect_filled(pad, 1.0, pal().field_bg);
    painter.rect_stroke(
        pad,
        1.0,
        Stroke::new(1.0, pal().separator),
        StrokeKind::Inside,
    );
    let (nx, ny) = (
        x.map_or(0.5, |p| norm(panel, p)),
        y.map_or(0.5, |p| norm(panel, p)),
    );
    let at = Pos2::new(
        pad.left() + pad.width() * nx,
        pad.bottom() - pad.height() * ny,
    );
    let color = if x.is_some() || y.is_some() {
        pal().accent
    } else {
        shade(0x6a6a6a)
    };
    painter.hline(
        pad.x_range(),
        at.y,
        Stroke::new(1.0, color.gamma_multiply(0.4)),
    );
    painter.vline(
        at.x,
        pad.y_range(),
        Stroke::new(1.0, color.gamma_multiply(0.4)),
    );
    painter.circle_filled(at, 4.0, color);
    if let (true, Some(pos)) = (
        resp.dragged() || resp.clicked(),
        resp.interact_pointer_pos(),
    ) {
        let (px, py) = (
            ((pos.x - pad.left()) / pad.width()).clamp(0.0, 1.0) as f64,
            ((pad.bottom() - pos.y) / pad.height()).clamp(0.0, 1.0) as f64,
        );
        if let Some(p) = x {
            panel.set(ParamId(p.id), p.from_normalized(px));
        }
        if let Some(p) = y {
            panel.set(ParamId(p.id), p.from_normalized(py));
        }
    }
    info(resp, "X-Y Field — moves the two parameters chosen below");
    let row = |n: f32| {
        Rect::from_min_size(
            Pos2::new(rect.left(), rect.bottom() - CHOOSER_H * n),
            Vec2::new(rect.width(), CHOOSER_H),
        )
    };
    chooser(ui, panel, 0, row(2.0));
    chooser(ui, panel, 1, row(1.0));
}
