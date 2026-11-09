//! The Groove Pool: the Set's grooves and their settings, below the browser.

use super::theme::{font, pal};
use super::widgets::{self, info};
use super::UiState;
use crate::session::{ClipRef, Session};
use egui::{Rect, RichText, UiBuilder, Vec2};
use rlive_core::GrooveId;
use rlive_project::{factory_grooves, Groove, BASE_CHOICES, GROOVE_EXTENSION};

/// A groove being dragged, onto a clip or into a MIDI track.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GrooveDrag(pub GrooveId);

pub const HEIGHT: f32 = 268.0;
const ROW_H: f32 = 84.0;

fn groove_file_dialog() -> rfd::FileDialog {
    rfd::FileDialog::new().add_filter("rLive groove", &[GROOVE_EXTENSION])
}

/// What the Load or Hot-Swap menu chose.
fn pick_groove(ui: &mut egui::Ui) -> Option<Groove> {
    let mut picked = None;
    if ui.button("From File…").clicked() {
        if let Some(path) = groove_file_dialog().pick_file() {
            let text = std::fs::read_to_string(&path).ok()?;
            match Groove::from_file(&text) {
                Ok(mut g) => {
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        g.name = stem.to_string();
                    }
                    picked = Some(g);
                }
                Err(e) => log::info!("Groove {}: {e}", path.display()),
            }
        }
        ui.close();
    }
    ui.separator();
    for g in factory_grooves() {
        if ui.button(g.name.as_str()).clicked() {
            picked = Some(g);
            ui.close();
        }
    }
    picked
}

/// The whole section, in `area`.
pub fn show(ui: &mut egui::Ui, area: Rect, session: &mut Session, state: &mut UiState) {
    let mut pu = ui.new_child(UiBuilder::new().max_rect(area.shrink(6.0)));
    pu.horizontal(|ui| {
        widgets::section_label(ui, "GROOVE POOL");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let r = ui.menu_button("Load", |ui| {
                if let Some(g) = pick_groove(ui) {
                    session.add_groove(g);
                }
            });
            info(
                r.response,
                "Load — a groove file or one of rLive's own grooves",
            );
        });
    });
    let mut amount = session.project.groove_pool.amount;
    pu.horizontal(|ui| {
        ui.label(
            RichText::new("Global Amount")
                .color(pal().text_dim)
                .font(font(10.0)),
        );
        let r = ui.add(
            egui::DragValue::new(&mut amount)
                .range(0.0..=1.3)
                .speed(0.005)
                .custom_formatter(|v, _| format!("{:.0} %", v * 100.0))
                .custom_parser(|s| {
                    s.trim_end_matches([' ', '%'])
                        .parse::<f64>()
                        .ok()
                        .map(|v| v / 100.0)
                }),
        );
        if r.changed() {
            session.set_groove_amount(amount);
        }
        info(
            r,
            "Global Amount — scales Timing, Random and Velocity of every groove, up to 130%",
        );
    });
    pu.separator();
    let list = pu.available_rect_before_wrap();
    let drop_target = pu.interact(
        list,
        pu.scope_id().with("groove_pool_drop"),
        egui::Sense::hover(),
    );
    // A clip dropped on the pool becomes a groove.
    if let Some(src) = widgets::released::<super::session_view::ClipDrag>(&drop_target) {
        session.extract_groove(ClipRef::slot(src.track, src.slot));
    }
    let grooves: Vec<GrooveId> = session
        .project
        .groove_pool
        .grooves
        .iter()
        .map(|g| g.id)
        .collect();
    egui::ScrollArea::vertical()
        .id_salt("groove_pool_scroll")
        .auto_shrink([false, false])
        .show(&mut pu, |ui| {
            if grooves.is_empty() {
                ui.label(
                    RichText::new("Load a groove, or drop a clip here to make one")
                        .color(pal().text_faint)
                        .font(font(10.0)),
                );
            }
            for id in grooves {
                groove_row(ui, session, state, id);
            }
        });
}

