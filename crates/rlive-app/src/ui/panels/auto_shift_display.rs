//! Auto Shift's display: input pitch, the correction meter or the MIDI piano, and the range LEDs.

use super::{PanelCtx, PanelDef};
use crate::ui::theme::{self, *};
use egui::{Align2, Pos2, Rect, Sense, Vec2};
use rlive_core::{note_name, ParamId};
use rlive_devices::devices::b8::auto_shift as asft;

pub const PANEL: PanelDef = PanelDef {
    name: "auto_shift_display",
    size: Vec2::new(360.0, 170.0),
    views: asft::VIEW_LEN,
    draw,
};

const BLACK_KEYS: [bool; 12] = [
    false, true, false, true, false, false, true, false, true, false, true, false,
];

fn draw(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    let mut display = [0.0f32; asft::VIEW_LEN];
    let live = panel
        .view
        .is_some_and(|v| v.read(asft::DISPLAY, &mut display));
    let [hz, note, cents, correction, target, led_high, led_mid, led_bass, onset, latency] = [
        display[0], display[1], display[2], display[3], display[4], display[5], display[6],
        display[7], display[8], display[9],
    ];
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 2.0, pal().field_bg);

    // Input section: pitch, range LEDs and the latency readout.
    let input = if hz > 0.0 {
        format!(
            "{}  {:+.0} ct",
            note_name(note.clamp(0.0, 127.0) as u8),
            cents
        )
    } else {
        "--".to_string()
    };
    p.text(
        rect.min + Vec2::new(6.0, 11.0),
        Align2::LEFT_CENTER,
        input,
        theme::font_bold(14.0),
        pal().text,
    );
    let range = panel
        .value(ParamId(asft::PITCH_RANGE))
        .unwrap_or(1.0)
        .round() as usize;
    for (i, (label, led, selected)) in [
        ("High", led_high, range == asft::RANGE_HIGH),
        ("Mid", led_mid, range == asft::RANGE_MID),
        ("Bass", led_bass, range == asft::RANGE_BASS),
    ]
    .iter()
    .enumerate()
    {
        let x = rect.left() + 6.0 + i as f32 * 46.0;
        let y = rect.top() + 28.0;
        let colour = if *led > 0.0 {
            pal().play_green
        } else {
            pal().knob_track
        };
        p.circle_filled(Pos2::new(x + 4.0, y), 3.0, colour);
        p.text(
            Pos2::new(x + 10.0, y),
            Align2::LEFT_CENTER,
            *label,
            theme::font(10.0),
            if *selected {
                pal().text
            } else {
                pal().text_faint
            },
        );
    }
    p.text(
        Pos2::new(rect.right() - 6.0, rect.top() + 11.0),
        Align2::RIGHT_CENTER,
        format!("{latency:.1} ms"),
        theme::font(10.0),
        pal().text_dim,
    );
    p.circle_filled(
        Pos2::new(rect.right() - 8.0, rect.top() + 28.0),
        3.0,
        if onset > 0.0 {
            pal().play_green
        } else {
            pal().knob_track
        },
    );

    let body = Rect::from_min_max(
        Pos2::new(rect.left() + 4.0, rect.top() + 42.0),
        Pos2::new(rect.right() - 4.0, rect.bottom() - 4.0),
    );
    if panel.value(ParamId(asft::MIDI_ON)).unwrap_or(0.0) >= 0.5 {
        piano(ui, body, &display[asft::DISPLAY_LEN..]);
    } else {
        correction_meter(ui, panel, body, correction, target, live, display[10]);
    }
    if live {
        ui.ctx().request_repaint();
    }
}

