//! Playback position and global timing.

use crate::CountIn;
use crate::{retire, Engine, Garbage, JumpKind, MetronomeConfig};
use rlive_core::{time::TimeMaps, Quantize, TimeSignature};
use std::sync::Arc;

pub enum TransportCmd {
    Play,
    Stop,
    SetTempo(f64),
    SetTimeSignature(TimeSignature),
    /// Replace the song's tempo and meter together. The old maps go back as garbage. Keep no
    /// other reference to them: `SetTempo` and `SetTimeSignature` edit the engine's copy in place.
    SetTimeMaps(Arc<TimeMaps>),
    SetMetronome(bool),
    /// The metronome's sound, rhythm and whether it only sounds while recording.
    SetMetronomeConfig(MetronomeConfig),
    SetQuantize(Quantize),
    /// Move the playhead and the insert marker to this tick.
    Seek(f64),
    /// Loop playback over `(start, end)` in ticks, or stop looping.
    SetLoop(Option<(f64, f64)>),
}

impl Engine {
    pub(crate) fn handle_transport(&mut self, cmd: TransportCmd) {
        use TransportCmd as C;
        match cmd {
            C::Play => self.start_transport(),
            C::Stop => self.stop_transport(),
            C::SetTempo(t) => {
                if let Some(maps) = Arc::get_mut(&mut self.maps) {
                    maps.tempo.set_constant(t);
                }
                self.mark_tempo_change();
            }
            C::SetTimeSignature(s) => {
                if let Some(maps) = Arc::get_mut(&mut self.maps) {
                    maps.bars.set_base(s);
                }
            }
            C::SetTimeMaps(maps) => {
                let old = std::mem::replace(&mut self.maps, maps);
                retire(&mut self.garbage, Garbage::Shared(old));
                self.mark_tempo_change();
            }
            C::SetMetronome(on) => self.metronome.enabled = on,
            C::SetMetronomeConfig(config) => self.metronome.config = config,
            C::SetQuantize(q) => self.quantize = q,
            C::Seek(to) => {
                if to.is_finite() {
                    let to = to.max(0.0);
                    self.insert_marker = to;
                    self.jump(to, JumpKind::Seek);
                }
            }
            C::SetLoop(range) => {
                self.loop_range = range.filter(|&(s, e)| s >= 0.0 && e > s && e.is_finite());
                self.reached_loop_end = false;
            }
        }
    }

    /// Start the transport, after the count-in if recording is waiting for one.
    pub(super) fn start_transport(&mut self) {
        if self.playing || self.count_in.is_some() {
            return;
        }
        let recording = self.recording || self.tracks.iter().any(|t| t.slot_rec.is_some());
        if recording && self.count_in_bars > 0 {
            let sig = self.maps.bars.signature_at(self.tick);
            let ticks = self.count_in_bars as f64 * sig.ticks_per_bar() as f64;
            let run = self.tempo_run();
            self.count_in = Some(CountIn {
                frames_left: run.frames_to(ticks).round() as u64,
                frames_total: run.frames_to(ticks).round() as u64,
                ticks,
                sig,
            });
            self.shared.count_in_left.store(ticks);
            return;
        }
        self.begin_playing();
    }

    /// The moment the transport actually starts running.
    pub(crate) fn begin_playing(&mut self) {
        self.count_in = None;
        self.shared.count_in_left.store(0.0);
        self.playing = true;
        self.record_floor = self.tick;
        self.rec_from = self.tick;
        self.sync_sample_pos();
        for t in self.tracks.iter_mut() {
            if let Some(sink) = t.capture.as_mut() {
                sink.restart();
            }
        }
    }

    /// A change of tempo while recording becomes a Warp Marker on every take.
    fn mark_tempo_change(&mut self) {
        if !self.playing {
            return;
        }
        let tps = self.tempo_run().ticks_per_sample;
        for t in self.tracks.iter_mut() {
            if let Some(sink) = &t.capture {
                sink.mark_tempo(t.id, tps, &mut self.events);
            }
        }
    }

    fn stop_transport(&mut self) {
        if self.playing || self.count_in.is_some() {
            // Before the tick resets, so the UI sees every recorded note first.
            self.finish_recording();
            self.abort_slot_recordings(self.tick);
        } else {
            self.insert_marker = 0.0;
        }
        self.count_in = None;
        self.shared.count_in_left.store(0.0);
        self.playing = false;
        self.tick = self.insert_marker;
        self.reached_loop_end = false;
        self.sync_sample_pos();
        for t in self.tracks.iter_mut() {
            t.player.pending = None;
            t.player.release_all(0, &mut t.live);
            t.lane.release_and_resync(0, &mut t.live);
            if let Some(p) = t.player.playing.take() {
                retire(&mut self.garbage, Garbage::Shared(p.clip));
            }
            for d in t.devices.iter_mut() {
                d.reset();
            }
        }
        for d in self.master.devices.iter_mut() {
            d.reset();
        }
    }
}
