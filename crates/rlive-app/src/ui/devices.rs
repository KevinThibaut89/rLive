//! Device chain view: one card per device, laid out left to right (signal flow).

use super::widgets::{self, ACCENT, TEXT_DIM};
use super::{BrowserItem, UiState};
use crate::session::{ChainRef, Session};
use egui::{Color32, RichText, Sense, Vec2};
use rlive_core::{DeviceId, ParamInfo};
use rlive_project::DeviceKind;

const MAX_KNOBS_COLLAPSED: usize = 24;

enum Action {
    Remove(DeviceId),
    Move(DeviceId, usize),
    Bypass(DeviceId, bool),
    Param(DeviceId, u32, f64),
    OpenEditor(DeviceId),
    CloseEditor(DeviceId),
    LoadSample(DeviceId),
    Reload(DeviceId),
}

pub fn show(ui: &mut egui::Ui, session: &mut Session, state: &mut UiState) {
    let Some(chain) = state.selected else {
        ui.centered_and_justified(|ui| {
            ui.label(RichText::new("Select a track to see its devices").color(TEXT_DIM))
        });
        return;
    };
    let title = match chain {
        ChainRef::Master => "Master".to_string(),
        ChainRef::Track(t) => session
            .project
            .track(t)
            .map(|t| t.name.clone())
            .unwrap_or_default(),
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).strong());
        ui.label(RichText::new("Devices").color(TEXT_DIM));
        if state.selected_clip.is_some() && ui.small_button("Clip >").clicked() {
            state.detail = super::Detail::Clip;
        }
    });

    let devices = session.devices(chain).to_vec();
    let mut actions = Vec::new();
    egui::ScrollArea::horizontal().auto_shrink([false, false]).show(ui, |ui| {
        ui.horizontal_top(|ui| {
            for (index, dev) in devices.iter().enumerate() {
                device_card(ui, session, chain, dev, index, devices.len(), &mut actions);
                // Arrow between devices = signal flow.
                ui.label(RichText::new("›").size(20.0).color(Color32::from_gray(90)));
            }
            // Drop zone at the end of the chain.
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(150.0, 170.0), Sense::hover());
            let hovering = resp.dnd_hover_payload::<BrowserItem>().is_some();
            ui.painter().rect_stroke(
                rect,
                4.0,
                egui::Stroke::new(1.0, if hovering { ACCENT } else { Color32::from_gray(60) }),
                egui::StrokeKind::Inside,
            );
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Drop instruments,\neffects or plugins here",
                egui::FontId::proportional(11.0),
                Color32::from_gray(110),
            );
            if let Some(item) = resp.dnd_release_payload::<BrowserItem>() {
                match &*item {
                    BrowserItem::Builtin(t) => session.add_builtin(chain, t),
                    BrowserItem::Plugin(p) => session.add_plugin(chain, p),
                    BrowserItem::Sample(path) => {
                        // Dropping a sample on the chain adds a Sampler loaded with it.
                        session.add_builtin(chain, "rlive.sampler");
                        if let Some(d) = session.devices(chain).iter().rev().find(|d| matches!(&d.kind, DeviceKind::Builtin { type_id, .. } if type_id == "rlive.sampler")) {
                            let id = d.id;
                            session.set_device_sample(chain, id, path.clone());
                        }
                    }
                }
            }
        });
    });

    for a in actions {
        match a {
            Action::Remove(id) => session.remove_device(chain, id),
            Action::Move(id, to) => session.move_device(chain, id, to),
            Action::Bypass(id, b) => session.set_bypass(chain, id, b),
            Action::Param(id, p, v) => session.set_param(chain, id, p, v),
            Action::OpenEditor(id) => {
                if let Some(h) = session
                    .runtimes
                    .get_mut(&id)
                    .and_then(|r| r.plugin.as_mut())
                {
                    if let Err(e) = h.open_editor() {
                        state.status = format!("Editor: {e}");
                    }
                }
            }
            Action::CloseEditor(id) => {
                if let Some(h) = session
                    .runtimes
                    .get_mut(&id)
                    .and_then(|r| r.plugin.as_mut())
                {
                    h.close_editor();
                }
            }
            Action::LoadSample(id) => {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("WAV", &["wav"])
                    .pick_file()
                {
                    session.set_device_sample(chain, id, path);
                }
            }
            Action::Reload(id) => session.reload_device(chain, id),
        }
    }
}