fn correction_meter(
    ui: &mut egui::Ui,
    panel: &mut PanelCtx,
    body: Rect,
    correction: f32,
    target: f32,
    live: bool,
    published: f32,
) {
    let p = ui.painter_at(body);
    let meter = Rect::from_min_size(body.min, Vec2::new(body.width(), 14.0));
    p.rect_filled(meter, 1.0, pal().knob_track);
    let centre = meter.center().x;
    let t = (correction / 100.0).clamp(-1.0, 1.0);
    let edge = centre + t * meter.width() * 0.5;
    p.rect_filled(
        Rect::from_min_max(
            Pos2::new(centre.min(edge), meter.top()),
            Pos2::new(centre.max(edge), meter.bottom()),
        ),
        0.0,
        pal().accent,
    );
    if live {
        p.text(
            meter.center_bottom() + Vec2::new(0.0, 9.0),
            Align2::CENTER_CENTER,
            format!(
                "{} {:+.0} ct",
                note_name(target.round().clamp(0.0, 127.0) as u8),
                correction
            ),
            theme::font(10.0),
            pal().text_dim,
        );
    }

    // Included Notes in Scale.
    let in_use_current = panel.value(ParamId(asft::USE_SCALE)).unwrap_or(0.0) >= 0.5;
    // Scale awareness shows what's really in use; otherwise it's the device's own notes.
    let scale = if in_use_current {
        published as u16
    } else {
        current_mask(panel)
    };
    let top = body.top() + 34.0;
    let width = body.width() / 12.0;
    for (pc, is_black) in BLACK_KEYS.iter().enumerate() {
        let cell = Rect::from_min_size(
            Pos2::new(body.left() + pc as f32 * width, top),
            Vec2::new(width - 1.0, body.bottom() - top),
        );
        let on = scale & (1 << pc) != 0;
        let colour = match (on, in_use_current) {
            (true, true) => shade(0x9b6bd8),
            (true, false) => pal().accent,
            _ if *is_black => pal().bar_bg,
            _ => pal().raised,
        };
        p.rect_filled(cell, 1.0, colour);
        let id = ui.scope_id().with(("auto_shift_note", panel.device.id, pc));
        let response = ui.interact(cell, id, Sense::click());
        if response.clicked() && !in_use_current {
            let mask = scale ^ (1 << pc);
            panel.set(ParamId(asft::CUSTOM_MASK), mask as f64);
            panel.set(ParamId(asft::SCALE), custom_scale_index(panel) as f64);
        }
    }
}

fn custom_scale_index(panel: &PanelCtx) -> usize {
    panel
        .param(ParamId(asft::SCALE))
        .map_or(0, |p| p.range().max as usize)
}

/// The notes the Quantizer is using, as a mask from C.
fn current_mask(panel: &PanelCtx) -> u16 {
    let root = panel.value(ParamId(asft::ROOT)).unwrap_or(0.0).round() as usize;
    let scale = panel.value(ParamId(asft::SCALE)).unwrap_or(0.0).round() as usize;
    let custom = panel
        .value(ParamId(asft::CUSTOM_MASK))
        .unwrap_or(4095.0)
        .round() as u16;
    let info = asft::quantizer_scale(root, scale, custom, false, None);
    let mut out = 0u16;
    for n in 0..12 {
        if info.mask & (1 << n) != 0 {
            out |= 1 << ((n + info.root as usize) % 12);
        }
    }
    out
}

fn piano(ui: &mut egui::Ui, body: Rect, notes: &[f32]) {
    let (first, last) = (36usize, 96usize);
    let width = body.width() / (last - first + 1) as f32;
    let p = ui.painter_at(body);
    for key in first..=last {
        let black = BLACK_KEYS[key % 12];
        let x = body.left() + (key - first) as f32 * width;
        let top = if black {
            body.top()
        } else {
            body.top() + body.height() * 0.45
        };
        let r = Rect::from_min_max(Pos2::new(x, top), Pos2::new(x + width - 1.0, body.bottom()));
        p.rect_filled(r, 0.0, if black { pal().bar_bg } else { pal().raised });
        if notes.get(key).copied().unwrap_or(0.0) > 0.0 {
            p.rect_filled(r, 0.0, pal().accent);
        }
    }
}
