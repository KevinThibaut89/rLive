//! Looper's display and transport: the status area, the transport buttons, and the big
//! Multi-Purpose Transport Button with its press, double press and hold behaviours.

use super::{PanelCtx, PanelDef};
use crate::ui::theme::*;
use egui::{Align2, CornerRadius, Pos2, Rect, Sense, Vec2};
use rlive_core::ParamId;
use rlive_devices::devices::b6::looper;

pub const PANEL: PanelDef = PanelDef {
    name: "looper_transport",
    size: Vec2::new(300.0, 150.0),
    views: looper::VIEW_LEN,
    draw,
};

#[derive(Clone, Default)]
struct State {
    /// A button that was pressed last frame, to let go of.
    release: Option<u32>,
}

/// The display text for a position, as bars.beats out of the length.
fn position_text(progress: f32, beats: f32, per_bar: f32) -> String {
    let now = progress * beats;
    let bar = (now / per_bar).floor() as i32 + 1;
    let beat = (now % per_bar).floor() as i32 + 1;
    let length_bars = (beats / per_bar).round() as i32;
    format!("{bar}.{beat}  /  {length_bars} bars")
}

fn button(
    ui: &mut egui::Ui,
    panel: &mut PanelCtx,
    state: &mut State,
    rect: Rect,
    label: &str,
    id: u32,
) {
    let response = ui.interact(
        rect,
        ui.scope_id().with(("looper_btn", panel.device.id, id)),
        Sense::click(),
    );
    let colour = if response.is_pointer_button_down_on() {
        pal().raised_hover
    } else {
        pal().raised
    };
    ui.painter()
        .rect_filled(rect, CornerRadius::same(2), colour);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        font(10.0),
        pal().text,
    );
    if response.clicked() {
        panel.set(ParamId(id), 1.0);
        state.release = Some(id);
    }
}

