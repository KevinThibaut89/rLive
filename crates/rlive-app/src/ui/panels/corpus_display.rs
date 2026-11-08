//! Corpus's resonator display: an X-Y pad for Decay against Material or Radius, the partials the
//! current body rings at, the limiter LED, and the MIDI note Tune corresponds to.

use super::{PanelCtx, PanelDef};
use crate::ui::theme::{self, *};
use egui::{Align2, CornerRadius, Pos2, Rect, Sense, Stroke, Vec2};
use rlive_core::ParamId;
use rlive_devices::devices::b7::corpus;

pub const PANEL: PanelDef = PanelDef {
    name: "corpus_display",
    size: Vec2::new(280.0, 120.0),
    views: corpus::VIEW_LEN,
    draw,
};

/// The MIDI note and cents a frequency is, the way Live spells them under Tune.
fn note_and_cents(hz: f32) -> (i32, i32) {
    let note = 69.0 + 12.0 * (hz.max(1.0) / 440.0).log2();
    let rounded = note.round();
    (rounded as i32, ((note - rounded) * 100.0).round() as i32)
}

fn draw(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    let mut view = [0.0f32; corpus::VIEW_LEN];
    let live = panel.view.is_some_and(|v| v.read(corpus::VIEW, &mut view));
    let body = panel.value(ParamId(corpus::BODY)).unwrap_or(0.0).round() as usize;
    let tube = matches!(body, corpus::PIPE | corpus::TUBE);
    let y_id = ParamId(if tube {
        corpus::RADIUS
    } else {
        corpus::MATERIAL
    });

    let split = rect.left() + rect.width() * 0.5;
    let pad = Rect::from_min_max(rect.min, Pos2::new(split - 4.0, rect.bottom()));
    let response = ui.interact(
        pad,
        ui.scope_id().with(("corpus_xy", panel.device.id)),
        Sense::drag(),
    );

    if let (Some(x_info), Some(y_info)) = (panel.param(ParamId(corpus::DECAY)), panel.param(y_id)) {
        if response.dragged() {
            if let Some(p) = response.interact_pointer_pos() {
                let nx = ((p.x - pad.left()) / pad.width()).clamp(0.0, 1.0) as f64;
                let ny = (1.0 - (p.y - pad.top()) / pad.height()).clamp(0.0, 1.0) as f64;
                panel.set(ParamId(corpus::DECAY), x_info.from_normalized(nx));
                panel.set(y_id, y_info.from_normalized(ny));
            }
        }
        let p = ui.painter();
        p.rect_filled(pad, CornerRadius::same(2), pal().knob_track);
        p.rect_stroke(
            pad,
            CornerRadius::same(2),
            Stroke::new(1.0, pal().separator),
            egui::StrokeKind::Inside,
        );
        let x = x_info.to_normalized(panel.value(ParamId(corpus::DECAY)).unwrap_or(0.0)) as f32;
        let y = y_info.to_normalized(panel.value(y_id).unwrap_or(0.0)) as f32;
        p.circle_filled(
            Pos2::new(
                pad.left() + x * pad.width(),
                pad.bottom() - y * pad.height(),
            ),
            4.0,
            pal().accent,
        );
        p.text(
            pad.left_bottom() + Vec2::new(3.0, -2.0),
            Align2::LEFT_BOTTOM,
            "Decay",
            theme::font(9.0),
            pal().text_dim,
        );
        p.text(
            pad.left_top() + Vec2::new(3.0, 2.0),
            Align2::LEFT_TOP,
            if tube { "Radius" } else { "Material" },
            theme::font(9.0),
            pal().text_dim,
        );
    }

    let plot = Rect::from_min_max(
        Pos2::new(split, rect.top()),
        Pos2::new(rect.right(), rect.bottom() - 18.0),
    );
    let p = ui.painter();
    p.rect_filled(plot, CornerRadius::same(2), pal().knob_track);
    if live && !tube {
        let partials = view[2..].as_chunks::<2>().0;
        let highest = partials.iter().fold(1.0f32, |m, c| m.max(c[0]));
        let span = highest.max(2.0).log2();
        for c in partials {
            if c[0] <= 0.0 {
                continue;
            }
            let x = plot.left() + plot.width() * c[0].log2() / span;
            p.line_segment(
                [
                    Pos2::new(x, plot.bottom()),
                    Pos2::new(x, plot.bottom() - c[1] * plot.height()),
                ],
                Stroke::new(1.0, pal().accent),
            );
        }
    }
    if tube {
        p.text(
            plot.center(),
            Align2::CENTER_CENTER,
            "Waveguide",
            theme::font(10.0),
            pal().text_dim,
        );
    }

    let hz = panel.value(ParamId(corpus::TUNE)).unwrap_or(220.0) as f32;
    let (note, cents) = note_and_cents(hz);
    let text = if panel.value(ParamId(corpus::MIDI_FREQ)).unwrap_or(0.0) >= 0.5 {
        "MIDI Frequency".to_string()
    } else {
        format!("Note {note} {cents:+} ct")
    };
    p.text(
        Pos2::new(split, rect.bottom() - 8.0),
        Align2::LEFT_CENTER,
        text,
        theme::font(10.0),
        pal().text,
    );
    let limiting = live && view[0] >= 0.5;
    p.circle_filled(
        Pos2::new(rect.right() - 8.0, rect.bottom() - 8.0),
        3.5,
        if limiting {
            pal().arm_red
        } else {
            pal().knob_track
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frequency_reads_as_its_nearest_note_and_the_cents_off_it() {
        assert_eq!(note_and_cents(440.0), (69, 0));
        assert_eq!(note_and_cents(440.0 * 2f32.powf(0.25 / 12.0)), (69, 25));
        assert_eq!(note_and_cents(261.63), (60, 0));
    }
}
