//! The Tuning section of the browser: the loaded tuning system and its reference pitch.

use super::theme::{font, pal};
use super::widgets::{self, info};
use super::UiState;
use crate::session::Session;
use egui::{Rect, RichText, UiBuilder};
use rlive_project::{ScaleNote, Tuning};

pub const HEIGHT: f32 = 118.0;
pub const HEIGHT_OPEN: f32 = 176.0;

/// A tuning waiting for the Retune Set dialog.
#[derive(Clone, Debug)]
pub enum PendingTuning {
    Load(Tuning),
    Clear,
}

/// Asks to load a tuning, confirming first when Retune Set is on.
pub fn ask_to_load(session: &mut Session, state: &mut UiState, retune: bool, tuning: Tuning) {
    if retune {
        state.pending_tuning = Some(PendingTuning::Load(tuning));
    } else {
        session.load_tuning(tuning, false);
    }
}

fn ask_to_clear(session: &mut Session, state: &mut UiState, retune: bool) {
    if retune {
        state.pending_tuning = Some(PendingTuning::Clear);
    } else {
        session.clear_tuning(false);
    }
}

fn pick_file() -> Option<std::path::PathBuf> {
    rfd::FileDialog::new()
        .add_filter("Tuning", &["ascl", "scl"])
        .pick_file()
}

/// The section, in `area`.
pub fn show(
    ui: &mut egui::Ui,
    area: Rect,
    session: &mut Session,
    state: &mut UiState,
    retune: bool,
) {
    let mut tu = ui.new_child(UiBuilder::new().max_rect(area.shrink(6.0)));
    let tuning = session.tuning().cloned();
    tu.horizontal(|ui| {
        let open = state.tuning_open;
        let arrow = if open { "-" } else { "+" };
        if ui.small_button(arrow).clicked() {
            state.tuning_open = !open;
        }
        widgets::section_label(ui, "TUNING");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if tuning.is_some() {
                let r = ui.small_button("x");
                if info(r, "Remove this tuning and go back to 12-TET").clicked() {
                    ask_to_clear(session, state, retune);
                }
                let r = ui.small_button("Save");
                if info(r, "Save Tuning — write it as an .ascl file").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("ASCL", &["ascl"])
                        .set_file_name("Tuning.ascl")
                        .save_file()
                    {
                        if let Err(e) = session.save_tuning_file(&path) {
                            session.messages.push(format!("Could not save tuning: {e}"));
                        }
                    }
                }
            }
            let r = ui.menu_button("Load", |ui| {
                if ui.button("From File…").clicked() {
                    ui.close();
                    if let Some(path) = pick_file() {
                        let stem = path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("Tuning")
                            .to_string();
                        match std::fs::read_to_string(&path)
                            .map_err(|e| e.to_string())
                            .and_then(|t| Tuning::parse(&t, &stem))
                        {
                            Ok(t) => ask_to_load(session, state, retune, t),
                            Err(e) => session.messages.push(format!("Could not read tuning: {e}")),
                        }
                    }
                }
                ui.separator();
                for t in rlive_project::factory_tunings() {
                    let summary = t.summary();
                    let r = ui.button(t.name.as_str());
                    if info(r, summary).clicked() {
                        ask_to_load(session, state, retune, t);
                        ui.close();
                    }
                }
            });
            info(
                r.response,
                "Load a tuning — a Scala .scl or .ascl file, or one of rLive's own",
            );
        });
    });
    let Some(t) = tuning else {
        tu.label(
            RichText::new("12-TET")
                .color(pal().text_faint)
                .font(font(10.0)),
        );
        return;
    };
    let name = tu.label(
        RichText::new(t.name.as_str())
            .color(pal().text)
            .font(font(11.0)),
    );
    info(name, t.summary());
    let steps = t.steps() as u32;
    egui::Grid::new("tuning_fields")
        .num_columns(4)
        .spacing([6.0, 3.0])
        .show(&mut tu, |ui| {
            field_label(ui, "Octave");
            let mut octave = t.reference.octave;
            let r = ui.add(egui::DragValue::new(&mut octave).range(-2..=8));
            if r.changed() {
                session.set_tuning_reference_octave(octave);
            }
            info(
                r,
                "Octave — of the reference note. Nothing is heard to change",
            );
            field_label(ui, "Note");
            let mut degree = t.reference.degree;
            note_chooser(ui, "tuning_note", &t, &mut degree, steps);
            if degree != t.reference.degree {
                session.set_tuning_reference_degree(degree);
            }
            ui.end_row();
            field_label(ui, "Ref. Pitch");
            let mut hz = t.reference_hz;
            let r = ui.add(
                egui::DragValue::new(&mut hz)
                    .range(1.0..=20_000.0)
                    .speed(0.1)
                    .suffix(" Hz")
                    .fixed_decimals(2),
            );
            if r.changed() {
                session.edit_tuning(|x| x.reference_hz = hz);
            }
            info(
                r,
                "Reference Pitch — raises or lowers every note in the Set",
            );
            ui.end_row();
        });
    if state.tuning_open {
        egui::Grid::new("tuning_range")
            .num_columns(5)
            .spacing([6.0, 3.0])
            .show(&mut tu, |ui| {
                let row = |ui: &mut egui::Ui,
                           label: &str,
                           salt: &str,
                           session: &mut Session,
                           note: ScaleNote,
                           set: fn(&mut Tuning, ScaleNote)| {
                    field_label(ui, label);
                    let mut octave = note.octave;
                    let r = ui.add(egui::DragValue::new(&mut octave).range(-16..=16));
                    if r.changed() {
                        session.edit_tuning(|x| {
                            set(
                                x,
                                ScaleNote {
                                    octave,
                                    degree: note.degree,
                                },
                            )
                        });
                    }
                    info(r, "Which octave this end of the keyboard plays");
                    let mut degree = note.degree;
                    let current = session.tuning().cloned();
                    if let Some(t) = current {
                        note_chooser(ui, salt, &t, &mut degree, steps);
                    }
                    if degree != note.degree {
                        session.edit_tuning(|x| {
                            set(
                                x,
                                ScaleNote {
                                    octave: note.octave,
                                    degree,
                                },
                            )
                        });
                    }
                    ui.end_row();
                };
                row(ui, "Lowest", "tuning_low", session, t.lowest, |x, n| {
                    x.lowest = n
                });
                row(
                    ui,
                    "Highest",
                    "tuning_high",
                    session,
                    t.highest(),
                    |x, n| x.set_highest(n),
                );
            });
    }
}

