//! Live MIDI and recording.

use crate::capture::CaptureSink;
use crate::shared::NO_SLOT;
use crate::{retire, Engine, EngineEvent, Garbage, PlayedNote};
use rlive_core::{MidiEvent, MidiMessage, TrackId};
use std::sync::atomic::Ordering::Relaxed;
use std::time::Instant;

pub enum RecordCmd {
    /// Arm or disarm recording of played MIDI.
    SetRecording(bool),
    /// Automation Arm: while recording, parameter gestures are recorded as automation.
    SetAutomationArm(bool),
    /// Whether gestures on this track record into its playing Session clip (Automation Arm still
    /// applies). Turning it off ends every gesture in progress on the track.
    SetSessionAutomation { track: TrackId, on: bool },

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
    /// Give a track what its next take goes into. It records nothing until the recording it
    /// belongs to starts. One a track already has is closed and retired.
    InstallCapture {
        track: TrackId,
        sink: Box<CaptureSink>,
    },
    /// Take a track's sink back, unused.
    RemoveCapture { track: TrackId },
    /// Punch-In and Punch-Out. Recording keeps within the two, either may be open-ended.
    SetPunch {
        punch_in: Option<f64>,
        punch_out: Option<f64>,
    },
    /// Whether notes played on a track are going into its playing clip.
    SetOverdub { track: TrackId, on: bool },
    /// Silence a track's Arrangement clips under a take.
    MuteLane { track: TrackId, on: bool },
    /// Bars of count-in before recording starts.
    SetCountIn(u8),
}

impl Engine {
    pub(crate) fn handle_record(&mut self, cmd: RecordCmd) {
        use RecordCmd as C;
        match cmd {
            C::SetRecording(true) => {
                self.recording = true;
                self.rec_from = self.tick;
            }
            C::SetRecording(false) => self.finish_recording(),
            C::SetAutomationArm(on) => self.automation_arm = on,
            C::SetSessionAutomation { track, on } => {
                if let Some(t) = self.track_mut(track) {
                    t.session_automation = on;
                    if !on {
                        t.bus.release_all_touches();
                    }
                }
            }
            C::Midi { track, message } => {
                if let Some(t) = self.track_mut(track) {
                    if t.live.len() < t.live.capacity() {
                        t.live.push(MidiEvent::new(0, message));
                    }
                }
            }
            C::PlayedMidi { track, message, at } => self.play_midi(track, message, at),
            C::InstallCapture { track, sink } => {
                let Some(t) = self.tracks.iter_mut().find(|t| t.id == track) else {
                    retire(&mut self.garbage, Garbage::Boxed(sink));
                    return;
                };
                if let Some(old) = t.capture.replace(sink) {
                    retire(&mut self.garbage, Garbage::Boxed(old));
                }
            }
            C::RemoveCapture { track } => {
                if let Some(t) = self.tracks.iter_mut().find(|t| t.id == track) {
                    if let Some(old) = t.capture.take() {
                        retire(&mut self.garbage, Garbage::Boxed(old));
                    }
                }
            }
            C::SetPunch {
                punch_in,
                punch_out,
            } => {
                self.punch_in = punch_in;
                self.punch_out = punch_out;
            }
            C::SetOverdub { track, on } => {
                if let Some(t) = self.tracks.iter_mut().find(|t| t.id == track) {
                    t.overdub = on;
                    t.shared.overdubbing.store(on, Relaxed);
                }
            }
            C::MuteLane { track, on } => {
                if let Some(t) = self.tracks.iter_mut().find(|t| t.id == track) {
                    t.replacing = on;
                }
            }
            C::SetCountIn(bars) => self.count_in_bars = bars,
        }
    }

    /// Where recording is allowed on the timeline: from where it started, within the punch points.
    pub(crate) fn arrangement_window(&self) -> Option<(f64, f64)> {
        self.recording.then(|| {
            (
                self.rec_from
                    .max(self.punch_in.unwrap_or(f64::NEG_INFINITY)),
                self.punch_out.unwrap_or(f64::INFINITY),
            )
        })
    }

