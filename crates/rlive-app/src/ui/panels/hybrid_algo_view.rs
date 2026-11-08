//! Hybrid Reverb's algorithm controls: the parameters of the algorithm in use, greyed out when the
//! routing leaves it out, the way the device's own panel changes in the manual.

use super::{PanelCtx, PanelDef};
use crate::ui::theme::{self, *};
use egui::{Align2, Pos2, Vec2};
use rlive_core::ParamId;
use rlive_devices::devices::b7::hybrid_reverb as hr;

pub const PANEL: PanelDef = PanelDef {
    name: "hybrid_algo_view",
    size: Vec2::new(260.0, 138.0),
    views: 0,
    draw,
};

/// The parameters unique to `algorithm`, in the order of the manual.
fn controls(algorithm: usize) -> &'static [u32] {
    match algorithm {
        hr::QUARTZ => &[
            hr::DAMPING,
            hr::LO_DAMP,
            hr::MODULATION,
            hr::DIFFUSION,
            hr::DISTANCE,
        ],
        hr::SHIMMER_ALGO => &[
            hr::DAMPING,
            hr::MODULATION,
            hr::PITCH,
            hr::DIFFUSION,
            hr::SHIMMER,
        ],
        hr::TIDES => &[hr::DAMPING, hr::WAVE, hr::TIDE, hr::PHASE, hr::RATE],
        hr::PRISM => &[hr::LOW_MULT, hr::HIGH_MULT, hr::XOVER],
        _ => &[
            hr::DAMPING,
            hr::MODULATION,
            hr::SHAPE,
            hr::BASS_X,
            hr::BASS_MULT,
        ],
    }
}

fn draw(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    let algorithm = panel.value(ParamId(hr::ALGORITHM)).unwrap_or(0.0).round() as usize;
    let routing = panel.value(ParamId(hr::ROUTING)).unwrap_or(0.0).round() as usize;
    let unused = routing == hr::ROUTE_CONVOLUTION;
    let mut row = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    row.add_enabled_ui(!unused, |ui| {
        for &id in controls(algorithm) {
            let Some(info) = panel.param(ParamId(id)) else {
                continue;
            };
            let mut value = panel.value(ParamId(id)).unwrap_or(info.default);
            let before = value;
            ui.horizontal(|ui| {
                ui.add(
                    egui::Slider::new(&mut value, info.min..=info.max)
                        .text(info.name.clone())
                        .custom_formatter(|v, _| info.format(v)),
                );
            });
            if value != before {
                panel.set(ParamId(id), value);
            }
        }
    });
    if unused {
        ui.painter().text(
            Pos2::new(rect.left(), rect.bottom()),
            Align2::LEFT_BOTTOM,
            "Algorithm engine not in use",
            theme::font(9.0),
            pal().text_dim,
        );
    }
}
