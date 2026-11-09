//! The browser (manual ch. 4): sidebar, Filter View, content pane and Preview Tab.
//!
//! What's indexed and how it's found is `rlive_browser`'s, on threads of its own. Everything here
//! reads what the worker last published and sends it requests, so nothing here waits on a disk.

mod content;
mod filters;
mod hot_swap;
mod load;
mod preview;
mod sidebar;
#[cfg(test)]
mod tests;

pub use hot_swap::{
    exit_hot_swap, poll_requests, toggle_drum_pad_target, toggle_hot_swap, SwapRequest,
};
pub use load::{apply_selection, drop_item, item_of, load_tuning, AssetSource, PresetSource};
pub use preview::start as preview_selected;

use super::theme::{self, *};
use super::widgets::{self, info};
use super::UiState;
use crate::session::Session;
use crate::settings::Settings;
use egui::{Align2, Pos2, Rect, Sense, Stroke, UiBuilder, Vec2};
use rlive_browser::catalog::{self, PluginEntry};
use rlive_browser::db::Config;
use rlive_browser::defaults::UserLibrary;
use rlive_browser::item::{Item, ItemKey};
use rlive_browser::query::{complete_hashtag, trailing_hashtag};
use rlive_browser::{BrowserDb, Model, Op};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// How wide the sidebar can be dragged.
const SIDEBAR_MIN: f32 = 90.0;
const SIDEBAR_MAX: f32 = 260.0;

/// What's being typed somewhere in the browser's menus.
#[derive(Default)]
pub struct Renaming {
    pub key: String,
    pub text: String,
}

/// The browser's state in the UI: everything it needs each frame.
pub struct BrowserState {
    pub db: Option<BrowserDb>,
    pub model: Model,
    sidebar_w: f32,
    /// The search field wants the keyboard.
    pub focus_search: bool,
    /// The search field had the keyboard last frame.
    search_focused: bool,
    /// The content pane should bring the cursor into view.
    pub scroll_to_cursor: bool,
    /// The cursor moved: preview it, or Hot-Swap it in with `-EnableHotSwapOnSelection`.
    pub apply_after_move: bool,
    pub hot_swap: Option<hot_swap::HotSwap>,
    /// `-EnableHotSwapOnSelection` in Options.txt: selecting a preset loads it.
    pub hot_swap_on_selection: bool,
    /// Preview plays what's selected.
    pub preview_on: bool,
    /// Plays at the file's own tempo, once.
    pub raw: bool,
    /// The file last previewed, for the Preview Tab.
    previewing: Option<PathBuf>,
    wave: Option<(PathBuf, Vec<f32>)>,
    pub renaming: Option<Renaming>,
    edit_collections: bool,
    edit_library: bool,
    quick_add: Option<String>,
    /// A new tag waiting for its parent or group, from Quick Tags.
    quick_new: Option<String>,
    quick_new_parent: String,
    tag_new: HashMap<u32, String>,
    tag_fold: HashSet<u32>,
    /// The Groove Pool entry a groove being dragged from the content pane landed in.
    dragged_groove: Option<rlive_core::GrooveId>,
    /// A preset waiting for a name before it's saved.
    pub pending_save: Option<SaveName>,
    pub open_set: Option<PathBuf>,
    plugin_stamp: usize,
    project_stamp: (usize, usize, Option<PathBuf>),
    pub library_path: PathBuf,
    /// Full-height browser (manual p. 63).
    pub full_height: bool,
    /// Bottom of the content pane: how tall it was left.
    filter_h: f32,
}

/// A preset's name being typed in the browser.
pub struct SaveName {
    pub text: String,
    pub path: PathBuf,
}

impl Default for BrowserState {
    fn default() -> Self {
        Self {
            db: None,
            model: Model::new(),
            sidebar_w: 118.0,
            focus_search: false,
            search_focused: false,
            scroll_to_cursor: false,
            apply_after_move: false,
            hot_swap: None,
            hot_swap_on_selection: false,
            preview_on: false,
            raw: false,
            previewing: None,
            wave: None,
            renaming: None,
            edit_collections: false,
            edit_library: false,
            quick_add: None,
            quick_new: None,
            quick_new_parent: String::new(),
            tag_new: HashMap::new(),
            tag_fold: HashSet::new(),
            dragged_groove: None,
            pending_save: None,
            open_set: None,
            plugin_stamp: usize::MAX,
            project_stamp: (usize::MAX, usize::MAX, None),
            library_path: PathBuf::new(),
            full_height: false,
            filter_h: 96.0,
        }
    }
}

