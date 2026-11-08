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
pub mod graph;
pub mod lane;
pub mod params;
pub mod rack;
pub mod shared;
pub mod track;

use clip::BlockTime;
pub use clip::{AudioClip, ClipData, MidiClip, Note};
pub use commands::{ClipCmd, GraphOp, GraphTxn, LaunchCmd, ParamCmd, RecordCmd, TransportCmd};
pub use graph::{
    AudioIo, BusIn, ChainAddr, ChainPath, ChainStep, RenderPlan, SidechainIn, Step, Tap, TrackRef,
    MASTER_STEP, MAX_RACK_DEPTH,
};
pub use lane::{ArrangementLane, LaneClip};
pub use params::{
    mix_cell, BusLayout, CellIx, CellSpec, CellTarget, ControlHooks, ControlStep, ParamBus,
    TrackRoutes, CONTROL_STEP,
};
pub use rack::{ChainNode, RackKind, RackNode, Zones, MAX_CHAINS};
use rlive_core::{
    time::ticks_per_sample, DeviceId, MidiMessage, Quantize, TimeSignature, TrackId, TransportInfo,
};
use shared::EngineShared;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;
use track::BlockCtx;
pub use track::{DeviceBody, DeviceSlot, MasterNode, MixSettings, TrackNode};

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

/// Messages from the UI to the audio thread.
pub enum EngineCommand {
    Transport(TransportCmd),
    Launch(LaunchCmd),
    Clip(ClipCmd),
    Param(ParamCmd),
    Record(RecordCmd),
    /// Structural edits, applied together between two blocks.
    Graph(Box<GraphTxn>),
}

impl EngineCommand {
    /// A transaction of one edit.
    pub fn graph(op: GraphOp) -> Self {
        Self::Graph(GraphTxn::new(vec![op], None))
    }
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
    /// A transaction that has been applied, holding only the plan it replaced.
    Spent(Box<GraphTxn>),
}

/// Notifications from the audio thread.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EngineEvent {
    /// A processor changed one of its own parameters (e.g. from a plugin editor).
    ParamChanged {
        target: TrackRef,
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

    /// Free everything the audio thread has released, and say how many objects that was. A spent
    /// transaction is only the wrapper its edits came in, so it doesn't count. Call regularly from
    /// the UI thread.
    pub fn collect_garbage(&mut self) -> usize {
        let mut n = 0;
        while let Ok(g) = self.garbage.pop() {
            n += usize::from(!matches!(g, Garbage::Spent(_)));
            drop(g);
        }
        n
    }

    pub fn poll_event(&mut self) -> Option<EngineEvent> {
        self.events.pop().ok()
    }
}

/// The plans the engine can run.
#[derive(Default)]
struct Plans {
    /// The plan the UI compiled, when it has sent one that still applies.
    compiled: Option<RenderPlan>,
    /// Today's topology over the current tracks, for when it hasn't.
    flat: RenderPlan,
}

