//! The File Manager: the Set's files, Projects and the User Library (manual pp. 140-161).

use super::{pal, widgets, BrowserItem, RliveApp};
use crate::session::{
    CollectOptions, FileEntry, FileLocation, ManagedScope, ScopeReport, SearchOptions,
};
use egui::{RichText, Sense, Vec2};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// What the File Manager window remembers.
pub struct FileManagerUi {
    pub open: bool,
    pub scope: ManagedScope,
    report: Option<ScopeReport>,
    list: Vec<FileEntry>,
    expanded: HashSet<PathBuf>,
    show_files: bool,
    search: SearchOptions,
    collect: CollectOptions,
    delete: Option<(String, Vec<PathBuf>)>,
    pub status: String,
}

impl Default for FileManagerUi {
    fn default() -> Self {
        Self {
            open: false,
            scope: ManagedScope::Set,
            report: None,
            list: Vec::new(),
            expanded: HashSet::new(),
            show_files: false,
            search: SearchOptions {
                project: true,
                ..SearchOptions::default()
            },
            collect: CollectOptions::default(),
            delete: None,
            status: String::new(),
        }
    }
}

impl FileManagerUi {
    pub fn show_scope(&mut self, scope: ManagedScope) {
        self.scope = scope;
        self.open = true;
        self.report = None;
    }

    /// Opens at the file reference list, showing one file.
    pub fn show_file(&mut self, path: PathBuf) {
        self.show_scope(ManagedScope::Set);
        self.show_files = true;
        self.expanded.insert(path);
    }

    pub fn is_blocking(&self) -> bool {
        self.delete.is_some()
    }

    fn invalidate(&mut self) {
        self.report = None;
    }
}

fn human_size(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    if bytes as f64 >= MB * 1024.0 {
        format!("{:.1} GB", bytes as f64 / MB / 1024.0)
    } else if bytes as f64 >= MB {
        format!("{:.1} MB", bytes as f64 / MB)
    } else {
        format!("{} kB", bytes / 1024)
    }
}

fn reveal_in_finder(path: &Path) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .spawn();
    #[cfg(not(target_os = "macos"))]
    let _ = std::process::Command::new("xdg-open")
        .arg(path.parent().unwrap_or(path))
        .spawn();
}

/// A file dropped from the browser onto a row.
fn dropped_file(resp: &egui::Response) -> Option<PathBuf> {
    match &*widgets::released::<BrowserItem>(resp)? {
        BrowserItem::Sample(path) => Some(path.clone()),
        _ => None,
    }
}

fn scope_name(scope: &ManagedScope) -> String {
    match scope {
        ManagedScope::Set => "Live Set".into(),
        ManagedScope::UserLibrary => "User Library".into(),
        ManagedScope::Project(root) => root
            .file_name()
            .map_or_else(|| "Project".into(), |n| n.to_string_lossy().into_owned()),
    }
}

impl RliveApp {
    fn scope_buttons(&mut self, ui: &mut egui::Ui) {
        let root = self.session.project_root();
        ui.horizontal(|ui| {
            let fm = &mut self.ui.files.manager;
            if ui
                .selectable_label(fm.scope == ManagedScope::Set, "Manage Set")
                .clicked()
            {
                fm.show_scope(ManagedScope::Set);
            }
            let is_project = matches!(fm.scope, ManagedScope::Project(_));
            let project = ui.add_enabled(
                root.is_some(),
                egui::Button::selectable(is_project, "Manage Project"),
            );
            if project.clicked() {
                if let Some(r) = root.clone() {
                    fm.show_scope(ManagedScope::Project(r));
                }
            }
            if ui
                .selectable_label(fm.scope == ManagedScope::UserLibrary, "Manage User Library")
                .clicked()
            {
                fm.show_scope(ManagedScope::UserLibrary);
            }
        });
    }

