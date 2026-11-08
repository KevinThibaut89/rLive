//! Left-hand browser: built-in devices, plugins and samples. Drag items onto tracks,
//! or double-click to add them to the selected track.

use super::{widgets, BrowserItem, UiState};
use crate::session::{ChainRef, Session};
use crate::settings::Settings;
use egui::{Id, RichText};
use rlive_core::PluginFormat;
use rlive_devices::DeviceRole;
use rlive_project::TrackKind;
use std::path::{Path, PathBuf};

pub fn list_samples(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        if depth > 4 || out.len() > 5000 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, depth + 1, out);
            } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("wav")) {
                out.push(p);
            }
        }
    }
    walk(dir, 0, &mut out);
    out.sort();
    out
}

fn matches(filter: &str, text: &str) -> bool {
    filter.is_empty() || text.to_lowercase().contains(&filter.to_lowercase())
}

/// Apply a browser item to the selected track (double-click action).
pub fn apply(session: &mut Session, ui_state: &mut UiState, item: &BrowserItem) {
    let chain = ui_state.selected.unwrap_or_else(|| {
        let id = session.add_track(TrackKind::Midi, None);
        ChainRef::Track(id)
    });
    match item {
        BrowserItem::Builtin(id) => session.add_builtin(chain, id),
        BrowserItem::Plugin(desc) => session.add_plugin(chain, desc),
        BrowserItem::Sample(path) => {
            // Put the sample into the first empty slot of the selected audio track (or a new one).
            let track = match chain {
                ChainRef::Track(t)
                    if session
                        .project
                        .track(t)
                        .is_some_and(|t| t.kind == TrackKind::Audio) =>
                {
                    t
                }
                _ => session.add_track(TrackKind::Audio, None),
            };
            match session.audio_clip_from_file(path) {
                Ok(clip) => {
                    let slot = session
                        .project
                        .track(track)
                        .and_then(|t| t.clips.iter().position(|c| c.is_none()))
                        .unwrap_or(0);
                    session.set_clip(track, slot, Some(clip));
                    ui_state.selected = Some(ChainRef::Track(track));
                }
                Err(e) => ui_state.status = e,
            }
        }
    }
}

fn item_row(
    ui: &mut egui::Ui,
    _id: Id,
    item: BrowserItem,
    label: RichText,
    hover: &str,
) -> Option<BrowserItem> {
    let text = label.text().to_string();
    let resp = ui.add(
        egui::Label::new(label)
            .selectable(false)
            .sense(egui::Sense::click_and_drag()),
    );
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
    if resp.dragged() {
        // Drag preview following the pointer.
        if let Some(pos) = ui.ctx().pointer_latest_pos() {
            egui::Area::new(Id::unique("browser_drag_preview"))
                .order(egui::Order::Tooltip)
                .fixed_pos(pos + egui::vec2(12.0, 4.0))
                .interactable(false)
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| ui.label(&text));
                });
        }
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }
    let double = resp.double_clicked();
    resp.dnd_set_drag_payload(item.clone());
    resp.on_hover_text(hover);
    double.then_some(item)
}

pub fn show(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    settings: &mut Settings,
) {
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut state.browser_filter).hint_text("Search"));
    });
    ui.add_space(4.0);
    let filter = state.browser_filter.clone();
    let mut activate: Option<BrowserItem> = None;

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (title, role) in [
                ("Instruments", DeviceRole::Instrument),
                ("Audio Effects", DeviceRole::AudioEffect),
            ] {
                egui::CollapsingHeader::new(RichText::new(title).strong())
                    .default_open(true)
                    .show(ui, |ui| {
                        for b in rlive_devices::builtins().iter().filter(|b| b.role == role) {
                            if !matches(&filter, b.name) {
                                continue;
                            }
                            if let Some(it) = item_row(
                                ui,
                                Id::unique(("builtin", b.id)),
                                BrowserItem::Builtin(b.id),
                                RichText::new(b.name),
                                "Drag onto a track or double-click to add",
                            ) {
                                activate = Some(it);
                            }
                        }
                    });
            }

            egui::CollapsingHeader::new(RichText::new("Plugins").strong())
                .default_open(true)
                .show(ui, |ui| {
                    if session.plugins.plugins.is_empty() {
                        ui.label(RichText::new("No plugins scanned yet.").color(widgets::TEXT_DIM));
                        if ui.button("Scan plugins").clicked() {
                            state.request_scan = true;
                        }
                    }
                    for format in PluginFormat::ALL {
                        let list: Vec<_> = session
                            .plugins_of(format)
                            .filter(|p| matches(&filter, &p.name) || matches(&filter, &p.vendor))
                            .cloned()
                            .collect();
                        if list.is_empty() {
                            continue;
                        }
                        egui::CollapsingHeader::new(format!("{} ({})", format.label(), list.len()))
                            .id_salt(("fmt", format.key()))
                            .show(ui, |ui| {
                                let mut vendor = String::new();
                                let mut sorted = list;
                                sorted.sort_by_key(|a| {
                                    (a.vendor.to_lowercase(), a.name.to_lowercase())
                                });
                                for p in sorted {
                                    if p.vendor != vendor {
                                        vendor = p.vendor.clone();
                                        ui.label(
                                            RichText::new(&vendor).small().color(widgets::TEXT_DIM),
                                        );
                                    }
                                    let icon = if p.is_instrument { "[inst] " } else { "" };
                                    let hover = format!(
                                        "{} {} {}\n{}",
                                        p.vendor,
                                        p.name,
                                        p.version,
                                        p.path.display()
                                    );
                                    if let Some(it) = item_row(
                                        ui,
                                        Id::unique(("plugin", format.key(), &p.id)),
                                        BrowserItem::Plugin(p.clone()),
                                        RichText::new(format!("{icon}{}", p.name)),
                                        &hover,
                                    ) {
                                        activate = Some(it);
                                    }
                                }
                            });
                    }
                });

            egui::CollapsingHeader::new(RichText::new("Samples").strong())
                .default_open(true)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let label = settings
                            .sample_dir
                            .as_ref()
                            .and_then(|d| d.file_name())
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "Choose folder…".into());
                        if ui.button(label).clicked() {
                            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                                state.sample_files = list_samples(&dir);
                                settings.sample_dir = Some(dir);
                                settings.save();
                            }
                        }
                        if settings.sample_dir.is_some()
                            && ui
                                .small_button("Refresh")
                                .on_hover_text("Refresh")
                                .clicked()
                        {
                            if let Some(d) = &settings.sample_dir {
                                state.sample_files = list_samples(d);
                            }
                        }
                    });
                    let base = settings.sample_dir.clone().unwrap_or_default();
                    for path in state.sample_files.iter().take(2000) {
                        let rel = path
                            .strip_prefix(&base)
                            .unwrap_or(path)
                            .display()
                            .to_string();
                        if !matches(&filter, &rel) {
                            continue;
                        }
                        let name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        if let Some(it) = item_row(
                            ui,
                            Id::unique(("sample", path)),
                            BrowserItem::Sample(path.clone()),
                            RichText::new(name.clone()),
                            &rel,
                        ) {
                            activate = Some(it);
                        }
                    }
                });
        });

    if let Some(item) = activate {
        apply(session, state, &item);
    }
}