    /// Close every Arrangement take, reporting what it took.
    fn close_arrangement_takes(&mut self) {
        for t in self.tracks.iter_mut() {
            if t.capture
                .as_ref()
                .is_some_and(|c| c.mode == crate::capture::CaptureMode::Arrangement)
            {
                if let Some(sink) = t.capture.take() {
                    let _ = self.events.push(EngineEvent::CaptureClosed {
                        track: t.id,
                        frames: sink.frames(),
                    });
                    retire(&mut self.garbage, Garbage::Boxed(sink));
                }
            }
        }
    }

    /// End every slot's recording here and now, as stopping the transport does.
    pub(super) fn abort_slot_recordings(&mut self, tick: f64) {
        for i in 0..self.tracks.len() {
            if self.tracks[i].slot_rec.is_some() {
                self.end_slot_recording(i, tick);
            }
        }
    }

    /// Finish a slot's recording at `tick`.
    pub(crate) fn end_slot_recording(&mut self, i: usize, tick: f64) {
        let t = &mut self.tracks[i];
        let Some(rec) = t.slot_rec.take() else {
            return;
        };
        t.shared.recording_slot.store(NO_SLOT, Relaxed);
        t.shared.record_queued_slot.store(NO_SLOT, Relaxed);
        let track = t.id;
        let sink = t.capture.take();
        if let Some(sink) = sink {
            let _ = self.events.push(EngineEvent::CaptureClosed {
                track,
                frames: sink.frames(),
            });
            retire(&mut self.garbage, Garbage::Boxed(sink));
        }
        let _ = self.events.push(EngineEvent::SlotRecordEnded {
            track,
            slot: rec.slot,
            tick,
        });
    }

    /// Tell the UI that recording ended, at the current playhead.
    pub(super) fn finish_recording(&mut self) {
        if self.recording && self.playing {
            let tick = self.tick;
            self.close_arrangement_takes();
            let _ = self.events.push(EngineEvent::RecordingStopped { tick });
            for t in self.tracks.iter_mut() {
                t.bus.release_all_touches();
            }
            self.master.bus.release_all_touches();
        } else if self.recording {
            self.close_arrangement_takes();
            let _ = self
                .events
                .push(EngineEvent::RecordingStopped { tick: self.tick });
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
        if !is_note && !matches!(message, MidiMessage::NoteExpr { .. }) {
            return;
        }
        let ago = at.elapsed().as_secs_f64();
        // Every note goes to the buffer Capture MIDI looks back through.
        if is_note {
            let _ = self.played.push(PlayedNote {
                track,
                seconds: self.clock as f64 / self.sample_rate - ago,
                tick: self.playing.then(|| self.tick_seconds_ago(ago)),
                message,
            });
        }
        if !self.playing {
            return;
        }
        let tick = self.tick_seconds_ago(ago).max(self.record_floor);
        let Some(i) = self.tracks.iter().position(|t| t.id == track) else {
            return;
        };
        let t = &self.tracks[i];
        if t.overdub {
            if let Some(p) = &t.player.playing {
                let position = t.player.position(tick);
                let _ = self.events.push(EngineEvent::OverdubMidi {
                    track,
                    slot: p.slot,
                    position,
                    tick,
                    message,
                });
                return;
            }
        }
        let in_slot = t
            .slot_rec
            .is_some_and(|r| r.started && r.close.is_none_or(|c| tick < c.0));
        let in_window = self
            .arrangement_window()
            .is_some_and(|(from, until)| tick >= from && tick < until);
        if in_slot || in_window {
            let _ = self.events.push(EngineEvent::RecordedMidi {
                track,
                tick,
                message,
            });
        }
    }

    /// Route played MIDI to its track and the recorder.
    pub(crate) fn play_midi(&mut self, track: TrackId, message: MidiMessage, at: Instant) {
        // The controller's layout comes first: what's recorded is what was heard.
        let message = match self
            .tracks
            .iter()
            .find(|t| t.id == track)
            .and_then(|t| t.key_layout.as_deref())
        {
            Some(layout) => match layout.apply(message) {
                Some(m) => m,
                None => return,
            },
            None => message,
        };
        self.record_midi(track, message, at);
        if let Some(t) = self.track_mut(track) {
            if t.live.len() < t.live.capacity() {
                t.live.push(MidiEvent::new(0, message));
            }
        }
    }
}
