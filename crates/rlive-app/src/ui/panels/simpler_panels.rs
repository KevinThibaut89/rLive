//! Panels for Simpler (waveform and slices), Drum Sampler (waveform, X/Y pad) and Impulse (slots).

use super::sample_ui::*;
use super::{PanelCtx, PanelDef};
use crate::session::SamplingCommand;
use crate::ui::theme::*;
use egui::{Align2, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2};
use rlive_core::ParamId;
use rlive_devices::devices::b10::common::WAVE_VALUES;
use rlive_devices::devices::b10::{drum_sampler as drum, impulse as imp, simpler as si};

pub const WAVE: PanelDef = PanelDef {
    name: "simpler_wave",
    size: Vec2::new(500.0, 118.0),
    views: si::VIEW_LEN,
    draw: draw_simpler,
};

pub const DRUM_WAVE: PanelDef = PanelDef {
    name: "drum_sampler_wave",
    size: Vec2::new(300.0, 72.0),
    views: drum::VIEW_LEN,
    draw: draw_drum_wave,
};

pub const DRUM_XY: PanelDef = PanelDef {
    name: "drum_sampler_xy",
    size: Vec2::new(130.0, 118.0),
    views: 0,
    draw: draw_xy,
};

pub const IMPULSE: PanelDef = PanelDef {
    name: "impulse_slots",
    size: Vec2::new(480.0, 190.0),
    views: 0,
    draw: draw_impulse,
};

fn frame(ui: &egui::Ui, rect: Rect) {
    ui.painter().rect_filled(rect, 1.0, pal().field_bg);
    ui.painter().rect_stroke(
        rect,
        1.0,
        Stroke::new(1.0, pal().separator),
        StrokeKind::Inside,
    );
}

fn read_wave(panel: &PanelCtx, view: rlive_core::ViewId) -> [f32; WAVE_VALUES] {
    let mut wave = [0.0f32; WAVE_VALUES];
    if let Some(v) = panel.view {
        v.read(view, &mut wave);
    }
    wave
}

