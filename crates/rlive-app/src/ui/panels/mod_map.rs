//! Map buttons: what each of a modulator's outputs drives and how (manual p. 844).

use super::{PanelCtx, PanelDef};
use crate::ui::theme::*;
use egui::{Align2, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use rlive_core::ParamId;
use rlive_devices::devices::b1::expression_control as ec;
use rlive_devices::devices::b2::common::{
    map_param, MAPPINGS, MAP_AMOUNT, MAP_MAX, MAP_MIN, MAP_MODE, MAP_POLARITY,
};

pub const PANEL: PanelDef = PanelDef {
    name: "mod_map",
    size: Vec2::new(270.0, 150.0),
    views: 0,
    draw: draw_modulator,
};

pub const EXPRESSION: PanelDef = PanelDef {
    name: "expression_map",
    size: Vec2::new(150.0, 100.0),
    views: 0,
    draw: draw_expression,
};

const ROW_H: f32 = 17.0;

fn button(
    ui: &mut egui::Ui,
    rect: Rect,
    id: (rlive_core::DeviceId, &'static str, usize),
    text: &str,
    on: bool,
) -> egui::Response {
    let resp = ui.interact(rect, ui.scope_id().with(id), Sense::click());
    let p = ui.painter();
    let fill = if on { pal().accent } else { pal().field_bg };
    p.rect_filled(rect, 2.0, fill);
    p.rect_stroke(
        rect,
        2.0,
        Stroke::new(1.0, pal().separator),
        StrokeKind::Inside,
    );
    let colour = if on {
        text_on(pal().accent)
    } else {
        pal().text
    };
    p.text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        font(9.0),
        colour,
    );
    resp
}

/// A number dragged up and down, in the row's units.
fn drag_field(
    ui: &mut egui::Ui,
    rect: Rect,
    id: (rlive_core::DeviceId, &'static str, usize),
    text: &str,
) -> Option<f32> {
    let resp = ui.interact(rect, ui.scope_id().with(id), Sense::drag());
    let p = ui.painter();
    p.rect_filled(rect, 2.0, pal().field_bg);
    p.text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        font(9.0),
        pal().text,
    );
    resp.dragged().then(|| -resp.drag_delta().y)
}

/// The name of what a mapping drives.
fn target_label(panel: &PanelCtx, slot: usize) -> Option<String> {
    let target = panel
        .device
        .modulations
        .iter()
        .find(|m| usize::from(m.slot) == slot)?
        .target;
    Some(panel.session?.modulator_target_name(target))
}

fn map_button(ui: &mut egui::Ui, panel: &mut PanelCtx, rect: Rect, slot: usize) {
    let mapped = target_label(panel, slot);
    let armed = panel
        .session
        .and_then(|s| s.armed_mod_map())
        .is_some_and(|(d, n)| d == panel.device.id && n == slot);
    let text = mapped.as_deref().unwrap_or("Map");
    let resp = button(ui, rect, (panel.device.id, "map", slot), text, armed);
    if resp.clicked() {
        panel.arm_mod_map(slot, !armed);
    }
}

fn draw_modulator(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, 2.0, pal().knob_track);
    let id = ui.scope_id().with(("multimap", panel.device.id));
    let mut multi: bool = ui.ctx().data_mut(|d| d.get_temp(id).unwrap_or(false));
    let toggle = Rect::from_min_size(
        Pos2::new(rect.right() - 16.0, rect.top() + 1.0),
        Vec2::splat(14.0),
    );
    if button(
        ui,
        toggle,
        (panel.device.id, "multimap", 0),
        if multi { "-" } else { "+" },
        multi,
    )
    .clicked()
    {
        multi = !multi;
        ui.ctx().data_mut(|d| d.insert_temp(id, multi));
    }
    let rows = if multi { MAPPINGS } else { 1 };
    for slot in 0..rows {
        let y = rect.top() + 1.0 + slot as f32 * ROW_H;
        let param = |p| ParamId(map_param(slot, p));
        let value = |panel: &PanelCtx, p| panel.value(param(p)).unwrap_or(0.0);
        let map_r = Rect::from_min_size(Pos2::new(rect.left() + 2.0, y), Vec2::new(82.0, 15.0));
        map_button(ui, panel, map_r, slot);
        let unmap = Rect::from_min_size(map_r.right_top() + Vec2::new(1.0, 0.0), Vec2::splat(15.0));
        if button(ui, unmap, (panel.device.id, "unmap", slot), "x", false).clicked() {
            panel.unmap_mod(slot);
        }
        let remote = value(panel, MAP_MODE) >= 0.5;
        let mode = Rect::from_min_size(
            unmap.right_top() + Vec2::new(2.0, 0.0),
            Vec2::new(30.0, 15.0),
        );
        let label = if remote { "Rem" } else { "Mod" };
        if button(ui, mode, (panel.device.id, "mode", slot), label, remote).clicked() {
            panel.set(param(MAP_MODE), if remote { 0.0 } else { 1.0 });
        }
        let field_w = 32.0;
        let mut x = mode.right() + 2.0;
        let mut place = |w: f32| {
            let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, 15.0));
            x += w + 2.0;
            r
        };
        if remote {
            let min = value(panel, MAP_MIN);
            let max = value(panel, MAP_MAX);
            let (r_min, r_max) = (place(field_w), place(field_w));
            if let Some(d) = drag_field(
                ui,
                r_min,
                (panel.device.id, "min", slot),
                &format!("{min:.0}"),
            ) {
                panel.set(param(MAP_MIN), (min + f64::from(d)).clamp(0.0, 100.0));
            }
            if let Some(d) = drag_field(
                ui,
                r_max,
                (panel.device.id, "max", slot),
                &format!("{max:.0}"),
            ) {
                panel.set(param(MAP_MAX), (max + f64::from(d)).clamp(0.0, 100.0));
            }
        } else {
            let unipolar = value(panel, MAP_POLARITY) >= 0.5;
            let r_pol = place(22.0);
            if button(
                ui,
                r_pol,
                (panel.device.id, "pol", slot),
                if unipolar { "Un" } else { "Bi" },
                false,
            )
            .clicked()
            {
                panel.set(param(MAP_POLARITY), if unipolar { 0.0 } else { 1.0 });
            }
            let amount = value(panel, MAP_AMOUNT);
            let r_amt = place(field_w);
            if let Some(d) = drag_field(
                ui,
                r_amt,
                (panel.device.id, "amt", slot),
                &format!("{amount:.0}"),
            ) {
                panel.set(
                    param(MAP_AMOUNT),
                    (amount + f64::from(d)).clamp(-100.0, 100.0),
                );
            }
        }
    }
}

fn draw_expression(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, 2.0, pal().knob_track);
    for slot in 0..ec::SLOTS {
        let y = rect.top() + 2.0 + slot as f32 * ROW_H;
        let map_r = Rect::from_min_size(Pos2::new(rect.left() + 2.0, y), Vec2::new(118.0, 15.0));
        map_button(ui, panel, map_r, slot);
        let unmap = Rect::from_min_size(map_r.right_top() + Vec2::new(1.0, 0.0), Vec2::splat(15.0));
        if button(ui, unmap, (panel.device.id, "unmap", slot), "x", false).clicked() {
            panel.unmap_mod(slot);
        }
    }
}
