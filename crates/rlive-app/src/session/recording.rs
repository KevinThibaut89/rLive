//! Recording onto the Arrangement: starting and ending takes, and what a MIDI take becomes.

use super::Session;
use crate::recording::Recording;
use rlive_core::{MidiMessage, TrackId};
use rlive_engine::capture::CaptureMode;
use rlive_engine::{EngineCommand, RecordCmd};
use rlive_project::{ArrangementClip, NoteData, RecordQuantize, TrackKind};

impl Session {
    // ------------------------------------------------------------- recording

    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    fn armed_kind(&self, kind: TrackKind) -> Vec<TrackId> {
        self.project
            .tracks
            .iter()
            .filter(|t| t.arm && t.kind == kind)
            .map(|t| t.id)
            .collect()
    }

    /// Starts recording onto armed tracks, and with Automation Arm automation on every track, as
    /// the Start Playback with Record preference says. Armed or not, what's launched in the
    /// Session is logged into the Arrangement (manual p. 197), so there is always something to
    /// record.
    pub fn start_recording(&mut self) -> bool {
        let play = self.record.start_playback_with_record;
        self.start_recording_with(play)
    }

    /// As [`start_recording`](Self::start_recording), with Shift's say on whether playback starts.
    pub fn start_recording_inverted(&mut self) -> bool {
        let play = !self.record.start_playback_with_record;
        self.start_recording_with(play)
    }

    fn start_recording_with(&mut self, play: bool) -> bool {
        let midi = self.armed_kind(TrackKind::Midi);
        let audio = self.armed_kind(TrackKind::Audio);
        let at = if self.is_playing() {
            self.engine.shared.tick.load()
        } else {
            self.insert_marker()
        };
        let mut recording = Recording::new(at as u64);
        self.start_launch_log(at);
        let overdub = self.project.midi_arrangement_overdub;
        for track in &midi {
            if overdub {
                recording.overdub_onto(*track, self.lane_notes(*track));
            } else {
                self.send(EngineCommand::Record(RecordCmd::MuteLane {
                    track: *track,
                    on: true,
                }));
            }
        }
        self.recording = Some(recording);
        for track in audio {
            self.open_take(track, CaptureMode::Arrangement);
        }
        self.send_punch();
        self.send(EngineCommand::Record(RecordCmd::SetRecording(true)));
        if play {
            self.play();
        }
        true
    }

    pub fn stop_recording(&mut self) {
        self.send(EngineCommand::Record(RecordCmd::SetRecording(false)));
    }

    /// Sends where recording is allowed. The punch points are the Arrangement Loop's edges.
    pub(super) fn send_punch(&mut self) {
        let a = &self.project.arrangement;
        let punch_in = a.punch_in.then_some(a.loop_start as f64);
        let punch_out = a.punch_out.then_some(a.loop_end as f64);
        self.send(EngineCommand::Record(RecordCmd::SetPunch {
            punch_in,
            punch_out,
        }));
    }

    /// Turns Punch-In and Punch-Out on or off. View and transport state, not an edit of the Set.
    pub fn set_punch(&mut self, punch_in: bool, punch_out: bool) {
        let a = &mut self.project.arrangement;
        if (a.punch_in, a.punch_out) == (punch_in, punch_out) {
            return;
        }
        a.punch_in = punch_in;
        a.punch_out = punch_out;
        self.send_punch();
        self.mark_edit("Punch");
    }

    /// New MIDI mixes with the Arrangement (on), or replaces it (off).
    pub fn set_midi_arrangement_overdub(&mut self, on: bool) {
        self.project.midi_arrangement_overdub = on;
        self.mark_edit("MIDI Arrangement Overdub");
    }

    /// The Record Quantization chooser. It stays put while recording, except for a looped overdub,
    /// which picks up a change at once (manual p. 419).
    pub fn set_record_quantize(&mut self, q: RecordQuantize) -> bool {
        if self.is_recording() || !self.slot_takes.is_empty() {
            return false;
        }
        self.project.record_quantize = q;
        self.mark_edit("Record Quantization");
        true
    }

