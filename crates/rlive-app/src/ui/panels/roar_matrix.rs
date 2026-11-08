//! Roar's modulation matrix: a grid of sources against targets, one amount per cell.

use super::{PanelCtx, PanelDef};
use crate::ui::theme::{self, *};
use egui::{Align2, Color32, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use rlive_core::ParamId;
use rlive_devices::devices::b5::roar;

pub const PANEL: PanelDef = PanelDef {
    name: "roar_matrix",
    size: Vec2::new(344.0, 150.0),
    views: roar::VIEW_LEN,
    draw,
};

const LABEL_W: f32 = 112.0;
const CELL_W: f32 = 54.0;
const ROW_H: f32 = 15.0;
const HEADER_H: f32 = 18.0;

fn cell_colour(value: f32) -> Color32 {
    if value >= 0.0 {
        pal().knob_arc
    } else {
        pal().arm_red
    }
}

fn draw(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let mut levels = [0.0f32; roar::VIEW_LEN];
    let live = panel
        .view
        .is_some_and(|v| v.read(roar::MOD_LEVELS, &mut levels));
    let rect = ui.max_rect();
    let header = Rect::from_min_size(rect.min, Vec2::new(rect.width(), HEADER_H));
    header_row(ui, panel, header, &levels, live);

    let body = Rect::from_min_max(Pos2::new(rect.left(), header.bottom()), rect.max);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body));
    egui::ScrollArea::vertical()
        .id_salt("roar_matrix")
        .show(&mut child, |ui| {
            for (dest, name) in roar::DESTS.iter().enumerate() {
                let (row, _) = ui.allocate_exact_size(
                    Vec2::new(LABEL_W + CELL_W * roar::SOURCE_COUNT as f32, ROW_H),
                    Sense::hover(),
                );
                ui.painter().text(
                    Pos2::new(row.left() + 2.0, row.center().y),
                    Align2::LEFT_CENTER,
                    name,
                    theme::font(9.0),
                    pal().text_dim,
                );
                for source in 0..roar::SOURCE_COUNT {
                    let cell = Rect::from_min_size(
                        Pos2::new(row.left() + LABEL_W + CELL_W * source as f32, row.top()),
                        Vec2::new(CELL_W - 2.0, ROW_H - 1.0),
                    );
                    cell_widget(ui, panel, cell, source, dest);
                }
            }
        });
}

fn header_row(
    ui: &mut egui::Ui,
    panel: &mut PanelCtx,
    rect: Rect,
    levels: &[f32; roar::VIEW_LEN],
    live: bool,
) {
    let clear = Rect::from_min_size(rect.min, Vec2::new(LABEL_W - 4.0, rect.height() - 2.0));
    let response = ui.interact(clear, ui.scope_id().with("roar_clear"), Sense::click());
    ui.painter().rect_stroke(
        clear,
        1.0,
        Stroke::new(1.0, pal().separator),
        StrokeKind::Inside,
    );
    ui.painter().text(
        clear.center(),
        Align2::CENTER_CENTER,
        "X Clear",
        theme::font(9.0),
        pal().text_dim,
    );
    if response.clicked() {
        for source in 0..roar::SOURCE_COUNT {
            for dest in 0..roar::DEST_COUNT {
                panel.set(ParamId(roar::cell_param(source, dest)), 0.0);
            }
        }
    }
    for (i, name) in roar::SOURCES.iter().enumerate() {
        let x = rect.left() + LABEL_W + CELL_W * i as f32;
        let led = Pos2::new(x + 6.0, rect.center().y);
        let level = if live {
            levels[i].abs().clamp(0.0, 1.0)
        } else {
            0.0
        };
        ui.painter()
            .circle_filled(led, 3.0, darken(pal().knob_arc, 1.0 - level));
        ui.painter().text(
            Pos2::new(x + 12.0, rect.center().y),
            Align2::LEFT_CENTER,
            *name,
            theme::font(9.0),
            pal().text_dim,
        );
    }
}

fn cell_widget(ui: &mut egui::Ui, panel: &mut PanelCtx, cell: Rect, source: usize, dest: usize) {
    let id = ParamId(roar::cell_param(source, dest));
    let value = panel.value(id).unwrap_or(0.0) as f32;
    let response = ui.interact(cell, ui.scope_id().with(id.0), Sense::click_and_drag());
    ui.painter().rect_filled(cell, 1.0, pal().knob_track);
    if value != 0.0 {
        let centre = cell.center().x;
        let edge = centre + value.clamp(-1.0, 1.0) * cell.width() * 0.5;
        let bar = Rect::from_two_pos(
            Pos2::new(centre.min(edge), cell.top()),
            Pos2::new(centre.max(edge), cell.bottom()),
        );
        ui.painter().rect_filled(bar, 1.0, cell_colour(value));
    }
    if response.dragged() {
        let moved = value as f64 - response.drag_delta().y as f64 * 0.01;
        panel.set(id, moved.clamp(-1.0, 1.0));
    }
    if response.double_clicked() {
        panel.set(id, 0.0);
    }
}
