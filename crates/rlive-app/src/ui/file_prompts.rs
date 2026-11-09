//! The small dialogs of the File menu: saving changes, Collect All, templates, Open Recent.

use super::{pal, CopyPrompt, FileAction, RliveApp};
use crate::session::ClipRef;
use crate::settings::CollectFiles;
use std::path::Path;

impl RliveApp {
    fn save_changes_prompt(&mut self, ctx: &egui::Context) {
        let Some(action) = self.ui.files.pending_action.clone() else {
            return;
        };
        let name = self.set_file_name();
        let mut result = None;
        egui::Modal::new(egui::Id::unique("save changes")).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.label(format!("Save changes to \u{201c}{name}\u{201d}?"));
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    result = Some(true);
                }
                if ui.button("Don't Save").clicked() {
                    result = Some(false);
                }
                if ui.button("Cancel").clicked() {
                    self.ui.files.pending_action = None;
                }
            });
        });
        match result {
            Some(true) => {
                if self.save_before_action() {
                    self.ui.files.pending_action = None;
                    self.run_file_action(action);
                }
            }
            Some(false) => {
                self.ui.files.pending_action = None;
                self.run_file_action(action);
            }
            None => {}
        }
    }

    fn collect_all_prompt(&mut self, ctx: &egui::Context) {
        let Some(mut options) = self.ui.files.collect_prompt else {
            return;
        };
        let mut result = None;
        egui::Modal::new(egui::Id::unique("collect all")).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.label("Collect All and Save");
            ui.label(
                egui::RichText::new("Copy files into this Set's Project from:")
                    .small()
                    .color(pal().text_dim),
            );
            ui.checkbox(&mut options.elsewhere, "Elsewhere");
            ui.checkbox(&mut options.other_projects, "Other Projects");
            ui.checkbox(&mut options.user_library, "User Library");
            ui.horizontal(|ui| {
                if ui.button("OK").clicked() {
                    result = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    result = Some(false);
                }
            });
        });
        self.ui.files.collect_prompt = Some(options);
        match result {
            Some(true) => {
                self.ui.files.collect_prompt = None;
                if self.session.path.is_none() && self.save_as_dialog().is_none() {
                    return;
                }
                self.ui.status = match self.session.collect_all_and_save(options) {
                    Ok(r) => format!(
                        "Collected {} files{}",
                        r.copied,
                        if r.missing.is_empty() {
                            String::new()
                        } else {
                            format!("; {} are missing", r.missing.len())
                        }
                    ),
                    Err(e) => format!("Collect failed: {e}"),
                };
            }
            Some(false) => self.ui.files.collect_prompt = None,
            None => {}
        }
    }

    fn default_set_prompt(&mut self, ctx: &egui::Context) {
        if !self.ui.files.confirm_default {
            return;
        }
        let mut result = None;
        egui::Modal::new(egui::Id::unique("default set")).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.label("Save this Set as the Default Set? New Sets will start from it.");
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    result = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    result = Some(false);
                }
            });
        });
        if let Some(save) = result {
            self.ui.files.confirm_default = false;
            if save {
                self.ui.status = match self.session.save_as_default_set() {
                    Ok(_) => "Saved as the Default Set".into(),
                    Err(e) => e,
                };
            }
        }
    }

    fn template_name_prompt(&mut self, ctx: &egui::Context) {
        let Some(mut name) = self.ui.files.template_name.take() else {
            return;
        };
        let mut result = None;
        egui::Modal::new(egui::Id::unique("template name")).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.label("Save Live Set As Template");
            ui.text_edit_singleline(&mut name);
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    result = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    result = Some(false);
                }
            });
        });
        match result {
            Some(true) => {
                self.ui.status = match self.session.save_as_template(name.trim()) {
                    Ok(p) => format!("Saved template {}", p.display()),
                    Err(e) => e,
                };
                if let Some(db) = &self.ui.browser.db {
                    db.rescan();
                }
            }
            Some(false) => {}
            None => self.ui.files.template_name = Some(name),
        }
    }

    fn open_recent_window(&mut self, ctx: &egui::Context) {
        if !self.ui.files.show_recent {
            return;
        }
        let mut open = true;
        let mut chosen = None;
        egui::Window::new("Open Recent")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                if self.settings.recent_sets.is_empty() {
                    ui.label(egui::RichText::new("No recent Sets").color(pal().text_dim));
                }
                for path in self.settings.recent_sets.clone() {
                    let label = path
                        .file_stem()
                        .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
                    let resp = ui.add_enabled(path.exists(), egui::Button::new(label));
                    if resp.clicked() {
                        chosen = Some(path.clone());
                    }
                    resp.on_hover_text(path.display().to_string());
                }
            });
        self.ui.files.show_recent = open;
        if let Some(path) = chosen {
            self.ui.files.show_recent = false;
            self.request_file_action(FileAction::Open(path));
        }
    }

    fn copy_prompt_dialog(&mut self, ctx: &egui::Context) {
        if self.ui.files.copy_prompt.is_none() {
            return;
        }
        let mut result = None;
        egui::Modal::new(egui::Id::unique("copy on export")).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.label("Copy the files this depends on to the destination?");
            ui.horizontal(|ui| {
                if ui.button("Copy").clicked() {
                    result = Some(Some(true));
                }
                if ui.button("Don't Copy").clicked() {
                    result = Some(Some(false));
                }
                if ui.button("Cancel").clicked() {
                    result = Some(None);
                }
            });
        });
        if let Some(answer) = result {
            let Some(prompt) = self.ui.files.copy_prompt.take() else {
                return;
            };
            if let Some(copy) = answer {
                self.run_copy_prompt(prompt, copy);
            }
        }
    }

    fn run_copy_prompt(&mut self, prompt: CopyPrompt, copy: bool) {
        let before = self.session.collect_on_export;
        self.session.collect_on_export = copy;
        let result = match prompt {
            CopyPrompt::Device { chain, id, path } => self
                .session
                .save_device_preset_as(chain, id, &path)
                .map(|()| path),
            CopyPrompt::Clip { clip, dir, name } => {
                self.session.save_clip_to(clip, &dir, &name, copy)
            }
        };
        self.session.collect_on_export = before;
        self.ui.status = match result {
            Ok(p) => format!("Saved {}", p.display()),
            Err(e) => e,
        };
        if let Some(db) = &self.ui.browser.db {
            db.rescan();
        }
    }

    pub(super) fn file_prompts(&mut self, ctx: &egui::Context) {
        self.save_changes_prompt(ctx);
        self.collect_all_prompt(ctx);
        self.default_set_prompt(ctx);
        self.template_name_prompt(ctx);
        self.open_recent_window(ctx);
        self.copy_prompt_dialog(ctx);
    }

    /// Keeps the window title on the Set's name and its Project.
    pub(super) fn window_title(&mut self, ctx: &egui::Context) {
        let name = self.session.path.as_deref().map_or_else(
            || "Untitled".to_string(),
            |p| {
                p.file_stem().map_or_else(
                    || "Untitled".to_string(),
                    |s| s.to_string_lossy().into_owned(),
                )
            },
        );
        let dirty = if self.session.dirty { "*" } else { "" };
        let title = match self.session.project_name() {
            Some(project) => format!("{name}{dirty} [{project}] - rLive"),
            None => format!("{name}{dirty} - rLive"),
        };
        if title != self.ui.files.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.ui.files.title = title;
        }
    }

    /// Asks about unsaved changes when the window is closing.
    pub(super) fn confirm_close(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested())
            && self.session.dirty
            && !self.ui.files.quitting
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.ui.files.pending_action = Some(FileAction::Quit);
        }
        if std::mem::take(&mut self.pending_quit) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// A Pack dropped on the window installs it.
    pub(super) fn install_dropped_packs(&mut self, ctx: &egui::Context) {
        let packs: Vec<std::path::PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .filter(|p| is_pack(p))
                .collect()
        });
        for pack in packs {
            self.install_pack_from(&pack);
        }
    }

    /// Saves where a clip was dropped, following Collect Files on Export.
    pub(super) fn save_clip_with_policy(&mut self, clip: ClipRef, dir: &Path, name: &str) {
        match self.settings.collect_on_export {
            CollectFiles::Ask => {
                self.ui.files.copy_prompt = Some(CopyPrompt::Clip {
                    clip,
                    dir: dir.to_path_buf(),
                    name: name.to_string(),
                });
            }
            CollectFiles::Always => self.run_copy_prompt(
                CopyPrompt::Clip {
                    clip,
                    dir: dir.to_path_buf(),
                    name: name.to_string(),
                },
                true,
            ),
            CollectFiles::Never => self.run_copy_prompt(
                CopyPrompt::Clip {
                    clip,
                    dir: dir.to_path_buf(),
                    name: name.to_string(),
                },
                false,
            ),
        }
    }
}

fn is_pack(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(rlive_browser::files::PACK_EXTENSION))
}
