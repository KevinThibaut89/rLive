//! Commands that add tracks and scenes.

use super::{Entry, Entry::Item, Entry::Separator, MenuCommand as M, MenuState};
use crate::session::ChainRef;
use crate::ui::RliveApp;
use rlive_core::TrackId;
use rlive_project::TrackKind;

super::domain! {
    /// The Create menu.
    Create { NewMidiTrack, NewAudioTrack, NewReturnTrack, NewScene, GroupTracks, UngroupTracks }
}

pub const MENU: &[Entry] = &[
    Item("MIDI Track", M::Create(Create::NewMidiTrack)),
    Item("Audio Track", M::Create(Create::NewAudioTrack)),
    Item("Return Track", M::Create(Create::NewReturnTrack)),
    Item("Scene", M::Create(Create::NewScene)),
    Separator,
    Item("Group Tracks", M::Create(Create::GroupTracks)),
    Item("Ungroup Tracks", M::Create(Create::UngroupTracks)),
];

impl RliveApp {
    fn select_new_track(&mut self, kind: TrackKind) {
        let id = self.session.add_track(kind, None);
        self.ui.selected = Some(ChainRef::Track(id));
    }

    /// The tracks a Group or Ungroup command applies to: the multi-selection, or the single
    /// selected track.
    pub(in crate::ui) fn selected_track_ids(&self) -> Vec<TrackId> {
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

    pub(super) fn create_state(&self, s: &mut MenuState) {
        let selected = self.selected_track_ids();
        let project = &self.session.project;
        s.enable(M::Create(Create::GroupTracks), !selected.is_empty());
        s.enable(
            M::Create(Create::UngroupTracks),
            selected
                .iter()
                .any(|t| project.track(*t).is_some_and(|t| t.is_group())),
        );
        s.enable(
            M::Create(Create::NewReturnTrack),
            project.returns.len() < rlive_engine::MAX_SENDS,
        );
    }

    pub(super) fn create_command(&mut self, c: Create) {
        match c {
            Create::NewMidiTrack => self.select_new_track(TrackKind::Midi),
            Create::NewAudioTrack => self.select_new_track(TrackKind::Audio),
            Create::NewReturnTrack => {
                if let Some(r) = self.session.add_return() {
                    self.ui.selected = Some(ChainRef::Track(r));
                }
            }
            Create::NewScene => self.session.add_scene(),
            Create::GroupTracks => self.group_selected(),
            Create::UngroupTracks => self.ungroup_selected(),
        }
    }
}