/// Reads the flags that come from Options.txt, kept beside the settings.
fn options_flag(flag: &str) -> bool {
    let Some(dir) = Settings::path().parent().map(std::path::Path::to_path_buf) else {
        return false;
    };
    std::fs::read_to_string(dir.join("Options.txt"))
        .is_ok_and(|text| text.lines().any(|l| l.trim() == flag))
}

impl BrowserState {
    /// Opens the database. Everything slow it does, it does on its own thread.
    pub fn open(&mut self, ctx: &egui::Context, settings: &Settings) {
        let data_dir = Settings::path()
            .parent()
            .map_or_else(|| PathBuf::from("."), std::path::Path::to_path_buf);
        let user_library = settings
            .user_library
            .clone()
            .unwrap_or_else(UserLibrary::default_location);
        self.library_path = user_library.clone();
        let repaint = ctx.clone();
        let db = BrowserDb::open_with_waker(
            Config {
                data_dir,
                user_library,
                audio_extensions: rlive_audio_io::AUDIO_EXTENSIONS
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            },
            std::sync::Arc::new(move || repaint.request_repaint()),
        );
        db.set_catalog("builtin", catalog::builtin_items());
        self.db = Some(db);
        self.hot_swap_on_selection = options_flag("-EnableHotSwapOnSelection");
    }

    pub fn op(&self, op: Op) {
        if let Some(db) = &self.db {
            db.op(op);
        }
    }

    /// Hands the worker what changed, picks up what it said, and asks for new rows if due.
    fn sync(&mut self, session: &Session) {
        let Some(db) = &mut self.db else {
            return;
        };
        let plugins = session.plugins.plugins.len();
        if plugins != self.plugin_stamp {
            self.plugin_stamp = plugins;
            let entries: Vec<PluginEntry> = session
                .plugins
                .plugins
                .iter()
                .map(|p| PluginEntry {
                    format: p.format.label().to_string(),
                    id: p.id.clone(),
                    name: p.name.clone(),
                    vendor: p.vendor.clone(),
                    is_instrument: p.is_instrument,
                    subcategories: p.subcategories.clone(),
                })
                .collect();
            db.set_catalog("plugins", catalog::plugin_items(&entries));
        }
        let stamp = (
            session.history.position(),
            session.project.tracks.len() + session.project.returns.len(),
            session.path.clone(),
        );
        if stamp != self.project_stamp {
            self.project_stamp = stamp;
            db.set_catalog("project", load::project_items(session));
            db.set_project_folder(
                session
                    .path
                    .as_deref()
                    .and_then(|p| p.parent())
                    .map(PathBuf::from),
            );
        }
        db.poll();
        if !std::sync::Arc::ptr_eq(db.results(), &self.model.rows) {
            self.model.take_results(db.results().clone());
            self.scroll_to_cursor = true;
        }
        if std::mem::take(&mut self.model.query_due) {
            let q = self.model.query();
            db.query(q);
        }
    }

    /// Runs every frame, even with the browser closed, so scans are picked up.
    pub fn idle(&mut self, session: &Session) {
        self.sync(session);
    }

    /// Up or Down with the sidebar in focus: the next label there.
    pub fn sidebar_step(&mut self, delta: i32) {
        let lib = self.db.as_ref().map_or_else(
            || std::sync::Arc::new(rlive_browser::LibraryData::default()),
            |d| d.library().clone(),
        );
        let scopes = sidebar::scopes(&lib, &self.library_path);
        let at = scopes
            .iter()
            .position(|s| *s == self.model.nav.scope)
            .unwrap_or(0) as i32;
        let next = (at + delta).clamp(0, scopes.len() as i32 - 1) as usize;
        self.model.navigate(scopes[next].clone());
    }

    /// Changes where the User Library is.
    pub fn set_user_library(&mut self, session: &mut Session, path: PathBuf) {
        self.library_path = path.clone();
        session.library = Some(UserLibrary::new(path.clone()));
        if let Some(db) = &self.db {
            db.set_user_library(path);
        }
    }

    pub fn scanning(&self) -> bool {
        self.db.as_ref().is_some_and(BrowserDb::scanning)
    }

    fn row_keys(&self) -> Vec<ItemKey> {
        self.model
            .selected_rows()
            .iter()
            .map(|r| r.item.key.clone())
            .collect()
    }

