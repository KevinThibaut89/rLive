//! Tuner's display: Classic (Target or Strobe) and Histogram, with note spelling and Hz or cents.

use super::{PanelCtx, PanelDef};
use crate::ui::theme::{self, *};
use egui::{Align2, Pos2, Rect, Sense, Stroke, Vec2};
use rlive_core::ParamId;
use rlive_devices::devices::b8::tuner as tu;
use std::collections::VecDeque;

pub const PANEL: PanelDef = PanelDef {
    name: "tuner_display",
    size: Vec2::new(320.0, 170.0),
    views: tu::VIEW_LEN,
    draw,
};

const HISTORY: usize = 280;
/// In-tune is within this many cents.
const IN_TUNE: f32 = 3.0;

#[derive(Clone)]
struct State {
    history: VecDeque<(f32, f32)>,
    strobe: f32,
    /// The histogram's centre note and how many semitones the view spans.
    centre: f32,
    span: f32,
}

impl Default for State {
    fn default() -> Self {
        Self {
            history: VecDeque::with_capacity(HISTORY),
            strobe: 0.0,
            centre: 69.0,
            span: 12.0,
        }
    }
}

/// A note's name in the chosen spelling.
pub fn spelled(note: i32, spelling: usize) -> String {
    const SHARPS: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    const FLATS: [&str; 12] = [
        "C",
        "D\u{266d}",
        "D",
        "E\u{266d}",
        "E",
        "F",
        "G\u{266d}",
        "G",
        "A\u{266d}",
        "A",
        "B\u{266d}",
        "B",
    ];
    let pc = note.rem_euclid(12) as usize;
    let octave = note.div_euclid(12) - 2;
    match spelling {
        tu::SPELL_FLATS => format!("{}{}", FLATS[pc], octave),
        tu::SPELL_BOTH if SHARPS[pc] != FLATS[pc] => {
            format!("{}/{}{}", SHARPS[pc], FLATS[pc], octave)
        }
        _ => format!("{}{}", SHARPS[pc], octave),
    }
}

fn accuracy_colour(cents: f32) -> egui::Color32 {
    if cents.abs() <= IN_TUNE {
        pal().play_green
    } else {
        pal().arm_red
    }
}

fn draw(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    let id = ui.make_persistent_id(("tuner_display", panel.device.id));
    let response = ui.interact(rect, id, Sense::click_and_drag());
    let mut reading = [0.0f32; tu::VIEW_LEN];
    panel.view.map(|v| v.read(tu::READING, &mut reading));
    let [hz, _confidence, note, cents] = reading;
    let view = panel.value(ParamId(tu::VIEW_MODE)).unwrap_or(0.0).round() as usize;
    let unit = panel.value(ParamId(tu::UNIT)).unwrap_or(1.0).round() as usize;
    let spelling = panel.value(ParamId(tu::SPELLING)).unwrap_or(0.0).round() as usize;

    let mut state: State = ui.data_mut(|d| d.get_temp(id).unwrap_or_default());
    if hz > 0.0 {
        let exact = note + cents * 0.01;
        if state.history.len() >= HISTORY {
            state.history.pop_front();
        }
        state.history.push_back((exact, cents));
        state.strobe = (state.strobe + cents * panel.dt * 0.05).fract();
    } else if state.history.len() >= HISTORY {
        state.history.pop_front();
        state.history.push_back((f32::NAN, 0.0));
    } else {
        state.history.push_back((f32::NAN, 0.0));
    }

    let p = ui.painter_at(rect);
    p.rect_filled(rect, 2.0, shade(0x121212));
    let readout = match unit {
        tu::UNIT_HERTZ if hz > 0.0 => format!("{hz:.1} Hz"),
        tu::UNIT_CENTS if hz > 0.0 => format!("{cents:+.1} ct"),
        _ => "--".to_string(),
    };
    if view == tu::VIEW_HISTOGRAM {
        histogram(&p, rect, panel, &mut state, &response, spelling, readout);
    } else {
        classic(&p, rect, panel, &state, hz, note, cents, spelling, readout);
    }
    response.context_menu(|ui| {
        for (index, label) in [
            (tu::SPELL_SHARPS, "Sharps (C#)"),
            (tu::SPELL_FLATS, "Flats (D\u{266d})"),
            (tu::SPELL_BOTH, "Sharps and Flats (C#/D\u{266d})"),
        ] {
            if ui.selectable_label(spelling == index, label).clicked() {
                panel.set(ParamId(tu::SPELLING), index as f64);
                ui.close();
            }
        }
    });
    ui.data_mut(|d| d.insert_temp(id, state));
    if hz > 0.0 {
        ui.ctx().request_repaint();
    }
}

