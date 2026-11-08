//! Session commands for launching, Follow Actions and scenes.

use super::{Entry, Entry::Item, MenuCommand as M, MenuState};
use crate::session::ClipRef;
use crate::ui::session_select::{launch_targets, selected_slots};
use crate::ui::{scene_view, RliveApp};

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
    }
}

pub const EDIT_ITEMS: &[Entry] = &[Item(
    "Add/Remove Stop Button",
    M::Launch(Launch::ToggleStopButton),
)];

impl RliveApp {
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
        }
    }
}
