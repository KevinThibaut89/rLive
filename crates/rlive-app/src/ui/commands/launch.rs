//! Session commands for launching, Follow Actions and scenes.

use super::{Entry, Entry::Item, MenuCommand as M, MenuState};
use crate::session::{ChainRef, ClipRef};
use crate::ui::session_select::{launch_targets, selected_slots};
use crate::ui::session_select::{select_one, SceneField};
use crate::ui::{scene_view, session_view, RliveApp};
use rlive_core::TrackId;

super::domain! {
    /// Commands for the Session grid's launching.
    Launch {
        ToggleStopButton,
        ToggleFollow,
        CreateFollowChain,
        MoveScenesUp,
        MoveScenesDown,
        DeactivateClip,
        CancelSceneLaunch,
        ArrowUp,
        ArrowDown,
        ArrowLeft,
        ArrowRight,
        PageUp,
        PageDown,
        SlotHome,
        SlotEnd,
    }
}

pub const EDIT_ITEMS: &[Entry] = &[Item(
    "Add/Remove Stop Button",
    M::Launch(Launch::ToggleStopButton),
)];

impl RliveApp {
    /// The tracks with a column in the grid, left to right.
    fn grid_tracks(&self) -> Vec<TrackId> {
        self.session
            .project
            .visible_tracks()
            .into_iter()
            .map(|i| self.session.project.tracks[i].id)
            .collect()
    }

    /// The slot the keyboard is on, if it's on one.
    fn keyboard_slot(&self) -> Option<(TrackId, usize)> {
        self.ui
            .selected_clip
            .and_then(ClipRef::as_slot)
            .or_else(|| self.ui.sel.slots.last().copied())
            .or(self.ui.group_slot)
    }

    /// Moves the selection to one slot and scrolls it into view.
    fn select_slot_by_keys(&mut self, track: TrackId, slot: usize) {
        self.ui.title_focus = None;
        self.ui.group_slot = None;
        if self
            .session
            .project
            .track(track)
            .is_some_and(|t| t.is_group())
        {
            session_view::select_group_slot(&self.session, &mut self.ui, track, slot);
        } else {
            select_one(&mut self.ui, track, slot);
            self.ui.selected = Some(ChainRef::Track(track));
        }
        self.ui.layout.reveal = Some((track, slot));
    }

    fn select_scene_by_keys(&mut self, scene: usize) {
        self.ui.title_focus = None;
        self.ui.group_slot = None;
        scene_view::select_scene(&mut self.ui, scene);
    }

    /// The arrow keys in the Session View: title bars above, slots, the scene column on the
    /// right and its tempo and signature fields beyond it (manual pp. 191, 1006-1007).
    fn grid_move(&mut self, dx: isize, dy: isize) {
        let tracks = self.grid_tracks();
        let n = self.session.project.scenes.len();
        if n == 0 || tracks.is_empty() {
            return;
        }
        if let Some(t) = self.ui.title_focus {
            if dy != 0 {
                let scene = self.selected_scene().min(n - 1);
                let to = if dy > 0 {
                    scene
                } else {
                    scene.saturating_sub(1)
                };
                self.select_slot_by_keys(t, to);
            } else if let Some(i) = tracks.iter().position(|x| *x == t) {
                if let Some(&next) = tracks.get(i.wrapping_add_signed(dx)) {
                    self.ui.title_focus = Some(next);
                    self.ui.selected = Some(ChainRef::Track(next));
                }
            }
            return;
        }
        if let Some((scene, field)) = self.ui.sel.scene_field {
            self.ui.sel.scene_field = None;
            if dy != 0 {
                let to = scene.saturating_add_signed(dy).min(n - 1);
                self.ui.sel.scene_field = Some((to, field));
            } else {
                match (field, dx) {
                    (SceneField::Tempo, -1) => self.select_scene_by_keys(scene),
                    (SceneField::Tempo, 1) => {
                        self.ui.sel.scene_field = Some((scene, SceneField::Signature))
                    }
                    (SceneField::Signature, -1) => {
                        self.ui.sel.scene_field = Some((scene, SceneField::Tempo))
                    }
                    _ => self.ui.sel.scene_field = Some((scene, field)),
                }
            }
            return;
        }
        if let Some(&scene) = self.ui.sel.scenes.last() {
            if dy != 0 {
                self.select_scene_by_keys(scene.saturating_add_signed(dy).min(n - 1));
            } else if dx > 0 {
                self.ui.sel.scenes.clear();
                self.ui.sel.scene_field = Some((scene, SceneField::Tempo));
            } else if let Some(&last) = tracks.last() {
                self.ui.sel.scenes.clear();
                self.select_slot_by_keys(last, scene);
            }
            return;
        }
        let Some((track, slot)) = self.keyboard_slot() else {
            return;
        };
        let Some(i) = tracks.iter().position(|x| *x == track) else {
            return;
        };
        if dy < 0 && slot == 0 {
            self.select_title(track);
            return;
        }
        let to = slot.saturating_add_signed(dy).min(n - 1);
        match tracks.get(i.wrapping_add_signed(dx)) {
            Some(&t) => self.select_slot_by_keys(t, to),
            None if dx > 0 => self.select_scene_by_keys(to),
            None => {}
        }
    }