fn draw(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    let storage = ui.scope_id().with(("looper_state", panel.device.id));
    let mut state: State = ui
        .ctx()
        .data_mut(|d| d.get_temp(storage).unwrap_or_default());
    if let Some(id) = state.release.take() {
        panel.set(ParamId(id), 0.0);
    }

    let mut values = [0.0f32; looper::VIEW_LEN];
    let have = panel
        .view
        .is_some_and(|v| v.read(looper::VIEW, &mut values));
    let (status, progress, beats, tempo, pending, undo, redo) = if have {
        (
            values[0] as i32,
            values[1],
            values[2],
            values[3],
            values[4] > 0.5,
            values[5] > 0.5,
            values[6] > 0.5,
        )
    } else {
        (0, 0.0, 0.0, 0.0, false, false, false)
    };

    let area = Rect::from_min_size(rect.min, Vec2::new(rect.width(), 60.0));
    let recording = status == 1;
    let surface = if recording {
        pal().arm_red
    } else {
        pal().knob_track
    };
    ui.painter()
        .rect_filled(area, CornerRadius::same(2), surface);
    let label = match status {
        1 => "Recording",
        2 => "Overdub",
        3 => "Playing",
        _ if beats > 0.0 => "Stopped",
        _ => "Empty",
    };
    let text = if recording {
        pal().text_dark
    } else {
        pal().text
    };
    ui.painter().text(
        area.left_top() + Vec2::new(6.0, 5.0),
        Align2::LEFT_TOP,
        label,
        font_bold(13.0),
        text,
    );
    ui.painter().text(
        area.right_top() + Vec2::new(-6.0, 5.0),
        Align2::RIGHT_TOP,
        if pending { "launching" } else { "" },
        font(10.0),
        pal().accent,
    );
    if beats > 0.0 {
        let sig = position_text(progress, beats, 4.0);
        ui.painter().text(
            area.left_bottom() + Vec2::new(6.0, -6.0),
            Align2::LEFT_BOTTOM,
            sig,
            font(13.0),
            text,
        );
        let bar = area.right_bottom() + Vec2::new(-6.0 - 100.0, -12.0);
        let width = 100.0;
        ui.painter().rect_filled(
            Rect::from_min_size(bar, Vec2::new(width, 3.0)),
            CornerRadius::ZERO,
            pal().bar_bg,
        );
        ui.painter().rect_filled(
            Rect::from_min_size(bar, Vec2::new(width * progress.clamp(0.0, 1.0), 3.0)),
            CornerRadius::ZERO,
            pal().play_green,
        );
        if tempo > 0.0 {
            ui.painter().text(
                area.right_bottom() + Vec2::new(-6.0, -18.0),
                Align2::RIGHT_BOTTOM,
                format!("{tempo:.1} BPM"),
                font(10.0),
                text,
            );
        }
    }

    // The transport buttons, then the arrows and the buffer length.
    let row_y = area.bottom() + 4.0;
    let w = (rect.width() - 7.0 * 3.0) / 8.0;
    let at = |i: usize| {
        Rect::from_min_size(
            Pos2::new(rect.left() + i as f32 * (w + 3.0), row_y),
            Vec2::new(w, 24.0),
        )
    };
    let undo_label = if redo { "Redo" } else { "Undo" };
    let buttons = [
        ("Rec", looper::CMD_RECORD),
        ("Ovr", looper::CMD_OVERDUB),
        ("Play", looper::CMD_PLAY),
        ("Stop", looper::CMD_STOP),
        ("Clear", looper::CMD_CLEAR),
        (undo_label, looper::CMD_UNDO),
        ("x2", looper::CMD_DOUBLE),
        ("/2", looper::CMD_HALVE),
    ];
    for (i, (name, id)) in buttons.iter().enumerate() {
        let enabled = *id != looper::CMD_UNDO || undo;
        if enabled {
            button(ui, panel, &mut state, at(i), name, *id);
        } else {
            ui.painter()
                .rect_filled(at(i), CornerRadius::same(2), pal().bar_bg);
            ui.painter().text(
                at(i).center(),
                Align2::CENTER_CENTER,
                *name,
                font(10.0),
                pal().text_faint,
            );
        }
    }

    let oct_y = row_y + 28.0;
    let up = Rect::from_min_size(Pos2::new(rect.left(), oct_y), Vec2::new(40.0, 24.0));
    let down = Rect::from_min_size(Pos2::new(rect.left() + 44.0, oct_y), Vec2::new(40.0, 24.0));
    button(ui, panel, &mut state, up, "Oct+", looper::CMD_OCTAVE_UP);
    button(ui, panel, &mut state, down, "Oct-", looper::CMD_OCTAVE_DOWN);

    // The Multi-Purpose button sends its press and release, so holding it means something.
    let big = Rect::from_min_max(
        Pos2::new(rect.left() + 96.0, oct_y),
        Pos2::new(rect.right(), rect.bottom()),
    );
    let response = ui.interact(
        big,
        ui.scope_id().with(("looper_multi", panel.device.id)),
        Sense::click_and_drag(),
    );
    let colour = if response.is_pointer_button_down_on() {
        pal().play_green
    } else {
        pal().accent
    };
    ui.painter().rect_filled(big, CornerRadius::same(3), colour);
    let hint = match status {
        1 => "Play",
        3 => "Overdub",
        2 => "Play",
        _ if beats > 0.0 => "Play",
        _ => "Record",
    };
    ui.painter().text(
        big.center(),
        Align2::CENTER_CENTER,
        hint,
        font_bold(13.0),
        pal().text_dark,
    );
    if response.is_pointer_button_down_on() && !state_pressed(ui, storage) {
        panel.set(ParamId(looper::CMD_MULTI), 1.0);
        set_pressed(ui, storage, true);
    }
    if !response.is_pointer_button_down_on() && state_pressed(ui, storage) {
        panel.set(ParamId(looper::CMD_MULTI), 0.0);
        set_pressed(ui, storage, false);
    }
    ui.ctx().data_mut(|d| d.insert_temp(storage, state));
}

fn state_pressed(ui: &egui::Ui, id: egui::Id) -> bool {
    ui.ctx()
        .data_mut(|d| d.get_temp::<bool>(id.with("pressed")).unwrap_or(false))
}

fn set_pressed(ui: &egui::Ui, id: egui::Id, down: bool) {
    ui.ctx()
        .data_mut(|d| d.insert_temp(id.with("pressed"), down));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_position_reads_as_bars_and_beats() {
        assert_eq!(position_text(0.0, 8.0, 4.0), "1.1  /  2 bars");
        assert_eq!(position_text(0.5, 8.0, 4.0), "2.1  /  2 bars");
    }
}