fn draw_simpler(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    let id = ui.scope_id().with(("simpler_wave", panel.device.id));
    let (wave_rect, tools) = rect.split_top_bottom_at_y(rect.bottom() - 34.0);
    let wave_rect = wave_rect.shrink(1.0);
    frame(ui, rect);
    let wave = read_wave(panel, si::VIEW_WAVE);
    draw_wave(ui, wave_rect, &wave);
    if ui.rect_contains_pointer(wave_rect) {
        let at = Rect::from_min_size(
            wave_rect.right_bottom() - Vec2::new(18.0, 16.0),
            Vec2::new(18.0, 16.0),
        );
        panel.hot_swap_sample(ui, at, 0);
    }

    let start = panel.value(ParamId(si::START_FLAG)).unwrap_or(0.0) as f32;
    let end = panel.value(ParamId(si::END_FLAG)).unwrap_or(1.0) as f32;
    let x_of = |t: f32| wave_rect.left() + t * wave_rect.width();
    // Dim what's outside the flags.
    let dim = pal().app_bg.gamma_multiply(0.7);
    ui.painter().rect_filled(
        Rect::from_min_max(wave_rect.min, Pos2::new(x_of(start), wave_rect.bottom())),
        0.0,
        dim,
    );
    ui.painter().rect_filled(
        Rect::from_min_max(Pos2::new(x_of(end), wave_rect.top()), wave_rect.max),
        0.0,
        dim,
    );

    // Flags: drag them near the top of the display. Below that, the display drags slices.
    let area = drag_area(ui, wave_rect, id);
    let flag_height = 14.0;
    let flags = Rect::from_min_max(
        wave_rect.min,
        Pos2::new(wave_rect.right(), wave_rect.top() + flag_height),
    );
    let flag_id = id.with("flag");
    if area.drag_started() {
        let pos = area.interact_pointer_pos().unwrap_or_default();
        if flags.contains(pos) {
            let t = ((pos.x - wave_rect.left()) / wave_rect.width()).clamp(0.0, 1.0);
            let which = if (t - start).abs() < (t - end).abs() {
                1
            } else {
                2
            };
            remember(ui, flag_id, which);
        }
    }
    if area.dragged() {
        let which: i32 = memory(ui, flag_id);
        if which != 0 {
            let pos = area.interact_pointer_pos().unwrap_or_default();
            let t = ((pos.x - wave_rect.left()) / wave_rect.width()).clamp(0.0, 1.0) as f64;
            panel.set(
                ParamId(if which == 1 {
                    si::START_FLAG
                } else {
                    si::END_FLAG
                }),
                t,
            );
        }
    }
    if area.drag_stopped() {
        remember(ui, flag_id, 0);
    }
    for (x, label) in [(x_of(start), "S"), (x_of(end), "E")] {
        ui.painter().line_segment(
            [
                Pos2::new(x, wave_rect.top()),
                Pos2::new(x, wave_rect.bottom()),
            ],
            Stroke::new(1.5, pal().text),
        );
        ui.painter().text(
            Pos2::new(x, wave_rect.top()),
            Align2::CENTER_TOP,
            label,
            font(9.0),
            pal().text,
        );
    }

    // Slices: drag moves one; double-click deletes it or makes a new one.
    let mut slice_view = [0.0f32; si::SLICE_VALUES];
    let mut have = false;
    if let Some(v) = panel.view {
        have = v.read(si::VIEW_SLICES, &mut slice_view);
    }
    let slicing =
        panel.value(ParamId(si::MODE)).unwrap_or(0.0).round() as usize == si::MODE_SLICING;
    let count = slice_view[0] as usize;
    if have && slicing {
        let mut state = si::ParsedState::parse(panel.state_text()).state;
        let slice_x: Vec<f32> = slice_view[1..=count.min(si::MAX_SLICES)]
            .iter()
            .map(|t| x_of(*t))
            .collect();
        for x in &slice_x {
            ui.painter().line_segment(
                [
                    Pos2::new(*x, wave_rect.top() + flag_height),
                    Pos2::new(*x, wave_rect.bottom()),
                ],
                Stroke::new(1.0, shade(0x6fa8d8)),
            );
        }
        for m in &state.manual {
            let total = wave_len(panel).max(1) as f32;
            let x = x_of(*m as f32 / total);
            ui.painter().line_segment(
                [
                    Pos2::new(x, wave_rect.top()),
                    Pos2::new(x, wave_rect.bottom()),
                ],
                Stroke::new(1.0, pal().text),
            );
        }
        let total_frames = wave_len(panel);
        if area.double_clicked() {
            if let Some(pos) = area.interact_pointer_pos() {
                let near = slice_x
                    .iter()
                    .enumerate()
                    .find(|(_, x)| (pos.x - **x).abs() < 5.0)
                    .map(|(i, _)| i);
                let t = ((pos.x - wave_rect.left()) / wave_rect.width()).clamp(0.0, 1.0);
                let frame = t as f64 * total_frames as f64;
                match near {
                    Some(index) => {
                        let at = slice_view[1 + index] as f64 * total_frames as f64;
                        if let Some(i) = state
                            .manual
                            .iter()
                            .position(|m| (m - at).abs() < total_frames as f64 * 0.002)
                        {
                            state.manual.remove(i);
                        } else {
                            state.removed.push(at);
                        }
                    }
                    None => state.manual.push(frame),
                }
                panel.set_state(si::ParsedState::serialize(&state));
            }
        }
    }
    // Where the voices are.
    let mut heads = [0.0f32; si::HEAD_VALUES];
    if let Some(v) = panel.view {
        v.read(si::VIEW_HEADS, &mut heads);
    }
    for t in heads.iter().filter(|t| **t >= 0.0) {
        let x = x_of(*t);
        ui.painter().line_segment(
            [
                Pos2::new(x, wave_rect.top()),
                Pos2::new(x, wave_rect.bottom()),
            ],
            Stroke::new(1.0, pal().play_green),
        );
    }

    // Commands.
    let mut tool_ui =
        ui.new_child(egui::UiBuilder::new().max_rect(tools.shrink2(Vec2::new(4.0, 2.0))));
    tool_ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        if slicing {
            if small_button(ui, "Slice to Drum Rack", 104.0).clicked() {
                panel.sampling(SamplingCommand::SliceToDrumRack);
            }
            if small_button(ui, "Slice to MIDI Track", 104.0).clicked() {
                panel.sampling(SamplingCommand::SliceToMidiTrack);
            }
        }
        if small_button(ui, "Normalize", 62.0).clicked() {
            panel.sampling(SamplingCommand::NormalizeGain);
        }
        if small_button(ui, "Crop", 36.0).clicked() {
            panel.sampling(SamplingCommand::Crop);
        }
        if small_button(ui, "To Sampler", 62.0).clicked() {
            panel.sampling(SamplingCommand::SimplerToSampler);
        }
        if small_button(ui, "Warp /2", 44.0).clicked() {
            let beats = panel.value(ParamId(si::WARP_BEATS)).unwrap_or(4.0);
            panel.set(ParamId(si::WARP_BEATS), (beats * 0.5).max(0.25));
        }
        if small_button(ui, "Warp x2", 44.0).clicked() {
            let beats = panel.value(ParamId(si::WARP_BEATS)).unwrap_or(4.0);
            panel.set(ParamId(si::WARP_BEATS), (beats * 2.0).min(256.0));
        }
    });
}