    /// Puts what's selected in collection `n`, or `None` to clear them.
    pub fn assign_color(&self, collection: Option<u8>) {
        let items = self.row_keys();
        if items.is_empty() {
            return;
        }
        match collection {
            Some(collection) => {
                let library = self.db.as_ref().map(|d| d.library().clone());
                let on = library.is_none_or(|l| {
                    !items
                        .iter()
                        .all(|i| l.colors_of(i) & (1 << collection) != 0)
                });
                self.op(Op::SetColor {
                    items,
                    collection,
                    on,
                });
            }
            None => self.op(Op::ClearColors(items)),
        }
    }

    fn item_tags(&self, item: &Item) -> Vec<String> {
        let Some(db) = &self.db else {
            return Vec::new();
        };
        let lib = db.library();
        lib.tags_of(&item.key, &item.factory_tags)
            .iter()
            .filter_map(|t| lib.tags.tag(*t).map(|t| t.name))
            .collect()
    }
}

/// The history arrows, the search field and the filter toggles.
fn search_row(ui: &mut egui::Ui, full: Rect, state: &mut UiState) -> f32 {
    let bar = Rect::from_min_size(
        full.min + Vec2::new(4.0, 4.0),
        Vec2::new(full.width() - 8.0, 20.0),
    );
    let mut bu = ui.new_child(
        UiBuilder::new()
            .max_rect(bar)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    bu.spacing_mut().item_spacing.x = 3.0;
    let can_back = state.browser.model.can_go_back();
    let can_forward = state.browser.model.can_go_forward();
    if info(
        bu.add_enabled(can_back, egui::Button::new("‹").small()),
        "Browse Back — the previous search or place (Cmd+[)",
    )
    .clicked()
    {
        state.browser.model.back();
    }
    if info(
        bu.add_enabled(can_forward, egui::Button::new("›").small()),
        "Browse Forward — the next search or place (Cmd+])",
    )
    .clicked()
    {
        state.browser.model.forward();
    }
    let filters_on = state
        .browser
        .db
        .as_ref()
        .is_some_and(|d| d.library().show_filters);
    let reserved = 44.0;
    let w = bu.available_width() - reserved;
    let mut text = state.browser.model.nav.text.clone();
    // Read before the field takes the keys for itself.
    let (down, enter, esc) = ui.input(|i| {
        (
            i.key_pressed(egui::Key::ArrowDown),
            i.key_pressed(egui::Key::Enter),
            i.key_pressed(egui::Key::Escape),
        )
    });
    let was_typing = state.browser.search_focused;
    let edit_resp = bu.add_sized(
        [w, 18.0],
        egui::TextEdit::singleline(&mut text)
            .hint_text("Search (Cmd+F)")
            .font(theme::font(11.0)),
    );
    if edit_resp.changed() {
        state.browser.model.set_text(text.clone());
    }
    if std::mem::take(&mut state.browser.focus_search) {
        edit_resp.request_focus();
    }
    let field = edit_resp.rect;
    let typing = ui.ctx().memory(|m| m.has_focus(edit_resp.id));
    state.browser.search_focused = typing;
    if edit_resp.lost_focus() {
        state.browser.model.commit_search();
    }
    // The keyboard-only flow (manual p. 68): Down or Enter goes to the results, Esc clears.
    if was_typing {
        if esc {
            state.browser.model.clear_all();
            edit_resp.surrender_focus();
        } else if down || enter {
            state.browser.model.commit_search();
            edit_resp.surrender_focus();
            state.focus = crate::ui::keymap::FocusArea::Browser;
            state.browser.model.move_cursor(1, false);
            state.browser.scroll_to_cursor = true;
        }
    }
    autocomplete(ui, field, state, typing);
    let xs = Rect::from_center_size(
        Pos2::new(field.right() - 9.0, field.center().y),
        Vec2::splat(14.0),
    );
    if !state.browser.model.nav.text.is_empty() {
        let r = ui.interact(xs, ui.scope_id().with("browser_clear_text"), Sense::click());
        ui.painter().text(
            xs.center(),
            Align2::CENTER_CENTER,
            "×",
            theme::font(12.0),
            if r.hovered() {
                pal().text
            } else {
                pal().text_dim
            },
        );
        if info(r, "Clear — clear the search words, leaving the filters").clicked() {
            state.browser.model.clear_text();
        }
    }
    let r = widgets::state_button(
        &mut bu,
        Vec2::new(22.0, 18.0),
        filters_on,
        pal().accent,
        "F",
    );
    if info(r, "Filter View — show or hide the filters (Cmd+Alt+G)").clicked() {
        state.browser.op(Op::SetShowFilters(!filters_on));
    }
    let r = widgets::icon_button(
        &mut bu,
        Vec2::new(16.0, 18.0),
        false,
        pal().accent,
        super::icons::menu_arrow,
    );
    let r = info(
        r,
        "Filter View Menu — which groups show, Tag Editor, Quick Tags",
    );
    egui::Popup::menu(&r).show(|ui| filters::menu(ui, state));
    bar.bottom() + 4.0 - full.top()
}

/// Suggestions for the `#tag` being typed.
fn autocomplete(ui: &mut egui::Ui, field: Rect, state: &mut UiState, typing: bool) {
    if !typing {
        return;
    }
    let text = state.browser.model.nav.text.clone();
    let Some(fragment) = trailing_hashtag(&text) else {
        return;
    };
    let Some(db) = &state.browser.db else {
        return;
    };
    let completions: Vec<String> = db
        .library()
        .tags
        .completions(fragment)
        .into_iter()
        .take(8)
        .map(|t| t.name)
        .collect();
    if completions.is_empty() {
        return;
    }
    let mut chosen = None;
    egui::Area::new(ui.scope_id().with("browser_tag_suggestions"))
        .order(egui::Order::Foreground)
        .fixed_pos(field.left_bottom())
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_width(field.width() - 12.0);
                for c in &completions {
                    if ui.selectable_label(false, format!("#{c}")).clicked() {
                        chosen = Some(c.clone());
                    }
                }
            });
        });
    if let Some(tag) = chosen {
        let next = complete_hashtag(&text, &tag);
        state.browser.model.set_text(next);
        state.browser.focus_search = true;
    }
}

