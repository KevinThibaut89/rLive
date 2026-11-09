//! Collections, Library and Places.

use super::super::theme::{self, *};
use super::super::widgets::info;
use super::super::UiState;
use super::Renaming;
use crate::session::Session;
use crate::settings::Settings;
use egui::{Align2, Pos2, Rect, Sense, Vec2};
use rlive_browser::item::Label;
use rlive_browser::library::LABEL_ICONS;
use rlive_browser::{LibraryData, Op, Scope};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const ROW_H: f32 = 19.0;

pub(super) fn show(
    ui: &mut egui::Ui,
    side: Rect,
    session: &mut Session,
    state: &mut UiState,
    settings: &mut Settings,
) {
    ui.painter().rect_filled(side, 0.0, pal().panel_dark);
    let lib = state
        .browser
        .db
        .as_ref()
        .map_or_else(|| Arc::new(LibraryData::default()), |d| d.library().clone());
    egui::ScrollArea::vertical()
        .id_salt("browser_sidebar")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            collections(ui, &lib, state);
            library(ui, &lib, state);
            places(ui, &lib, session, state, settings);
        });
    drop_folders(ui, side, state);
}

/// A folder dragged in from the OS is added to Places (manual p. 116).
fn drop_folders(ui: &egui::Ui, side: Rect, state: &mut UiState) {
    let (files, pos) = ui.input(|i| (i.raw.dropped_files.clone(), i.pointer.latest_pos()));
    let inside = pos.is_some_and(|p| side.contains(p));
    if !inside {
        return;
    }
    for f in files {
        let path = f.path();
        if path.is_dir() {
            state.browser.op(Op::AddFolder(path.to_path_buf()));
        }
    }
}

fn header(ui: &mut egui::Ui, text: &str, editing: Option<&mut bool>, busy: bool) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 18.0), Sense::hover());
    ui.painter().text(
        Pos2::new(rect.left() + 6.0, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        theme::font_bold(9.5),
        pal().text_faint,
    );
    if busy {
        let spot = Rect::from_center_size(
            rect.right_center() - Vec2::new(10.0, 0.0),
            Vec2::splat(10.0),
        );
        ui.put(spot, egui::Spinner::new().size(9.0));
    }
    if let Some(on) = editing {
        let spot = Rect::from_min_size(
            Pos2::new(rect.right() - 28.0, rect.top() + 2.0),
            Vec2::new(26.0, 14.0),
        );
        let r = ui.interact(
            spot,
            ui.scope_id().with(("edit_header", text)),
            Sense::click(),
        );
        ui.painter().text(
            spot.center(),
            Align2::CENTER_CENTER,
            if *on { "Done" } else { "Edit" },
            theme::font(9.5),
            if r.hovered() {
                pal().text
            } else {
                pal().text_faint
            },
        );
        if info(r, "Edit — choose which labels show here").clicked() {
            *on = !*on;
        }
    }
}

/// One label's row. Returns the response.
#[allow(clippy::too_many_arguments)]
fn row(
    ui: &mut egui::Ui,
    text: &str,
    selected: bool,
    greyed: bool,
    swatch: Option<[u8; 3]>,
    edit_shown: Option<bool>,
) -> (egui::Response, Option<bool>) {
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::click());
    if selected {
        ui.painter().rect_filled(rect, 0.0, shade(0x46525e));
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, 0.0, pal().raised);
    }
    let mut x = rect.left() + 8.0;
    if let Some(c) = swatch {
        let r = Rect::from_center_size(Pos2::new(x + 4.0, rect.center().y), Vec2::splat(8.0));
        super::color_square(ui, r, c);
        x += 12.0;
    }
    let color = if greyed {
        pal().text_faint
    } else if selected {
        shade(0xffffff)
    } else {
        pal().text
    };
    ui.painter().with_clip_rect(rect).text(
        Pos2::new(x, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        theme::font(11.0),
        color,
    );
    let mut toggled = None;
    if let Some(on) = edit_shown {
        let spot = Rect::from_center_size(
            rect.right_center() - Vec2::new(12.0, 0.0),
            Vec2::splat(12.0),
        );
        let r = ui.interact(
            spot,
            ui.scope_id().with(("show_label", text)),
            Sense::click(),
        );
        ui.painter().rect_stroke(
            spot,
            1.0,
            egui::Stroke::new(1.0, pal().text_dim),
            egui::StrokeKind::Inside,
        );
        if on {
            ui.painter().text(
                spot.center(),
                Align2::CENTER_CENTER,
                "✓",
                theme::font(10.0),
                pal().accent,
            );
        }
        if r.clicked() {
            toggled = Some(!on);
        }
    }
    (resp, toggled)
}

fn rename_ui(ui: &mut egui::Ui, state: &mut UiState, key: &str, apply: impl FnOnce(String) -> Op) {
    let Some(renaming) = &mut state.browser.renaming else {
        return;
    };
    if renaming.key != key {
        return;
    }
    let edit = ui.add(egui::TextEdit::singleline(&mut renaming.text).font(theme::font(11.0)));
    edit.request_focus();
    if edit.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
        let text = state.browser.renaming.take().expect("just read").text;
        state.browser.op(apply(text));
    }
}