#[allow(clippy::too_many_arguments)]
fn classic(
    p: &egui::Painter,
    rect: Rect,
    panel: &PanelCtx,
    state: &State,
    hz: f32,
    note: f32,
    cents: f32,
    spelling: usize,
    readout: String,
) {
    let strobe = panel
        .value(ParamId(tu::CLASSIC_MODE))
        .unwrap_or(0.0)
        .round() as usize
        == tu::MODE_STROBE;
    let centre = Pos2::new(rect.center().x, rect.bottom() - 8.0);
    let radius = (rect.width() * 0.5 - 24.0).min(rect.height() - 26.0);
    let angle_at = |t: f32| -std::f32::consts::FRAC_PI_2 + t * std::f32::consts::FRAC_PI_3;
    let on_arc = |t: f32, r: f32| {
        let a = angle_at(t);
        Pos2::new(centre.x + r * a.cos(), centre.y + r * a.sin())
    };
    // The curve.
    let track: Vec<Pos2> = (0..=48)
        .map(|i| on_arc(i as f32 / 24.0 - 1.0, radius))
        .collect();
    p.add(egui::Shape::line(track, Stroke::new(2.0, pal().text_faint)));

    if strobe {
        // A band of lights, moving right when sharp and left when flat.
        let phase = state.strobe * 8.0;
        for i in 0..32 {
            let t = i as f32 / 16.0 - 1.0;
            let dot = ((i as f32 - phase).rem_euclid(4.0) < 2.0) && hz > 0.0;
            let colour = if dot {
                accuracy_colour(cents)
            } else {
                pal().knob_track
            };
            p.circle_filled(on_arc(t, radius), 4.0, colour);
        }
    } else {
        // Target: the outline is the note, the ball the signal.
        p.circle_stroke(on_arc(0.0, radius), 9.0, Stroke::new(1.5, pal().text_dim));
        if hz > 0.0 {
            let t = (cents / 50.0).clamp(-1.0, 1.0);
            p.circle_filled(on_arc(t, radius), 7.0, accuracy_colour(cents));
        }
    }

    let name = if hz > 0.0 {
        spelled(note as i32, spelling)
    } else {
        "-".to_string()
    };
    let mid = Pos2::new(rect.center().x, rect.bottom() - radius * 0.32);
    p.text(
        mid,
        Align2::CENTER_CENTER,
        name,
        theme::font_bold(30.0),
        pal().text,
    );
    // Arrows: which way to tune.
    if hz > 0.0 {
        let (low, high) = (accuracy_colour(cents), pal().knob_track);
        let (left, right) = if cents > IN_TUNE {
            (high, low)
        } else if cents < -IN_TUNE {
            (low, high)
        } else {
            (high, high)
        };
        arrow(p, mid + Vec2::new(-60.0, 0.0), -1.0, left);
        arrow(p, mid + Vec2::new(60.0, 0.0), 1.0, right);
    }
    p.text(
        Pos2::new(rect.right() - 6.0, rect.top() + 8.0),
        Align2::RIGHT_CENTER,
        readout,
        theme::font(11.0),
        pal().text_dim,
    );
}

fn arrow(p: &egui::Painter, tip: Pos2, direction: f32, colour: egui::Color32) {
    let (a, b) = (
        tip + Vec2::new(-8.0 * direction, -6.0),
        tip + Vec2::new(-8.0 * direction, 6.0),
    );
    p.line_segment([a, tip], Stroke::new(2.0, colour));
    p.line_segment([b, tip], Stroke::new(2.0, colour));
}

fn histogram(
    p: &egui::Painter,
    rect: Rect,
    panel: &mut PanelCtx,
    state: &mut State,
    response: &egui::Response,
    spelling: usize,
    readout: String,
) {
    let auto = panel.value(ParamId(tu::AUTO)).unwrap_or(1.0) >= 0.5;
    if auto {
        if let Some((exact, _)) = state.history.iter().rev().find(|(n, _)| n.is_finite()) {
            let wanted = exact.round();
            state.centre += (wanted - state.centre) * (panel.dt * 8.0).min(1.0);
        }
    }
    if response.dragged() {
        let delta = response.drag_delta();
        state.centre += delta.y / rect.height() * state.span;
        state.span = (state.span * (1.0 - delta.x * 0.01)).clamp(2.0, 48.0);
    }
    let (top_note, bottom_note) = (
        state.centre + state.span * 0.5,
        state.centre - state.span * 0.5,
    );
    let y = |note: f32| rect.top() + (top_note - note) / state.span * rect.height();
    let labels = 42.0;
    let plot_right = rect.right() - labels;
    let line = Stroke::new(1.0, pal().text_faint);
    for note in bottom_note.ceil() as i32..=top_note.floor() as i32 {
        let ny = y(note as f32);
        p.line_segment(
            [Pos2::new(rect.left(), ny), Pos2::new(plot_right, ny)],
            line,
        );
        p.text(
            Pos2::new(plot_right + 4.0, ny),
            Align2::LEFT_CENTER,
            spelled(note, spelling),
            theme::font(9.0),
            pal().text_dim,
        );
    }
    let step = (plot_right - rect.left()) / HISTORY as f32;
    let values: Vec<(f32, f32)> = state.history.iter().copied().collect();
    let start = rect.left() + (HISTORY - values.len()) as f32 * step;
    for (i, w) in values.windows(2).enumerate() {
        let ((a, _), (b, cents)) = (w[0], w[1]);
        if a.is_finite() && b.is_finite() {
            let x = start + i as f32 * step;
            p.line_segment(
                [Pos2::new(x, y(a)), Pos2::new(x + step, y(b))],
                Stroke::new(2.0, accuracy_colour(cents)),
            );
        }
    }
    p.text(
        Pos2::new(rect.left() + 6.0, rect.top() + 8.0),
        Align2::LEFT_CENTER,
        readout,
        theme::font(11.0),
        pal().text_dim,
    );
}
