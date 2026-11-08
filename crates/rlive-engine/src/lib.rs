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

pub mod automation;
pub mod clip;
mod commands;
pub mod delay;
pub mod graph;
pub mod lane;
pub mod params;
pub mod rack;
pub mod shared;
pub mod track;

pub use automation::AutomationLane;
pub use automation::MacroRoute;
use clip::BlockTime;
pub use clip::{AudioClip, ClipData, ExprPoint, MidiClip, Note, NoteExpression, WarpedClip};
pub use commands::{ClipCmd, GraphOp, GraphTxn, LaunchCmd, ParamCmd, RecordCmd, TransportCmd};
use delay::FadingDelay;
pub use delay::{StereoLine, TapLines, FADE_FRAMES, MAX_COMPENSATION_FRAMES};
pub use graph::{
    AudioIo, BusIn, ChainAddr, ChainPath, ChainStep, RenderPlan, SidechainIn, Step, Tap, TrackRef,
    MAX_RACK_DEPTH,
};
pub use lane::{ArrangementLane, LaneClip};
pub use params::{
    mix_cell, BusLayout, CellIx, CellSpec, CellState, CellTarget, ControlHooks, ControlStep,
    ParamBus, TrackRoutes, CONTROL_STEP, MAX_SENDS, SEND_MAX_DB, SEND_MIN_DB,
};
pub use rack::{ChainMix, ChainNode, DrumPad, RackKind, RackNode, Zone, Zones, MAX_CHAINS};
use rlive_core::{
    time::{BarMap, TempoRun, TimeMaps},
    DeviceId, MidiMessage, Quantize, TrackId, TransportInfo,
};
use shared::EngineShared;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;
use track::BlockCtx;
pub use track::{DeviceBody, DeviceSlot, MixSettings, RackOuts, TrackNode};

