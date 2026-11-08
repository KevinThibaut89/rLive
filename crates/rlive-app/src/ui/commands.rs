//! Application commands, shared by the native macOS menu bar, the egui menu
//! used on other platforms, and keyboard shortcuts.

use super::{piano_roll, session_view, Detail, MainView, RliveApp};
use crate::session::{ChainRef, ClipRef};
use egui::{Key, KeyboardShortcut, Modifiers};
use rlive_core::TrackId;
use rlive_project::{ArrangementClip, Clip, NoteData, TrackKind};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum MenuCommand {
    About,
    Preferences,
    Quit,
    NewSet,
    Open,
    Save,
    SaveAs,
    ExportAudio,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    Duplicate,
    Delete,
    SelectAll,
    NewMidiTrack,
    NewAudioTrack,
    NewScene,
    ToggleMainView,
    SwitchDetail,
    ToggleBrowser,
    ToggleDetail,
    ToggleMixer,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    RescanPlugins,
    ToggleComputerKeys,
    GroupTracks,
    UngroupTracks,
    NewReturnTrack,
    ToggleSoloInPlace,
}

use MenuCommand as C;

impl MenuCommand {
    /// Every command, in declaration order, so `ALL[c as usize] == c`.
    pub const ALL: [MenuCommand; 33] = [
        C::About,
        C::Preferences,
        C::Quit,
        C::NewSet,
        C::Open,
        C::Save,
        C::SaveAs,
        C::ExportAudio,
        C::Undo,
        C::Redo,
        C::Cut,
        C::Copy,
        C::Paste,
        C::Duplicate,
        C::Delete,
        C::SelectAll,
        C::NewMidiTrack,
        C::NewAudioTrack,
        C::NewScene,
        C::ToggleMainView,
        C::SwitchDetail,
        C::ToggleBrowser,
        C::ToggleDetail,
        C::ToggleMixer,
        C::ZoomIn,
        C::ZoomOut,
        C::ZoomReset,
        C::RescanPlugins,
        C::ToggleComputerKeys,
        C::GroupTracks,
        C::UngroupTracks,
        C::NewReturnTrack,
        C::ToggleSoloInPlace,
    ];
}

/// The menu shortcut for a command. Tab and Delete are absent on purpose: they
/// must keep working in text fields and plugin windows, so they're handled
/// separately and never bound as menu key equivalents.
pub fn shortcut(c: MenuCommand) -> Option<KeyboardShortcut> {
    const CMD: Modifiers = Modifiers::COMMAND;
    const CMD_SHIFT: Modifiers = Modifiers {
        shift: true,
        ..Modifiers::COMMAND
    };
    const CMD_ALT: Modifiers = Modifiers {
        alt: true,
        ..Modifiers::COMMAND
    };
    let (m, k) = match c {
        C::Preferences => (CMD, Key::Comma),
        C::Quit => (CMD, Key::Q),
        C::NewSet => (CMD, Key::N),
        C::Open => (CMD, Key::O),
        C::Save => (CMD, Key::S),
        C::SaveAs => (CMD_SHIFT, Key::S),
        C::Undo => (CMD, Key::Z),
        C::Redo => (CMD_SHIFT, Key::Z),
        C::Cut => (CMD, Key::X),
        C::Copy => (CMD, Key::C),
        C::Paste => (CMD, Key::V),
        C::Duplicate => (CMD, Key::D),
        C::SelectAll => (CMD, Key::A),
        C::NewMidiTrack => (CMD, Key::T),
        C::NewReturnTrack => (CMD_ALT, Key::T),
        C::GroupTracks => (CMD, Key::G),
        C::UngroupTracks => (CMD_SHIFT, Key::G),
        C::ToggleBrowser => (CMD_ALT, Key::B),
        C::ToggleDetail => (CMD_ALT, Key::L),
        C::ZoomIn => (CMD, Key::Equals),
        C::ZoomOut => (CMD, Key::Minus),
        C::ZoomReset => (CMD, Key::Num0),
        _ => return None,
    };
    Some(KeyboardShortcut::new(m, k))
}

/// The unadorned title of an item whose title otherwise changes.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn plain_title(c: MenuCommand) -> Option<&'static str> {
    match c {
        C::Undo => Some("Undo"),
        C::Redo => Some("Redo"),
        _ => None,
    }
}

