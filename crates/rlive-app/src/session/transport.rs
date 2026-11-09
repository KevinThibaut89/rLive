//! Transport controls and clip launching.

use super::Session;
use rlive_core::{Quantize, TimeSignature, TrackId};
use rlive_engine::{EngineCommand, LaunchCmd, RecordCmd, TransportCmd};
use rlive_project::MetronomeOptions;

impl Session {
    // ------------------------------------------------------------- transport

    pub fn is_playing(&self) -> bool {
        self.engine
            .shared
            .playing
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn play(&mut self) {
        // Set Song Start Time Here overrules the insert marker.
        if let Some(start) = self.project.arrangement.song_start {
            if !self.is_playing() {
                self.seek(start as f64);
            }
        }
        self.send(EngineCommand::Transport(TransportCmd::Play));
    }

    pub fn stop(&mut self) {
        // Stop ends Session recording along with the transport (manual p. 513).
        self.set_session_automation_record(false);
        self.send(EngineCommand::Transport(TransportCmd::Stop));
    }

    pub fn set_tempo(&mut self, bpm: f64) {
        // While clips lead the tempo, the tempo field is theirs.
        if self.project.has_tempo_leader() {
            return;
        }
        self.project.tempo = bpm.clamp(20.0, 999.0);
        self.send(EngineCommand::Transport(TransportCmd::SetTempo(
            self.project.tempo,
        )));
        self.mark_edit("Change Tempo");
    }

    pub fn set_time_signature(&mut self, sig: TimeSignature) {
        self.project.time_signature = sig;
        self.send(EngineCommand::Transport(TransportCmd::SetTimeSignature(
            sig,
        )));
        self.mark_edit("Change Time Signature");
    }

    pub fn set_quantize(&mut self, q: Quantize) {
        self.project.quantize = q;
        self.send(EngineCommand::Transport(TransportCmd::SetQuantize(q)));
        self.mark_edit("Change Quantization");
    }

    pub fn set_metronome(&mut self, on: bool) {
        self.project.metronome = on;
        self.send(EngineCommand::Transport(TransportCmd::SetMetronome(on)));
        self.mark_edit("Metronome");
    }

    /// The metronome's menu: count-in, sound, rhythm and when it sounds.
    pub fn set_metronome_options(&mut self, options: MetronomeOptions) {
        if self.project.metronome_options == options {
            return;
        }
        self.project.metronome_options = options;
        self.send(EngineCommand::Transport(TransportCmd::SetMetronomeConfig(
            super::build::metronome_config(&self.project),
        )));
        self.send(EngineCommand::Record(RecordCmd::SetCountIn(
            options.count_in,
        )));
        self.mark_edit("Metronome Settings");
    }

    pub fn launch_clip(&mut self, track: TrackId, slot: usize) {
        self.clip_down(track, slot, 1.0);
    }

    /// A clip's launch button went down: at `velocity`, for one played from a key or note.
    pub fn clip_down(&mut self, track: TrackId, slot: usize, velocity: f32) {
        // Switched off: launching it does nothing, rather than stopping what's playing.
        if self.clip(track, slot).is_some_and(|c| !c.active) {
            return;
        }
        // Its sample isn't ready: the slot is empty as far as the engine knows, and launching an
        // empty slot would stop the track.
        if self.clip_waiting_for_sample(track, slot) {
            return;
        }
        self.re_enable_clip_automation(track, slot);
        self.send(EngineCommand::Launch(LaunchCmd::ClipDown {
            track,
            slot,
            velocity,
        }));
    }

    /// A clip's launch button came up.
    pub fn clip_up(&mut self, track: TrackId, slot: usize) {
        self.send(EngineCommand::Launch(LaunchCmd::ClipUp { track, slot }));
    }

    pub fn stop_track(&mut self, track: TrackId) {
        self.send(EngineCommand::Launch(LaunchCmd::StopTrack(track)));
    }

    pub fn launch_scene(&mut self, scene: usize) {
        let tracks: Vec<TrackId> = self.project.tracks.iter().map(|t| t.id).collect();
        for track in tracks.iter().copied() {
            self.re_enable_clip_automation(track, scene);
        }
        let record_empty = self.record.start_recording_on_scene_launch;
        let to_record: Vec<TrackId> = if record_empty {
            self.armed_empty_slots(scene)
        } else {
            Vec::new()
        };
        if !to_record.is_empty() {
            self.send(EngineCommand::Launch(LaunchCmd::LaunchScene(scene)));
            for track in to_record {
                self.record_slot(track, scene);
            }
            return;
        }
        if tracks
            .iter()
            .any(|t| self.clip_waiting_for_sample(*t, scene))
        {
            // Launch the rest, leaving tracks whose clip can't play yet as they are.
            let returns = self.project.returns.iter().map(|t| t.id);
            let everyone: Vec<TrackId> = tracks.iter().copied().chain(returns).collect();
            for track in everyone {
                if !self.clip_waiting_for_sample(track, scene) {
                    self.send(EngineCommand::Launch(LaunchCmd::LaunchClip {
                        track,
                        slot: scene,
                    }));
                }
            }
            return;
        }
        self.send(EngineCommand::Launch(LaunchCmd::LaunchScene(scene)));
    }

    pub fn stop_all_clips(&mut self) {
        self.send(EngineCommand::Launch(LaunchCmd::StopAllClips));
    }
}
