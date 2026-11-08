//! The rLive real-time engine.
//!
//! The [`Engine`] lives on the audio thread; the UI talks to it through an [`EngineHandle`]:
//!
//! * UI → audio: [`EngineCommand`]s over a lock-free SPSC ring.
//! * audio → UI: [`Garbage`] (anything the engine let go of, so it is freed off the audio
//!   thread), [`EngineEvent`]s, and atomics in [`shared`] for meters and playheads.
//!
//! The engine never allocates or frees memory while processing.

#![allow(clippy::vec_box)] // nodes are boxed so the audio thread only moves pointers

pub mod clip;
mod commands;
pub mod lane;
pub mod params;
pub mod shared;
pub mod track;

use clip::BlockTime;
pub use clip::{AudioClip, ClipData, MidiClip, Note};
pub use commands::{ClipCmd, GraphCmd, LaunchCmd, ParamCmd, RecordCmd, TransportCmd};
pub use lane::{ArrangementLane, LaneClip};
pub use params::{
    mix_cell, BusLayout, CellIx, CellSpec, CellTarget, ControlHooks, ControlStep, ParamBus,
    TrackRoutes, CONTROL_STEP,
};
use rlive_core::{
    time::ticks_per_sample, DeviceId, MidiMessage, Quantize, TimeSignature, TrackId, TransportInfo,
};
use shared::EngineShared;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;
use track::BlockCtx;
pub use track::{DeviceSlot, MasterNode, MixSettings, TrackNode};

pub const MAX_TRACKS: usize = 256;
const COMMAND_CAPACITY: usize = 4096;
const GARBAGE_CAPACITY: usize = 4096;
const EVENT_CAPACITY: usize = 8192;
const MIDI_CAPACITY: usize = 2048;

/// Sends live MIDI straight to the audio thread (e.g. from a MIDI-input callback thread),
/// bypassing the UI. Messages are timestamped here, so recording can place them where they
/// were played rather than where the audio thread picked them up.
pub struct MidiSender {
    producer: rtrb::Producer<(TrackId, MidiMessage, Instant)>,
}

impl MidiSender {
    pub fn send(&mut self, track: TrackId, message: MidiMessage) -> bool {
        self.producer.push((track, message, Instant::now())).is_ok()
    }
}

/// Which device chain a command targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChainTarget {
    Track(TrackId),
    Master,
}

/// Messages from the UI to the audio thread.
pub enum EngineCommand {
    Transport(TransportCmd),
    Launch(LaunchCmd),
    Clip(ClipCmd),
    Param(ParamCmd),
    Record(RecordCmd),
    Graph(GraphCmd),
}

/// Things the engine is done with. They are dropped on the UI thread.
///
/// Retiring something only turns its pointer into a wide one, so the audio thread never
/// allocates to do it, and a new kind of retired object needs no change here.
pub enum Garbage {
    /// Anything the engine owned outright: tracks, the master, device slots.
    Boxed(Box<dyn Send>),
    /// Anything shared with the UI: clips, lanes.
    Shared(Arc<dyn Send + Sync>),
    /// A resource a device replaced or refused.
    Resource(rlive_core::Resource),
}

/// Notifications from the audio thread.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EngineEvent {
    /// A processor changed one of its own parameters (e.g. from a plugin editor).
    ParamChanged {
        target: ChainTarget,
        device: DeviceId,
        param: u32,
        value: f64,
    },
    /// A note was played while recording. `tick` is where it was played on the timeline.
    RecordedMidi {
        track: TrackId,
        tick: f64,
        message: MidiMessage,
    },
    /// The playhead jumped while recording (a loop wrap or a seek).
    Jumped { from: f64, to: f64 },
    /// Recording ended with the playhead at `tick`. Follows every note it recorded.
    RecordingStopped { tick: f64 },
}

/// UI-side handle to a running engine.
pub struct EngineHandle {
    commands: rtrb::Producer<EngineCommand>,
    garbage: rtrb::Consumer<Garbage>,
    events: rtrb::Consumer<EngineEvent>,
    pub shared: Arc<EngineShared>,
    pub sample_rate: f64,
    pub max_block: usize,
    midi: Option<MidiSender>,
}