/// One row of a menu.
#[derive(Clone, Copy)]
pub enum Entry {
    Item(&'static str, MenuCommand),
    Separator,
}

use Entry::{Item, Separator};

pub const FILE_MENU: &[Entry] = &[
    Item("New Live Set", C::NewSet),
    Item("Open…", C::Open),
    Separator,
    Item("Save", C::Save),
    Item("Save As…", C::SaveAs),
    Separator,
    Item("Export Audio…", C::ExportAudio),
];

pub const EDIT_MENU: &[Entry] = &[
    Item("Undo", C::Undo),
    Item("Redo", C::Redo),
    Separator,
    Item("Cut", C::Cut),
    Item("Copy", C::Copy),
    Item("Paste", C::Paste),
    Item("Duplicate", C::Duplicate),
    Item("Delete", C::Delete),
    Separator,
    Item("Select All", C::SelectAll),
];

pub const CREATE_MENU: &[Entry] = &[
    Item("MIDI Track", C::NewMidiTrack),
    Item("Audio Track", C::NewAudioTrack),
    Item("Return Track", C::NewReturnTrack),
    Item("Scene", C::NewScene),
    Separator,
    Item("Group Tracks", C::GroupTracks),
    Item("Ungroup Tracks", C::UngroupTracks),
];

pub const VIEW_MENU: &[Entry] = &[
    Item("Session / Arrangement (Tab)", C::ToggleMainView),
    Item("Clip / Device View (Shift+Tab)", C::SwitchDetail),
    Separator,
    Item("Browser", C::ToggleBrowser),
    Item("Detail View", C::ToggleDetail),
    Item("Mixer", C::ToggleMixer),
    Separator,
    Item("Zoom In", C::ZoomIn),
    Item("Zoom Out", C::ZoomOut),
    Item("Actual Size", C::ZoomReset),
];

pub const OPTIONS_MENU: &[Entry] = &[
    Item("Computer MIDI Keyboard", C::ToggleComputerKeys),
    Item("Solo in Place", C::ToggleSoloInPlace),
    Item("Rescan Plugins", C::RescanPlugins),
];

/// What the menus should show right now.
#[derive(Clone, PartialEq)]
pub struct MenuState {
    enabled: [bool; MenuCommand::ALL.len()],
    checked: [bool; MenuCommand::ALL.len()],
    pub undo_label: Option<&'static str>,
    pub redo_label: Option<&'static str>,
}

impl Default for MenuState {
    fn default() -> Self {
        Self {
            enabled: [true; MenuCommand::ALL.len()],
            checked: [false; MenuCommand::ALL.len()],
            undo_label: None,
            redo_label: None,
        }
    }
}

impl MenuState {
    pub fn is_enabled(&self, c: MenuCommand) -> bool {
        self.enabled[c as usize]
    }

    pub fn is_checked(&self, c: MenuCommand) -> bool {
        self.checked[c as usize]
    }

    fn enable(&mut self, c: MenuCommand, on: bool) {
        self.enabled[c as usize] = on;
    }

    fn check(&mut self, c: MenuCommand, on: bool) {
        self.checked[c as usize] = on;
    }

    /// The title an item should show, for items whose title changes.
    pub fn title(&self, c: MenuCommand) -> Option<String> {
        match c {
            C::Undo => Some(match self.undo_label {
                Some(l) => format!("Undo {l}"),
                None => "Undo".to_string(),
            }),
            C::Redo => Some(match self.redo_label {
                Some(l) => format!("Redo {l}"),
                None => "Redo".to_string(),
            }),
            _ => None,
        }
    }
}

/// Which part of the app Edit commands act on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EditFocus {
    Text,
    Notes,
    ArrClips,
    SessionClip,
    None,
}

/// The area the user last clicked in.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum FocusArea {
    #[default]
    Session,
    Notes,
    Arrangement,
}

/// Internal clipboard for musical content.
#[derive(Clone)]
pub enum ClipBoard {
    Notes(Vec<NoteData>),
    Slot(Box<Clip>),
    /// Clips with their tracks, their starts relative to the earliest.
    ArrClips(Vec<(TrackId, ArrangementClip)>),
}