fn device_card(
    ui: &mut egui::Ui,
    session: &Session,
    chain: ChainRef,
    dev: &rlive_project::Device,
    index: usize,
    count: usize,
    actions: &mut Vec<Action>,
) {
    let rt = session.runtimes.get(&dev.id);
    let frame = egui::Frame::new()
        .fill(Color32::from_gray(if dev.bypass { 36 } else { 48 }))
        .stroke(egui::Stroke::new(1.0, Color32::from_gray(64)))
        .corner_radius(4.0)
        .inner_margin(6.0);
    frame.show(ui, |ui| {
        ui.set_min_height(160.0);
        ui.vertical(|ui| {
            // Title bar
            ui.horizontal(|ui| {
                let on = !dev.bypass;
                if widgets::toggle_box(ui, on, "On", ACCENT, "Device on/off").clicked() {
                    actions.push(Action::Bypass(dev.id, on));
                }
                let mut title = RichText::new(&dev.name).strong();
                if let DeviceKind::Plugin { format, .. } = &dev.kind {
                    title = RichText::new(format!("{} ", dev.name)).strong();
                    ui.label(title);
                    ui.label(RichText::new(format.label()).small().color(TEXT_DIM));
                } else {
                    ui.label(title);
                }
                if index > 0 && ui.small_button("<").on_hover_text("Move left").clicked() {
                    actions.push(Action::Move(dev.id, index - 1));
                }
                if index + 1 < count && ui.small_button(">").on_hover_text("Move right").clicked() {
                    actions.push(Action::Move(dev.id, index + 1));
                }
                if ui
                    .small_button("x")
                    .on_hover_text("Delete device")
                    .clicked()
                {
                    actions.push(Action::Remove(dev.id));
                }
            });

            let Some(rt) = rt else {
                ui.label(RichText::new("Not loaded").color(Color32::LIGHT_RED));
                return;
            };
            if let Some(err) = &rt.error {
                ui.label(RichText::new(err).color(Color32::LIGHT_RED).small());
                if matches!(dev.kind, DeviceKind::Plugin { .. }) && ui.button("Retry").clicked() {
                    actions.push(Action::Reload(dev.id));
                }
            }

            if let Some(h) = &rt.plugin {
                ui.horizontal(|ui| {
                    if h.has_editor() {
                        if h.is_editor_open() {
                            if ui.button("Close Editor").clicked() {
                                actions.push(Action::CloseEditor(dev.id));
                            }
                        } else if ui.button("Open Editor").clicked() {
                            actions.push(Action::OpenEditor(dev.id));
                        }
                    }
                    let lat = h.latency();
                    if lat > 0 {
                        ui.label(
                            RichText::new(format!("{lat} smp latency"))
                                .small()
                                .color(TEXT_DIM),
                        );
                    }
                });
            }

            if let DeviceKind::Builtin { type_id, sample } = &dev.kind {
                if rlive_devices::find(type_id).is_some_and(|b| b.uses_sample) {
                    ui.horizontal(|ui| {
                        let name = sample
                            .as_ref()
                            .and_then(|p| p.file_name())
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "No sample".into());
                        ui.label(RichText::new(format!("Sample: {name}")).color(ACCENT));
                        if ui.small_button("Load…").clicked() {
                            actions.push(Action::LoadSample(dev.id));
                        }
                    });
                }
            }

            // Knobs, in rows.
            let id = ui.scope_id().with(("expanded", dev.id));
            let mut expanded = ui.data(|d| d.get_temp::<bool>(id).unwrap_or(false));
            let params: Vec<&ParamInfo> = rt.params.iter().collect();
            let shown = if expanded {
                params.len()
            } else {
                params.len().min(MAX_KNOBS_COLLAPSED)
            };
            let per_row = if params.len() > 12 { 8 } else { 6 };
            egui::Grid::new(("knobs", dev.id))
                .spacing([2.0, 2.0])
                .show(ui, |ui| {
                    for (i, p) in params.iter().take(shown).enumerate() {
                        let value = session.param_value(chain, dev.id, p);
                        let text = rt.format_value(p, value);
                        if let Some(v) = widgets::knob(ui, p, value, &text) {
                            actions.push(Action::Param(dev.id, p.id, v));
                        }
                        if (i + 1) % per_row == 0 {
                            ui.end_row();
                        }
                    }
                });
            if params.len() > MAX_KNOBS_COLLAPSED {
                let label = if expanded {
                    "Show fewer".to_string()
                } else {
                    format!("Show all {} parameters", params.len())
                };
                if ui.small_button(label).clicked() {
                    expanded = !expanded;
                    ui.data_mut(|d| d.insert_temp(id, expanded));
                }
            }
            if params.is_empty() && rt.error.is_none() {
                ui.label(RichText::new("No parameters").color(TEXT_DIM));
            }
        });
    });
}
