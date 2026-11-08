//! MIDI Monitor's display: a keyboard, a flowing event list or the MPE values of each voice.

use super::{PanelCtx, PanelDef};
use crate::ui::theme::*;
use egui::{Align2, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use rlive_core::{note_name, ParamId};
use rlive_devices::devices::b1::midi_monitor::{
    chord_name, LogKind, DISPLAY, LOG, LOG_COUNT, LOG_ENTRIES, LOG_ENTRY_VALUES, MPE, MPE_VALUES,
    MPE_VOICES, NOTES, NOTE_COUNT, VIEW_LEN,
};

pub const PANEL: PanelDef = PanelDef {
    name: "midi_monitor",
    size: Vec2::new(260.0, 130.0),
    views: VIEW_LEN,
    draw,
};

/// What a monitor keeps between frames.
#[derive(Clone, Default)]
struct State {
    /// How much of the log counts as cleared.
    cleared: f32,
}

fn draw(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    {
        let p = ui.painter();
        p.rect_filled(rect, 1.0, pal().field_bg);
        p.rect_stroke(
            rect,
            1.0,
            Stroke::new(1.0, pal().separator),
            StrokeKind::Inside,
        );
    }
    let display = panel.value(ParamId(DISPLAY)).unwrap_or(0.0) as i32;
    match display {
        1 => flow(ui, panel, rect),
        2 => mpe(ui, panel, rect),
        _ => keyboard(ui, panel, rect),
    }
}

fn keyboard(ui: &mut egui::Ui, panel: &mut PanelCtx, rect: Rect) {
    let mut notes = [0.0f32; NOTE_COUNT];
    if !panel.view.is_some_and(|v| v.read(NOTES, &mut notes)) {
        return;
    }
    let (first, last) = (21usize, 108usize);
    let width = rect.width() / (last - first + 1) as f32;
    let p = ui.painter();
    let mut held = Vec::new();
    for (key, velocity) in notes.iter().copied().enumerate().take(last + 1).skip(first) {
        let black = matches!(key % 12, 1 | 3 | 6 | 8 | 10);
        let x = rect.left() + (key - first) as f32 * width;
        let top = if black {
            rect.top() + 4.0
        } else {
            rect.top() + 26.0
        };
        let r = Rect::from_min_max(
            Pos2::new(x, top),
            Pos2::new(x + width - 1.0, rect.bottom() - 18.0),
        );
        p.rect_filled(r, 0.0, if black { pal().bar_bg } else { pal().raised });
        if velocity > 0.0 {
            held.push(key as u8);
            p.rect_filled(r, 0.0, pal().accent.gamma_multiply(0.35 + velocity * 0.65));
        }
    }
    let text = if held.is_empty() {
        String::new()
    } else if let Some(name) = chord_name(&held) {
        format!(
            "Root: {}   {}",
            note_name(*held.iter().min().unwrap()),
            name
        )
    } else {
        format!("Root: {}", note_name(*held.iter().min().unwrap()))
    };
    p.text(
        Pos2::new(rect.center().x, rect.bottom() - 8.0),
        Align2::CENTER_CENTER,
        text,
        font(10.0),
        pal().text_dim,
    );
}

fn describe(kind: LogKind, channel: f32, a: f32, b: f32) -> String {
    let ch = channel as i32 + 1;
    match kind {
        LogKind::NoteOn => format!("{:3}  On   {}  v{:.0}", ch, note_name(a as u8), b * 127.0),
        LogKind::NoteOff => format!("{:3}  Off  {}  v{:.0}", ch, note_name(a as u8), b * 127.0),
        LogKind::Control => format!("{:3}  CC {:.0} = {:.0}", ch, a, b),
        LogKind::Bend => format!("{:3}  Bend {:+.2}", ch, b),
        LogKind::Pressure => format!("{:3}  Pressure {:.2}", ch, b),
        LogKind::Program => format!("{:3}  Program {:.0}", ch, a),
        LogKind::Expr => {
            let which = ["Pitch", "Slide", "Press"][(a as usize).min(2)];
            format!("{:3}  MPE {} {:+.2}", ch, which, b)
        }
    }
}

fn flow(ui: &mut egui::Ui, panel: &mut PanelCtx, rect: Rect) {
    let mut count = [0.0f32; 1];
    let mut log = [0.0f32; LOG_ENTRIES * LOG_ENTRY_VALUES];
    let Some(view) = panel.view else {
        return;
    };
    if !(view.read(LOG_COUNT, &mut count) && view.read(LOG, &mut log)) {
        return;
    }
    let state_id = ui.scope_id().with(("midi_monitor", panel.device.id));
    let mut state: State = ui
        .ctx()
        .data_mut(|d| d.get_temp(state_id).unwrap_or_default());

    let clear_rect = Rect::from_min_size(
        Pos2::new(rect.right() - 44.0, rect.top() + 2.0),
        Vec2::new(42.0, 14.0),
    );
    let clear = ui.interact(clear_rect, state_id.with("clear"), Sense::click());
    if clear.clicked() {
        state.cleared = count[0];
    }
    ui.ctx()
        .data_mut(|d| d.insert_temp(state_id, state.clone()));

    let p = ui.painter();
    p.rect_filled(clear_rect, 1.0, pal().raised);
    p.text(
        clear_rect.center(),
        Align2::CENTER_CENTER,
        "Clear",
        font(9.0),
        pal().text,
    );

    let total = count[0] as i64 - state.cleared as i64;
    let lines = ((rect.height() - 4.0) / 12.0) as i64;
    let shown = total.clamp(0, lines);
    for i in 0..shown {
        let index = (count[0] as i64 - 1 - i) as usize % LOG_ENTRIES * LOG_ENTRY_VALUES;
        let Some(kind) = LogKind::from_value(log[index]) else {
            continue;
        };
        p.text(
            Pos2::new(rect.left() + 4.0, rect.top() + 3.0 + 12.0 * i as f32),
            Align2::LEFT_TOP,
            describe(kind, log[index + 1], log[index + 2], log[index + 3]),
            font(9.0),
            pal().text,
        );
    }
}

fn mpe(ui: &mut egui::Ui, panel: &mut PanelCtx, rect: Rect) {
    let mut values = [0.0f32; MPE_VOICES * MPE_VALUES];
    if !panel.view.is_some_and(|v| v.read(MPE, &mut values)) {
        return;
    }
    let p = ui.painter();
    p.text(
        rect.left_top() + Vec2::new(4.0, 2.0),
        Align2::LEFT_TOP,
        "Key  Vel  Slide Press Bend",
        font(9.0),
        pal().text_dim,
    );
    for i in 0..MPE_VOICES {
        let at = i * MPE_VALUES;
        if values[at] < 0.0 {
            continue;
        }
        p.text(
            rect.left_top() + Vec2::new(4.0, 14.0 + 11.0 * i as f32),
            Align2::LEFT_TOP,
            format!(
                "{:4} {:4.2} {:5.2} {:5.2} {:+5.2}",
                note_name(values[at] as u8),
                values[at + 1],
                values[at + 2],
                values[at + 3],
                values[at + 4]
            ),
            font(9.0),
            pal().text,
        );
    }
}