impl EngineHandle {
    /// Take the (single) live-MIDI sender. Returns `None` if it was already taken.
    pub fn take_midi_sender(&mut self) -> Option<MidiSender> {
        self.midi.take()
    }

    /// Queue a command. Returns it back if the queue is full.
    pub fn send(&mut self, cmd: EngineCommand) -> Result<(), EngineCommand> {
        self.commands
            .push(cmd)
            .map_err(|rtrb::PushError::Full(c)| c)
    }

    /// Free everything the audio thread has released. Call regularly from the UI thread.
    pub fn collect_garbage(&mut self) -> usize {
        let mut n = 0;
        while let Ok(g) = self.garbage.pop() {
            drop(g);
            n += 1;
        }
        n
    }

    pub fn poll_event(&mut self) -> Option<EngineEvent> {
        self.events.pop().ok()
    }
}

struct Metronome {
    enabled: bool,
    phase: f32,
    remaining: usize,
    freq: f32,
    amp: f32,
}

impl Metronome {
    fn render(&mut self, time: BlockTime, sig: TimeSignature, left: &mut [f32], right: &mut [f32]) {
        if !self.enabled {
            return;
        }
        let beat = sig.ticks_per_beat() as f64;
        let click_len = (time.sample_rate * 0.03) as usize;
        let first = (time.t0 / beat).ceil();
        let mut next_click = first * beat;
        let t1 = time.t1();
        let mut start = 0;
        loop {
            let click_at = if next_click < t1 {
                Some(time.offset(next_click))
            } else {
                None
            };
            let until = click_at.unwrap_or(time.frames);
            self.write(
                left,
                right,
                start,
                until,
                time.sample_rate as f32,
                click_len,
            );
            match click_at {
                Some(off) => {
                    let downbeat = ((next_click / beat).round() as u64)
                        .is_multiple_of(sig.numerator.max(1) as u64);
                    self.freq = if downbeat { 1760.0 } else { 880.0 };
                    self.amp = if downbeat { 0.5 } else { 0.3 };
                    self.phase = 0.0;
                    self.remaining = click_len;
                    start = off;
                    next_click += beat;
                }
                None => break,
            }
        }
    }

    fn write(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
        from: usize,
        to: usize,
        sr: f32,
        len: usize,
    ) {
        for i in from..to {
            if self.remaining == 0 {
                return;
            }
            let env = self.remaining as f32 / len.max(1) as f32;
            let s = (self.phase * std::f32::consts::TAU).sin() * self.amp * env * env;
            self.phase = (self.phase + self.freq / sr).fract();
            left[i] += s;
            right[i] += s;
            self.remaining -= 1;
        }
    }
}

/// The audio-thread half of the engine.
pub struct Engine {
    sample_rate: f64,
    max_block: usize,
    tracks: Vec<Box<TrackNode>>,
    master: Box<MasterNode>,

    playing: bool,
    tempo: f64,
    signature: TimeSignature,
    quantize: Quantize,
    tick: f64,
    sample_pos: u64,
    insert_marker: f64,
    loop_range: Option<(f64, f64)>,
    /// The last block played up to `loop_end`. Tells that apart from being sought onto it,
    /// since only the former wraps.
    reached_loop_end: bool,
    recording: bool,
    /// Backdated recorded notes never land before this tick.
    record_floor: f64,
    metronome: Metronome,
    smooth_coeff: f32,
    /// Whether any track was soloed at the last control step.
    any_solo: bool,

    commands: rtrb::Consumer<EngineCommand>,
    garbage: rtrb::Producer<Garbage>,
    events: rtrb::Producer<EngineEvent>,
    midi: rtrb::Consumer<(TrackId, MidiMessage, Instant)>,
    shared: Arc<EngineShared>,
}

