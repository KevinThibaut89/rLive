//! Live MIDI and recording.

use crate::{Engine, EngineEvent};
use rlive_core::{MidiEvent, MidiMessage, TrackId};
use std::time::Instant;

pub enum RecordCmd {
    /// Arm or disarm recording of played MIDI.
    SetRecording(bool),
    /// Automation Arm: while recording, parameter gestures are recorded as automation.
    SetAutomationArm(bool),

    /// Live MIDI (keyboard input) for a track. Never recorded, e.g. piano-roll previews.
    Midi {
        track: TrackId,
        message: MidiMessage,
    },
    /// MIDI performed by the user at time `at`. Audible, and recorded while recording.
    PlayedMidi {
        track: TrackId,
        message: MidiMessage,
        at: Instant,
    },
}

impl Engine {
    pub(crate) fn handle_record(&mut self, cmd: RecordCmd) {
        use RecordCmd as C;
        match cmd {
            C::SetRecording(true) => self.recording = true,
            C::SetRecording(false) => self.finish_recording(),
            C::SetAutomationArm(on) => self.automation_arm = on,
            C::Midi { track, message } => {
                if let Some(t) = self.track_mut(track) {
                    if t.live.len() < t.live.capacity() {
                        t.live.push(MidiEvent::new(0, message));
                    }
                }
            }
            C::PlayedMidi { track, message, at } => self.play_midi(track, message, at),
        }
    }

    /// Tell the UI that recording ended, at the current playhead.
    pub(super) fn finish_recording(&mut self) {
        if self.recording && self.playing {
            let tick = self.tick;
            let _ = self.events.push(EngineEvent::RecordingStopped { tick });
            for t in self.tracks.iter_mut() {
                t.bus.release_all_touches();
            }
            self.master.bus.release_all_touches();
        }
        self.recording = false;
    }

    /// Report a played message if it is a note and we are recording, placing it where the
    /// user played it rather than where this block happens to be.
    fn record_midi(&mut self, track: TrackId, message: MidiMessage, at: Instant) {
        let is_note = matches!(
            message,
            MidiMessage::NoteOn { .. } | MidiMessage::NoteOff { .. }
        );
        if !(is_note && self.recording && self.playing) {
            return;
        }
        let tick = self.tick_seconds_ago(at.elapsed().as_secs_f64());
        let _ = self.events.push(EngineEvent::RecordedMidi {
            track,
            tick: tick.max(self.record_floor),
            message,
        });
    }

    /// Route played MIDI to its track and the recorder.
    pub(crate) fn play_midi(&mut self, track: TrackId, message: MidiMessage, at: Instant) {
        self.record_midi(track, message, at);
        if let Some(t) = self.track_mut(track) {
            if t.live.len() < t.live.capacity() {
                t.live.push(MidiEvent::new(0, message));
            }
        }
    }
}