impl Plans {
    fn active(&self) -> &RenderPlan {
        self.compiled.as_ref().unwrap_or(&self.flat)
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
    plans: Plans,

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
        plans: Plans {
            compiled: None,
            flat: {
                let mut plan = RenderPlan::with_capacity(MAX_TRACKS);
                plan.make_flat(0);
                plan
            },
        },
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

    /// Whether the engine is running a plan the UI compiled, rather than its own fallback.
    pub fn has_compiled_plan(&self) -> bool {
        self.plans.compiled.is_some()
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

    fn bus_mut(&mut self, target: TrackRef) -> Option<&mut ParamBus> {
        match target {
            TrackRef::Master => Some(&mut self.master.bus),
            TrackRef::Track(id) => self.track_mut(id).map(|t| &mut *t.bus),
        }
    }

    /// The devices of a chain, however deep. Walks the path, so it is for structural edits only.
    fn chain_mut(&mut self, target: ChainAddr) -> Option<&mut Vec<Box<DeviceSlot>>> {
        let mut devices = match target.owner {
            TrackRef::Master => &mut self.master.devices,
            TrackRef::Track(id) => &mut self.track_mut(id)?.devices,
        };
        for step in target.path.steps() {
            let rack = devices
                .iter_mut()
                .find(|d| d.id == step.rack)?
                .as_rack_mut()?;
            let chain = rack.chains.iter_mut().find(|c| c.id == step.chain)?;
            devices = &mut chain.devices;
        }
        Some(devices)
    }

    /// The rack `device` in the chain at `target`.
    fn rack_mut(&mut self, target: ChainAddr, device: DeviceId) -> Option<&mut RackNode> {
        self.chain_mut(target)?
            .iter_mut()
            .find(|d| d.id == device)?
            .as_rack_mut()
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

    /// Render as many frames as the shortest output has. Handles any buffer size by splitting
    /// into `max_block` chunks, and at the loop end. Inputs are accepted but nothing reads them
    /// yet.
    pub fn process(&mut self, io: AudioIo<'_>) {
        let started = std::time::Instant::now();
        let total = io.outputs.iter().map(|o| o.len()).min().unwrap_or(0);
        let mut done = 0;
        while done < total {
            self.drain_input();
            let tps = self.ticks_per_sample();
            let mut n = (total - done).min(self.max_block);
            let loop_end = self.prepare_loop(n, tps);
            if let Some(end) = loop_end {
                n = self.frames_until(end, tps).max(1);
            }
            self.process_block(io.outputs, done, n, tps, loop_end);
            done += n;
        }
        if total > 0 {
            let budget = total as f64 / self.sample_rate;
            let load = (started.elapsed().as_secs_f64() / budget) as f32;
            let prev = self.shared.cpu_load.load();
            self.shared.cpu_load.store(prev + (load - prev) * 0.1);
        }
    }

    /// [`process`](Self::process) with stereo output and no inputs.
    pub fn process_stereo(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.process(AudioIo {
            inputs: &[],
            outputs: &mut [left, right],
        });
    }

    fn process_block(
        &mut self,
        outputs: &mut [&mut [f32]],
        start: usize,
        frames: usize,
        tps: f64,
        loop_end: Option<f64>,
    ) {
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
        self.master.begin_block();

        let ctx = BlockCtx {
            transport: &transport,
            ticks_per_sample: tps,
            frames,
        };
        // Taken out for the block so the plan can be read while everything else changes. Holds
        // no allocation either way.
        let plans = std::mem::take(&mut self.plans);
        let plan = plans.active();
        for (level, steps) in plan.levels.iter().enumerate() {
            let steps = &plan.steps[steps.start as usize..steps.end as usize];
            for step in steps {
                self.gather_inputs(step, time, &transport);
            }
            self.run_control_steps(steps, level == 0, &ctx);
            self.finish_steps(steps, time, &ctx);
        }
        self.plans = plans;

        for (channel, out) in outputs.iter_mut().enumerate() {
            let out = &mut out[start..start + frames];
            match channel {
                0 => out.copy_from_slice(&self.master.left[..frames]),
                1 => out.copy_from_slice(&self.master.right[..frames]),
                _ => out.fill(0.0),
            }
        }

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

    /// Sum what a step takes as input into its track, before its devices run. Only the Master
    /// has inputs for now, and the metronome joins it there.
    fn gather_inputs(&mut self, step: &Step, time: BlockTime, transport: &TransportInfo) {
        if step.track != MASTER_STEP {
            return;
        }
        let frames = time.frames;
        let ml = &mut self.master.left[..frames];
        let mr = &mut self.master.right[..frames];
        ml.fill(0.0);
        mr.fill(0.0);
        for input in &step.inputs {
            let Some(t) = self.tracks.get(input.from as usize) else {
                continue;
            };
            let (l, r) = t.output(frames);
            for i in 0..frames {
                ml[i] += l[i];
                mr[i] += r[i];
            }
        }
        if transport.playing {
            self.metronome.render(time, self.signature, ml, mr);
        }
    }

    /// Resolve the parameters of some steps at each control step of the block.
    fn run_control_steps(&mut self, steps: &[Step], take_commands: bool, ctx: &BlockCtx) {
        let has_tracks = steps.iter().any(|s| s.track != MASTER_STEP);
        let mut index = 0;
        let mut offset = 0;
        while offset < ctx.frames {
            if take_commands && offset > 0 {
                self.drain_param_commands();
            }
            let events = &mut self.events;
            for step in steps {
                if step.track == MASTER_STEP {
                    self.master
                        .control_step(index, offset, ctx, &mut |d, p, v| {
                            push_param_changed(events, TrackRef::Master, d, p, v)
                        });
                } else if let Some(t) = self.tracks.get_mut(step.track as usize) {
                    t.control_step(index, offset, ctx, &mut |t, d, p, v| {
                        push_param_changed(events, TrackRef::Track(t), d, p, v)
                    });
                }
            }
            if has_tracks {
                let any_solo = self.tracks.iter().any(|t| t.mix.solo);
                let solo_changed = any_solo != self.any_solo;
                self.any_solo = any_solo;
                for step in steps {
                    if let Some(t) = self.tracks.get_mut(step.track as usize) {
                        t.update_mix(offset, any_solo, solo_changed);
                    }
                }
            }
            index += 1;
            offset += CONTROL_STEP;
        }
    }

    /// Run what is left of some steps' chains and take them through the mixer stage.
    fn finish_steps(&mut self, steps: &[Step], time: BlockTime, ctx: &BlockCtx) {
        let events = &mut self.events;
        for step in steps {
            if step.track == MASTER_STEP {
                self.master.finish(
                    ctx,
                    &self.shared.master_meter,
                    self.smooth_coeff,
                    &mut |d, p, v| push_param_changed(events, TrackRef::Master, d, p, v),
                );
            } else if let Some(t) = self.tracks.get_mut(step.track as usize) {
                t.finish_block(time, ctx, &mut |t, d, p, v| {
                    push_param_changed(events, TrackRef::Track(t), d, p, v)
                });
                t.apply_mix(time.frames, self.smooth_coeff);
            }
        }
    }
}

fn push_param_changed(
    events: &mut rtrb::Producer<EngineEvent>,
    target: TrackRef,
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
