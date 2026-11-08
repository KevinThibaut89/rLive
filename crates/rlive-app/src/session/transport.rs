//! Transport controls and clip launching.

use super::Session;
use rlive_core::{Quantize, TimeSignature, TrackId};
use rlive_engine::{EngineCommand, LaunchCmd, TransportCmd};

impl Session {
    // ------------------------------------------------------------- transport

    pub fn is_playing(&self) -> bool {
        self.engine
            .shared
            .playing
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn play(&mut self) {
        self.send(EngineCommand::Transport(TransportCmd::Play));
    }

    pub fn stop(&mut self) {
        self.send(EngineCommand::Transport(TransportCmd::Stop));
    }

    pub fn set_tempo(&mut self, bpm: f64) {
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

    pub fn launch_clip(&mut self, track: TrackId, slot: usize) {
        self.re_enable_clip_automation(track, slot);
        self.send(EngineCommand::Launch(LaunchCmd::LaunchClip { track, slot }));
    }

    pub fn stop_track(&mut self, track: TrackId) {
        self.send(EngineCommand::Launch(LaunchCmd::StopTrack(track)));
    }

    pub fn launch_scene(&mut self, scene: usize) {
        let tracks: Vec<TrackId> = self.project.tracks.iter().map(|t| t.id).collect();
        for track in tracks {
            self.re_enable_clip_automation(track, scene);
        }
        self.send(EngineCommand::Launch(LaunchCmd::LaunchScene(scene)));
    }

    pub fn stop_all_clips(&mut self) {
        self.send(EngineCommand::Launch(LaunchCmd::StopAllClips));
    }
}