pub const MAX_TRACKS: usize = 256;
/// How deep a group can be nested for solo purposes, in case a plan links groups in a loop.
const MAX_GROUP_DEPTH: usize = 64;
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
    /// The user moved a parameter while automation records. `before` is what it was sounding at.
    AutomationMoved {
        target: TrackRef,
        cell: CellIx,
        value: f64,
        before: f64,
        tick: f64,
    },
    /// The user let go of a parameter whose automation is being recorded.
    AutomationReleased {
        target: TrackRef,
        cell: CellIx,
        tick: f64,
    },
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
    fn render(&mut self, time: BlockTime, bars: &BarMap, left: &mut [f32], right: &mut [f32]) {
        if !self.enabled {
            return;
        }
        let click_len = (time.sample_rate * 0.03) as usize;
        let mut beat = bars.first_beat(time.t0);
        let t1 = time.t1();
        let mut start = 0;
        loop {
            let click_at = if beat.tick < t1 {
                Some(time.offset(beat.tick))
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
                    self.freq = if beat.downbeat { 1760.0 } else { 880.0 };
                    self.amp = if beat.downbeat { 0.5 } else { 0.3 };
                    self.phase = 0.0;
                    self.remaining = click_len;
                    start = off;
                    beat = bars.next_beat(beat);
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
    /// The Main output: a track like any other, rendered last.
    master: Box<TrackNode>,
    plans: Plans,

    playing: bool,
    /// The song's tempo and meter. Swapped only between blocks.
    maps: Arc<TimeMaps>,
    quantize: Quantize,
    tick: f64,
    sample_pos: u64,
    insert_marker: f64,
    loop_range: Option<(f64, f64)>,
    /// The last block played up to `loop_end`. Tells that apart from being sought onto it,
    /// since only the former wraps.
    reached_loop_end: bool,
    recording: bool,
    /// Parameter gestures are recorded as automation while recording.
    automation_arm: bool,
    /// Where on the timeline commands being handled now take effect.
    command_tick: f64,
    /// Backdated recorded notes never land before this tick.
    record_floor: f64,
    metronome: Metronome,
    /// The click rendered on its own, when it is held back to stay with the tracks.
    click: ClickDelay,
    smooth_coeff: f32,
    /// Solo has to be worked out again, as the plan or a Solo switch changed.
    solo_dirty: bool,

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
        master: Box::new(TrackNode::master(max_block)),
        plans: Plans {
            compiled: None,
            flat: {
                let mut plan = RenderPlan::with_capacity(MAX_TRACKS);
                plan.make_flat(0);
                plan
            },
        },
        playing: false,
        maps: Arc::new(TimeMaps::default()),
        quantize: Quantize::Bar,
        tick: 0.0,
        sample_pos: 0,
        insert_marker: 0.0,
        loop_range: None,
        reached_loop_end: false,
        recording: false,
        automation_arm: false,
        command_tick: 0.0,
        record_floor: 0.0,
        metronome: Metronome {
            enabled: false,
            phase: 0.0,
            remaining: 0,
            freq: 880.0,
            amp: 0.3,
        },
        click: ClickDelay::new(max_block),
        // ~5 ms one-pole smoothing.
        smooth_coeff: 1.0 - (-1.0 / (0.005 * sample_rate as f32)).exp(),
        solo_dirty: true,
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

    /// The plan the engine is running.
    pub fn active_plan(&self) -> &RenderPlan {
        self.plans.active()
    }

    fn transport(&self) -> TransportInfo {
        let bar = self.maps.bars.bar_at(self.tick);
        TransportInfo {
            playing: self.playing,
            tempo: self.maps.tempo.bpm_at(self.tick),
            time_signature: bar.signature,
            // Only ever negative by less than a frame, after a loop from the very start.
            tick: self.tick.max(0.0),
            sample_pos: self.sample_pos,
            sample_rate: self.sample_rate,
            bar_start: bar.start,
            loop_range: self.loop_range,
            scale: None,
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
            let chain = rack
                .chains
                .iter_mut()
                .chain(rack.returns.iter_mut())
                .find(|c| c.id == step.chain)?;
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
        self.maps.bars.next_boundary(self.tick, self.quantize)
    }

    fn tempo_run(&self) -> TempoRun {
        self.maps.tempo.run_at(self.tick, self.sample_rate)
    }

    fn sync_sample_pos(&mut self) {
        let samples = if self.maps.tempo.is_constant() {
            self.tick / self.tempo_run().ticks_per_sample
        } else {
            self.maps.tempo.tick_to_seconds(self.tick) * self.sample_rate
        };
        self.sample_pos = samples.round() as u64;
    }

    /// The song position `seconds` before the playhead.
    pub(crate) fn tick_seconds_ago(&self, seconds: f64) -> f64 {
        let tempo = &self.maps.tempo;
        if tempo.is_constant() {
            self.tick - seconds * (self.tempo_run().ticks_per_sample * self.sample_rate)
        } else {
            tempo.seconds_to_tick(tempo.tick_to_seconds(self.tick) - seconds)
        }
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
        self.command_tick = self.tick;
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
    fn frames_until(&self, end: f64, run: &TempoRun) -> usize {
        (run.frames_to(end - self.tick) + 1e-6).floor().max(0.0) as usize
    }

    /// The loop, while the playhead is going around it.
    fn active_loop(&self, run: &TempoRun) -> Option<(f64, f64)> {
        if !self.playing {
            return None;
        }
        let (start, end) = self.loop_range?;
        // A tick already past the end (sought there) plays on; a loop shorter than one frame
        // could never advance.
        if end - start < run.ticks_per_sample || !(self.tick < end || self.reached_loop_end) {
            return None;
        }
        Some((start, end))
    }

    /// Where a wrap from `(start, end)` lands. Keeping the sub-sample remainder stops loop passes
    /// drifting against the sample grid. With one tempo that is the exact loop length in ticks;
    /// across tempo changes it has to be kept in seconds, since it covers a different number of
    /// ticks at each end of the loop. The playhead can land before tick 0, by less than a frame,
    /// so a loop from the very start doesn't lose it either.
    fn loop_wrap_target(&self, start: f64, end: f64) -> f64 {
        let tempo = &self.maps.tempo;
        if tempo.is_constant() {
            (self.tick - (end - start)).max(0.0)
        } else {
            let remainder = tempo.tick_to_seconds(end) - tempo.tick_to_seconds(self.tick);
            tempo.seconds_to_tick(tempo.tick_to_seconds(start) - remainder)
        }
    }

    /// Wrap at the loop end if we are there, re-reading the tempo at the new position.
    fn wrap_at_loop_end(&mut self, run: &mut TempoRun) {
        let Some((start, end)) = self.active_loop(run) else {
            return;
        };
        if self.frames_until(end, run) == 0 {
            self.jump(self.loop_wrap_target(start, end), JumpKind::Wrap);
            *run = self.tempo_run();
        }
    }

    /// Cut the next block short at the next tempo point, so every block runs at one tempo and
    /// events can be placed exactly in it. When the point is inside the very next frame that
    /// frame straddles it: one frame long, at its true, blended rate. Taken from the map, so
    /// time is never lost at a tempo change. A constant tempo has no points to reach.
    fn limit_to_tempo_point(&self, run: &mut TempoRun, n: usize) -> usize {
        if !self.playing || !run.until.is_finite() {
            return n;
        }
        let frames = self.frames_until(run.until, run);
        if frames > 0 {
            return n.min(frames);
        }
        let tempo = &self.maps.tempo;
        let t1 = tempo.seconds_to_tick(tempo.tick_to_seconds(self.tick) + 1.0 / self.sample_rate);
        run.ticks_per_sample = t1 - self.tick;
        run.ramp = 0.0;
        1
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
            let mut run = self.tempo_run();
            self.wrap_at_loop_end(&mut run);
            let mut n = self.limit_to_tempo_point(&mut run, (total - done).min(self.max_block));
            let loop_end = self
                .active_loop(&run)
                .and_then(|(_, end)| (self.frames_until(end, &run) <= n).then_some(end));
            if let Some(end) = loop_end {
                n = self.frames_until(end, &run).max(1);
            }
            self.process_block(io.outputs, done, n, run, loop_end);
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
        run: TempoRun,
        loop_end: Option<f64>,
    ) {
        let transport = self.transport();
        let t1 = self.tick + run.advance(frames as f64);
        let time = BlockTime {
            t0: self.tick,
            ticks_per_sample: run.ticks_per_sample,
            ramp: run.ramp,
            end: loop_end.map_or(t1, |e| t1.min(e)),
            frames,
            sample_rate: self.sample_rate,
            tempo: transport.tempo,
        };
        // Held for the block so the maps can be read while everything else changes. The engine
        // keeps its own reference, so this never frees them.
        let maps = Arc::clone(&self.maps);

        let garbage = &mut self.garbage;
        let mut retire_clip = |c: Arc<ClipData>| retire(garbage, Garbage::Shared(c));
        let smooth = self.smooth_coeff;
        for t in self.tracks.iter_mut() {
            t.smooth = smooth;
            t.begin_block(time, &transport, &mut retire_clip);
        }
        self.master.smooth = smooth;
        self.master.begin_block(time, &transport, &mut retire_clip);

        let ctx = BlockCtx {
            transport: &transport,
            time,
            bars: &maps.bars,
            frames,
        };
        // Taken out for the block so the plan can be read while everything else changes. Holds
        // no allocation either way.
        let plans = std::mem::take(&mut self.plans);
        let plan = plans.active();
        for (level, steps) in plan.levels.iter().enumerate() {
            let steps = &plan.steps[steps.start as usize..steps.end as usize];
            for step in steps {
                self.gather_inputs(step, time, &transport, &maps.bars);
            }
            self.run_control_steps(plan, steps, level == 0, &ctx);
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
        self.shared.tick.store(self.tick.max(0.0));
        self.shared.insert_marker.store(self.insert_marker);
        self.shared.tempo.store(self.maps.tempo.bpm_at(self.tick));
    }

    /// Sum what a step takes as input into its track, before its devices run. Track `tracks.len()`
    /// is the Master, and the metronome joins it there. A group or return track's buffer holds
    /// nothing else; a clip track's holds its clips, and takes no inputs yet.
    fn gather_inputs(
        &mut self,
        step: &Step,
        time: BlockTime,
        transport: &TransportInfo,
        bars: &BarMap,
    ) {
        let frames = time.frames;
        let coeff = self.smooth_coeff;
        let to = step.track as usize;
        if to == self.tracks.len() {
            let t = &mut *self.master;
            let (l, r) = (&mut t.left[..frames], &mut t.right[..frames]);
            for input in &step.inputs {
                if let Some(from) = self.tracks.get_mut(input.from as usize) {
                    add_input(from, input, l, r, frames, coeff);
                }
            }
            self.click
                .render(&mut self.metronome, time, bars, l, r, transport.playing);
            return;
        }
        if to > self.tracks.len() {
            return;
        }
        let (before, rest) = self.tracks.split_at_mut(to);
        let Some((dest, after)) = rest.split_first_mut() else {
            return;
        };
        for input in &step.inputs {
            let from = input.from as usize;
            if from == to {
                dest.feedback_into_self(input, frames, coeff);
                continue;
            }
            let source = if from < to {
                before.get_mut(from)
            } else {
                after.get_mut(from - to - 1)
            };
            if let Some(source) = source {
                add_input(
                    source,
                    input,
                    &mut dest.left,
                    &mut dest.right,
                    frames,
                    coeff,
                );
            }
        }
    }

    /// Resolve the parameters of some steps at each control step of the block.
    fn run_control_steps(
        &mut self,
        plan: &RenderPlan,
        steps: &[Step],
        take_commands: bool,
        ctx: &BlockCtx,
    ) {
        let master = self.tracks.len() as u32;
        let mut index = 0;
        let mut offset = 0;
        while offset < ctx.frames {
            if take_commands && offset > 0 {
                self.command_tick = ctx.tick_at(offset);
                self.drain_param_commands();
            }
            let events = &mut self.events;
            for step in steps {
                let is_master = step.track == master;
                let t = if is_master {
                    Some(&mut *self.master)
                } else {
                    self.tracks.get_mut(step.track as usize).map(|t| &mut **t)
                };
                if let Some(t) = t {
                    t.control_step(index, offset, ctx, &mut |t, d, p, v| {
                        let owner = if is_master {
                            TrackRef::Master
                        } else {
                            TrackRef::Track(t)
                        };
                        push_param_changed(events, owner, d, p, v)
                    });
                }
            }
            if take_commands {
                self.resolve_solo(plan);
            }
            for step in steps {
                let t = if step.track == master {
                    Some(&mut *self.master)
                } else {
                    self.tracks.get_mut(step.track as usize).map(|t| &mut **t)
                };
                if let Some(t) = t {
                    t.update_mix(offset);
                }
            }
            index += 1;
            offset += CONTROL_STEP;
        }
    }

    /// Work out what solo leaves audible across every track, when a Solo switch changed. Reads
    /// each switch's cell as well as its resolved value, so a group that renders later is seen
    /// at this same step, and a Macro on Solo still counts.
    fn resolve_solo(&mut self, plan: &RenderPlan) {
        let (mut any_track, mut any_return) = (false, false);
        let mut changed = std::mem::take(&mut self.solo_dirty);
        for (i, t) in self.tracks.iter_mut().enumerate() {
            let solo = t.mix.solo || t.bus.base(mix_cell::SOLO).is_some_and(|v| v >= 0.5);
            changed |= solo != t.soloed;
            t.soloed = solo;
            if solo {
                if plan.returns.contains(&(i as u32)) {
                    any_return = true;
                } else {
                    any_track = true;
                }
            }
        }
        if !changed {
            return;
        }
        for t in self.tracks.iter_mut() {
            t.holds_solo = false;
        }
        let parent = |at: usize| plan.groups.get(at).copied().flatten().map(|g| g as usize);
        for i in 0..self.tracks.len() {
            if !self.tracks[i].soloed {
                continue;
            }
            let (mut at, mut depth) = (parent(i), 0);
            while let Some(g) = at.filter(|_| depth < MAX_GROUP_DEPTH) {
                if let Some(t) = self.tracks.get_mut(g) {
                    t.holds_solo = true;
                }
                at = parent(g);
                depth += 1;
            }
        }
        let any_solo = any_track || any_return;
        for i in 0..self.tracks.len() {
            let t = &self.tracks[i];
            let is_return = plan.returns.contains(&(i as u32));
            let open = if is_return {
                t.soloed || (!any_return && (!any_track || plan.solo_in_place))
            } else if !any_solo || t.soloed || t.holds_solo {
                true
            } else {
                let (mut at, mut depth, mut soloed) = (parent(i), 0, false);
                while let Some(g) = at.filter(|_| depth < MAX_GROUP_DEPTH && !soloed) {
                    soloed = self.tracks.get(g).is_some_and(|t| t.soloed);
                    at = parent(g);
                    depth += 1;
                }
                soloed
            };
            self.tracks[i].set_solo_gates(open, open || any_return);
        }
    }

    /// Run what is left of some steps' chains and take them through the mixer stage.
    fn finish_steps(&mut self, steps: &[Step], time: BlockTime, ctx: &BlockCtx) {
        let events = &mut self.events;
        let master = self.tracks.len() as u32;
        let coeff = self.smooth_coeff;
        for step in steps {
            let is_master = step.track == master;
            let t = if is_master {
                Some(&mut *self.master)
            } else {
                self.tracks.get_mut(step.track as usize).map(|t| &mut **t)
            };
            let Some(t) = t else {
                continue;
            };
            t.finish_block(time, ctx, &mut |t, d, p, v| {
                let owner = if is_master {
                    TrackRef::Master
                } else {
                    TrackRef::Track(t)
                };
                push_param_changed(events, owner, d, p, v)
            });
            t.prepare_sends(time.frames, coeff);
            t.apply_mix(time.frames, coeff);
            t.feed_lines(time.frames);
            if is_master {
                let (l, r) = t.output(time.frames);
                self.shared.master_meter.update(l, r);
            }
        }
    }
}

/// Add one input to a track's buffers: the source's main output, or its send.
fn add_input(
    from: &mut TrackNode,
    input: &BusIn,
    left: &mut [f32],
    right: &mut [f32],
    frames: usize,
    coeff: f32,
) {
    if let Tap::Aux(_, slot) = input.tap {
        from.rack_out_into(slot as usize, left, right, frames);
        return;
    }
    if input.gain.is_some() {
        from.send_into(
            input,
            (&mut left[..frames], &mut right[..frames]),
            frames,
            coeff,
        );
        return;
    }
    from.main_into(left, right, frames);
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

/// The metronome, held back as far as the tracks it plays with. Without a line it writes straight
/// into the Master, exactly as it always has.
struct ClickDelay {
    line: Option<Box<StereoLine>>,
    delay: FadingDelay,
    buf: [Vec<f32>; 2],
}

impl ClickDelay {
    fn new(max_block: usize) -> Self {
        Self {
            line: None,
            delay: FadingDelay::default(),
            buf: [vec![0.0; max_block], vec![0.0; max_block]],
        }
    }

    fn render(
        &mut self,
        metronome: &mut Metronome,
        time: BlockTime,
        bars: &BarMap,
        left: &mut [f32],
        right: &mut [f32],
        playing: bool,
    ) {
        let Some(line) = &mut self.line else {
            if playing {
                metronome.render(time, bars, left, right);
            }
            return;
        };
        let frames = left.len();
        let [bl, br] = &mut self.buf;
        let (bl, br) = (&mut bl[..frames], &mut br[..frames]);
        bl.fill(0.0);
        br.fill(0.0);
        if playing {
            metronome.render(time, bars, bl, br);
        }
        line.push(bl, br);
        self.delay.read(line, bl, br);
        for i in 0..frames {
            left[i] += bl[i];
            right[i] += br[i];
        }
    }
}

/// Does nothing. Used as a placeholder when moving device slots in and out of boxes.
pub struct NullProcessor;

impl rlive_core::Processor for NullProcessor {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, _: &mut rlive_core::ProcessContext) {}
    fn set_param(&mut self, _: u32, _: f64) {}
}
