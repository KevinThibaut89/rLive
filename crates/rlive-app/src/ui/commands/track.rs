//! Commands that act on the selected track or clip.

use crate::session::{ChainRef, ClipRef};
use crate::ui::session_view;
use crate::ui::RliveApp;
use rlive_core::TrackId;
use rlive_project::{Clip, TrackKind};

super::domain! {
    /// Commands with no menu, reached only through the keymap.
    Track {
        Arm,
        Solo,
        ToggleActivator,
        Activator1,
        Activator2,
        Activator3,
        Activator4,
        Activator5,
        Activator6,
        Activator7,
        Activator8,
        MoveLeft,
        MoveRight,
        ShowGroupedTracks,
        HideGroupedTracks,
        ToggleGroupFold,
        LaunchSelectedClip,
        StopSelectedTrack,
        InsertMidiClip,
    }
}

impl RliveApp {
    fn selected_track(&self) -> Option<TrackId> {
        match self.ui.selected {
            Some(ChainRef::Track(id)) => Some(id),
            _ => None,
        }
    }

    fn toggle_activator(&mut self, id: TrackId) {
        if let Some(t) = self.session.project.track_mut(id) {
            t.mute = !t.mute;
            self.session.sync_mix(id);
        }
    }

    /// Arms the selected tracks that can record; Group Tracks and Return Tracks can't.
    fn arm_selected(&mut self) {
        let armable: Vec<TrackId> = self
            .selected_track_ids()
            .into_iter()
            .filter(|id| {
                let project = &self.session.project;
                project
                    .track(*id)
                    .is_some_and(|t| matches!(t.kind, TrackKind::Midi | TrackKind::Audio))
            })
            .collect();
        for (i, id) in armable.into_iter().enumerate() {
            let arm = self.session.project.track(id).is_some_and(|t| t.arm);
            self.session.set_arm(id, !arm, i == 0);
        }
    }

    /// Folds or unfolds the selected Group Tracks; `None` flips each one.
    fn set_selected_groups_folded(&mut self, folded: Option<bool>) {
        for id in self.selected_track_ids() {
            let Some(t) = self.session.project.track(id).filter(|t| t.is_group()) else {
                continue;
            };
            let next = folded.unwrap_or(!t.ui.folded);
            self.session.set_folded(id, next);
        }
    }

    fn move_selected_track(&mut self, delta: isize) {
        let Some(id) = self.selected_track() else {
            return;
        };
        let Some(i) = self.session.project.track_index(id) else {
            return;
        };
        if let Some(to) = i.checked_add_signed(delta) {
            self.session.move_track(id, to);
        }
    }

    /// Puts an empty one-bar MIDI clip in the selected slot.
    fn insert_midi_clip(&mut self) {
        let Some((track, slot)) = self.ui.selected_clip.and_then(ClipRef::as_slot) else {
            return;
        };
        let session = &mut self.session;
        let is_midi = session
            .project
            .track(track)
            .is_some_and(|t| t.kind == TrackKind::Midi);
        if !is_midi || session.clip(track, slot).is_some() {
            return;
        }
        let sig = session.project.time_signature;
        let color = session
            .project
            .track(track)
            .map(|t| t.color)
            .unwrap_or(rlive_project::PALETTE[0]);
        let clip = Clip::new_midi("MIDI Clip", color, 1, sig);
        session.set_clip(track, slot, Some(clip));
    }

    pub(super) fn track_command(&mut self, c: Track) {
        let activator = |n: usize, app: &mut Self| {
            if let Some(id) = app.session.project.tracks.get(n).map(|t| t.id) {
                app.toggle_activator(id);
            }
        };
        match c {
            Track::Arm => self.arm_selected(),
            Track::Solo => {
                // Each after the first adds to the solo, as Cmd-click does.
                for (i, id) in self.selected_track_ids().into_iter().enumerate() {
                    let solo = self.session.project.track(id).is_some_and(|t| t.solo);
                    session_view::toggle_solo(&mut self.session, id, solo, i > 0);
                }
            }
            Track::ToggleActivator => {
                for id in self.selected_track_ids() {
                    self.toggle_activator(id);
                }
            }
            Track::Activator1 => activator(0, self),
            Track::Activator2 => activator(1, self),
            Track::Activator3 => activator(2, self),
            Track::Activator4 => activator(3, self),
            Track::Activator5 => activator(4, self),
            Track::Activator6 => activator(5, self),
            Track::Activator7 => activator(6, self),
            Track::Activator8 => activator(7, self),
            Track::MoveLeft => self.move_selected_track(-1),
            Track::MoveRight => self.move_selected_track(1),
            Track::ShowGroupedTracks => self.set_selected_groups_folded(Some(false)),
            Track::HideGroupedTracks => self.set_selected_groups_folded(Some(true)),
            Track::ToggleGroupFold => self.set_selected_groups_folded(None),
            Track::LaunchSelectedClip => {
                if let Some((track, slot)) = self.ui.selected_clip.and_then(ClipRef::as_slot) {
                    self.session.launch_clip(track, slot);
                }
            }
            Track::StopSelectedTrack => {
                if let Some(id) = self.selected_track() {
                    self.session.stop_track(id);
                }
            }
            Track::InsertMidiClip => self.insert_midi_clip(),
        }
    }
}