    fn missing_section(&mut self, ui: &mut egui::Ui) {
        let scope = self.ui.files.manager.scope.clone();
        let Some(report) = self.ui.files.manager.report.clone() else {
            return;
        };
        let title = format!("Missing Files ({})", report.missing.len());
        egui::CollapsingHeader::new(RichText::new(title).color(if report.missing.is_empty() {
            pal().text
        } else {
            pal().arm_red
        }))
        .default_open(!report.missing.is_empty())
        .show(ui, |ui| {
            for path in &report.missing {
                let name = path
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                let (resp, candidates) = ui
                    .horizontal(|ui| {
                        let resp =
                            ui.add(egui::Label::new(RichText::new(&name)).sense(Sense::click()));
                        ui.label(
                            RichText::new("drop a file here")
                                .small()
                                .color(pal().text_dim),
                        );
                        let candidates = self.session.candidates_for(path).to_vec();
                        if !candidates.is_empty() {
                            ui.menu_button(format!("{} candidates", candidates.len()), |ui| {
                                for c in &candidates {
                                    if ui.button(c.display().to_string()).clicked() {
                                        self.session.relink_file(path, c);
                                        self.ui.files.manager.invalidate();
                                        ui.close();
                                    }
                                }
                            });
                        }
                        (resp, candidates)
                    })
                    .inner;
                let _ = candidates;
                if let Some(file) = dropped_file(&resp) {
                    self.session.relink_file(path, &file);
                    self.ui.files.manager.invalidate();
                }
                widgets::info(resp, path.display().to_string());
            }
            ui.add_space(4.0);
            ui.label(RichText::new("Automatic Search").strong());
            let fm = &mut self.ui.files.manager;
            ui.horizontal(|ui| {
                ui.label("Search Folder");
                if ui
                    .button(
                        fm.search
                            .folder
                            .as_ref()
                            .map_or_else(|| "Set Folder…".to_string(), |f| f.display().to_string()),
                    )
                    .clicked()
                {
                    if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                        fm.search.folder = Some(folder);
                    }
                }
            });
            ui.checkbox(&mut fm.search.project, "Search Project");
            ui.checkbox(&mut fm.search.library, "Search Library");
            ui.checkbox(&mut fm.search.rescan, "Fully Rescan Folders");
            let busy = self.session.file_task_running();
            if ui.add_enabled(!busy, egui::Button::new("Go")).clicked() {
                let options = fm.search.clone();
                match scope {
                    ManagedScope::Set => self.session.start_search(&options),
                    _ => match self.session.repair_scope(&scope, &options) {
                        Ok(r) => {
                            self.ui.files.manager.status = format!(
                                "{} relinked, {} not found",
                                r.relinked.len(),
                                r.not_found.len()
                            );
                            self.ui.files.manager.invalidate();
                        }
                        Err(e) => self.ui.files.manager.status = e,
                    },
                }
            }
            if busy {
                ui.spinner();
            }
        });
    }

    fn external_section(&mut self, ui: &mut egui::Ui) {
        let Some(report) = self.ui.files.manager.report.clone() else {
            return;
        };
        egui::CollapsingHeader::new("External Files")
            .default_open(true)
            .show(ui, |ui| {
                let summaries: Vec<_> = report
                    .external
                    .iter()
                    .filter(|s| s.location != FileLocation::Project)
                    .copied()
                    .collect();
                if summaries.is_empty() {
                    ui.label(RichText::new("Nothing outside this Project.").color(pal().text_dim));
                }
                egui::Grid::new("external_files")
                    .num_columns(4)
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        for s in &summaries {
                            ui.label(s.location.label());
                            ui.label(format!("{} files, {}", s.files, human_size(s.bytes)));
                            if ui.small_button("Show").clicked() {
                                if let Some(entry) = self
                                    .session
                                    .file_list()
                                    .iter()
                                    .find(|e| e.location == s.location && !e.missing)
                                {
                                    reveal_in_finder(&entry.path);
                                }
                            }
                            let c = &mut self.ui.files.manager.collect;
                            match s.location {
                                FileLocation::UserLibrary => {
                                    ui.checkbox(&mut c.user_library, "Collect Into Project");
                                }
                                FileLocation::OtherProject => {
                                    ui.checkbox(&mut c.other_projects, "Collect Into Project");
                                }
                                FileLocation::Elsewhere => {
                                    ui.checkbox(&mut c.elsewhere, "Collect Into Project");
                                }
                                FileLocation::Project => {
                                    ui.label("");
                                }
                            }
                            ui.end_row();
                        }
                    });
                ui.add_space(4.0);
                if ui.button("Collect and Save").clicked() {
                    let scope = self.ui.files.manager.scope.clone();
                    let options = self.ui.files.manager.collect;
                    self.ui.files.manager.status = match self.session.collect_scope(&scope, options)
                    {
                        Ok(r) => format!("Collected {} files ({})", r.copied, human_size(r.bytes)),
                        Err(e) => e,
                    };
                    self.ui.files.manager.invalidate();
                }
            });
    }

    fn file_list_section(&mut self, ui: &mut egui::Ui) {
        let fm = &mut self.ui.files.manager;
        ui.checkbox(&mut fm.show_files, "View Files");
        if !fm.show_files {
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt("fm_files")
            .max_height(220.0)
            .show(ui, |ui| {
                for entry in self.ui.files.manager.list.clone() {
                    let unfolded = self.ui.files.manager.expanded.contains(&entry.path);
                    let row = ui
                        .horizontal(|ui| {
                            if ui.small_button(if unfolded { "▾" } else { "▸" }).clicked() {
                                let e = &mut self.ui.files.manager.expanded;
                                if unfolded {
                                    e.remove(&entry.path);
                                } else {
                                    e.insert(entry.path.clone());
                                }
                            }
                            let resp = ui.add(
                                egui::Label::new(RichText::new(&entry.name)).sense(Sense::hover()),
                            );
                            let at = if entry.missing {
                                ("Missing", pal().arm_red)
                            } else {
                                (entry.location.label(), pal().text_dim)
                            };
                            ui.label(RichText::new(at.0).color(at.1).small());
                            if let Some(size) = entry.size {
                                ui.label(
                                    RichText::new(human_size(size))
                                        .small()
                                        .color(pal().text_dim),
                                );
                            }
                            let edit = !entry.missing && ui.small_button("Edit").clicked();
                            (resp, edit)
                        })
                        .inner;
                    let (resp, edit) = row;
                    if edit {
                        self.edit_first_clip_of(&entry.path);
                    }
                    if let Some(file) = dropped_file(&resp) {
                        self.ui.status = format!(
                            "Replaced {} in {} places",
                            entry.name,
                            self.session.replace_file(&entry.path, &file)
                        );
                        self.ui.files.manager.invalidate();
                    }
                    widgets::info(resp, entry.path.display().to_string());
                    if unfolded {
                        ui.indent(("uses", &entry.path), |ui| {
                            for place in &entry.uses {
                                ui.label(RichText::new(place).small().color(pal().text_dim));
                            }
                        });
                    }
                }
            });
    }

    fn edit_first_clip_of(&mut self, path: &Path) {
        let clip = self.session.clip_using_file(path);
        if let Some(r) = clip {
            if let Err(e) = self.session.edit_sample_externally(r) {
                self.ui.files.manager.status = e;
            }
        } else {
            self.ui.files.manager.status = "No clip uses that file to edit".into();
        }
    }

    fn unused_section(&mut self, ui: &mut egui::Ui) {
        let Some(report) = self.ui.files.manager.report.clone() else {
            return;
        };
        egui::CollapsingHeader::new(format!(
            "Unused Files ({})",
            report.unused.iter().map(|g| g.files.len()).sum::<usize>()
        ))
        .show(ui, |ui| {
            egui::Grid::new("unused")
                .num_columns(4)
                .spacing([12.0, 4.0])
                .show(ui, |ui| {
                    for group in &report.unused {
                        ui.label(&group.category);
                        ui.label(format!("{} files", group.files.len()));
                        if ui.small_button("Show").clicked() {
                            if let Some(f) = group.files.first() {
                                reveal_in_finder(f);
                            }
                        }
                        if ui.small_button("Delete…").clicked() {
                            self.ui.files.manager.delete =
                                Some((group.category.clone(), group.files.clone()));
                        }
                        ui.end_row();
                    }
                });
        });
    }

    fn packing_section(&mut self, ui: &mut egui::Ui) {
        let ManagedScope::Project(root) = self.ui.files.manager.scope.clone() else {
            return;
        };
        egui::CollapsingHeader::new("Packing").show(ui, |ui| {
            ui.label(
                RichText::new("External files are not packed. Collect them first.")
                    .small()
                    .color(pal().text_dim),
            );
            let busy = self.session.file_task_running();
            if ui
                .add_enabled(!busy, egui::Button::new("Create Pack…"))
                .clicked()
            {
                let stem = root.file_name().map_or_else(
                    || "Project".to_string(),
                    |n| n.to_string_lossy().into_owned(),
                );
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("rLive Pack", &[rlive_browser::files::PACK_EXTENSION])
                    .set_file_name(format!("{stem}.{}", rlive_browser::files::PACK_EXTENSION))
                    .save_file()
                {
                    self.session.start_pack(root.clone(), path);
                    self.ui.files.manager.status = "Packing…".into();
                }
            }
        });
    }

    /// The File Manager, if it's open.
    pub(super) fn file_manager_window(&mut self, ctx: &egui::Context) {
        if let Some(path) = self.session.file_manager_target.take() {
            self.ui.files.manager.show_file(path);
        }
        if !self.ui.files.manager.open {
            return;
        }
        if self.ui.files.manager.report.is_none() {
            let scope = self.ui.files.manager.scope.clone();
            self.ui.files.manager.report = Some(self.session.scope_report(&scope));
            self.ui.files.manager.list = self.session.file_list();
        }
        let mut open = self.ui.files.manager.open;
        egui::Window::new(format!(
            "File Manager — {}",
            scope_name(&self.ui.files.manager.scope)
        ))
        .open(&mut open)
        .default_width(460.0)
        .resizable(true)
        .show(ctx, |ui| {
            self.scope_buttons(ui);
            ui.separator();
            self.missing_section(ui);
            self.external_section(ui);
            if self.ui.files.manager.scope == ManagedScope::Set {
                self.file_list_section(ui);
            } else {
                self.unused_section(ui);
                self.packing_section(ui);
            }
            let status = self.ui.files.manager.status.clone();
            if !status.is_empty() {
                ui.separator();
                ui.label(RichText::new(status).small().color(pal().text_dim));
            }
            let _ = ui.allocate_response(Vec2::ZERO, Sense::hover());
        });
        self.ui.files.manager.open = open;
        self.confirm_delete_unused(ctx);
    }

    fn confirm_delete_unused(&mut self, ctx: &egui::Context) {
        let Some((category, files)) = self.ui.files.manager.delete.clone() else {
            return;
        };
        let mut close = false;
        egui::Modal::new(egui::Id::unique("delete unused")).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.label(format!(
                "Move {} unused {category} files to the Trash? Other Projects could still use them, so collect those first.",
                files.len()
            ));
            ui.horizontal(|ui| {
                if ui.button("Move to Trash").clicked() {
                    self.ui.files.manager.status = match self.session.trash_unused(&files) {
                        Ok(n) => format!("Moved {n} files to the Trash"),
                        Err(e) => e,
                    };
                    self.ui.files.manager.invalidate();
                    close = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });
        if close {
            self.ui.files.manager.delete = None;
        }
    }
}
