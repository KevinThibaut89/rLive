//! The choosers of External Audio Effect and External Instrument (manual pp. 611, 741).

use super::{PanelCtx, PanelDef};
use crate::ui::theme::*;
use egui::{Pos2, Rect, Stroke, StrokeKind, Vec2};
use rlive_devices::devices::b9::{external_audio_effect as eae, external_instrument as ei};
use rlive_project::{ExternalMidi, ExternalReturn, ExternalRoute};

pub const PANEL: PanelDef = PanelDef {
    name: "external_io",
    size: Vec2::new(190.0, 96.0),
    views: 2,
    draw,
};

fn route_label(items: &[(String, ExternalRoute)], route: Option<&ExternalRoute>) -> String {
    route
        .and_then(|r| items.iter().find(|(_, x)| x == r))
        .map_or_else(|| "None".to_string(), |(n, _)| n.clone())
}

fn peak_bar(ui: &egui::Ui, rect: Rect, level: f32) {
    let p = ui.painter();
    p.rect_filled(rect, 1.0, pal().knob_track);
    let w = rect.width() * level.clamp(0.0, 1.0);
    p.rect_filled(
        Rect::from_min_size(rect.min, Vec2::new(w, rect.height())),
        1.0,
        pal().play_green,
    );
    p.rect_stroke(
        rect,
        1.0,
        Stroke::new(1.0, pal().separator),
        StrokeKind::Inside,
    );
}