/// Create an engine and its UI handle.
pub fn create_engine(sample_rate: f64, max_block: usize) -> (Engine, EngineHandle) {
    let (cmd_tx, cmd_rx) = rtrb::RingBuffer::new(COMMAND_CAPACITY);
    let (gb_tx, gb_rx) = rtrb::RingBuffer::new(GARBAGE_CAPACITY);
    let (ev_tx, ev_rx) = rtrb::RingBuffer::new(EVENT_CAPACITY);
    let (midi_tx, midi_rx) = rtrb::RingBuffer::new(MIDI_CAPACITY);
    let shared = Arc::new(EngineShared::default());
    shared.sample_rate.store(sample_rate);
    let engine = Engine {
        sample_rate,
        max_block,
        tracks: Vec::with_capacity(MAX_TRACKS),
        master: Box::new(MasterNode::new(max_block)),
        playing: false,
        tempo: 120.0,
        signature: TimeSignature::default(),
        quantize: Quantize::Bar,
        tick: 0.0,
        sample_pos: 0,
        insert_marker: 0.0,
        loop_range: None,
        reached_loop_end: false,
        recording: false,
        record_floor: 0.0,
        metronome: Metronome {
            enabled: false,
            phase: 0.0,
            remaining: 0,
            freq: 880.0,
            amp: 0.3,
        },
        // ~5 ms one-pole smoothing.
        smooth_coeff: 1.0 - (-1.0 / (0.005 * sample_rate as f32)).exp(),
        any_solo: false,
        commands: cmd_rx,
        garbage: gb_tx,
        events: ev_tx,
        midi: midi_rx,
        shared: shared.clone(),
    };
    let handle = EngineHandle {
        commands: cmd_tx,
        garbage: gb_rx,
        events: ev_rx,
        shared,
        sample_rate,
        max_block,
        midi: Some(MidiSender { producer: midi_tx }),
    };
    (engine, handle)
}

fn retire(garbage: &mut rtrb::Producer<Garbage>, g: Garbage) {
    if let Err(rtrb::PushError::Full(g)) = garbage.push(g) {
        // Never free on the audio thread; leaking is the lesser evil.
        std::mem::forget(g);
    }
}

impl Engine {
    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    pub fn max_block(&self) -> usize {
        self.max_block
    }

    fn transport(&self) -> TransportInfo {
        TransportInfo {
            playing: self.playing,
            tempo: self.tempo,
            time_signature: self.signature,
            tick: self.tick,
            sample_pos: self.sample_pos,
            sample_rate: self.sample_rate,
            bar_start: self.signature.bar_start(self.tick),
            loop_range: self.loop_range,
        }
    }

    fn track_mut(&mut self, id: TrackId) -> Option<&mut TrackNode> {
        self.tracks
            .iter_mut()
            .find(|t| t.id == id)
            .map(|b| &mut **b)
    }

    fn bus_mut(&mut self, target: ChainTarget) -> Option<&mut ParamBus> {
        match target {
            ChainTarget::Master => Some(&mut self.master.bus),
            ChainTarget::Track(id) => self.track_mut(id).map(|t| &mut *t.bus),
        }
    }

    fn chain_mut(&mut self, target: ChainTarget) -> Option<&mut Vec<Box<DeviceSlot>>> {
        match target {
            ChainTarget::Master => Some(&mut self.master.devices),
            ChainTarget::Track(id) => self.track_mut(id).map(|t| &mut t.devices),
        }
    }

    fn launch_point(&self) -> f64 {
        self.quantize.next_boundary(self.tick, self.signature)
    }

    fn ticks_per_sample(&self) -> f64 {
        ticks_per_sample(self.tempo, self.sample_rate)
    }

    fn sync_sample_pos(&mut self) {
        self.sample_pos = (self.tick / self.ticks_per_sample()).round() as u64;
    }