fn field_label(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).font(font(10.0)).color(pal().text_dim));
}

fn note_chooser(ui: &mut egui::Ui, salt: &str, t: &Tuning, degree: &mut u32, steps: u32) {
    egui::ComboBox::from_id_salt(salt)
        .width(50.0)
        .selected_text(t.degree_name(*degree))
        .show_ui(ui, |ui| {
            for d in 0..steps {
                ui.selectable_value(degree, d, t.degree_name(d));
            }
        });
}

/// The Retune Set confirmation.
pub fn retune_dialog(ctx: &egui::Context, session: &mut Session, state: &mut UiState) {
    let Some(pending) = state.pending_tuning.clone() else {
        return;
    };
    let preview = session.retune_preview();
    let mut choice = None;
    egui::Window::new("Retune Set")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.label(format!(
                "Retuning moves {} notes in {} clips to the nearest pitch of the new tuning.",
                preview.notes, preview.clips
            ));
            ui.label(
                RichText::new("Notes that land together may be shortened or removed.")
                    .color(pal().text_dim),
            );
            ui.horizontal(|ui| {
                if ui.button("Retune Set").clicked() {
                    choice = Some(true);
                }
                if ui.button("Keep Notes Where They Are").clicked() {
                    choice = Some(false);
                }
            });
        });
    if let Some(retune) = choice {
        state.pending_tuning = None;
        match pending {
            PendingTuning::Load(t) => session.load_tuning(t, retune),
            PendingTuning::Clear => session.clear_tuning(retune),
        }
    }
}