fn start_rename(state: &mut UiState, key: &str, text: &str) {
    state.browser.renaming = Some(Renaming {
        key: key.to_string(),
        text: text.to_string(),
    });
}

/// The mark a label wears, if the user gave it one.
fn with_icon(icon: Option<&str>, name: &str) -> String {
    let glyph = match icon {
        Some("Folder") => "▫",
        Some("Star") => "★",
        Some("Heart") => "♥",
        Some("Note") => "♪",
        Some("Wave") => "~",
        Some("Drum") => "◉",
        Some("Knob") => "◌",
        Some("Tag") => "#",
        _ => return name.to_string(),
    };
    format!("{glyph} {name}")
}

fn collections(ui: &mut egui::Ui, lib: &Arc<LibraryData>, state: &mut UiState) {
    let mut editing = state.browser.edit_collections;
    header(ui, "COLLECTIONS", Some(&mut editing), false);
    state.browser.edit_collections = editing;
    for (i, c) in lib.collections.iter().enumerate() {
        if !c.visible && !editing {
            continue;
        }
        let key = format!("collection:{i}");
        if state
            .browser
            .renaming
            .as_ref()
            .is_some_and(|r| r.key == key)
        {
            rename_ui(ui, state, &key, |t| Op::RenameCollection(i as u8, t));
            continue;
        }
        let scope = Scope::Collection(i as u8);
        let selected = state.browser.model.nav.scope == scope;
        let (resp, toggled) = row(
            ui,
            &c.name,
            selected,
            false,
            Some(c.color),
            editing.then_some(c.visible),
        );
        if let Some(on) = toggled {
            state.browser.op(Op::SetCollectionVisible(i as u8, on));
        }
        let name = c.name.clone();
        resp.context_menu(|ui| {
            if ui.button("Rename").clicked() {
                start_rename(state, &key, &name);
                ui.close();
            }
        });
        if info(resp, format!("{} — everything with this colour", c.name)).clicked() {
            state.browser.model.navigate(scope);
        }
    }
}

fn icon_menu(ui: &mut egui::Ui, state: &mut UiState, apply: impl Fn(String) -> Op) {
    ui.menu_button("Change Icon", |ui| {
        for icon in LABEL_ICONS {
            if ui.button(*icon).clicked() {
                state.browser.op(apply((*icon).to_string()));
                ui.close();
            }
        }
    });
}

fn library(ui: &mut egui::Ui, lib: &Arc<LibraryData>, state: &mut UiState) {
    let mut editing = state.browser.edit_library;
    header(ui, "LIBRARY", Some(&mut editing), false);
    state.browser.edit_library = editing;
    for label in Label::ALL {
        let key = label.name().to_string();
        if lib.label_hidden(&key) && !editing {
            continue;
        }
        if state
            .browser
            .renaming
            .as_ref()
            .is_some_and(|r| r.key == key)
        {
            rename_ui(ui, state, &key.clone(), |t| Op::RenameLabel(key.clone(), t));
            continue;
        }
        let name = lib.label_name(&key, label.name());
        let scope = Scope::Label(label);
        let selected = state.browser.model.nav.scope == scope;
        let shown = with_icon(lib.labels.get(&key).and_then(|p| p.icon.as_deref()), &name);
        let (resp, toggled) = row(
            ui,
            &shown,
            selected,
            false,
            None,
            editing.then_some(!lib.label_hidden(&key)),
        );
        if let Some(on) = toggled {
            state.browser.op(Op::SetLabelHidden(key.clone(), !on));
        }
        resp.context_menu(|ui| {
            if ui.button("Rename").clicked() {
                start_rename(state, &key, &name);
                ui.close();
            }
            let k = key.clone();
            icon_menu(ui, state, move |icon| Op::SetLabelIcon(k.clone(), icon));
        });
        if info(resp, format!("{} — show this part of the Library", name)).clicked() {
            state.browser.model.navigate(scope);
        }
    }
    for l in &lib.custom_labels {
        let key = format!("custom:{}", l.id);
        if state
            .browser
            .renaming
            .as_ref()
            .is_some_and(|r| r.key == key)
        {
            let id = l.id;
            rename_ui(ui, state, &key, move |t| Op::RenameCustomLabel(id, t));
            continue;
        }
        let scope = Scope::Custom(l.id);
        let selected = state.browser.model.nav.scope == scope;
        let shown = with_icon(Some(&l.icon), &l.name);
        let (resp, _) = row(ui, &shown, selected, false, None, None);
        let (id, name) = (l.id, l.name.clone());
        resp.context_menu(|ui| {
            if ui.button("Rename").clicked() {
                start_rename(state, &key, &name);
                ui.close();
            }
            icon_menu(ui, state, move |icon| Op::SetCustomLabelIcon(id, icon));
            if ui.button("Delete").clicked() {
                state.browser.op(Op::DeleteCustomLabel(id));
                ui.close();
            }
        });
        if info(resp, format!("{} — a saved search", l.name)).clicked() {
            state.browser.model.navigate(scope);
        }
    }
}

