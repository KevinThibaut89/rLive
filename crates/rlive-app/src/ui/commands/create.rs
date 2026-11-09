//! Commands that add tracks and scenes.

use super::{Entry, Entry::Item, Entry::Separator, MenuCommand as M, MenuState};
use crate::session::{ChainRef, ClipRef};
use crate::ui::keymap::FocusArea;
use crate::ui::{session_select, RliveApp};
use rlive_core::TrackId;
use rlive_project::TrackKind;

super::domain! {
    /// The Create menu.
    Create {
        NewMidiTrack, NewAudioTrack, NewReturnTrack, NewScene, CaptureAndInsertScene, GroupTracks,
        UngroupTracks, ImportMidi, SliceToMidi,
    }
}

pub const MENU: &[Entry] = &[
    Item("MIDI Track", M::Create(Create::NewMidiTrack)),
    Item("Audio Track", M::Create(Create::NewAudioTrack)),
    Item("Return Track", M::Create(Create::NewReturnTrack)),
    Item("Insert Scene", M::Create(Create::NewScene)),
    Item(
        "Capture and Insert Scene",
        M::Create(Create::CaptureAndInsertScene),
    ),
    Separator,
    Item("Group Tracks", M::Create(Create::GroupTracks)),
    Item("Ungroup Tracks", M::Create(Create::UngroupTracks)),
    Separator,
    Item(
        "Insert Take Lane",
        M::Comp(super::comping::Comp::InsertLane),
    ),
    Item("Import MIDI File…", M::Create(Create::ImportMidi)),
    Item("Slice to New MIDI Track…", M::Create(Create::SliceToMidi)),
    Separator,
    super::bounce::CONSOLIDATE_TO_SCENE,
];

impl RliveApp {
    fn select_new_track(&mut self, kind: TrackKind) {
        let id = self.session.add_track(kind, None);
        self.ui.selected = Some(ChainRef::Track(id));
    }

    fn select_new_scene(&mut self, scene: usize) {
        session_select::click_scene(&mut self.ui, scene, false, false);
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

    fn import_midi_dialog(&mut self) {
        use crate::session::MidiImportTarget as To;
        let Some(track) = self
            .selected_track_ids()
            .first()
            .copied()
            .or(match self.ui.selected {
                Some(ChainRef::Track(t)) => Some(t),
                _ => None,
            })
        else {
            self.ui.status = "Select a MIDI track first".into();
            return;
        };
        let Some(path) = rfd::FileDialog::new()
            .add_filter("MIDI File", &["mid", "midi"])
            .pick_file()
        else {
            return;
        };
        let into = if self.ui.focus == FocusArea::Arrangement {
            To::Timeline(self.session.insert_marker() as u64)
        } else {
            let slot = self
                .ui
                .selected_clip
                .and_then(ClipRef::as_slot)
                .filter(|(t, _)| *t == track)
                .map(|(_, s)| s)
                .or_else(|| {
                    self.session
                        .project
                        .track(track)
                        .and_then(|t| t.clips.iter().position(|c| c.is_none()))
                })
                .unwrap_or(0);
            To::Slot(slot)
        };
        match self.session.import_midi_file(&path, track, into) {
            Ok(r) => {
                self.ui.selected_clip = Some(r);
                self.ui.status = format!("Imported {}", path.display());
            }
            Err(e) => self.ui.status = e,
        }
    }

    pub(super) fn create_state(&self, s: &mut MenuState) {
        let selected = self.selected_track_ids();
        let audio_clip = self
            .ui
            .selected_clip
            .and_then(|c| self.session.clip_at(c))
            .is_some_and(|c| !c.is_midi());
        s.enable(M::Create(Create::SliceToMidi), audio_clip);
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
            Create::ImportMidi => self.import_midi_dialog(),
            Create::SliceToMidi => {
                if let Some(r) = self.ui.selected_clip {
                    if self.session.clip_at(r).is_some_and(|c| !c.is_midi()) {
                        self.ui.files.slice = Some(crate::ui::slice_dialog::SliceDialog::new(r));
                    }
                }
            }
            Create::NewMidiTrack => self.select_new_track(TrackKind::Midi),
            Create::NewAudioTrack => self.select_new_track(TrackKind::Audio),
            Create::NewReturnTrack => {
                if let Some(r) = self.session.add_return() {
                    self.ui.selected = Some(ChainRef::Track(r));
                }
            }
            Create::NewScene => {
                let below = self.selected_scene();
                if let Some(new) = self.session.insert_scene(below) {
                    self.select_new_scene(new);
                }
            }
            Create::CaptureAndInsertScene => {
                let below = self.selected_scene();
                if let Some(new) = self.session.capture_and_insert_scene(below) {
                    self.select_new_scene(new);
                }
            }
            Create::GroupTracks => self.group_selected(),
            Create::UngroupTracks => self.ungroup_selected(),
        }
    }
}