fn draw(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    let mut io = panel.device.external.clone().unwrap_or_default();
    let instrument = matches!(
        &panel.device.kind,
        rlive_project::DeviceKind::Builtin { type_id, .. } if type_id == "rlive.external_instrument"
    );
    let hw = panel.hardware.cloned().unwrap_or_default();
    let session = panel.session;
    let mut changed = false;
    let mut y = rect.top() + 2.0;
    let label = |ui: &egui::Ui, y: f32, text: &str| {
        ui.painter().text(
            Pos2::new(rect.left() + 2.0, y),
            egui::Align2::LEFT_TOP,
            text,
            font(9.0),
            pal().text_dim,
        );
    };
    let chooser = |ui: &mut egui::Ui, y: f32| {
        ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(
            Pos2::new(rect.left() + 2.0, y),
            Vec2::new(rect.width() - 4.0, 18.0),
        )))
    };
    if instrument {
        label(ui, y, "MIDI To");
        y += 12.0;
        let mut text = match &io.midi_to {
            None => "None".to_string(),
            Some(ExternalMidi::Port { port, channel }) => format!("{port} {}", channel + 1),
            Some(ExternalMidi::Track { track, channel, .. }) => {
                let name = session
                    .and_then(|s| s.project.track(*track))
                    .map_or_else(String::new, |t| t.name.clone());
                format!("{name} {}", channel + 1)
            }
        };
        text.truncate(28);
        let mut row = chooser(ui, y);
        egui::ComboBox::from_id_salt(("ext_midi", panel.device.id))
            .width(rect.width() - 6.0)
            .selected_text(text)
            .show_ui(&mut row, |ui| {
                if ui.selectable_label(io.midi_to.is_none(), "None").clicked() {
                    io.midi_to = None;
                    changed = true;
                }
                let ports = session.map_or(&[][..], |s| s.midi_out_ports.as_slice());
                for port in ports {
                    for channel in 0..16u8 {
                        let label = format!("{port} Ch. {}", channel + 1);
                        if ui.selectable_label(false, label).clicked() {
                            io.midi_to = Some(ExternalMidi::Port {
                                port: port.clone(),
                                channel,
                            });
                            io.audio_from = None;
                            changed = true;
                        }
                    }
                }
                if let Some(s) = session {
                    for target in s.plugin_targets(panel.device.id) {
                        for channel in 0..16u8 {
                            let label = format!("{} Ch. {}", target.label, channel + 1);
                            if ui.selectable_label(false, label).clicked() {
                                io.midi_to = Some(ExternalMidi::Track {
                                    track: target.track,
                                    device: Some(target.device),
                                    channel,
                                });
                                io.audio_from = None;
                                changed = true;
                            }
                        }
                    }
                }
            });
    } else {
        label(ui, y, "Audio To");
        y += 12.0;
        let mut row = chooser(ui, y);
        egui::ComboBox::from_id_salt(("ext_out", panel.device.id))
            .width(rect.width() - 6.0)
            .selected_text(route_label(&hw.outputs, io.audio_to.as_ref()))
            .show_ui(&mut row, |ui| {
                if ui.selectable_label(io.audio_to.is_none(), "None").clicked() {
                    io.audio_to = None;
                    changed = true;
                }
                for (name, route) in &hw.outputs {
                    let on = io.audio_to.as_ref() == Some(route);
                    if ui.selectable_label(on, name).clicked() {
                        io.audio_to = Some(route.clone());
                        changed = true;
                    }
                }
            });
    }
    label(ui, rect.top() + 42.0, "Audio From");
    let from_text = match &io.audio_from {
        None => "None".to_string(),
        Some(ExternalReturn::Hardware(r)) => route_label(&hw.inputs, Some(r)),
        Some(ExternalReturn::Aux { output, .. }) => format!("Aux {}", output + 1),
    };
    let mut row = chooser(ui, rect.top() + 54.0);
    egui::ComboBox::from_id_salt(("ext_in", panel.device.id))
        .width(rect.width() - 6.0)
        .selected_text(from_text)
        .show_ui(&mut row, |ui| {
            if ui
                .selectable_label(io.audio_from.is_none(), "None")
                .clicked()
            {
                io.audio_from = None;
                changed = true;
            }
            for (name, route) in &hw.inputs {
                let on = matches!(&io.audio_from, Some(ExternalReturn::Hardware(r)) if r == route);
                if ui.selectable_label(on, name).clicked() {
                    io.audio_from = Some(ExternalReturn::Hardware(route.clone()));
                    changed = true;
                }
            }
            if let (Some(s), true) = (session, instrument) {
                if let Some(ExternalMidi::Track {
                    track,
                    device: Some(d),
                    ..
                }) = &io.midi_to
                {
                    if let Some(target) = s
                        .plugin_targets(panel.device.id)
                        .into_iter()
                        .find(|t| t.track == *track && t.device == *d)
                    {
                        for (n, aux) in target.aux.iter().enumerate() {
                            let label = format!("{} | {aux}", target.label);
                            if ui.selectable_label(false, label).clicked() {
                                io.audio_from = Some(ExternalReturn::Aux {
                                    track: *track,
                                    device: *d,
                                    output: n as u8,
                                });
                                changed = true;
                            }
                        }
                    }
                }
            }
        });
    if changed {
        panel.set_external(io);
    }
    let bars = Rect::from_min_max(
        Pos2::new(rect.left() + 2.0, rect.bottom() - 10.0),
        Pos2::new(rect.right() - 2.0, rect.bottom() - 2.0),
    );
    let mut peaks = [0.0f32; 2];
    let view = if instrument { ei::PEAK } else { eae::PEAKS };
    let id = ui.scope_id().with(("external_peaks", panel.device.id));
    let mut held: [f32; 2] = ui.ctx().data_mut(|d| d.get_temp(id).unwrap_or([0.0; 2]));
    if panel.view.is_some_and(|v| v.read(view, &mut peaks)) {
        held[0] = held[0].max(peaks[0]);
        held[1] = held[1].max(peaks[1]);
    }
    // The indicators show the highest level so far; a click resets them (manual p. 611).
    if ui
        .interact(bars, id.with("reset"), egui::Sense::click())
        .clicked()
    {
        held = [0.0; 2];
    }
    ui.ctx().data_mut(|d| d.insert_temp(id, held));
    let peaks = held;
    {
        if instrument {
            peak_bar(ui, bars, peaks[0]);
        } else {
            let half = bars.height() * 0.5;
            peak_bar(
                ui,
                Rect::from_min_size(bars.min, Vec2::new(bars.width(), half)),
                peaks[0],
            );
            peak_bar(
                ui,
                Rect::from_min_size(
                    bars.min + Vec2::new(0.0, half),
                    Vec2::new(bars.width(), half),
                ),
                peaks[1],
            );
        }
    }
}