/// The bar that says Hot-Swap is on.
fn hot_swap_bar(ui: &mut egui::Ui, rect: Rect, session: &Session, state: &mut UiState) {
    ui.painter().rect_filled(rect, 0.0, pal().accent);
    let name = hot_swap::target_name(session, state.browser.hot_swap.as_ref());
    let on = theme::text_on(pal().accent);
    ui.painter().text(
        rect.left_center() + Vec2::new(6.0, 0.0),
        Align2::LEFT_CENTER,
        format!("Hot-Swap: {name}"),
        theme::font_bold(10.5),
        on,
    );
    let x = Rect::from_center_size(
        rect.right_center() - Vec2::new(10.0, 0.0),
        Vec2::splat(14.0),
    );
    let r = ui.interact(x, ui.scope_id().with("hot_swap_x"), Sense::click());
    ui.painter().text(
        x.center(),
        Align2::CENTER_CENTER,
        "×",
        theme::font(13.0),
        on,
    );
    if info(r, "Exit Hot-Swap (Q or Esc)").clicked() {
        hot_swap::exit_hot_swap(state);
    }
}

/// The name a preset gets as it's saved, typed in the browser (manual p. 463).
fn save_name_bar(ui: &mut egui::Ui, rect: Rect, state: &mut UiState) {
    let Some(save) = &mut state.browser.pending_save else {
        return;
    };
    let mut bu = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    bu.label(egui::RichText::new("Save as").color(pal().text_dim));
    let edit = bu.add(egui::TextEdit::singleline(&mut save.text).font(theme::font(11.0)));
    edit.request_focus();
    let (enter, esc) = ui.input(|i| {
        (
            i.key_pressed(egui::Key::Enter),
            i.key_pressed(egui::Key::Escape),
        )
    });
    if enter {
        let done = state.browser.pending_save.take().expect("just read");
        let dir = done
            .path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_default();
        let to = rlive_browser::preset::unused_path(&dir, done.text.trim());
        if to != done.path {
            if let Ok(text) = std::fs::read_to_string(&done.path) {
                if let Ok(mut preset) = rlive_browser::Preset::parse(&text) {
                    preset.name = done.text.trim().to_string();
                    if preset.save(&to).is_ok() {
                        let _ = std::fs::remove_file(&done.path);
                    }
                }
            }
        }
        if let Some(db) = &state.browser.db {
            db.rescan();
        }
    } else if esc {
        let done = state.browser.pending_save.take().expect("just read");
        let _ = std::fs::remove_file(&done.path);
        if let Some(db) = &state.browser.db {
            db.rescan();
        }
    }
}