    /// Move the playhead without disturbing Session clips or device state.
    fn jump(&mut self, to: f64, kind: JumpKind) {
        if self.recording && self.playing {
            let from = self.tick;
            let _ = self.events.push(EngineEvent::Jumped { from, to });
        }
        let delta = to - self.tick;
        self.tick = to;
        self.record_floor = to;
        self.reached_loop_end = false;
        self.sync_sample_pos();
        let launch_at = self.launch_point();
        for t in self.tracks.iter_mut() {
            if let Some(p) = t.player.playing.as_mut() {
                p.start_tick += delta;
            }
            if let Some(p) = t.player.pending.as_mut() {
                p.set_at(match kind {
                    JumpKind::Wrap => p.at() + delta,
                    JumpKind::Seek => launch_at,
                });
            }
            t.lane.release_and_resync(0, &mut t.live);
        }
    }

    fn handle_command(&mut self, cmd: EngineCommand) {
        match cmd {
            EngineCommand::Transport(c) => self.handle_transport(c),
            EngineCommand::Launch(c) => self.handle_launch(c),
            EngineCommand::Clip(c) => self.handle_clip(c),
            EngineCommand::Param(c) => self.handle_param(c),
            EngineCommand::Record(c) => self.handle_record(c),
            EngineCommand::Graph(c) => self.handle_graph(c),
        }
    }

    fn drain_input(&mut self) {
        // MIDI first: a note played just before a stop or seek belongs before it.
        while let Ok((track, message, at)) = self.midi.pop() {
            self.play_midi(track, message, at);
        }
        while let Ok(cmd) = self.commands.pop() {
            self.handle_command(cmd);
        }
    }

    /// Parameter commands waiting at the head of the queue, between control steps. Anything else
    /// stays queued until the next block, so commands never overtake each other.
    fn drain_param_commands(&mut self) {
        while matches!(self.commands.peek(), Ok(EngineCommand::Param(_))) {
            if let Ok(EngineCommand::Param(c)) = self.commands.pop() {
                self.handle_param(c);
            }
        }
    }

    /// Whole frames left before `end`, tolerating float error from accumulated block lengths
    /// so a block never comes up one frame short of a loop end.
    fn frames_until(&self, end: f64, tps: f64) -> usize {
        (((end - self.tick) / tps) + 1e-6).floor().max(0.0) as usize
    }

    /// Wrap at the loop end if we are there, and return the loop end when the next block of
    /// up to `n` frames runs into it.
    fn prepare_loop(&mut self, n: usize, tps: f64) -> Option<f64> {
        if !self.playing {
            return None;
        }
        let (start, end) = self.loop_range?;
        // A tick already past the end (sought there) plays on; a loop shorter than one frame
        // could never advance.
        if end - start < tps || !(self.tick < end || self.reached_loop_end) {
            return None;
        }
        if self.frames_until(end, tps) == 0 {
            // Subtracting the exact loop length keeps the sub-sample remainder, so loop passes
            // do not drift against the sample grid.
            self.jump((self.tick - (end - start)).max(0.0), JumpKind::Wrap);
        }
        (self.frames_until(end, tps) <= n).then_some(end)
    }