/// Resolves focus to an Edit target; pure so the precedence can be tested.
fn resolve_focus(
    text_focused: bool,
    area: FocusArea,
    notes_open: bool,
    slot_selected: bool,
) -> EditFocus {
    if text_focused {
        EditFocus::Text
    } else if area == FocusArea::Notes && notes_open {
        EditFocus::Notes
    } else if area == FocusArea::Arrangement {
        EditFocus::ArrClips
    } else if slot_selected {
        EditFocus::SessionClip
    } else if notes_open {
        EditFocus::Notes
    } else {
        EditFocus::None
    }
}

impl RliveApp {
    fn notes_open(&self) -> bool {
        self.ui.show_detail
            && self.ui.detail == Detail::Clip
            && self
                .ui
                .selected_clip
                .and_then(|r| self.session.clip_at(r))
                .is_some_and(|c| c.is_midi())
    }

    pub(super) fn edit_focus(&self, ctx: &egui::Context) -> EditFocus {
        resolve_focus(
            ctx.text_edit_focused(),
            self.ui.focus,
            self.notes_open(),
            self.ui.selected_clip.and_then(|r| r.as_slot()).is_some(),
        )
    }

    pub(super) fn menu_state(&self, ctx: &egui::Context) -> MenuState {
        let mut s = MenuState::default();
        let focus = self.edit_focus(ctx);
        let (has_selection, can_paste) = match focus {
            EditFocus::Text => (true, true),
            EditFocus::Notes => (
                self.ui.piano.has_selection(),
                matches!(self.ui.clipboard, Some(ClipBoard::Notes(_))),
            ),
            EditFocus::SessionClip => (
                self.ui
                    .selected_clip
                    .is_some_and(|r| self.session.clip_at(r).is_some()),
                matches!(self.ui.clipboard, Some(ClipBoard::Slot(_))),
            ),
            EditFocus::ArrClips => (
                !self.ui.arr.selection.is_empty(),
                matches!(self.ui.clipboard, Some(ClipBoard::ArrClips(_))),
            ),
            EditFocus::None => (false, false),
        };
        s.enable(C::Cut, has_selection);
        s.enable(C::Copy, has_selection);
        s.enable(C::Paste, can_paste);
        s.enable(C::Duplicate, focus != EditFocus::Text && has_selection);
        s.enable(C::Delete, focus != EditFocus::Text && has_selection);
        s.enable(
            C::SelectAll,
            matches!(
                focus,
                EditFocus::Text | EditFocus::Notes | EditFocus::ArrClips
            ),
        );
        let (undo, redo) = if focus == EditFocus::Text {
            (true, true)
        } else {
            s.undo_label = self.session.history.undo_label();
            s.redo_label = self.session.history.redo_label();
            (s.undo_label.is_some(), s.redo_label.is_some())
        };
        s.enable(C::Undo, undo);
        s.enable(C::Redo, redo);
        s.enable(C::RescanPlugins, self.ui.scan_rx.is_none());
        s.check(C::ToggleBrowser, self.ui.show_browser);
        s.check(C::ToggleDetail, self.ui.show_detail);
        s.check(C::ToggleMixer, self.ui.layout.show_mixer);
        s.check(C::ToggleComputerKeys, self.ui.computer_keys);
        let selected = self.selected_track_ids();
        let project = &self.session.project;
        s.enable(C::GroupTracks, !selected.is_empty());
        s.enable(
            C::UngroupTracks,
            selected
                .iter()
                .any(|t| project.track(*t).is_some_and(|t| t.is_group())),
        );
        s.enable(
            C::NewReturnTrack,
            project.returns.len() < rlive_engine::MAX_SENDS,
        );
        s.check(C::ToggleSoloInPlace, project.solo_in_place);
        s
    }

    /// The tracks a Group or Ungroup command applies to: the multi-selection, or the single
    /// selected track.
    fn selected_track_ids(&self) -> Vec<TrackId> {
        let mut ids = self.ui.selected_tracks.clone();
        ids.retain(|t| self.session.project.track_index(*t).is_some());
        if ids.is_empty() {
            if let Some(ChainRef::Track(t)) = self.ui.selected {
                if self.session.project.track_index(t).is_some() {
                    ids.push(t);
                }
            }
        }
        ids
    }

    fn group_selected(&mut self) {
        let ids = self.selected_track_ids();
        if let Some(g) = self.session.group_tracks(&ids) {
            self.ui.selected = Some(ChainRef::Track(g));
            self.ui.selected_tracks = vec![g];
        }
    }

