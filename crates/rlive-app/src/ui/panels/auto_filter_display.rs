//! Auto Filter's display: the filter's curve, the modulated curves of each channel, and the
//! output spectrum. Drag the handle to set Freq and Res, or Pitch and Formant for the Vowel filter.

use super::{plot, PanelCtx, PanelDef};
use crate::ui::theme::*;
use egui::{Pos2, Rect, Sense, Stroke, Vec2};
use rlive_core::{ParamId, SCOPE_VALUES};
use rlive_devices::devices::b4::auto_filter as af;

pub const PANEL: PanelDef = PanelDef {
    name: "auto_filter_display",
    size: Vec2::new(230.0, 120.0),
    views: af::VIEW_LEN,
    draw,
};

const RANGE_DB: f32 = 30.0;

fn cutoff_of(shape: &af::FilterShape, freq: f32, hz: f32, sr: f32) -> f32 {
    20.0 * shape.magnitude(freq, hz, sr).max(1e-5).log10()
}

/// The handle's position for a control pair, each in `0..=1`.
fn handle(rect: Rect, x: f32, y: f32) -> Pos2 {
    Pos2::new(
        rect.left() + x * rect.width(),
        rect.bottom() - y * rect.height(),
    )
}

fn draw(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    let response = ui.interact(
        rect,
        ui.scope_id().with(("auto_filter_display", panel.device.id)),
        Sense::click_and_drag(),
    );
    // A copy, so changes can be queued while it's read.
    let values: [f64; af::PARAM_COUNT] =
        std::array::from_fn(|id| panel.value(ParamId(id as u32)).unwrap_or(0.0));
    let value = |id: u32| values[id as usize];
    let shape = af::FilterShape::read(value);
    let vowel = shape.filter_type == af::T_VOWEL;

    let mut spectrum = [0.0f32; SCOPE_VALUES];
    let mut cutoffs = [0.0f32; 2];
    let mut sample_rate = [48_000.0f32];
    let read = panel.view.is_some_and(|v| {
        v.read(af::SPECTRUM, &mut spectrum)
            && v.read(af::CUTOFFS, &mut cutoffs)
            && v.read(af::SAMPLE_RATE, &mut sample_rate)
    });
    let sr = if sample_rate[0] > 0.0 {
        sample_rate[0]
    } else {
        48_000.0
    };

    if response.dragged() {
        if let Some(pos) = response.interact_pointer_pos() {
            if vowel {
                let pitch = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64;
                let formant = ((rect.bottom() - pos.y) / rect.height()).clamp(0.0, 1.0) as f64;
                panel.set(ParamId(af::PITCH), pitch * 48.0 - 24.0);
                panel.set(ParamId(af::FORMANT), formant * 100.0);
            } else {
                let res = ((rect.bottom() - pos.y) / rect.height()).clamp(0.0, 1.0) as f64;
                panel.set(ParamId(af::FREQ), plot::x_to_hz(rect, pos.x) as f64);
                panel.set(ParamId(af::RES), res * 100.0);
            }
        }
    }

    let freq = value(af::FREQ) as f32;
    let p = ui.painter();
    plot::background(p, rect, RANGE_DB);
    if read {
        plot::spectrum(p, rect, &spectrum, sr);
        for (cutoff, colour) in cutoffs.iter().zip([pal().play_green, pal().solo_blue]) {
            plot::curve(p, rect, RANGE_DB, Stroke::new(1.0, colour), |hz| {
                cutoff_of(&shape, *cutoff, hz, sr)
            });
        }
    }
    plot::curve(p, rect, RANGE_DB, Stroke::new(1.5, pal().accent), |hz| {
        cutoff_of(&shape, freq, hz, sr)
    });
    let at = if vowel {
        handle(
            rect,
            ((value(af::PITCH) + 24.0) / 48.0) as f32,
            (value(af::FORMANT) * 0.01) as f32,
        )
    } else {
        handle(
            rect,
            (plot::hz_to_x(rect, freq) - rect.left()) / rect.width(),
            (value(af::RES) * 0.01) as f32,
        )
    };
    p.circle_filled(at, 4.0, pal().accent);
}