pub fn show(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    settings: &mut Settings,
) {
    let full = ui.max_rect();
    // Claim the whole panel so egui keeps its size (children don't advance the cursor).
    ui.expand_to_include_rect(full);
    state.browser.sync(session);
    if std::mem::take(&mut state.browser.apply_after_move) {
        preview::selected(session, state);
        if state.browser.hot_swap_on_selection {
            hot_swap::swap_selected(session, state);
        }
    }
    if ui.input(|i| i.pointer.any_pressed())
        && ui.rect_contains_pointer(full)
        && state.focus != crate::ui::keymap::FocusArea::Browser
    {
        state.focus = crate::ui::keymap::FocusArea::Browser;
    }
    let esc = state.focus == crate::ui::keymap::FocusArea::Browser
        && !ui.ctx().egui_wants_keyboard_input()
        && ui.input(|i| i.key_pressed(egui::Key::Escape));
    let esc_swap = state.browser.hot_swap.is_some()
        && !ui.ctx().egui_wants_keyboard_input()
        && ui.input(|i| i.key_pressed(egui::Key::Escape));
    if esc_swap {
        hot_swap::exit_hot_swap(state);
    } else if esc {
        state.browser.model.clear_all();
    }

    let mut top = full.top() + search_row(ui, full, state);
    if state.browser.hot_swap.is_some() {
        let bar = Rect::from_min_size(Pos2::new(full.left(), top), Vec2::new(full.width(), 18.0));
        hot_swap_bar(ui, bar, session, state);
        top = bar.bottom() + 2.0;
    }
    if state.browser.pending_save.is_some() {
        let bar = Rect::from_min_size(
            Pos2::new(full.left() + 4.0, top),
            Vec2::new(full.width() - 8.0, 20.0),
        );
        save_name_bar(ui, bar, state);
        top = bar.bottom() + 2.0;
    }
    let preview_h = preview::HEIGHT;
    let preview_rect =
        Rect::from_min_max(Pos2::new(full.left(), full.bottom() - preview_h), full.max);
    let body = Rect::from_min_max(
        Pos2::new(full.left(), top),
        Pos2::new(full.right(), preview_rect.top()),
    );
    ui.painter().line_segment(
        [body.left_top(), body.right_top()],
        Stroke::new(1.0, pal().separator),
    );

    let sidebar_w = state
        .browser
        .sidebar_w
        .clamp(SIDEBAR_MIN, (body.width() - 80.0).max(SIDEBAR_MIN));
    let side = Rect::from_min_size(body.min, Vec2::new(sidebar_w, body.height()));
    let mut su = ui.new_child(UiBuilder::new().max_rect(side));
    su.set_clip_rect(side.intersect(ui.clip_rect()));
    sidebar::show(&mut su, side, session, state, settings);

    // The divider between the two halves, dragged to share the width (manual p. 63).
    let divider = Rect::from_min_size(
        side.right_top() - Vec2::new(2.0, 0.0),
        Vec2::new(4.0, body.height()),
    );
    let r = ui.interact(
        divider,
        ui.scope_id().with("browser_divider"),
        Sense::drag(),
    );
    if r.hovered() || r.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    if r.dragged() {
        state.browser.sidebar_w =
            (state.browser.sidebar_w + r.drag_delta().x).clamp(SIDEBAR_MIN, SIDEBAR_MAX);
    }
    ui.painter().line_segment(
        [side.right_top(), side.right_bottom()],
        Stroke::new(1.0, pal().separator),
    );

    let right = Rect::from_min_max(Pos2::new(side.right() + 1.0, body.top()), body.max);
    let mut ru = ui.new_child(UiBuilder::new().max_rect(right));
    ru.set_clip_rect(right.intersect(ui.clip_rect()));
    content::right_side(&mut ru, right, session, state);

    let mut pu = ui.new_child(UiBuilder::new().max_rect(preview_rect));
    pu.set_clip_rect(preview_rect.intersect(ui.clip_rect()));
    preview::show(&mut pu, preview_rect, session, state);
    filters::tag_editor_window(ui.ctx(), state);
    // Preview follows the selection while it's on.
    preview::follow_selection(session, state);
}

/// Draws a small coloured square.
pub(super) fn color_square(ui: &egui::Ui, rect: Rect, c: [u8; 3]) {
    ui.painter().rect_filled(rect, 1.0, super::widgets::rgb(c));
}

/// What follows the pointer while a device is dragged by its title bar.
pub(crate) fn drag_device_label(ui: &egui::Ui, name: &str) {
    drag_label(ui, name);
}

/// What follows the pointer while a row or column header is dragged.
pub(super) fn drag_label(ui: &egui::Ui, text: &str) {
    if let Some(pos) = ui.ctx().pointer_latest_pos() {
        egui::Area::new(egui::Id::unique("browser_drag_preview"))
            .order(egui::Order::Tooltip)
            .fixed_pos(pos + egui::vec2(12.0, 4.0))
            .interactable(false)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| ui.label(text));
            });
    }
}
