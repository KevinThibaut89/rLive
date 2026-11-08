//! Scale's 13x13 note matrix: incoming notes across, outgoing notes up.

use super::{PanelCtx, PanelDef};
use crate::ui::theme::*;
use egui::{Rect, Sense, Stroke, StrokeKind, Vec2};
use rlive_core::ParamId;
use rlive_devices::devices::b1::scale::{self, Mapping, REMOVED};

const CELLS: usize = 13;
const CELL: f32 = 11.0;
const BLACK_KEYS: [bool; 12] = [
    false, true, false, true, false, false, true, false, true, false, true, false,
];

pub const PANEL: PanelDef = PanelDef {
    name: "scale_matrix",
    size: Vec2::new(CELL * CELLS as f32, CELL * CELLS as f32),
    views: 0,
    draw,
};

fn user_map(panel: &PanelCtx) -> [i32; 12] {
    let mut user = [0; 12];
    for (i, u) in user.iter_mut().enumerate() {
        let id = ParamId(scale::FIRST_MAP + i as u32);
        *u = panel.value(id).map_or(i as i32, |v| v as i32);
    }
    user
}

fn draw(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    let base = panel.value(ParamId(scale::BASE)).unwrap_or(0.0) as u8;
    let name = panel.value(ParamId(scale::SCALE_NAME)).unwrap_or(0.0) as usize;
    let user = user_map(panel);
    let is_user = name == scale::user_scale_index();
    let mapping: Mapping = scale::mapping_for(base, name, &user, None);
    let cell = |column: usize, row: usize| {
        Rect::from_min_size(
            rect.min + Vec2::new(column as f32 * CELL, (CELLS - 1 - row) as f32 * CELL),
            Vec2::splat(CELL),
        )
    };
    {
        let p = ui.painter();
        p.rect_filled(rect, 0.0, pal().field_bg);
        for column in 0..CELLS {
            for row in 0..CELLS {
                let r = cell(column, row);
                let black = |n: usize| BLACK_KEYS[(n + base as usize) % 12];
                if black(column) || black(row) {
                    p.rect_filled(r, 0.0, pal().bar_bg);
                }
                p.rect_stroke(
                    r,
                    0.0,
                    Stroke::new(0.5, pal().separator),
                    StrokeKind::Inside,
                );
            }
        }
        for (column, target) in mapping.matrix.iter().enumerate() {
            if *target != REMOVED && (0..CELLS as i32).contains(target) {
                p.rect_filled(
                    cell(column, *target as usize).shrink(1.0),
                    0.0,
                    pal().accent,
                );
            }
        }
    }
    if !is_user {
        return;
    }
    let response = ui.interact(rect, ui.scope_id().with("scale_matrix"), Sense::click());
    if response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            let column = ((pos.x - rect.left()) / CELL) as usize;
            let row = CELLS - 1 - (((pos.y - rect.top()) / CELL) as usize).min(CELLS - 1);
            // The 13th column is the octave of the first, so it isn't edited separately.
            if column < 12 {
                let id = ParamId(scale::FIRST_MAP + column as u32);
                let current = panel.value(id).unwrap_or(column as f64) as i32;
                let target = if current == row as i32 {
                    REMOVED
                } else {
                    row as i32
                };
                panel.set(id, target as f64);
            }
        }
    }
}
