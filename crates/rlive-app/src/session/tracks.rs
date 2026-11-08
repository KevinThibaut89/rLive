//! Adding, removing and mixing tracks, and routing live MIDI to them.

use super::{mix_of, Session};
use rlive_core::{MidiMessage, TrackId};
use rlive_engine::shared::TrackShared;
use rlive_engine::{EngineCommand, GraphCmd, ParamCmd, RecordCmd};
use rlive_project::history::ParkedMix;
use rlive_project::{Device, Track, TrackKind};
use std::collections::HashMap;
use std::sync::Arc;

impl Session {
    // ------------------------------------------------------------- tracks

    pub fn add_track(&mut self, kind: TrackKind, index: Option<usize>) -> TrackId {
        let n = self.project.tracks.len() + 1;
        let (name, device) = match kind {
            TrackKind::Midi => (
                format!("{n} MIDI"),
                Some(Device::builtin("rlive.poly", "Poly Synth")),
            ),
            TrackKind::Audio => (format!("{n} Audio"), None),
        };
        let mut t = Track::new(
            &name,
            kind,
            self.project.next_color(),
            self.project.scenes.len(),
        );
        t.devices.extend(device);
        let id = t.id;
        let index = index.unwrap_or(self.project.tracks.len());
        self.install_track(t, index);
        self.mark_edit("Add Track");
        id
    }

    /// Adds a fully formed track to the project and the engine.
    pub(super) fn install_track(&mut self, t: Track, index: usize) {
        let index = index.min(self.project.tracks.len());
        let shared = Arc::new(TrackShared::default());
        self.track_shared.insert(t.id, shared.clone());
        let mut errors = Vec::new();
        let mut runtimes = HashMap::new();
        let (mut b, _) = self.builder();
        let node = b.track(&t, shared, &mut runtimes, &mut errors);
        self.runtimes.extend(runtimes);
        self.messages.extend(errors);
        self.project.tracks.insert(index, t);
        self.send(EngineCommand::Graph(GraphCmd::AddTrack {
            index,
            track: node,
        }));
    }

    pub fn delete_track(&mut self, id: TrackId) {
        if let Some(i) = self.project.track_index(id) {
            let mut t = self.project.tracks.remove(i);
            self.history.parked_mix.insert(id, ParkedMix::of(&t));
            for d in &mut t.devices {
                self.park_device(d);
                self.runtimes.remove(&d.id);
            }
            self.track_shared.remove(&id);
            self.send(EngineCommand::Graph(GraphCmd::RemoveTrack(id)));
            self.update_midi_targets();
            self.mark_edit("Delete Track");
        }
    }

    pub fn move_track(&mut self, id: TrackId, to: usize) {
        if let Some(i) = self.project.track_index(id) {
            let t = self.project.tracks.remove(i);
            let to = to.min(self.project.tracks.len());
            self.project.tracks.insert(to, t);
            self.send(EngineCommand::Graph(GraphCmd::MoveTrack { track: id, to }));
            self.mark_edit("Move Track");
        }
    }

    /// Push the track's volume/pan/mute/solo to the engine after editing the project.
    pub fn sync_mix(&mut self, id: TrackId) {
        if let Some(t) = self.project.track(id) {
            let mix = mix_of(t);
            self.send(EngineCommand::Param(ParamCmd::SetMix { track: id, mix }));
            self.touch();
        }
    }

    pub fn set_master_volume(&mut self, db: f32) {
        self.project.master.volume_db = db;
        self.send(EngineCommand::Param(ParamCmd::SetMasterVolume(db)));
        self.touch();
    }

    pub fn set_arm(&mut self, id: TrackId, arm: bool, exclusive: bool) {
        for t in self.project.tracks.iter_mut() {
            if t.id == id {
                t.arm = arm;
            } else if exclusive && arm {
                t.arm = false;
            }
        }
        self.update_midi_targets();
    }

    /// Armed tracks receive MIDI; if none are armed, the selected track does (set by the UI).
    pub fn update_midi_targets(&mut self) {
        let armed: Vec<TrackId> = self
            .project
            .tracks
            .iter()
            .filter(|t| t.arm)
            .map(|t| t.id)
            .collect();
        *self.midi_targets.lock().unwrap() = armed;
    }

    pub fn midi_in(&mut self, track: TrackId, message: MidiMessage) {
        self.send(EngineCommand::Record(RecordCmd::Midi { track, message }));
    }

    /// MIDI the user performed: audible, and recorded while recording.
    pub fn play_midi(&mut self, track: TrackId, message: MidiMessage) {
        self.send(EngineCommand::Record(RecordCmd::PlayedMidi {
            track,
            message,
            at: std::time::Instant::now(),
        }));
    }
}
