//! Playback position and global timing.

use crate::sync::{LinkHolder, SyncConfig};
use crate::CountIn;
use crate::{retire, Engine, Garbage, JumpKind, MetronomeConfig};
use rlive_core::{time::TimeMaps, Quantize, TimeSignature};
use std::sync::Arc;

pub enum TransportCmd {
    Play,
    Stop,
    SetTempo(f64),
    SetTimeSignature(TimeSignature),
    /// One tempo for the whole song over whatever the map says: the user or something outside
    /// overriding it. `None` gives the map back, which the session follows with the map itself.
    OverrideTempo(Option<f64>),
    /// Phase Nudge: how much faster or slower time runs while it is held, as a ratio.
    SetNudge(f64),
    /// Replace the song's tempo and meter together. The old maps go back as garbage. Keep no
    /// other reference to them: `SetTempo` and `SetTimeSignature` edit the engine's copy in place.
    SetTimeMaps(Arc<TimeMaps>),
    /// MIDI sync, Tempo Follower and clock outputs.
    SetSync(Box<SyncConfig>),
    /// Connect a Link session, or disconnect one. The old one goes back as garbage.
    SetLink(Option<Box<LinkHolder>>),
    /// The user changed the tempo with Link on, which the Link session hears about.
    TellLinkTempo(f64),
    SetMetronome(bool),
    /// The metronome's sound, rhythm and whether it only sounds while recording.
    SetMetronomeConfig(MetronomeConfig),
    SetQuantize(Quantize),
    /// Move the playhead and the insert marker to this tick.
    Seek(f64),
    /// Play on from where the transport last stopped.
    Continue,
    /// Launch from `to`, at the next launch point when already playing. With `hold`, playback
    /// then repeats the quantized region starting there until [`Self::ReleaseHold`].
    Trigger {
        to: f64,
        hold: bool,
    },
    /// Let go of a held scrub.
    ReleaseHold,
    /// Stop when the playhead reaches this tick.
    StopAt(Option<f64>),
    SetChase(bool),
    /// Loop playback over `(start, end)` in ticks, or stop looping.
    SetLoop(Option<(f64, f64)>),
}

impl Engine {
    pub(crate) fn handle_transport(&mut self, cmd: TransportCmd) {
        use TransportCmd as C;
        match cmd {
            C::Play => self.start_transport(),
            C::Stop => self.stop_transport(),
            C::Continue => {
                if !self.playing {
                    self.jump(self.stop_point, JumpKind::Seek);
                    self.start_transport();
                }
            }
            C::Trigger { to, hold } => self.trigger(to, hold),
            C::ReleaseHold => self.hold_loop = None,
            C::StopAt(at) => self.stop_at = at.filter(|t| t.is_finite()),
            C::SetChase(on) => self.chase = on,
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
                // A tempo fixed from outside goes on holding over the fresh map.
                match self.sync.tempo_override {
                    Some(bpm) => {
                        if let Some(m) = Arc::get_mut(&mut self.maps) {
                            m.tempo.flatten(bpm);
                        }
                    }
                    None => self.mark_tempo_change(),
                }
            }
            C::OverrideTempo(Some(bpm)) => self.apply_tempo_override(bpm),
            C::OverrideTempo(None) => self.clear_tempo_override(),
            C::SetNudge(ratio) => self.sync.nudge = ratio.clamp(0.5, 1.5),
            C::SetSync(config) => {
                self.set_sync_config(*config);
                retire(&mut self.garbage, Garbage::Boxed(config));
            }
            C::SetLink(link) => {
                let old = std::mem::replace(&mut self.sync.link, link);
                self.sync.link_waiting = false;
                self.sync.link_tempo = 0.0;
                if let Some(old) = old {
                    retire(&mut self.garbage, Garbage::Boxed(old));
                }
            }
            C::TellLinkTempo(bpm) => {
                self.tempo_from_link(bpm);
                if let Some(link) = self.sync.link.as_mut() {
                    link.0.set_tempo(bpm);
                }
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
        if self.link_holds_start() {
            return;
        }
        let recording = self.recording || self.tracks.iter().any(|t| t.slot_rec.is_some());
        // Link has its own launch point, so there's no count-in under it.
        if recording && self.count_in_bars > 0 && !self.sync.link_enabled() {
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
        self.tell_link_playing(true);
    }

    /// A change of tempo while recording becomes a Warp Marker on every take.
    pub(crate) fn mark_tempo_change(&mut self) {
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

    /// The scrub area and the locators: a jump the global quantization holds back while playing.
    fn trigger(&mut self, to: f64, hold: bool) {
        if !to.is_finite() {
            return;
        }
        let to = to.max(0.0);
        let quantum = self.quantize.ticks(self.maps.bars.signature_at(to)) as f64;
        self.hold_loop = (hold && quantum > 0.0).then_some((to, to + quantum));
        if !self.playing {
            self.jump(to, JumpKind::Seek);
            self.start_transport();
            self.pending_jump = None;
            return;
        }
        let at = self.maps.bars.next_boundary(self.tick, self.quantize);
        if quantum > 0.0 && at > self.tick {
            self.pending_jump = Some(crate::PendingJump { at, to });
        } else {
            self.pending_jump = None;
            self.jump(to, JumpKind::Seek);
        }
    }

    pub(crate) fn stop_transport(&mut self) {
        if self.playing || self.count_in.is_some() {
            // Before the tick resets, so the UI sees every recorded note first.
            self.finish_recording();
            self.abort_slot_recordings(self.tick);
            self.stop_point = self.tick;
        } else {
            self.insert_marker = 0.0;
        }
        if self.playing {
            self.tell_link_playing(false);
        }
        self.sync.link_waiting = false;
        self.count_in = None;
        self.shared.count_in_left.store(0.0);
        self.pending_jump = None;
        self.pending_scene = None;
        self.scene_follow = None;
        self.hold_loop = None;
        self.stop_at = None;
        self.playing = false;
        self.tick = self.insert_marker;
        self.reached_loop_end = false;
        self.sync_sample_pos();
        for t in self.tracks.iter_mut() {
            t.player.pending = None;
            t.player.follow = None;
            t.player.repeat = None;
            t.player.released_early = false;
            t.player.from_scene = false;
            t.player.logged = None;
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
