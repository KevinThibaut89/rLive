//! Glue between the session and the MIDI take being recorded.

use super::Session;
use crate::recording::Recording;
use rlive_engine::{EngineCommand, RecordCmd};
use rlive_project::{ArrangementClip, TrackKind};

impl Session {
    // ------------------------------------------------------------- recording

    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    /// Starts recording MIDI onto armed tracks, starting the transport if needed.
    /// Returns false if no MIDI track is armed.
    pub fn start_recording(&mut self) -> bool {
        let armed = self
            .project
            .tracks
            .iter()
            .any(|t| t.arm && t.kind == TrackKind::Midi);
        if !armed {
            return false;
        }
        let at = if self.is_playing() {
            self.engine.shared.tick.load()
        } else {
            self.insert_marker()
        };
        self.recording = Some(Recording::new(at as u64));
        self.send(EngineCommand::Record(RecordCmd::SetRecording(true)));
        self.play();
        true
    }

    pub fn stop_recording(&mut self) {
        self.send(EngineCommand::Record(RecordCmd::SetRecording(false)));
    }

    /// Puts what's been recorded so far on the timeline.
    pub(super) fn flush_recording(&mut self) {
        let Some(recording) = self.recording.as_ref() else {
            return;
        };
        let placements = recording.placements(self.project.time_signature);
        for p in placements {
            let Some(t) = self.project.track_mut(p.track) else {
                continue;
            };
            if let Some(old) = p.existing {
                t.remove_arr_clip(old);
            }
            let mut clip = p.clip;
            clip.color = t.color;
            let mut placed = ArrangementClip::new(clip, p.start);
            if let Some(old) = p.existing {
                placed.id = old;
            }
            let id = placed.id;
            t.place_clip(placed);
            if let Some(r) = self.recording.as_mut() {
                r.set_clip(p.track, id);
            }
            self.sync_lane(p.track);
            self.mark_edit("Record");
        }
    }
}