    /// Render `left.len()` frames of output. Handles any buffer size by splitting into
    /// `max_block` chunks, and at the loop end.
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let started = std::time::Instant::now();
        let total = left.len().min(right.len());
        let mut done = 0;
        while done < total {
            self.drain_input();
            let tps = self.ticks_per_sample();
            let mut n = (total - done).min(self.max_block);
            let loop_end = self.prepare_loop(n, tps);
            if let Some(end) = loop_end {
                n = self.frames_until(end, tps).max(1);
            }
            self.process_block(
                &mut left[done..done + n],
                &mut right[done..done + n],
                tps,
                loop_end,
            );
            done += n;
        }
        if total > 0 {
            let budget = total as f64 / self.sample_rate;
            let load = (started.elapsed().as_secs_f64() / budget) as f32;
            let prev = self.shared.cpu_load.load();
            self.shared.cpu_load.store(prev + (load - prev) * 0.1);
        }
    }

    fn process_block(
        &mut self,
        out_l: &mut [f32],
        out_r: &mut [f32],
        tps: f64,
        loop_end: Option<f64>,
    ) {
        let frames = out_l.len();
        let transport = self.transport();
        let t1 = self.tick + frames as f64 * tps;
        let time = BlockTime {
            t0: self.tick,
            ticks_per_sample: tps,
            end: loop_end.map_or(t1, |e| t1.min(e)),
            frames,
            sample_rate: self.sample_rate,
            tempo: self.tempo,
        };

        let garbage = &mut self.garbage;
        let mut retire_clip = |c: Arc<ClipData>| retire(garbage, Garbage::Shared(c));
        for t in self.tracks.iter_mut() {
            t.begin_block(time, &transport, &mut retire_clip);
        }

        let ctx = BlockCtx {
            transport: &transport,
            ticks_per_sample: tps,
            frames,
        };
        let mut index = 0;
        let mut offset = 0;
        while offset < frames {
            if offset > 0 {
                self.drain_param_commands();
            }
            let events = &mut self.events;
            let mut on_param = |t: TrackId, d: DeviceId, p: u32, v: f64| {
                push_param_changed(events, ChainTarget::Track(t), d, p, v);
            };
            for t in self.tracks.iter_mut() {
                t.control_step(index, offset, &ctx, &mut on_param);
            }
            let any_solo = self.tracks.iter().any(|t| t.mix.solo);
            let solo_changed = any_solo != self.any_solo;
            self.any_solo = any_solo;
            for t in self.tracks.iter_mut() {
                t.update_mix(offset, any_solo, solo_changed);
            }
            index += 1;
            offset += CONTROL_STEP;
        }

        let events = &mut self.events;
        let mut on_param = |t: TrackId, d: DeviceId, p: u32, v: f64| {
            push_param_changed(events, ChainTarget::Track(t), d, p, v);
        };
        for t in self.tracks.iter_mut() {
            t.finish_block(time, &ctx, &mut on_param);
        }

        let ml = &mut self.master.left[..frames];
        let mr = &mut self.master.right[..frames];
        ml.fill(0.0);
        mr.fill(0.0);
        for t in self.tracks.iter_mut() {
            t.apply_mix(frames, self.smooth_coeff);
            let (l, r) = t.output(frames);
            for i in 0..frames {
                ml[i] += l[i];
                mr[i] += r[i];
            }
        }
        if transport.playing {
            self.metronome.render(time, self.signature, ml, mr);
        }

        let mut on_master_param = |d: DeviceId, p: u32, v: f64| {
            push_param_changed(events, ChainTarget::Master, d, p, v);
        };
        self.master.begin_block();
        let mut index = 0;
        let mut offset = 0;
        while offset < frames {
            self.master
                .control_step(index, offset, &ctx, &mut on_master_param);
            index += 1;
            offset += CONTROL_STEP;
        }
        self.master.finish(
            &ctx,
            &self.shared.master_meter,
            self.smooth_coeff,
            &mut on_master_param,
        );
        out_l.copy_from_slice(&self.master.left[..frames]);
        out_r.copy_from_slice(&self.master.right[..frames]);

        if self.playing {
            self.tick = time.t1();
            self.sample_pos += frames as u64;
            self.reached_loop_end = loop_end.is_some();
        }
        self.shared.playing.store(self.playing, Ordering::Relaxed);
        self.shared.tick.store(self.tick);
        self.shared.insert_marker.store(self.insert_marker);
        self.shared.tempo.store(self.tempo);
    }
}

fn push_param_changed(
    events: &mut rtrb::Producer<EngineEvent>,
    target: ChainTarget,
    device: DeviceId,
    param: u32,
    value: f64,
) {
    let _ = events.push(EngineEvent::ParamChanged {
        target,
        device,
        param,
        value,
    });
}

#[derive(Clone, Copy)]
enum JumpKind {
    /// The loop came around: everything in flight keeps its distance to the playhead.
    Wrap,
    /// The user moved the playhead: queued launches pick a new launch point.
    Seek,
}

/// Does nothing. Used as a placeholder when moving device slots in and out of boxes.
pub struct NullProcessor;

impl rlive_core::Processor for NullProcessor {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, _: &mut rlive_core::ProcessContext) {}
    fn set_param(&mut self, _: u32, _: f64) {}
}
