//! Channel EQ's display: the filter curve over the output spectrum.

use super::{plot, PanelCtx, PanelDef};
use crate::ui::theme::*;
use egui::{Stroke, Vec2};
use rlive_core::{ParamId, SCOPE_VALUES};
use rlive_devices::devices::b4::channel_eq as ceq;

pub const PANEL: PanelDef = PanelDef {
    name: "channel_eq_display",
    size: Vec2::new(200.0, 100.0),
    views: ceq::VIEW_LEN,
    draw,
};

const RANGE_DB: f32 = 18.0;

fn draw(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    let get = |id: u32, default: f64| panel.value(ParamId(id)).unwrap_or(default) as f32;
    let hp = panel.value(ParamId(ceq::HP_ON)).is_some_and(|v| v >= 0.5);
    let (low, mid, freq, high) = (
        get(ceq::LOW, 0.0),
        get(ceq::MID, 0.0),
        get(ceq::MID_FREQ, 1_000.0),
        get(ceq::HIGH, 0.0),
    );
    let mut spectrum = [0.0f32; SCOPE_VALUES];
    let mut sample_rate = [48_000.0f32];
    let read = panel.view.is_some_and(|v| {
        v.read(ceq::SPECTRUM, &mut spectrum) && v.read(ceq::SAMPLE_RATE, &mut sample_rate)
    });
    let sr = if sample_rate[0] > 0.0 {
        sample_rate[0]
    } else {
        48_000.0
    };
    let p = ui.painter();
    plot::background(p, rect, RANGE_DB);
    if read {
        plot::spectrum(p, rect, &spectrum, sr);
    }
    plot::curve(p, rect, RANGE_DB, Stroke::new(1.5, pal().accent), |hz| {
        ceq::response_db(hp, low, mid, freq, high, hz, sr)
    });
}