    /// Everything a track's timeline plays, as notes at their song position.
    fn lane_notes(&self, track: TrackId) -> Vec<NoteData> {
        let Some(t) = self.project.track(track) else {
            return Vec::new();
        };
        let mut notes = Vec::new();
        for c in &t.arrangement {
            if !c.clip.is_midi() {
                continue;
            }
            for n in c.clip.notes() {
                if n.start < c.offset || n.start >= c.offset + c.length {
                    continue;
                }
                notes.push(NoteData {
                    start: n.start - c.offset + c.start,
                    ..n.clone()
                });
            }
        }
        notes
    }

    pub(super) fn recorded_midi(&mut self, track: TrackId, tick: f64, message: MidiMessage) {
        if let Some(take) = self.slot_takes.get_mut(&track) {
            take.recording.message(track, tick, message);
            return;
        }
        let armed = self
            .project
            .track(track)
            .is_some_and(|t| t.arm && t.kind == TrackKind::Midi);
        if let (true, Some(r)) = (armed, self.recording.as_mut()) {
            r.message(track, tick, message);
        }
    }

    /// Recording ended: everything taken becomes a clip, and a separate step applies quantize.
    pub(super) fn recording_stopped(&mut self, tick: f64) {
        if let Some(r) = self.recording.as_mut() {
            r.finish(tick);
        }
        let midi: Vec<TrackId> = self.project.tracks.iter().map(|t| t.id).collect();
        for track in midi {
            self.send(EngineCommand::Record(RecordCmd::MuteLane {
                track,
                on: false,
            }));
        }
        self.flush_recording();
        self.quantize_finished_take();
        self.recording = None;
    }

    /// Applies Record Quantization to what was just recorded, as a step of its own, so undo goes
    /// back to the raw take first (manual p. 419).
    fn quantize_finished_take(&mut self) {
        let q = self.project.record_quantize;
        if q == RecordQuantize::Off {
            return;
        }
        let Some(recording) = self.recording.as_ref() else {
            return;
        };
        // Every clip the recording made, on the take lanes and the main lane alike.
        let placed = recording.placed_clips(self.project.time_signature);
        if placed.is_empty() {
            return;
        }
        self.commit_now();
        for (track, id) in placed {
            let Some(c) = self
                .project
                .track_mut(track)
                .and_then(|t| t.arr_clip_mut(id))
            else {
                continue;
            };
            if let Some(notes) = c.clip.notes_mut() {
                for n in notes.iter_mut() {
                    n.start = q.apply(n.start);
                }
            }
            self.sync_lane(track);
        }
        self.mark_edit("Record Quantization");
        self.commit_now();
    }

    /// Puts what's been recorded so far on the timeline. With MIDI Arrangement Overdub the notes
    /// join what is there; otherwise each finished pass of the loop becomes a take of its own.
    pub(super) fn flush_recording(&mut self) {
        if self.project.midi_arrangement_overdub {
            self.flush_overdub();
        } else {
            self.flush_takes();
        }
    }

    /// Places each pass that has finished on a take lane and copies it to the main lane. Each is
    /// an undo step, as for audio takes.
    fn flush_takes(&mut self) {
        let Some(recording) = self.recording.as_mut() else {
            return;
        };
        let passes = recording.take_finished_passes(self.project.time_signature);
        for pass in passes {
            let color = self.take_color(pass.track);
            let Some(t) = self.project.track_mut(pass.track) else {
                continue;
            };
            let mut clip = pass.clip;
            clip.color = color;
            let placed = ArrangementClip::new(clip, pass.start);
            let take = placed.id;
            let (_, main) = t.record_take(placed);
            if let Some(r) = self.recording.as_mut() {
                r.note_placed(pass.track, take);
                r.note_placed(pass.track, main);
            }
            self.sync_lane(pass.track);
            self.mark_edit("Record");
            self.commit_now();
        }
    }

    fn flush_overdub(&mut self) {
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
            t.settle_fades();
            if let Some(r) = self.recording.as_mut() {
                r.set_clip(p.track, id);
            }
            self.sync_lane(p.track);
            self.mark_edit("Record");
        }
    }
}
