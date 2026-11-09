//! The MPE/Multi-channel Settings and Configure MIDI Layout dialogs.

use super::UiState;
use crate::session::{ChainRef, Session};
use egui::RichText;
use rlive_core::{DeviceId, MpeOutput, MpeZone, TrackId};
use rlive_project::{CustomLayout, TuningLayout};

/// Whose channels the MPE/Multi-channel Settings dialog is setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MpeTarget {
    Track(TrackId),
    Device(ChainRef, DeviceId),
}

const KEYS: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

fn get(session: &Session, target: MpeTarget) -> Option<MpeOutput> {
    match target {
        MpeTarget::Track(t) => session.project.track(t).and_then(|x| x.mpe_out),
        MpeTarget::Device(c, d) => session.device_mpe(c, d),
    }
}

fn set(session: &mut Session, target: MpeTarget, out: Option<MpeOutput>) {
    match target {
        MpeTarget::Track(t) => session.set_track_mpe_out(t, out),
        MpeTarget::Device(c, d) => session.set_device_mpe(c, d, out),
    }
}

/// The MPE/Multi-channel Settings dialog, while one is open.
pub fn mpe_dialog(ctx: &egui::Context, session: &mut Session, state: &mut UiState) {
    let Some(target) = state.mpe_dialog else {
        return;
    };
    let current = get(session, target);
    let mut out = current;
    let mut open = true;
    egui::Window::new("MPE/Multi-channel Settings")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            let mut on = out.is_some();
            if ui.checkbox(&mut on, "MPE Mode").changed() {
                out = on.then(MpeOutput::default);
            }
            let Some(mode) = out else {
                return;
            };
            let zone = matches!(
                mode,
                MpeOutput::Zone {
                    zone: MpeZone::Lower,
                    ..
                }
            );
            let upper = matches!(
                mode,
                MpeOutput::Zone {
                    zone: MpeZone::Upper,
                    ..
                }
            );
            let multi = matches!(mode, MpeOutput::MultiChannel { .. });
            let members = match mode {
                MpeOutput::Zone { members, .. } => members,
                MpeOutput::MultiChannel { .. } => 15,
            };
            ui.horizontal(|ui| {
                if ui.selectable_label(zone, "Lower Zone").clicked() {
                    out = Some(MpeOutput::Zone {
                        zone: MpeZone::Lower,
                        members,
                    });
                }
                if ui.selectable_label(upper, "Upper Zone").clicked() {
                    out = Some(MpeOutput::Zone {
                        zone: MpeZone::Upper,
                        members,
                    });
                }
                if ui.selectable_label(multi, "Multi-channel").clicked() {
                    out = Some(MpeOutput::MultiChannel { first: 0, last: 15 });
                }
            });
            match out.unwrap_or(mode) {
                MpeOutput::Zone { zone: z, members } => {
                    let mut m = members;
                    ui.horizontal(|ui| {
                        ui.label("Note Channels");
                        ui.add(egui::DragValue::new(&mut m).range(1..=15));
                    });
                    let global = z.manager() + 1;
                    let (first, last) = match z {
                        MpeZone::Lower => (2, 1 + m),
                        MpeZone::Upper => (16 - m, 15),
                    };
                    ui.label(RichText::new(format!(
                        "Global channel {global}, notes on {first}-{last}"
                    )));
                    if m != members {
                        out = Some(MpeOutput::Zone {
                            zone: z,
                            members: m,
                        });
                    }
                }
                MpeOutput::MultiChannel { first, last } => {
                    let (mut a, mut b) = (first + 1, last + 1);
                    ui.horizontal(|ui| {
                        ui.label("First Channel");
                        ui.add(egui::DragValue::new(&mut a).range(1..=16));
                        ui.label("Last Channel");
                        ui.add(egui::DragValue::new(&mut b).range(1..=16));
                    });
                    if (a, b) != (first + 1, last + 1) {
                        let (a, b) = (a.min(16) - 1, b.min(16) - 1);
                        out = Some(MpeOutput::MultiChannel {
                            first: a.min(b),
                            last: a.max(b),
                        });
                    }
                }
            }
        });
    if out != current {
        set(session, target, out);
    }
    if !open {
        state.mpe_dialog = None;
    }
}

/// The Configure MIDI Layout dialog, while one is open.
pub fn layout_dialog(ctx: &egui::Context, session: &mut Session, state: &mut UiState) {
    let Some(track) = state.layout_dialog else {
        return;
    };
    let Some(TuningLayout::Custom(layout)) = session
        .project
        .track(track)
        .map(|t| t.tuning_layout.clone())
    else {
        state.layout_dialog = None;
        return;
    };
    let mut edited: CustomLayout = layout.clone();
    let mut open = true;
    egui::Window::new("Configure MIDI Layout")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Controller key");
                ui.add(egui::DragValue::new(&mut edited.anchor).range(0..=127));
                ui.label("plays note");
                ui.add(egui::DragValue::new(&mut edited.note).range(0..=127));
            });
            ui.label("Keys that play:");
            ui.horizontal(|ui| {
                for (i, name) in KEYS.iter().enumerate() {
                    ui.vertical(|ui| {
                        ui.checkbox(&mut edited.keys[i], "");
                        ui.label(RichText::new(*name).small());
                    });
                }
            });
        });
    if edited != layout {
        session.set_tuning_layout(track, TuningLayout::Custom(edited));
    }
    if !open {
        state.layout_dialog = None;
    }
}