    fn ungroup_selected(&mut self) {
        let groups: Vec<TrackId> = self
            .selected_track_ids()
            .into_iter()
            .filter(|t| self.session.project.track(*t).is_some_and(|t| t.is_group()))
            .collect();
        for g in groups {
            self.session.ungroup(g);
        }
        self.ui.selected_tracks.clear();
        self.ui.selected = self
            .session
            .project
            .tracks
            .first()
            .map(|t| ChainRef::Track(t.id));
    }

    fn select_new_track(&mut self, kind: TrackKind) {
        let id = self.session.add_track(kind, None);
        self.ui.selected = Some(ChainRef::Track(id));
    }

    pub(super) fn dispatch(&mut self, ctx: &egui::Context, c: MenuCommand) {
        let zoom = ctx.zoom_factor();
        match c {
            C::About => self.ui.show_about = true,
            C::Preferences => {
                self.ui.show_settings = true;
                self.ui.audio_devices = None;
                self.ui.midi_ports = None;
            }
            C::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            C::NewSet => {
                self.session.new_project();
                self.ui.selected_clip = None;
            }
            C::Open => self.open_dialog(),
            C::Save => self.save(false),
            C::SaveAs => self.save(true),
            C::ExportAudio => self.ui.show_export = true,
            C::Undo | C::Redo => self.undo_redo(ctx, c),
            C::Cut | C::Copy | C::Paste | C::Duplicate | C::Delete | C::SelectAll => {
                self.edit_command(ctx, c)
            }
            C::NewMidiTrack => self.select_new_track(TrackKind::Midi),
            C::NewAudioTrack => self.select_new_track(TrackKind::Audio),
            C::GroupTracks => self.group_selected(),
            C::UngroupTracks => self.ungroup_selected(),
            C::NewReturnTrack => {
                if let Some(r) = self.session.add_return() {
                    self.ui.selected = Some(ChainRef::Track(r));
                }
            }
            C::ToggleSoloInPlace => {
                let on = !self.session.project.solo_in_place;
                self.session.set_solo_in_place(on);
            }
            C::NewScene => self.session.add_scene(),
            C::ToggleMainView => {
                self.ui.main_view = match self.ui.main_view {
                    MainView::Session => MainView::Arrangement,
                    MainView::Arrangement => MainView::Session,
                };
            }
            C::SwitchDetail => {
                self.ui.show_detail = true;
                self.ui.detail = match self.ui.detail {
                    Detail::Devices if self.ui.selected_clip.is_some() => Detail::Clip,
                    _ => Detail::Devices,
                };
            }
            C::ToggleBrowser => self.toggle_browser(),
            C::ToggleDetail => self.toggle_detail(),
            C::ToggleMixer => self.ui.layout.show_mixer = !self.ui.layout.show_mixer,
            C::ZoomIn => self.set_zoom(ctx, zoom + 0.1),
            C::ZoomOut => self.set_zoom(ctx, zoom - 0.1),
            C::ZoomReset => self.set_zoom(ctx, 1.0),
            C::RescanPlugins => self.start_plugin_scan(),
            C::ToggleComputerKeys => self.ui.computer_keys = !self.ui.computer_keys,
        }
    }

    fn undo_redo(&mut self, ctx: &egui::Context, c: MenuCommand) {
        // Text fields keep their own undo inside egui.
        if self.edit_focus(ctx) == EditFocus::Text {
            return;
        }
        let changed = if c == C::Undo {
            self.session.undo()
        } else {
            self.session.redo()
        };
        if !changed {
            return;
        }
        self.ui.piano.selected.clear();
        let project = &self.session.project;
        self.ui
            .arr
            .selection
            .retain(|(t, id)| project.track(*t).is_some_and(|t| t.arr_clip(*id).is_some()));
        if self
            .ui
            .selected_clip
            .is_some_and(|r| self.session.clip_at(r).is_none() && r.as_slot().is_none())
        {
            self.ui.selected_clip = None;
        }
        let selected_exists = match self.ui.selected {
            Some(ChainRef::Track(t)) => self.session.project.track(t).is_some(),
            _ => true,
        };
        if !selected_exists {
            self.ui.selected = self
                .session
                .project
                .tracks
                .first()
                .map(|t| ChainRef::Track(t.id));
        }
    }