    fn select_title(&mut self, track: TrackId) {
        self.ui.title_focus = Some(track);
        self.ui.selected_clip = None;
        self.ui.group_slot = None;
        self.ui.sel.clear();
        self.ui.selected = Some(ChainRef::Track(track));
    }

    /// Home: the first track of the current scene.
    fn grid_home(&mut self) {
        let tracks = self.grid_tracks();
        let scene = self
            .keyboard_slot()
            .map_or_else(|| self.selected_scene(), |s| s.1);
        if let Some(&first) = tracks.first() {
            self.select_slot_by_keys(first, scene);
        }
    }

    /// End: the last track of the current scene, and from there, the scene itself.
    fn grid_end(&mut self) {
        let tracks = self.grid_tracks();
        let scene = self
            .keyboard_slot()
            .map_or_else(|| self.selected_scene(), |s| s.1);
        let Some(&last) = tracks.last() else {
            return;
        };
        if self.ui.title_focus.is_none() && self.keyboard_slot().is_some_and(|s| s.0 == last) {
            self.select_scene_by_keys(scene);
        } else {
            self.select_slot_by_keys(last, scene);
        }
    }

    pub(super) fn launch_state(&self, s: &mut MenuState) {
        let has_slot = self.ui.selected_clip.and_then(ClipRef::as_slot).is_some()
            || !self.ui.sel.slots.is_empty();
        s.enable(M::Launch(Launch::ToggleStopButton), has_slot);
        s.enable(M::Launch(Launch::CreateFollowChain), has_slot);
    }

    pub(super) fn launch_command(&mut self, c: Launch) {
        match c {
            Launch::ToggleStopButton => {
                let mut slots = self.ui.sel.slots.clone();
                if let Some(one) = self.ui.selected_clip.and_then(ClipRef::as_slot) {
                    if !slots.contains(&one) {
                        slots.push(one);
                    }
                }
                self.session.toggle_stop_buttons(&slots);
            }
            Launch::ToggleFollow => {
                if !self.ui.sel.scenes.is_empty() {
                    let scenes = self.ui.sel.scenes.clone();
                    self.session.toggle_scene_follow(&scenes);
                } else {
                    let targets = launch_targets(&self.ui);
                    self.session.toggle_clip_follow(&targets);
                }
            }
            Launch::CreateFollowChain => {
                let slots = selected_slots(&self.session, &self.ui);
                self.session.create_follow_chain(&slots);
            }
            Launch::MoveScenesUp => scene_view::shift_selected(&mut self.session, &mut self.ui, -1),
            Launch::MoveScenesDown => {
                scene_view::shift_selected(&mut self.session, &mut self.ui, 1)
            }
            Launch::DeactivateClip => {
                let slots = selected_slots(&self.session, &self.ui);
                self.session.toggle_clip_active(&slots);
            }
            Launch::CancelSceneLaunch => self.session.cancel_scene_launch(),
            Launch::ArrowUp => self.grid_move(0, -1),
            Launch::ArrowDown => self.grid_move(0, 1),
            Launch::ArrowLeft => self.grid_move(-1, 0),
            Launch::ArrowRight => self.grid_move(1, 0),
            Launch::PageUp => self.grid_move(0, -8),
            Launch::PageDown => self.grid_move(0, 8),
            Launch::SlotHome => self.grid_home(),
            Launch::SlotEnd => self.grid_end(),
        }
    }
}
