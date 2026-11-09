//! The modulation grids of Meld and Wavetable: sources across, targets down, drag a cell to set
//! its amount. Meld's has one grid for each engine, with the copy and clear buttons.

use super::{PanelCtx, PanelDef};
use crate::ui::theme::{self, *};
use egui::{Align2, CornerRadius, Pos2, Rect, Sense, Stroke, Vec2};
use rlive_core::ParamId;
use rlive_devices::devices::b11::meld as me;
use rlive_devices::devices::b11::wavetable as wt;

pub const MELD: PanelDef = PanelDef {
    name: "meld_matrix",
    size: Vec2::new(480.0, 215.0),
    views: 0,
    draw: draw_meld,
};

pub const WAVETABLE: PanelDef = PanelDef {
    name: "wavetable_matrix",
    size: Vec2::new(430.0, 215.0),
    views: 0,
    draw: draw_wavetable,
};

const LABEL_W: f32 = 104.0;
const HEADER_H: f32 = 50.0;
const ROW_H: f32 = 13.0;

/// Draws a grid at `rect`, reading and writing cells through `cell`. The target rows scroll under
/// the source names. Returns the hovered cell.
fn grid(
    ui: &mut egui::Ui,
    panel: &mut PanelCtx,
    rect: Rect,
    sources: &[&str],
    targets: &[&str],
    id_salt: (u64, u32),
    cell: impl Fn(usize, usize) -> u32,
) -> Option<(usize, usize, f64)> {
    let cw = (rect.width() - LABEL_W - 12.0) / sources.len() as f32;
    let p = ui.painter();
    for (s, name) in sources.iter().enumerate() {
        let x = rect.left() + LABEL_W + (s as f32 + 0.5) * cw + 4.0;
        let anchor = Pos2::new(x, rect.top() + HEADER_H - 3.0);
        let text = egui::epaint::TextShape::new(
            anchor,
            ui.painter()
                .layout_no_wrap((*name).to_string(), theme::font(9.0), pal().text_dim),
            pal().text_dim,
        )
        .with_angle(-std::f32::consts::FRAC_PI_2);
        p.add(text);
    }

    let body = Rect::from_min_max(
        Pos2::new(rect.left(), rect.top() + HEADER_H),
        rect.right_bottom(),
    );
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body));
    let mut hovered = None;
    egui::ScrollArea::vertical()
        .id_salt(("synth_matrix", id_salt))
        .show(&mut child, |ui| {
            for (t, name) in targets.iter().enumerate() {
                let (row, _) =
                    ui.allocate_exact_size(Vec2::new(rect.width() - 12.0, ROW_H), Sense::hover());
                ui.painter().text(
                    Pos2::new(row.left() + LABEL_W - 4.0, row.center().y),
                    Align2::RIGHT_CENTER,
                    *name,
                    theme::font(9.0),
                    pal().text,
                );
                for s in 0..sources.len() {
                    let r = Rect::from_min_size(
                        Pos2::new(row.left() + LABEL_W + s as f32 * cw, row.top()),
                        Vec2::new(cw - 1.0, ROW_H - 1.0),
                    );
                    let id = ParamId(cell(s, t));
                    let value = panel.value(id).unwrap_or(0.0);
                    let resp = ui.interact(
                        r,
                        ui.make_persistent_id(("matrix_cell", id_salt, s, t)),
                        Sense::click_and_drag(),
                    );
                    if resp.dragged() {
                        let next = (value - resp.drag_delta().y as f64 * 1.5).clamp(-100.0, 100.0);
                        panel.set(id, next);
                    }
                    if resp.double_clicked() {
                        panel.set(id, 0.0);
                    }
                    if resp.hovered() {
                        hovered = Some((s, t, value));
                    }
                    let p = ui.painter();
                    p.rect_filled(r, CornerRadius::same(1), pal().knob_track);
                    if value != 0.0 {
                        let color = if value > 0.0 {
                            pal().knob_arc
                        } else {
                            pal().arm_red
                        };
                        let frac = (value.abs() / 100.0) as f32;
                        let h = (r.height() - 2.0) * frac;
                        let bar = Rect::from_min_max(
                            Pos2::new(r.left() + 1.0, r.bottom() - 1.0 - h),
                            Pos2::new(r.right() - 1.0, r.bottom() - 1.0),
                        );
                        p.rect_filled(bar, CornerRadius::ZERO, color);
                    }
                    if resp.hovered() {
                        p.rect_stroke(
                            r,
                            CornerRadius::same(1),
                            Stroke::new(1.0, pal().text),
                            egui::StrokeKind::Inside,
                        );
                    }
                }
            }
        });
    hovered
}

fn hover_line(
    ui: &mut egui::Ui,
    rect: Rect,
    sources: &[&str],
    targets: &[&str],
    hovered: Option<(usize, usize, f64)>,
) {
    let text = hovered
        .map(|(s, t, v)| format!("{} > {}: {v:.0}%", sources[s], targets[t]))
        .unwrap_or_else(|| "Drag a cell to set its amount".to_string());
    ui.painter().text(
        Pos2::new(rect.left(), rect.top() + 2.0),
        Align2::LEFT_TOP,
        text,
        theme::font(9.0),
        pal().text_dim,
    );
}

fn draw_meld(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    let id = ui.make_persistent_id(("meld_engine", panel.device.id));
    let mut engine: usize = ui.data(|d| d.get_temp(id).unwrap_or(0));

    let bar = Rect::from_min_size(rect.min, Vec2::new(rect.width(), 20.0));
    let mut ui_bar = ui.new_child(egui::UiBuilder::new().max_rect(bar));
    ui_bar.horizontal(|ui| {
        for (i, name) in ["A", "B"].iter().enumerate() {
            if ui.selectable_label(engine == i, *name).clicked() {
                engine = i;
            }
        }
        if ui.button("Copy to A").clicked() {
            copy_cells(panel, 1, 0);
        }
        if ui.button("Copy to B").clicked() {
            copy_cells(panel, 0, 1);
        }
        if ui.button("X").clicked() {
            for s in 0..me::SOURCES.len() {
                for t in 0..me::TARGETS.len() {
                    panel.set(ParamId(me::cell(engine, s, t)), 0.0);
                }
            }
        }
    });
    ui.data_mut(|d| d.insert_temp(id, engine));

    let area = Rect::from_min_max(
        Pos2::new(rect.left(), bar.bottom() + 2.0),
        rect.right_bottom(),
    );
    let salt = (panel.device.id.0, engine as u32);
    let hovered = grid(ui, panel, area, &me::SOURCES, &me::TARGETS, salt, |s, t| {
        me::cell(engine, s, t)
    });
    hover_line(ui, area, &me::SOURCES, &me::TARGETS, hovered);
}

fn copy_cells(panel: &mut PanelCtx, from: usize, to: usize) {
    for s in 0..me::SOURCES.len() {
        for t in 0..me::TARGETS.len() {
            let value = panel.value(ParamId(me::cell(from, s, t))).unwrap_or(0.0);
            panel.set(ParamId(me::cell(to, s, t)), value);
        }
    }
}

fn draw_wavetable(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    let salt = (panel.device.id.0, 0);
    let hovered = grid(ui, panel, rect, &wt::SOURCES, &wt::TARGETS, salt, |s, t| {
        wt::cell(s, t)
    });
    hover_line(ui, rect, &wt::SOURCES, &wt::TARGETS, hovered);
}