    fn edit_command(&mut self, ctx: &egui::Context, c: MenuCommand) {
        match self.edit_focus(ctx) {
            EditFocus::Notes => self.edit_notes(c),
            EditFocus::ArrClips => self.edit_arr_clips(c),
            EditFocus::SessionClip => self.edit_session_clip(c),
            // Text fields handle their own editing inside egui.
            EditFocus::Text | EditFocus::None => {}
        }
    }

    fn edit_notes(&mut self, c: MenuCommand) {
        let (session, ui) = (&mut self.session, &mut self.ui);
        match c {
            C::Copy | C::Cut => {
                if let Some(notes) = piano_roll::copy_selection(session, ui) {
                    ui.clipboard = Some(ClipBoard::Notes(notes));
                    if c == C::Cut {
                        piano_roll::delete_selected(session, ui);
                    }
                }
            }
            C::Paste => {
                if let Some(ClipBoard::Notes(notes)) = ui.clipboard.clone() {
                    piano_roll::paste_notes(session, ui, &notes);
                }
            }
            C::Duplicate => piano_roll::duplicate_selection(session, ui),
            C::Delete => piano_roll::delete_selected(session, ui),
            C::SelectAll => piano_roll::select_all(session, ui),
            _ => {}
        }
    }

    fn copy_arr_clips(&self) -> Vec<(TrackId, ArrangementClip)> {
        let mut clips: Vec<(TrackId, ArrangementClip)> = self
            .ui
            .arr
            .selection
            .iter()
            .filter_map(|(t, id)| {
                let c = self.session.project.track(*t)?.arr_clip(*id)?;
                Some((*t, c.clone()))
            })
            .collect();
        let first = clips.iter().map(|(_, c)| c.start).min().unwrap_or(0);
        for (_, c) in &mut clips {
            c.start -= first;
        }
        clips
    }

    fn edit_arr_clips(&mut self, c: MenuCommand) {
        match c {
            C::Copy | C::Cut => {
                let clips = self.copy_arr_clips();
                if !clips.is_empty() {
                    self.ui.clipboard = Some(ClipBoard::ArrClips(clips));
                }
                if c == C::Cut {
                    let items = std::mem::take(&mut self.ui.arr.selection);
                    self.session.arr_delete(&items);
                }
            }
            C::Paste => {
                if let Some(ClipBoard::ArrClips(clips)) = self.ui.clipboard.clone() {
                    let at = self.session.insert_marker() as u64;
                    self.ui.arr.selection = self.session.arr_paste(&clips, at);
                }
            }
            C::Duplicate => {
                let items = self.ui.arr.selection.clone();
                self.ui.arr.selection = self.session.arr_duplicate(&items);
            }
            C::Delete => {
                let items = std::mem::take(&mut self.ui.arr.selection);
                self.session.arr_delete(&items);
            }
            C::SelectAll => {
                self.ui.arr.selection = self
                    .session
                    .project
                    .tracks
                    .iter()
                    .flat_map(|t| t.arrangement.iter().map(|c| (t.id, c.id)))
                    .collect();
            }
            _ => {}
        }
    }

    fn edit_session_clip(&mut self, c: MenuCommand) {
        let Some((track, slot)) = self.ui.selected_clip.and_then(|r| r.as_slot()) else {
            return;
        };
        match c {
            C::Copy | C::Cut => {
                if let Some(clip) = self.session.clip(track, slot).cloned() {
                    self.ui.clipboard = Some(ClipBoard::Slot(Box::new(clip)));
                    if c == C::Cut {
                        self.session.set_clip(track, slot, None);
                    }
                }
            }
            C::Paste => {
                if let Some(ClipBoard::Slot(clip)) = self.ui.clipboard.clone() {
                    self.paste_slot(track, slot, *clip);
                }
            }
            C::Duplicate => session_view::duplicate_clip(&mut self.session, track, slot),
            C::Delete => {
                self.session.set_clip(track, slot, None);
                self.ui.selected_clip = None;
            }
            _ => {}
        }
    }

    fn paste_slot(&mut self, track: rlive_core::TrackId, slot: usize, clip: Clip) {
        let kind_matches = self
            .session
            .project
            .track(track)
            .is_some_and(|t| (t.kind == TrackKind::Midi) == clip.is_midi());
        if !kind_matches {
            self.ui.status = "That clip can't be pasted on this kind of track".into();
            return;
        }
        let target = if self.session.clip(track, slot).is_none() {
            slot
        } else {
            session_view::next_free_slot(&mut self.session, track, slot)
        };
        self.session.set_clip(track, target, Some(clip));
        self.ui.selected_clip = Some(ClipRef::slot(track, target));
    }