fn groove_row(ui: &mut egui::Ui, session: &mut Session, state: &mut UiState, id: GrooveId) {
    let Some(groove) = session.project.groove_pool.get(id).cloned() else {
        return;
    };
    let _ = state;
    let active = session.groove_in_use(id);
    let (rect, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), egui::Sense::hover());
    let mut row = ui.new_child(UiBuilder::new().max_rect(rect.shrink2(Vec2::new(0.0, 2.0))));
    let color = if active { pal().text } else { pal().text_faint };
    row.horizontal(|ui| {
        let name = ui.add(
            egui::Label::new(
                RichText::new(groove.name.as_str())
                    .color(color)
                    .font(font(11.0)),
            )
            .truncate()
            .sense(egui::Sense::click_and_drag()),
        );
        name.dnd_set_drag_payload(GrooveDrag(id));
        info(name, "A groove — drag it onto a clip");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let r = ui.small_button("x");
            if info(r, "Remove this groove from the pool").clicked() {
                session.remove_groove(id);
            }
            let r = ui.menu_button("Swap", |ui| {
                if let Some(g) = pick_groove(ui) {
                    session.swap_groove(id, g);
                }
            });
            info(
                r.response,
                "Hot-Swap Groove — replace it, in every clip using it",
            );
            let r = ui.small_button("Save");
            if info(r, "Save Groove — write it to a file").clicked() {
                if let Some(path) = groove_file_dialog()
                    .set_file_name(format!("{}.{GROOVE_EXTENSION}", groove.name))
                    .save_file()
                {
                    if let Err(e) = session.save_groove_file(id, &path) {
                        session.messages.push(format!("Could not save groove: {e}"));
                    }
                }
            }
        });
    });
    // Inactive grooves are grayed out, and can still be adjusted.
    let alpha = if active { 1.0 } else { 0.55 };
    row.scope(|ui| {
        ui.set_opacity(alpha);
        egui::Grid::new(("groove_params", id))
            .num_columns(4)
            .spacing([6.0, 2.0])
            .show(ui, |ui| {
                ui.label(RichText::new("Base").font(font(9.5)).color(pal().text_dim));
                let mut base = groove.base;
                egui::ComboBox::from_id_salt(("groove_base", id))
                    .width(54.0)
                    .selected_text(
                        BASE_CHOICES
                            .iter()
                            .find(|(t, _)| *t == base)
                            .map_or("?", |(_, n)| n),
                    )
                    .show_ui(ui, |ui| {
                        for (ticks, name) in BASE_CHOICES {
                            ui.selectable_value(&mut base, ticks, name);
                        }
                    });
                if base != groove.base {
                    session.edit_groove(id, |g| g.base = base);
                }
                let field = |ui: &mut egui::Ui,
                             session: &mut Session,
                             label: &str,
                             tip: &str,
                             value: f32,
                             range: std::ops::RangeInclusive<f32>,
                             set: fn(&mut Groove, f32)| {
                    ui.label(RichText::new(label).font(font(9.5)).color(pal().text_dim));
                    let mut v = value;
                    let r = ui.add(
                        egui::DragValue::new(&mut v)
                            .range(range)
                            .speed(0.5)
                            .suffix(" %")
                            .max_decimals(0),
                    );
                    if r.changed() {
                        session.edit_groove(id, |g| set(g, v));
                    }
                    info(r, tip);
                };
                field(
                    ui,
                    session,
                    "Quantize",
                    "Quantize — straight quantization by Base, before the groove",
                    groove.quantize,
                    0.0..=100.0,
                    |g, v| g.quantize = v,
                );
                ui.end_row();
                field(
                    ui,
                    session,
                    "Timing",
                    "Timing — how much the groove moves the notes",
                    groove.timing,
                    0.0..=100.0,
                    |g, v| g.timing = v,
                );
                field(
                    ui,
                    session,
                    "Random",
                    "Random — a separate random offset for every voice",
                    groove.random,
                    0.0..=100.0,
                    |g, v| g.random = v,
                );
                ui.end_row();
                field(
                    ui,
                    session,
                    "Velocity",
                    "Velocity — how much the groove's dynamics affect notes, reversed below zero",
                    groove.velocity,
                    -100.0..=100.0,
                    |g, v| g.velocity = v,
                );
                ui.end_row();
            });
    });
}