fn is_missing(state: &UiState, path: &Path) -> bool {
    state
        .browser
        .db
        .as_ref()
        .and_then(|d| d.scan_of(path))
        .is_some_and(|s| s.missing)
}

fn reveal_in_files(path: &Path) {
    #[cfg(target_os = "macos")]
    let opener = "open";
    #[cfg(not(target_os = "macos"))]
    let opener = "xdg-open";
    let _ = std::process::Command::new(opener).arg(path).spawn();
}

/// Every label the sidebar shows, in order, for moving through them from the keyboard.
pub(super) fn scopes(lib: &LibraryData, library: &Path) -> Vec<Scope> {
    let mut out: Vec<Scope> = lib
        .collections
        .iter()
        .enumerate()
        .filter(|(_, c)| c.visible)
        .map(|(i, _)| Scope::Collection(i as u8))
        .collect();
    out.extend(
        Label::ALL
            .iter()
            .filter(|l| !lib.label_hidden(l.name()))
            .map(|l| Scope::Label(*l)),
    );
    out.extend(lib.custom_labels.iter().map(|l| Scope::Custom(l.id)));
    out.push(Scope::Place(library.to_path_buf()));
    out.push(Scope::Project);
    out.extend(lib.folders.iter().map(|f| Scope::Place(f.path.clone())));
    out
}

fn places(
    ui: &mut egui::Ui,
    lib: &Arc<LibraryData>,
    session: &mut Session,
    state: &mut UiState,
    settings: &mut Settings,
) {
    header(ui, "PLACES", None, state.browser.scanning());
    // The Library's own folder.
    let library = state.browser.library_path.clone();
    let scope = Scope::Place(library.clone());
    let selected = state.browser.model.nav.scope == scope;
    let (resp, _) = row(ui, "User Library", selected, false, None, None);
    resp.context_menu(|ui| {
        if ui.button("Show in Finder").clicked() {
            reveal_in_files(&library);
            ui.close();
        }
        if ui.button("Set Location…").clicked() {
            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                settings.user_library = Some(path.clone());
                settings.save();
                state.browser.set_user_library(session, path);
            }
            ui.close();
        }
    });
    if info(resp, "User Library — everything you've saved yourself").clicked() {
        state.browser.model.navigate(scope);
    }
    // The Set that's open.
    let scope = Scope::Project;
    let selected = state.browser.model.nav.scope == scope;
    let (resp, _) = row(ui, "Current Project", selected, false, None, None);
    let project = session
        .path
        .as_deref()
        .and_then(Path::parent)
        .map(Path::to_path_buf);
    resp.context_menu(|ui| {
        let can = project.is_some();
        if ui
            .add_enabled(can, egui::Button::new("Show in Finder"))
            .clicked()
        {
            if let Some(p) = &project {
                reveal_in_files(p);
            }
            ui.close();
        }
    });
    if info(
        resp,
        "Current Project — the open Set's tracks, clips and files",
    )
    .clicked()
    {
        state.browser.model.navigate(scope);
    }
    for f in &lib.folders {
        let name = f.path.file_name().map_or_else(
            || f.path.display().to_string(),
            |n| n.to_string_lossy().to_string(),
        );
        let scope = Scope::Place(f.path.clone());
        let selected = state.browser.model.nav.scope == scope;
        let missing = is_missing(state, &f.path);
        let (resp, _) = row(ui, &name, selected, missing, None, None);
        let path = f.path.clone();
        resp.context_menu(|ui| {
            if ui.button("Locate Folder…").clicked() {
                if let Some(new) = rfd::FileDialog::new().pick_folder() {
                    state.browser.op(Op::LocateFolder(path.clone(), new));
                }
                ui.close();
            }
            if ui.button("Remove from Sidebar").clicked() {
                state.browser.op(Op::RemoveFolder(path.clone()));
                ui.close();
            }
            if ui.button("Show in Finder").clicked() {
                reveal_in_files(&path);
                ui.close();
            }
        });
        let hover = if missing {
            format!("{name} — not found. Use Locate Folder to point it at where it is now")
        } else {
            format!("{name} — a folder you added")
        };
        if info(resp, hover).clicked() && !missing {
            state.browser.model.navigate(scope);
        }
    }
    let (resp, _) = row(ui, "Add Folder…", false, false, None, None);
    if info(resp, "Add Folder — add a folder from any drive to Places").clicked() {
        if let Some(path) = rfd::FileDialog::new().pick_folder() {
            add_folder(state, path);
        }
    }
}

fn add_folder(state: &mut UiState, path: PathBuf) {
    state.browser.op(Op::AddFolder(path));
}