    /// Runs shortcuts for menu commands where there's no native menu to do it.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub(super) fn menu_shortcuts(&mut self, ctx: &egui::Context) {
        let typing = ctx.text_edit_focused();
        // Shift variants first: consume_shortcut ignores extra Shift on matching.
        let mut order: Vec<(MenuCommand, KeyboardShortcut)> = MenuCommand::ALL
            .iter()
            .filter_map(|c| shortcut(*c).map(|s| (*c, s)))
            .collect();
        order.sort_by_key(|(_, s)| !s.modifiers.shift);
        let edit = |c: MenuCommand| {
            matches!(
                c,
                C::Undo | C::Redo | C::Cut | C::Copy | C::Paste | C::SelectAll
            )
        };
        for (c, s) in order {
            if typing && edit(c) {
                continue;
            }
            if ctx.input_mut(|i| i.consume_shortcut(&s)) {
                self.dispatch(ctx, c);
            }
        }
    }

    /// The egui menu bar, for platforms without a native one.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub(super) fn menu_bar(&mut self, ui: &mut egui::Ui) {
        fn entries(
            ui: &mut egui::Ui,
            list: &[Entry],
            st: &MenuState,
            fired: &mut Option<MenuCommand>,
        ) {
            for e in list {
                match *e {
                    Separator => {
                        ui.separator();
                    }
                    Item(label, c) => {
                        let title = st.title(c).unwrap_or_else(|| label.to_string());
                        let mut b = egui::Button::new(title).selected(st.is_checked(c));
                        if let Some(s) = shortcut(c) {
                            b = b.shortcut_text(ui.ctx().format_shortcut(&s));
                        }
                        if ui.add_enabled(st.is_enabled(c), b).clicked() {
                            *fired = Some(c);
                            ui.close();
                        }
                    }
                }
            }
        }
        let st = self.menu_state(ui.ctx());
        let mut fired = None;
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                entries(ui, FILE_MENU, &st, &mut fired);
                ui.separator();
                entries(ui, &[Item("Quit", C::Quit)], &st, &mut fired);
            });
            ui.menu_button("Edit", |ui| entries(ui, EDIT_MENU, &st, &mut fired));
            ui.menu_button("Create", |ui| entries(ui, CREATE_MENU, &st, &mut fired));
            ui.menu_button("View", |ui| entries(ui, VIEW_MENU, &st, &mut fired));
            ui.menu_button("Options", |ui| {
                entries(ui, &[Item("Preferences…", C::Preferences)], &st, &mut fired);
                entries(ui, OPTIONS_MENU, &st, &mut fired);
            });
            ui.menu_button("Help", |ui| {
                entries(ui, &[Item("About rLive", C::About)], &st, &mut fired);
            });
        });
        if let Some(c) = fired {
            self.dispatch(&ui.ctx().clone(), c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_indexed_in_order() {
        for (i, c) in MenuCommand::ALL.iter().enumerate() {
            assert_eq!(*c as usize, i, "{c:?} is out of order in ALL");
        }
    }

    #[test]
    fn shortcuts_are_unique() {
        let mut seen = Vec::new();
        for c in MenuCommand::ALL {
            if let Some(s) = shortcut(c) {
                assert!(!seen.contains(&s), "{c:?} reuses a shortcut");
                seen.push(s);
            }
        }
    }

    #[test]
    fn text_focus_wins_then_last_clicked_area() {
        use EditFocus as E;
        use FocusArea as A;
        assert_eq!(resolve_focus(true, A::Notes, true, true), E::Text);
        assert_eq!(resolve_focus(false, A::Notes, true, true), E::Notes);
        assert_eq!(resolve_focus(false, A::Session, true, true), E::SessionClip);
        assert_eq!(resolve_focus(false, A::Notes, false, true), E::SessionClip);
        assert_eq!(resolve_focus(false, A::Session, false, false), E::None);
        assert_eq!(
            resolve_focus(false, A::Arrangement, true, true),
            E::ArrClips
        );
    }
}
