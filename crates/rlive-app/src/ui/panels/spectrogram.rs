//! The spectrogram Spectral Resonator and Spectral Time share: dry in yellow, wet in blue.

use super::{PanelCtx, PanelDef};
use crate::ui::theme::{self, *};
use egui::epaint::Mesh;
use egui::{Align2, Pos2, Rect, Vec2};
use rlive_core::{ParamId, ViewId, SCOPE_VALUES};
use rlive_devices::devices::b8::{
    common::SPECTROGRAM_VIEW_LEN, spectral_resonator as res, spectral_time as st,
};

pub const TIME: PanelDef = PanelDef {
    name: "spectrogram_time",
    size: Vec2::new(360.0, 140.0),
    views: st::VIEW_LEN,
    draw: draw_time,
};

pub const RESONATOR: PanelDef = PanelDef {
    name: "spectrogram_resonator",
    size: Vec2::new(360.0, 140.0),
    views: res::VIEW_LEN,
    draw: draw_resonator,
};

const COLUMNS: usize = 96;
const ROWS: usize = 48;

/// What a spectrogram keeps: a scrolling picture of past columns, each `2 * ROWS` magnitudes.
#[derive(Clone)]
struct History {
    columns: Vec<f32>,
    /// The next column to write.
    head: usize,
}

impl Default for History {
    fn default() -> Self {
        Self {
            columns: vec![0.0; COLUMNS * ROWS * 2],
            head: 0,
        }
    }
}

fn draw_time(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let show = panel.value(ParamId(st::SPECTROGRAM)).unwrap_or(1.0) >= 0.5;
    picture(ui, panel, st::SPECTROGRAM_VIEW, show, None);
}

fn draw_resonator(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let show = panel.value(ParamId(res::SPECTROGRAM)).unwrap_or(1.0) >= 0.5;
    let mut led = [0.0f32];
    panel.view.map(|v| v.read(res::LIMITER_LED, &mut led));
    picture(ui, panel, res::SPECTROGRAM_VIEW, show, Some(led[0] > 0.0));
}

fn picture(
    ui: &mut egui::Ui,
    panel: &mut PanelCtx,
    view: ViewId,
    show: bool,
    limiter: Option<bool>,
) {
    let rect = ui.max_rect();
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 2.0, shade(0x121212));
    if !show {
        p.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "Spectrogram hidden",
            theme::font(11.0),
            pal().text_faint,
        );
        return;
    }
    let mut data = vec![0.0f32; SPECTROGRAM_VIEW_LEN];
    let live = panel.view.is_some_and(|v| v.read(view, &mut data));
    let id = ui.make_persistent_id(("spectrogram", panel.device.id, view.0));
    let mut history: History = ui.data_mut(|d| d.get_temp(id).unwrap_or_default());
    if live {
        let head = history.head;
        for row in 0..ROWS {
            let band = (row as f32 / ROWS as f32 * SCOPE_VALUES as f32) as usize;
            let next = (band + 1).min(SCOPE_VALUES - 1);
            history.columns[(head * 2 * ROWS) + 2 * row] = data[band].max(data[next]);
            history.columns[(head * 2 * ROWS) + 2 * row + 1] =
                data[SCOPE_VALUES + band].max(data[SCOPE_VALUES + next]);
        }
        history.head = (head + 1) % COLUMNS;
    }
    let (dry_colour, wet_colour) = (shade(0xe8c34a), shade(0x4aa0e8));
    let cell = Vec2::new(rect.width() / COLUMNS as f32, rect.height() / ROWS as f32);
    let mut mesh = Mesh::default();
    for x in 0..COLUMNS {
        let column = (history.head + x) % COLUMNS;
        for row in 0..ROWS {
            let dry = history.columns[column * 2 * ROWS + 2 * row];
            let wet = history.columns[column * 2 * ROWS + 2 * row + 1];
            if dry <= 0.02 && wet <= 0.02 {
                continue;
            }
            let colour = blend(dry_colour, dry, wet_colour, wet);
            let at =
                rect.left_bottom() + Vec2::new(x as f32 * cell.x, -(row as f32 + 1.0) * cell.y);
            mesh.add_colored_rect(Rect::from_min_size(at, cell), colour);
        }
    }
    if !mesh.is_empty() {
        p.add(mesh);
    }
    if let Some(on) = limiter {
        let colour = if on { pal().arm_red } else { pal().knob_track };
        p.circle_filled(Pos2::new(rect.right() - 8.0, rect.top() + 8.0), 3.0, colour);
    }
    ui.data_mut(|d| d.insert_temp(id, history));
    if live {
        ui.ctx().request_repaint();
    }
}

fn blend(a: egui::Color32, amount_a: f32, b: egui::Color32, amount_b: f32) -> egui::Color32 {
    let (amount_a, amount_b) = (amount_a.clamp(0.0, 1.0), amount_b.clamp(0.0, 1.0));
    let channel = |x: u8, y: u8| ((x as f32 * amount_a).max(y as f32 * amount_b)) as u8;
    egui::Color32::from_rgb(
        channel(a.r(), b.r()),
        channel(a.g(), b.g()),
        channel(a.b(), b.b()),
    )
}