/// A sample's length in frames, as far as a panel knows: it's only for placing marks.
fn wave_len(panel: &PanelCtx) -> usize {
    let mut info = [0.0f32; si::INFO_VALUES];
    if let Some(v) = panel.view {
        v.read(si::VIEW_INFO, &mut info);
    }
    info[0].max(0.0) as usize
}

fn draw_drum_wave(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    let (wave_rect, tools) = rect.split_top_bottom_at_y(rect.bottom() - 18.0);
    let wave_rect = wave_rect.shrink(1.0);
    frame(ui, rect);
    let wave = read_wave(panel, drum::VIEW_WAVE);
    draw_wave(ui, wave_rect, &wave);
    if ui.rect_contains_pointer(wave_rect) {
        let at = Rect::from_min_size(
            wave_rect.right_bottom() - Vec2::new(18.0, 16.0),
            Vec2::new(18.0, 16.0),
        );
        panel.hot_swap_sample(ui, at, 0);
    }
    let start = panel.value(ParamId(drum::START)).unwrap_or(0.0) as f32 * 0.01;
    let length = panel.value(ParamId(drum::LENGTH)).unwrap_or(100.0) as f32 * 0.01;
    let x_of = |t: f32| wave_rect.left() + t * wave_rect.width();
    let end = (start + length).min(1.0);
    let dim = pal().app_bg.gamma_multiply(0.7);
    ui.painter().rect_filled(
        Rect::from_min_max(wave_rect.min, Pos2::new(x_of(start), wave_rect.bottom())),
        0.0,
        dim,
    );
    ui.painter().rect_filled(
        Rect::from_min_max(Pos2::new(x_of(end), wave_rect.top()), wave_rect.max),
        0.0,
        dim,
    );
    let mut bar = ui.new_child(egui::UiBuilder::new().max_rect(tools.shrink2(Vec2::new(4.0, 1.0))));
    bar.horizontal(|ui| {
        if small_button(ui, "Normalize", 62.0).clicked() {
            panel.sampling(SamplingCommand::NormalizeGain);
        }
        if small_button(ui, "To Simpler", 62.0).clicked() {
            panel.sampling(SamplingCommand::DrumSamplerToSimpler);
        }
    });
}

