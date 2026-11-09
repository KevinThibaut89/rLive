//! Drift's displays: the combined waveform and the low-pass and high-pass curve.

use super::{plot, PanelCtx, PanelDef};
use crate::ui::theme::*;
use egui::{CornerRadius, Pos2, Rect, Shape, Stroke, Vec2};
use rlive_core::ParamId;
use rlive_devices::devices::b11::drift as dr;
use rlive_devices::dsp::filter::{Circuit, FilterKind, FilterSettings, FilterSlope};

pub const PANEL: PanelDef = PanelDef {
    name: "drift_display",
    size: Vec2::new(380.0, 96.0),
    views: 0,
    draw,
};

/// A few cycles' worth of the mix of what's switched on, at a low frequency.
fn waveform(panel: &PanelCtx) -> Vec<f32> {
    let v = |id: u32| panel.value(ParamId(id)).unwrap_or(0.0) as f32;
    let wave1 = v(dr::OSC1_WAVE).round() as u8;
    let wave2 = v(dr::OSC2_WAVE).round() as u8;
    let shape = v(dr::OSC1_SHAPE) * 0.01;
    let db = |d: f32| 10f32.powf(d / 20.0);
    let mut noise = 0.31f32;
    (0..180)
        .map(|i| {
            let phase = (i as f32 / 60.0).fract();
            let mut y = 0.0;
            if v(dr::OSC1_ON) >= 0.5 {
                y += dr::osc_sample(wave1, phase, 1e-4, shape) * db(v(dr::OSC1_GAIN));
            }
            if v(dr::OSC2_ON) >= 0.5 {
                let p2 = (phase * 2f32.powf(v(dr::OSC2_OCT) + v(dr::OSC2_DETUNE) / 12.0)).fract();
                y += dr::osc2_sample(wave2, p2, 1e-4) * db(v(dr::OSC2_GAIN));
            }
            if v(dr::NOISE_ON) >= 0.5 {
                // A fixed texture: the display shouldn't flicker frame to frame.
                noise = (noise * 3.9 * (1.0 - noise)).clamp(0.0, 1.0);
                y += (noise * 2.0 - 1.0) * db(v(dr::NOISE_GAIN));
            }
            y
        })
        .collect()
}

fn draw(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    let mid = rect.left() + rect.width() * 0.5;
    let left = Rect::from_min_max(rect.min, Pos2::new(mid - 3.0, rect.bottom()));
    let right = Rect::from_min_max(Pos2::new(mid + 3.0, rect.top()), rect.max);

    let p = ui.painter();
    p.rect_filled(left, CornerRadius::same(2), shade(0x121212));
    let wave = waveform(panel);
    let peak = wave.iter().fold(0.5f32, |m, s| m.max(s.abs()));
    let points: Vec<Pos2> = wave
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let x = left.left() + i as f32 / (wave.len() - 1) as f32 * left.width();
            Pos2::new(x, left.center().y - s / peak * left.height() * 0.45)
        })
        .collect();
    p.add(Shape::line(points, Stroke::new(1.3, pal().accent)));

    const RANGE_DB: f32 = 24.0;
    let v = |id: u32| panel.value(ParamId(id)).unwrap_or(0.0) as f32;
    let lp_type = v(dr::LP_TYPE).round() as usize;
    let lp = FilterSettings {
        circuit: if lp_type == 0 {
            Circuit::Dfm
        } else {
            Circuit::Ms2
        },
        slope: if lp_type == 0 {
            FilterSlope::Db12
        } else {
            FilterSlope::Db24
        },
        cutoff: v(dr::LP_FREQ),
        resonance: v(dr::LP_RES),
        ..Default::default()
    };
    let hp = FilterSettings {
        kind: FilterKind::HighPass,
        cutoff: v(dr::HP_FREQ),
        ..Default::default()
    };
    plot::background(p, right, RANGE_DB);
    plot::curve(p, right, RANGE_DB, Stroke::new(1.4, pal().accent), |hz| {
        20.0 * (lp.magnitude(hz, 48_000.0) * hp.magnitude(hz, 48_000.0))
            .max(1e-4)
            .log10()
    });
}
