//! Vocoder's filter bank: a bar per band, and clicking one attenuates it.

use super::{PanelCtx, PanelDef};
use crate::ui::theme::{self, *};
use egui::{Align2, Pos2, Rect, Sense, Vec2};
use rlive_core::ParamId;
use rlive_devices::devices::b8::vocoder as voc;

pub const PANEL: PanelDef = PanelDef {
    name: "vocoder_bands",
    size: Vec2::new(320.0, 100.0),
    views: voc::VIEW_LEN,
    draw,
};

fn draw(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    let id = ui.make_persistent_id(("vocoder_bands", panel.device.id));
    let response = ui.interact(rect, id, Sense::click_and_drag());
    let mut levels = [0.0f32; voc::VIEW_LEN];
    let live = panel.view.is_some_and(|v| v.read(voc::LEVELS, &mut levels));
    let chooser = panel.value(ParamId(voc::BANDS)).unwrap_or(4.0).round() as usize;
    let bands = voc::BAND_COUNTS[chooser.min(voc::BAND_COUNTS.len() - 1)];

    let width = rect.width() / bands as f32;
    let bar = |band: usize| {
        Rect::from_min_size(
            Pos2::new(rect.left() + band as f32 * width, rect.top()),
            Vec2::new(width, rect.height()),
        )
    };
    if response.is_pointer_button_down_on() {
        if let Some(pos) = response.interact_pointer_pos() {
            let band = ((pos.x - rect.left()) / width).floor();
            if band >= 0.0 && (band as usize) < bands {
                let level = 1.0 - ((pos.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
                panel.set(ParamId(voc::BAND_GAIN + band as u32), level as f64);
            }
        }
    }

    let p = ui.painter_at(rect);
    p.rect_filled(rect, 2.0, pal().field_bg);
    for (band, level) in levels.iter().enumerate().take(bands) {
        let area = bar(band);
        let gain = panel
            .value(ParamId(voc::BAND_GAIN + band as u32))
            .unwrap_or(1.0) as f32;
        let level = if live { level.min(1.0).powf(0.4) } else { 0.0 };
        let shown = (level * rect.height()).max(1.0);
        p.rect_filled(
            Rect::from_min_size(
                Pos2::new(area.left() + 1.0, area.bottom() - shown),
                Vec2::new((width - 2.0).max(1.0), shown),
            ),
            0.0,
            pal().accent,
        );
        // The ceiling a click set.
        let y = area.bottom() - gain * area.height();
        p.line_segment(
            [
                Pos2::new(area.left() + 1.0, y),
                Pos2::new(area.right() - 1.0, y),
            ],
            egui::Stroke::new(1.0, pal().text_dim),
        );
    }
    p.text(
        Pos2::new(rect.right() - 4.0, rect.top() + 7.0),
        Align2::RIGHT_CENTER,
        format!("{bands} bands"),
        theme::font(9.0),
        pal().text_faint,
    );
    if live {
        ui.ctx().request_repaint();
    }
}
