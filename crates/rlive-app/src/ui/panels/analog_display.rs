//! Analog's display: the Quick Routing buttons, both filters' curves and the MPE activity LEDs.

use super::{plot, PanelCtx, PanelDef};
use crate::ui::theme::{self, *};
use egui::{Align2, Pos2, Rect, Stroke, Vec2};
use rlive_core::ParamId;
use rlive_devices::devices::b11::analog as an;
use rlive_devices::dsp::filter::{FilterKind, FilterSettings, FilterSlope};

pub const PANEL: PanelDef = PanelDef {
    name: "analog_display",
    size: Vec2::new(360.0, 112.0),
    views: an::VIEW_LEN,
    draw,
};

const LABELS: [&str; 4] = ["Parallel", "Split", "F1 Only", "Serial"];

fn curve_settings(panel: &PanelCtx, base: u32) -> FilterSettings {
    let value = |id: u32| panel.value(ParamId(base + id)).unwrap_or(0.0) as f32;
    let index = value(an::F_TYPE).round() as usize;
    let (kind, slope) = match index {
        0 => (FilterKind::LowPass, FilterSlope::Db12),
        1 => (FilterKind::LowPass, FilterSlope::Db24),
        2 => (FilterKind::BandPass, FilterSlope::Db12),
        3 => (FilterKind::BandPass, FilterSlope::Db24),
        4 => (FilterKind::Notch, FilterSlope::Db12),
        5 => (FilterKind::Notch, FilterSlope::Db24),
        6 => (FilterKind::HighPass, FilterSlope::Db12),
        7 => (FilterKind::HighPass, FilterSlope::Db24),
        _ => (FilterKind::BandPass, FilterSlope::Db12),
    };
    FilterSettings {
        kind,
        slope,
        cutoff: value(an::F_FREQ),
        resonance: value(an::F_RES),
        ..Default::default()
    }
}

fn draw(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    let buttons = Rect::from_min_size(rect.min, Vec2::new(70.0, rect.height()));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(buttons));
    child.vertical(|ui| {
        for (i, label) in LABELS.iter().enumerate() {
            if ui.button(*label).clicked() {
                for (id, value) in an::quick_routing(i) {
                    panel.set(ParamId(id), value);
                }
            }
        }
    });

    let plot_rect = Rect::from_min_max(
        Pos2::new(buttons.right() + 8.0, rect.top()),
        Pos2::new(rect.right() - 16.0, rect.bottom()),
    );
    let p = ui.painter();
    const RANGE_DB: f32 = 30.0;
    plot::background(p, plot_rect, RANGE_DB);
    let colors = [pal().accent, pal().text_dim];
    for (i, base) in [an::FILTER1, an::FILTER2].into_iter().enumerate() {
        if panel.value(ParamId(base + an::F_ON)).unwrap_or(0.0) < 0.5 {
            continue;
        }
        let settings = curve_settings(panel, base);
        plot::curve(p, plot_rect, RANGE_DB, Stroke::new(1.4, colors[i]), |hz| {
            20.0 * settings.magnitude(hz, 48_000.0).max(1e-4).log10()
        });
    }

    let mut view = [0.0f32; an::VIEW_LEN];
    let live = panel.view.is_some_and(|v| v.read(an::VIEW, &mut view));
    let led_x = rect.right() - 7.0;
    for (i, label) in ["P", "S"].iter().enumerate() {
        let y = rect.top() + 10.0 + i as f32 * 22.0;
        let on = live && view[i] > 0.05;
        p.circle_filled(
            Pos2::new(led_x, y),
            3.5,
            if on { pal().accent } else { pal().knob_track },
        );
        p.text(
            Pos2::new(led_x, y + 9.0),
            Align2::CENTER_CENTER,
            *label,
            theme::font(8.0),
            pal().text_dim,
        );
    }
}