fn draw_xy(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    frame(ui, rect);
    let pad = rect.shrink(8.0);
    let response = ui.interact(
        rect,
        ui.scope_id().with(("xy", panel.device.id)),
        Sense::click_and_drag(),
    );
    let fx = panel.value(ParamId(drum::FX_TYPE)).unwrap_or(0.0).round() as usize;
    let fx1 = panel.value(ParamId(drum::FX1)).unwrap_or(0.5) as f32;
    let fx2 = panel.value(ParamId(drum::FX2)).unwrap_or(0.5) as f32;
    if response.dragged() {
        if let Some(pos) = response.interact_pointer_pos() {
            let x = ((pos.x - pad.left()) / pad.width()).clamp(0.0, 1.0) as f64;
            let y = (1.0 - (pos.y - pad.top()) / pad.height()).clamp(0.0, 1.0) as f64;
            panel.set(ParamId(drum::FX1), y);
            panel.set(ParamId(drum::FX2), x);
        }
    }
    let at = Pos2::new(
        pad.left() + fx2 * pad.width(),
        pad.bottom() - fx1 * pad.height(),
    );
    let p = ui.painter();
    p.line_segment(
        [Pos2::new(pad.left(), at.y), Pos2::new(pad.right(), at.y)],
        Stroke::new(1.0, pal().separator),
    );
    p.line_segment(
        [Pos2::new(at.x, pad.top()), Pos2::new(at.x, pad.bottom())],
        Stroke::new(1.0, pal().separator),
    );
    p.circle_filled(at, 4.0, pal().accent);
    let (first, second) = drum::FX_PARAMS[fx.min(8)];
    p.text(
        Pos2::new(rect.left() + 2.0, rect.top() + 1.0),
        Align2::LEFT_TOP,
        first,
        font(9.0),
        pal().text_dim,
    );
    p.text(
        Pos2::new(rect.right() - 2.0, rect.bottom() - 1.0),
        Align2::RIGHT_BOTTOM,
        second,
        font(9.0),
        pal().text_dim,
    );
}

fn draw_impulse(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    let id = ui.scope_id().with(("impulse", panel.device.id));
    frame(ui, rect);
    let mut selected: usize = memory(ui, id.with("selected"));
    let mut column = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(4.0)));
    column.spacing_mut().item_spacing.y = 3.0;
    column.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        for slot in 0..imp::SLOTS {
            let name = panel
                .device
                .sample(slot)
                .and_then(|f| f.path.file_stem())
                .map_or("", |s| s.to_str().unwrap_or(""));
            let label = if name.is_empty() {
                format!("{}", slot + 1)
            } else {
                name.chars().take(7).collect()
            };
            let on = slot == selected;
            let resp = crate::ui::widgets::state_button(
                ui,
                Vec2::new(50.0, 26.0),
                on,
                pal().accent,
                &label,
            );
            if resp.clicked() {
                selected = slot;
            }
            if resp.double_clicked() {
                panel.load_sample(slot);
            }
        }
    });
    remember(ui, id.with("selected"), selected);
    let s = selected;
    if ui.rect_contains_pointer(ui.max_rect()) {
        let rect = ui.max_rect();
        let at = Rect::from_min_size(
            rect.right_top() + Vec2::new(-18.0, 28.0),
            Vec2::new(18.0, 16.0),
        );
        panel.hot_swap_sample(ui, at, s);
    }
    let param = |offset| imp::slot_param(s, offset);
    egui::Grid::new(id.with("grid"))
        .num_columns(3)
        .spacing([6.0, 1.0])
        .show(&mut column, |ui| {
            let rows: &[(&str, u32)] = &[
                ("Start", imp::START),
                ("Transpose", imp::TRANSPOSE),
                ("Transp < Vel", imp::TRANSP_VEL),
                ("Transp < Rnd", imp::TRANSP_RND),
                ("Stretch", imp::STRETCH),
                ("Mode", imp::STRETCH_MODE),
                ("Stretch < Vel", imp::STRETCH_VEL),
                ("Filter", imp::FILTER_ON),
                ("Type", imp::FILTER_TYPE),
                ("Freq", imp::FREQ),
                ("Res", imp::RES),
                ("Freq < Vel", imp::FREQ_VEL),
                ("Freq < Rnd", imp::FREQ_RND),
                ("Sat", imp::SAT_ON),
                ("Drive", imp::DRIVE),
                ("Decay", imp::DECAY),
                ("Decay Mode", imp::DECAY_MODE),
                ("Pan", imp::PAN),
                ("Pan < Vel", imp::PAN_VEL),
                ("Pan < Rnd", imp::PAN_RND),
                ("Volume", imp::VOLUME),
                ("Vol < Vel", imp::VOL_VEL),
                ("Mute", imp::MUTE),
                ("Solo", imp::SOLO),
            ];
            for (i, (label, offset)) in rows.iter().enumerate() {
                param_control(ui, panel, param(*offset), label, 150.0);
                if i % 3 == 2 {
                    ui.end_row();
                }
            }
        });
    column.horizontal(|ui| {
        ui.label(
            RichText::new("Click selects, double-click loads")
                .size(9.0)
                .color(pal().text_dim),
        );
    });
}
