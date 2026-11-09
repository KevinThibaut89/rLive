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
pub mod capture;
pub mod clip;
mod commands;
pub mod delay;
pub mod graph;
pub mod lane;
pub mod params;
pub mod rack;
pub mod shared;
pub mod taps;
pub mod track;

pub use automation::AutomationLane;
pub use automation::MacroRoute;
pub use automation::{ClipEnvAction, ClipLane, ClipRoutes};
pub use capture::{capture_pair, CaptureSink, CaptureTail, Chunk as CaptureChunk};
use capture::{CaptureBlock, CaptureMode, InputSource};
use clip::BlockTime;
pub use clip::{
    velocity_gain, AudioClip, ClipCurve, ClipData, ClipEnvelopes, EnvCurve, EnvTiming,
    EnvelopedClip, ExprPoint, FrozenClip, MidiClip, Note, NoteExpression, PlayGroup, ProgramChange,
    SlotLaunch, WarpedClip,
};
pub use commands::{
    ClipCmd, GraphOp, GraphTxn, LaunchCmd, ParamCmd, PreviewCmd, RecordCmd, SceneInfo,
    TransportCmd, NO_OLD_SLOT,
};
use delay::FadingDelay;
pub use delay::{StereoLine, TapLines, FADE_FRAMES, MAX_COMPENSATION_FRAMES};
pub use graph::{
    AudioIo, BusIn, BusRead, ChainAddr, ChainPath, ChainStep, HardwareOut, InputGate, MidiRoute,
    OutputPair, RenderPlan, SidechainIn, Step, Tap, TapOut, TrackIo, TrackRef, MAX_RACK_DEPTH,
};
pub use lane::{ArrangementLane, LaneClip, LaneFades};
pub use params::{
    mix_cell, BusLayout, CellIx, CellSpec, CellState, CellTarget, ControlHooks, ControlStep,
    ParamBus, TrackRoutes, CONTROL_STEP, MAX_SENDS, SEND_MAX_DB, SEND_MIN_DB,
};
pub use rack::{
    ChainMix, ChainNode, DrumPad, PreviewNote, RackKind, RackNode, Zone, Zones, MAX_CHAINS,
};
use rlive_core::{
    db_to_gain,
    time::{BarMap, TempoRun, TimeMaps},
    DeviceId, MidiMessage, Quantize, TimeSignature, TrackId, TransportInfo,
};
use shared::EngineShared;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;
use track::BlockCtx;
pub use track::{DeviceBody, DeviceSlot, MixSettings, RackOuts, TrackNode, MAX_SCENES};

pub const MAX_TRACKS: usize = 256;
/// How deep a group can be nested for solo purposes, in case a plan links groups in a loop.
const MAX_GROUP_DEPTH: usize = 64;
const COMMAND_CAPACITY: usize = 4096;
const GARBAGE_CAPACITY: usize = 4096;
const EVENT_CAPACITY: usize = 8192;
const MIDI_CAPACITY: usize = 2048;
const PLAYED_CAPACITY: usize = 2048;

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
    /// A file heard from the browser.
    Preview(PreviewCmd),
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
    /// A take's file position `frame` is at timeline `tick`: where it started, re-entered its
    /// window, jumped, or the tempo changed.
    CaptureSegment {
        track: TrackId,
        frame: u64,
        tick: f64,
        /// The take did not carry on from where it was: a jump or a return to its window.
        jump: bool,
        ticks_per_frame: f64,
    },
    /// A take ended, having taken `frames` frames.
    CaptureClosed { track: TrackId, frames: u64 },
    /// A note landed in the clip a track is overdubbing, at `position` in it.
    OverdubMidi {
        track: TrackId,
        slot: usize,
        position: f64,
        tick: f64,
        message: MidiMessage,
    },
    /// Recording into a slot has reached its launch point.
    SlotRecordStarted {
        track: TrackId,
        slot: usize,
        tick: f64,
    },
    /// A slot's recording will end at `at`, which by then should have taken `frames` frames.
    SlotClosing {
        track: TrackId,
        slot: usize,
        at: f64,
        launch: bool,
        frames: Option<u64>,
    },
    /// A slot's recording is over.
    SlotRecordEnded {
        track: TrackId,
        slot: usize,
        tick: f64,
    },
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
    /// The user moved a parameter that's being recorded into the Session clip playing on `track`.
    /// `position` is where in the clip it moved, in ticks.
    ClipAutomationMoved {
        track: TrackId,
        slot: usize,
        cell: CellIx,
        value: f64,
        before: f64,
        position: f64,
    },
    /// The user let go of a parameter being recorded into a Session clip.
    ClipAutomationReleased {
        track: TrackId,
        slot: usize,
        cell: CellIx,
        position: f64,
    },
    /// A launched scene set the tempo or meter at `tick`.
    SceneTiming {
        scene: usize,
        tick: f64,
        tempo: Option<f64>,
        signature: Option<TimeSignature>,
    },
    /// While recording: `track` now plays `slot` (or nothing) from `at`. `start_tick` is the song
    /// tick of the clip's position 0.
    SessionPlayback {
        track: TrackId,
        slot: Option<usize>,
        at: f64,
        start_tick: f64,
    },
    /// The clip whose automation was moving this cell stopped counting. The cell stays where it
    /// was left, at `value`, and is its knob's again.
    ClipAutomationEnded {
        target: TrackRef,
        cell: CellIx,
        value: f64,
    },
}

/// A note played on a track, armed or not, for the buffer Capture MIDI reads. `seconds` is on the
/// engine's own clock, so it means something with the transport stopped; `tick` is set while it
/// runs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayedNote {
    pub track: TrackId,
    pub seconds: f64,
    pub tick: Option<f64>,
    pub message: MidiMessage,
}

/// UI-side handle to a running engine.
pub struct EngineHandle {
    commands: rtrb::Producer<EngineCommand>,
    garbage: rtrb::Consumer<Garbage>,
    events: rtrb::Consumer<EngineEvent>,
    played: rtrb::Consumer<PlayedNote>,
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

    /// The next note played since the last call.
    pub fn poll_played(&mut self) -> Option<PlayedNote> {
        self.played.pop().ok()
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

/// The metronome's options that shape what it plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MetronomeConfig {
    /// Which click sound, an index into its table.
    pub sound: u8,
    /// Ticks between clicks; `None` follows the time signature's denominator.
    pub division: Option<u32>,
    /// Audible only while recording.
    pub only_while_recording: bool,
}

/// A click sound: down and up frequencies, and how long it rings.
#[derive(Clone, Copy)]
struct Click {
    down: f32,
    up: f32,
    seconds: f64,
}

const CLICKS: [Click; 4] = [
    Click {
        down: 1760.0,
        up: 880.0,
        seconds: 0.03,
    },
    Click {
        down: 2500.0,
        up: 1800.0,
        seconds: 0.012,
    },
    Click {
        down: 1100.0,
        up: 780.0,
        seconds: 0.02,
    },
    Click {
        down: 1046.5,
        up: 784.0,
        seconds: 0.09,
    },
];

/// The beat the count-in is on, and how far through it.
pub(crate) struct CountIn {
    pub(crate) frames_left: u64,
    pub(crate) frames_total: u64,
    /// How long the whole count-in is, in ticks.
    pub(crate) ticks: f64,
    pub(crate) sig: TimeSignature,
}

struct Metronome {
    enabled: bool,
    config: MetronomeConfig,
    phase: f32,
    remaining: usize,
    freq: f32,
    amp: f32,
}

impl Metronome {
    /// The next click at or after `tick`, at the chosen division if it fits in the bar. A
    /// division that doesn't fit falls back to the denominator until one that does comes along.
    fn click_after(&self, bars: &BarMap, tick: f64) -> rlive_core::time::Beat {
        let Some(step) = self.config.division.map(|s| s as f64) else {
            return bars.first_beat(tick);
        };
        let bar = bars.bar_at(tick);
        let bar_len = bar.signature.ticks_per_bar() as f64;
        if step <= 0.0 || (bar_len / step).fract() != 0.0 {
            return bars.first_beat(tick);
        }
        let n = ((tick.max(bar.start) - bar.start) / step - 1e-6)
            .ceil()
            .max(0.0);
        let at = bar.start + n * step;
        if at >= bar.end {
            let next = bars.bar_at(bar.end + 1e-6);
            return rlive_core::time::Beat::at(next.start, true);
        }
        rlive_core::time::Beat::at(at, n == 0.0)
    }

    /// Clicks in this block, from song position `from` on (Punch-In holds an
    /// Enable-Only-While-Recording metronome back).
    fn render(
        &mut self,
        time: BlockTime,
        bars: &BarMap,
        left: &mut [f32],
        right: &mut [f32],
        from: f64,
    ) {
        if !self.enabled {
            return;
        }
        let sound = CLICKS[self.config.sound as usize % CLICKS.len()];
        let click_len = (time.sample_rate * sound.seconds) as usize;
        let mut beat = self.click_after(bars, time.t0);
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
                    if beat.tick >= from {
                        self.freq = if beat.downbeat { sound.down } else { sound.up };
                        self.amp = if beat.downbeat { 0.5 } else { 0.3 };
                        self.phase = 0.0;
                        self.remaining = click_len;
                    }
                    start = off;
                    beat = if self.config.division.is_some() {
                        self.click_after(bars, beat.tick + 1e-3)
                    } else {
                        bars.next_beat(beat)
                    };
                }
                None => break,
            }
        }
    }

    /// The count-in's clicks in this block, which sound whether or not the metronome is on.
    fn render_count_in(
        &mut self,
        count: &CountIn,
        frames: usize,
        sample_rate: f64,
        left: &mut [f32],
        right: &mut [f32],
    ) {
        let sound = CLICKS[self.config.sound as usize % CLICKS.len()];
        let click_len = (sample_rate * sound.seconds) as usize;
        let beats = count.sig.numerator.max(1) as u64;
        let frames_per_beat = count.frames_total as f64
            / ((count.ticks / count.sig.ticks_per_beat() as f64)
                .round()
                .max(1.0));
        let elapsed = (count.frames_total - count.frames_left) as f64;
        let mut n = (elapsed / frames_per_beat - 1e-6).ceil().max(0.0) as u64;
        let mut start = 0;
        loop {
            let off = n as f64 * frames_per_beat - elapsed;
            let click_at = (off < frames as f64).then_some(off.max(0.0) as usize);
            self.write(
                left,
                right,
                start,
                click_at.unwrap_or(frames),
                sample_rate as f32,
                click_len,
            );
            match click_at {
                Some(off) => {
                    let downbeat = n.is_multiple_of(beats);
                    self.freq = if downbeat { sound.down } else { sound.up };
                    self.amp = if downbeat { 0.5 } else { 0.3 };
                    self.phase = 0.0;
                    self.remaining = click_len;
                    start = off;
                    n += 1;
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
    /// Where the transport last stopped, for Continue.
    stop_point: f64,
    /// A jump waiting for the next launch point.
    pending_jump: Option<PendingJump>,
    /// Playback is held looping over this range, while the scrub area is held down.
    hold_loop: Option<(f64, f64)>,
    /// The transport stops when it reaches this tick.
    stop_at: Option<f64>,
    /// Whether clips entered in the middle of a note play that note.
    chase: bool,
    /// What each scene sets when launched. Always `MAX_SCENES` long.
    scenes: Vec<SceneInfo>,
    scene_count: usize,
    /// A launched scene waiting for its launch point.
    pending_scene: Option<commands::follow::PendingScene>,
    /// The launched scene's Follow Action, waiting for its time.
    scene_follow: Option<clip::ArmedFollow>,
    /// The Enable Follow Actions Globally button.
    follow_enabled: bool,
    follow_random: commands::follow::Random,
    /// Room for rearranging slots.
    slot_scratch: Vec<Option<Arc<ClipData>>>,
    launch_scratch: Vec<SlotLaunch>,
    /// The last block played up to `loop_end`. Tells that apart from being sought onto it,
    /// since only the former wraps.
    reached_loop_end: bool,
    recording: bool,
    /// Where the current recording started on the timeline.
    rec_from: f64,
    punch_in: Option<f64>,
    punch_out: Option<f64>,
    /// The count-in in progress: the transport starts when it ends.
    count_in: Option<CountIn>,
    /// Bars of count-in before recording.
    count_in_bars: u8,
    /// Frames rendered so far, transport or no transport.
    clock: u64,
    /// Read when a block must offer a sink nothing.
    silence: Vec<f32>,
    /// Parameter gestures are recorded as automation while recording.
    automation_arm: bool,
    /// MIDI Envelope Auto-Reset: a new clip sets some controllers back first.
    midi_auto_reset: bool,
    /// Clip Update Rate: the grid edits reach a running clip on, in ticks. 0 is at once.
    clip_update_rate: u64,
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
    /// Tapped signals, as the running plan numbers them.
    buses: Vec<taps::TapBus>,
    /// The crossfader's gain for each side at each control step of the block.
    crossfade_steps: Vec<(f32, f32)>,
    /// The Cue output, built up as tracks render.
    cue: [Vec<f32>; 2],
    /// Room to read a delayed tap or sum a hardware output into.
    scratch: [Vec<f32>; 2],
    preview: commands::Preview,

    commands: rtrb::Consumer<EngineCommand>,
    garbage: rtrb::Producer<Garbage>,
    events: rtrb::Producer<EngineEvent>,
    played: rtrb::Producer<PlayedNote>,
    midi: rtrb::Consumer<(TrackId, MidiMessage, Instant)>,
    shared: Arc<EngineShared>,
}

/// Create an engine and its UI handle.
pub fn create_engine(sample_rate: f64, max_block: usize) -> (Engine, EngineHandle) {
    let (cmd_tx, cmd_rx) = rtrb::RingBuffer::new(COMMAND_CAPACITY);
    let (gb_tx, gb_rx) = rtrb::RingBuffer::new(GARBAGE_CAPACITY);
    let (ev_tx, ev_rx) = rtrb::RingBuffer::new(EVENT_CAPACITY);
    let (played_tx, played_rx) = rtrb::RingBuffer::new(PLAYED_CAPACITY);
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
        stop_point: 0.0,
        pending_jump: None,
        hold_loop: None,
        stop_at: None,
        chase: true,
        scenes: vec![SceneInfo::default(); track::MAX_SCENES],
        scene_count: 0,
        pending_scene: None,
        scene_follow: None,
        follow_enabled: true,
        follow_random: commands::follow::Random::new(0),
        slot_scratch: Vec::with_capacity(track::MAX_SCENES),
        launch_scratch: Vec::with_capacity(track::MAX_SCENES),
        reached_loop_end: false,
        recording: false,
        rec_from: 0.0,
        punch_in: None,
        punch_out: None,
        count_in: None,
        count_in_bars: 0,
        clock: 0,
        silence: vec![0.0; max_block],
        automation_arm: false,
        midi_auto_reset: false,
        clip_update_rate: 0,
        command_tick: 0.0,
        record_floor: 0.0,
        metronome: Metronome {
            enabled: false,
            config: MetronomeConfig::default(),
            phase: 0.0,
            remaining: 0,
            freq: 880.0,
            amp: 0.3,
        },
        click: ClickDelay::new(max_block),
        // ~5 ms one-pole smoothing.
        smooth_coeff: 1.0 - (-1.0 / (0.005 * sample_rate as f32)).exp(),
        solo_dirty: true,
        buses: Vec::new(),
        crossfade_steps: vec![(1.0, 1.0); max_block / CONTROL_STEP + 2],
        cue: [vec![0.0; max_block], vec![0.0; max_block]],
        scratch: [vec![0.0; max_block], vec![0.0; max_block]],
        preview: Default::default(),
        commands: cmd_rx,
        garbage: gb_tx,
        events: ev_tx,
        played: played_tx,
        midi: midi_rx,
        shared: shared.clone(),
    };
    let handle = EngineHandle {
        commands: cmd_tx,
        garbage: gb_rx,
        events: ev_rx,
        played: played_rx,
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

    pub(crate) fn transport_rolling(&self) -> bool {
        self.playing
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
            self.rec_from = self.rec_from.min(to);
        }
        for t in self.tracks.iter_mut() {
            if let Some(sink) = t.capture.as_mut() {
                if sink.is_running() {
                    sink.jumped(self.clock, to);
                }
            }
        }
        let delta = to - self.tick;
        self.tick = to;
        self.record_floor = to;
        self.reached_loop_end = false;
        self.sync_sample_pos();
        let launch_at = self.launch_point();
        if let Some(s) = self.pending_scene.as_mut() {
            s.at = match kind {
                JumpKind::Wrap => s.at + delta,
                JumpKind::Seek => launch_at,
            };
        }
        if let Some(f) = self.scene_follow.as_mut() {
            f.at += delta;
        }
        for t in self.tracks.iter_mut() {
            if let Some(p) = t.player.playing.as_mut() {
                p.start_tick += delta;
            }
            if let Some(f) = t.player.follow.as_mut() {
                f.at += delta;
            }
            if let Some(r) = t.player.repeat.as_mut() {
                r.next = self.maps.bars.next_boundary(to, r.quantize);
            }
            if let Some(l) = t.player.logged.as_mut() {
                l.1 += delta;
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
            EngineCommand::Preview(c) => self.handle_preview(c),
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
        if let Some((start, end)) = self
            .hold_loop
            .filter(|&(start, end)| self.tick >= start && end - start >= run.ticks_per_sample)
        {
            return Some((start, end));
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

    /// The next tick something is due at: a queued jump or a stop.
    fn next_timed_event(&self) -> Option<f64> {
        let transport = match (self.pending_jump, self.stop_at) {
            (Some(j), Some(at)) => Some(j.at.min(at)),
            (Some(j), None) => Some(j.at),
            (None, at) => at,
        };
        match (transport, self.next_launch_event()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Carry out what has come due at the start of the next block.
    fn run_timed_events(&mut self, run: &mut TempoRun) {
        if !self.playing {
            return;
        }
        if self
            .stop_at
            .is_some_and(|at| self.frames_until(at, run) == 0)
        {
            self.stop_at = None;
            self.stop_transport();
            *run = self.tempo_run();
            return;
        }
        if let Some(j) = self
            .pending_jump
            .filter(|j| self.frames_until(j.at, run) == 0)
        {
            self.pending_jump = None;
            self.jump(j.to, JumpKind::Seek);
            *run = self.tempo_run();
        }
        self.run_launch_events(run);
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
    /// into `max_block` chunks, and at the loop end. An input channel missing or shorter than
    /// the block reads as silence.
    pub fn process(&mut self, io: AudioIo<'_>) {
        let started = std::time::Instant::now();
        let total = io.outputs.iter().map(|o| o.len()).min().unwrap_or(0);
        for (meter, input) in self.shared.input_meters.iter().zip(io.inputs) {
            meter.store_max(input.iter().fold(0f32, |m, s| m.max(s.abs())));
        }
        let mut done = 0;
        while done < total {
            self.drain_input();
            if let Some(left) = self.count_in.as_ref().map(|c| c.frames_left) {
                let n = (total - done).min(self.max_block).min(left as usize).max(1);
                let run = self.tempo_run();
                self.process_block(io.inputs, io.outputs, done, n, run, None);
                self.count_in_advance(n);
                done += n;
                continue;
            }
            let mut run = self.tempo_run();
            self.run_timed_events(&mut run);
            self.wrap_at_loop_end(&mut run);
            let mut n = self.limit_to_tempo_point(&mut run, (total - done).min(self.max_block));
            if self.playing {
                if let Some(frames) = self
                    .next_timed_event()
                    .map(|at| self.frames_until(at, &run))
                    .filter(|f| *f > 0)
                {
                    n = n.min(frames);
                }
            }
            let loop_end = self
                .active_loop(&run)
                .and_then(|(_, end)| (self.frames_until(end, &run) <= n).then_some(end));
            if let Some(end) = loop_end {
                n = self.frames_until(end, &run).max(1);
            }
            self.process_block(io.inputs, io.outputs, done, n, run, loop_end);
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

    /// Move the count-in along, starting the transport when it's over.
    fn count_in_advance(&mut self, frames: usize) {
        let Some(count) = self.count_in.as_mut() else {
            return;
        };
        count.frames_left = count.frames_left.saturating_sub(frames as u64);
        let left = count.frames_left;
        self.shared
            .count_in_left
            .store(count.ticks * left as f64 / count.frames_total.max(1) as f64);
        if left == 0 {
            self.begin_playing();
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn process_block(
        &mut self,
        inputs: &[&[f32]],
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

        let plan_for_takes = std::mem::take(&mut self.plans);
        self.step_slot_records(plan_for_takes.active(), time);
        self.plans = plan_for_takes;
        let garbage = &mut self.garbage;
        let mut retire_clip = |c: Arc<ClipData>| retire(garbage, Garbage::Shared(c));
        let smooth = self.smooth_coeff;
        let env = clip::LaunchEnv {
            bars: &maps.bars,
            global: self.quantize,
            chase: self.chase,
        };
        for t in self.tracks.iter_mut() {
            t.smooth = smooth;
            t.lane.set_chase(self.chase);
            t.begin_block(
                time,
                &transport,
                self.midi_auto_reset,
                env,
                &mut retire_clip,
            );
        }
        self.master.smooth = smooth;
        self.master.begin_block(
            time,
            &transport,
            self.midi_auto_reset,
            env,
            &mut retire_clip,
        );
        for t in self.tracks.iter_mut() {
            if let Some(lane) = t.lane.spent.take() {
                retire(&mut self.garbage, Garbage::Shared(lane));
            }
        }
        if self.recording {
            self.log_session_playback();
        }

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
        self.cue[0][..frames].fill(0.0);
        self.cue[1][..frames].fill(0.0);
        for (level, steps) in plan.levels.iter().enumerate() {
            let steps = &plan.steps[steps.start as usize..steps.end as usize];
            for step in steps {
                self.gather_inputs(plan, step, inputs, start, time, &transport, &maps.bars);
            }
            self.run_control_steps(plan, steps, level == 0, &ctx);
            self.finish_steps(plan, steps, time, &ctx);
        }
        self.run_captures(plan, inputs, start, time);
        self.write_outputs(plan, outputs, start, frames);
        self.plans = plans;
        self.clock += frames as u64;

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

    /// Tell the Arrangement what started and stopped in this block.
    fn log_session_playback(&mut self) {
        for t in self.tracks.iter_mut() {
            let now = t.player.playing.as_ref().map(|p| (p.slot, p.start_tick));
            if now != t.player.logged {
                t.player.logged = now;
                let _ = self.events.push(EngineEvent::SessionPlayback {
                    track: t.id,
                    slot: now.map(|n| n.0),
                    at: t.player.changed_at,
                    start_tick: now.map_or(0.0, |n| n.1),
                });
            }
        }
    }

    /// Put the rendered block on the hardware. A channel nothing plays through is silent.
    fn write_outputs(
        &mut self,
        plan: &RenderPlan,
        outputs: &mut [&mut [f32]],
        start: usize,
        frames: usize,
    ) {
        for out in outputs.iter_mut() {
            out[start..start + frames].fill(0.0);
        }
        // Main goes first, written rather than added, so a plain Set is bit exact.
        let (l, r) = self.master.output(frames);
        put_output(outputs, start, frames, plan.main_out, l, r, true);
        let [sl, sr] = &mut self.scratch;
        for hw in &plan.hardware {
            if let Some(t) = self.tracks.get_mut(hw.input.from as usize) {
                sl[..frames].fill(0.0);
                sr[..frames].fill(0.0);
                t.main_into(&mut sl[..frames], &mut sr[..frames], frames);
                put_output(outputs, start, frames, hw.out, sl, sr, false);
            }
        }
        // A preview is heard ahead of the mixer: through the Cue output if there is one, or
        // straight on the Main output.
        let mut done = None;
        if let Some(cue_out) = plan.cue {
            let [cl, cr] = &mut self.cue;
            done = self
                .preview
                .render(&mut cl[..frames], &mut cr[..frames], self.sample_rate);
            let gain = db_to_gain(self.master.mix.cue_db);
            for i in 0..frames {
                cl[i] *= gain;
                cr[i] *= gain;
            }
            put_output(outputs, start, frames, cue_out, cl, cr, false);
        } else if self.preview.is_active() {
            sl[..frames].fill(0.0);
            sr[..frames].fill(0.0);
            done = self
                .preview
                .render(&mut sl[..frames], &mut sr[..frames], self.sample_rate);
            put_output(outputs, start, frames, plan.main_out, sl, sr, false);
        }
        if let Some(sample) = done {
            retire(&mut self.garbage, Garbage::Shared(sample));
        }
    }

    /// Start and end the slot recordings that reach their launch point in this block, and work
    /// out what takes in progress silence.
    fn step_slot_records(&mut self, plan: &RenderPlan, time: BlockTime) {
        for t in self.tracks.iter_mut() {
            let Some(rec) = t.slot_rec.as_mut() else {
                continue;
            };
            if !rec.started && self.playing && rec.from < time.t1() {
                rec.started = true;
                t.shared
                    .record_queued_slot
                    .store(shared::NO_SLOT, Ordering::Relaxed);
                t.shared
                    .recording_slot
                    .store(rec.slot as i32, Ordering::Relaxed);
                let _ = self.events.push(EngineEvent::SlotRecordStarted {
                    track: t.id,
                    slot: rec.slot,
                    tick: rec.from,
                });
            }
        }
        // Under a take, what's on the timeline is on its way out.
        let window = self.arrangement_window();
        let in_take = window.is_some_and(|(from, until)| time.t0 >= from && time.t0 < until);
        for (i, t) in self.tracks.iter_mut().enumerate() {
            let arrangement_take = t
                .capture
                .as_ref()
                .is_some_and(|c| c.mode == CaptureMode::Arrangement);
            t.lane_muted = in_take && (arrangement_take || t.replacing);
            t.silenced = t.capture.is_some() && plan.input_of(i) == InputSource::Main;
        }
    }

    /// Hand each take its share of this block, and end the slot recordings that have finished.
    fn run_captures(
        &mut self,
        plan: &RenderPlan,
        inputs: &[&[f32]],
        start: usize,
        time: BlockTime,
    ) {
        if !self.playing {
            return;
        }
        let frames = time.frames;
        let arrangement = self.arrangement_window();
        for i in 0..self.tracks.len() {
            let window = match self.tracks[i].capture.as_ref().map(|c| c.mode) {
                None => continue,
                Some(CaptureMode::Arrangement) => arrangement,
                Some(CaptureMode::Slot) => self.tracks[i]
                    .slot_rec
                    .map(|r| (r.from, r.close.map_or(f64::INFINITY, |c| c.0))),
            };
            let silence = &self.silence[..frames];
            let (left, right): (&[f32], &[f32]) = match plan.input_of(i) {
                InputSource::Hardware { channel, stereo } => {
                    let channel = channel as usize;
                    let read = |c: usize| {
                        inputs
                            .get(c)
                            .and_then(|s| s.get(start..start + frames))
                            .unwrap_or(silence)
                    };
                    (
                        read(channel),
                        if stereo {
                            read(channel + 1)
                        } else {
                            read(channel)
                        },
                    )
                }
                InputSource::Main => self.master.output(frames),
                InputSource::None => {
                    let tap = plan
                        .steps
                        .iter()
                        .find(|s| s.track as usize == i)
                        .and_then(|s| s.inputs.iter().find_map(|i| i.bus))
                        .filter(|b| b.monitored)
                        .and_then(|b| self.buses.get(b.bus as usize))
                        .map(|bus| bus.input(0, frames));
                    match tap {
                        Some(input) => (input.left, input.right),
                        None => (silence, silence),
                    }
                }
            };
            let block = CaptureBlock {
                left,
                right,
                clock: self.clock,
                t0: time.t0,
                ticks_per_sample: time.ticks_per_sample,
                window,
            };
            let track = self.tracks[i].id;
            if let Some(sink) = self.tracks[i].capture.as_mut() {
                sink.run(block, track, &mut self.events);
            }
        }
        for i in 0..self.tracks.len() {
            let t = &self.tracks[i];
            let Some((at, _)) = t.slot_rec.and_then(|r| r.close) else {
                continue;
            };
            let reached = t.capture.as_ref().map_or(time.t1(), |c| c.clock());
            if reached >= at {
                self.end_slot_recording(i, at);
            }
        }
    }

    /// Sum what a step takes as input into its track, before its devices run. Track `tracks.len()`
    /// is the Master, and the metronome joins it there. A group or return track's buffer holds
    /// nothing else; a clip track's holds its clips and whatever input is monitored.
    #[allow(clippy::too_many_arguments)]
    fn gather_inputs(
        &mut self,
        plan: &RenderPlan,
        step: &Step,
        hw_inputs: &[&[f32]],
        start: usize,
        time: BlockTime,
        transport: &TransportInfo,
        bars: &BarMap,
    ) {
        let frames = time.frames;
        let coeff = self.smooth_coeff;
        let to = step.track as usize;
        if to == self.tracks.len() {
            let click_from = self.click_from();
            let t = &mut *self.master;
            let (l, r) = (&mut t.left[..frames], &mut t.right[..frames]);
            for input in &step.inputs {
                if let Some(from) = self.tracks.get_mut(input.from as usize) {
                    add_input(from, input, l, r, frames, coeff);
                }
            }
            if let Some(count) = &self.count_in {
                self.metronome
                    .render_count_in(count, frames, self.sample_rate, l, r);
                return;
            }
            let from = click_from;
            self.click.render(
                &mut self.metronome,
                time,
                bars,
                l,
                r,
                transport.playing && from.is_some(),
                from.unwrap_or(f64::INFINITY),
            );
            return;
        }
        if to > self.tracks.len() {
            return;
        }
        self.tracks[to].prepare_own(frames, coeff);
        let open = self.tracks[to].monitoring();
        if open {
            if let InputSource::Hardware { channel, stereo } = plan.input_of(to) {
                let read = |c: usize| hw_inputs.get(c).and_then(|s| s.get(start..start + frames));
                let t = &mut self.tracks[to];
                let channel = channel as usize;
                let (left, right) = (read(channel), read(channel + stereo as usize));
                if let Some(left) = left {
                    let right = right.unwrap_or(left);
                    for ((d, l), (e, r)) in t.left[..frames]
                        .iter_mut()
                        .zip(left)
                        .zip(t.right[..frames].iter_mut().zip(right))
                    {
                        *d += l;
                        *e += r;
                    }
                }
            }
        }
        self.deliver_midi(plan, to, open);
        for sc in plan.sidechains_into(step.track) {
            fetch_sidechain(&mut self.buses, sc, frames);
        }
        let (before, rest) = self.tracks.split_at_mut(to);
        let Some((dest, after)) = rest.split_first_mut() else {
            return;
        };
        for input in &step.inputs {
            if let Some(read) = input.bus {
                if read.monitored && !open {
                    continue;
                }
                if let Some(bus) = self.buses.get_mut(read.bus as usize) {
                    let [sl, sr] = &mut self.scratch;
                    bus.mix_into(
                        read.reader,
                        &mut dest.left,
                        &mut dest.right,
                        (sl, sr),
                        frames,
                    );
                }
                continue;
            }
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

    /// Give a track the MIDI routed to it from earlier tracks. MIDI for its input stage only
    /// arrives while it is monitoring; MIDI addressed to a device skips that stage.
    fn deliver_midi(&mut self, plan: &RenderPlan, to: usize, monitoring: bool) {
        let routes = plan.midi_into(to as u32);
        if routes.is_empty() {
            return;
        }
        for route in routes {
            let from = route.from as usize;
            if from == to || from >= self.tracks.len() {
                continue;
            }
            let (src, dest) = two_tracks(&mut self.tracks, from, to);
            for e in src.midi_out.iter() {
                let mut e = *e;
                if let Some(channel) = route.channel {
                    e.message.set_channel(channel);
                }
                match route.device {
                    Some(device) => {
                        if dest.direct.len() < dest.direct.capacity() {
                            dest.direct.push((device, e));
                        }
                    }
                    None => {
                        // A note let in before monitoring closed still has to end.
                        let release = matches!(
                            e.message,
                            MidiMessage::NoteOff { .. } | MidiMessage::AllNotesOff
                        );
                        if (monitoring || release) && dest.events.len() < dest.events.capacity() {
                            dest.events.push(e);
                        }
                    }
                }
            }
        }
        self.tracks[to]
            .events
            .sort_unstable_by_key(|e| e.sort_key());
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
            if take_commands {
                let position = self.master.crossfader_position(ctx.tick_at(offset));
                let gains = plan.crossfade_curve.gains(position);
                if let Some(slot) = self.crossfade_steps.get_mut(index as usize) {
                    *slot = gains;
                }
            }
            let crossfade = self
                .crossfade_steps
                .get(index as usize)
                .copied()
                .unwrap_or((1.0, 1.0));
            let events = &mut self.events;
            let buses = &mut self.buses;
            for step in steps {
                let is_master = step.track == master;
                let t = if is_master {
                    Some(&mut *self.master)
                } else {
                    self.tracks.get_mut(step.track as usize).map(|t| &mut **t)
                };
                if let Some(t) = t {
                    let target = if is_master {
                        TrackRef::Master
                    } else {
                        TrackRef::Track(t.id)
                    };
                    let sink = std::cell::RefCell::new(&mut *events);
                    t.control_step(
                        index,
                        offset,
                        ctx,
                        buses,
                        &mut |_, d, p, v| {
                            push_param_changed(&mut sink.borrow_mut(), target, d, p, v)
                        },
                        &mut |cell, value| {
                            let _ = sink.borrow_mut().push(EngineEvent::ClipAutomationEnded {
                                target,
                                cell,
                                value,
                            });
                        },
                    );
                }
            }
            for step in steps {
                if let Some(t) = self.tracks.get_mut(step.track as usize) {
                    t.set_crossfade(crossfade);
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
        if plan.cue.is_some() {
            // Cue mode: the switches feed the Cue output instead of silencing anything.
            for t in self.tracks.iter_mut() {
                t.cue_on = t.soloed;
                t.holds_solo = false;
                t.set_solo_gates(true, true);
            }
            return;
        }
        for t in self.tracks.iter_mut() {
            t.cue_on = false;
        }
        for t in self.tracks.iter_mut() {
            t.feeders_kept = false;
        }
        for &(tapper, tapped) in &plan.solo_taps {
            let soloed = self.tracks.get(tapper as usize).is_some_and(|t| t.soloed);
            if let Some(t) = self.tracks.get_mut(tapped as usize).filter(|_| soloed) {
                t.feeders_kept = true;
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
                    soloed = self
                        .tracks
                        .get(g)
                        .is_some_and(|t| t.soloed || t.feeders_kept);
                    at = parent(g);
                    depth += 1;
                }
                soloed
            };
            self.tracks[i].set_solo_gates(open, open || any_return);
        }
    }

    /// Run what is left of some steps' chains and take them through the mixer stage.
    fn finish_steps(&mut self, plan: &RenderPlan, steps: &[Step], time: BlockTime, ctx: &BlockCtx) {
        let events = &mut self.events;
        let master = self.tracks.len() as u32;
        let coeff = self.smooth_coeff;
        let [cue_l, cue_r] = &mut self.cue;
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
            t.finish_block(time, ctx, &mut self.buses, &mut |t, d, p, v| {
                let owner = if is_master {
                    TrackRef::Master
                } else {
                    TrackRef::Track(t)
                };
                push_param_changed(events, owner, d, p, v)
            });
            t.prepare_sends(time.frames, coeff);
            t.cue_into((&mut cue_l[..], &mut cue_r[..]), time.frames, coeff);
            t.apply_mix(time.frames, coeff);
            if t.silenced {
                t.left[..time.frames].fill(0.0);
                t.right[..time.frames].fill(0.0);
            }
            t.write_post_mix(&mut self.buses, time.frames);
            t.feed_lines(time.frames);
            for out in plan.taps_of(step.track) {
                self.buses[out.bus as usize].finish(time.frames);
            }
            if is_master {
                let (l, r) = t.output(time.frames);
                self.shared.master_meter.update(l, r);
            }
        }
    }
}

/// Two different tracks at once.
fn two_tracks(
    tracks: &mut [Box<TrackNode>],
    a: usize,
    b: usize,
) -> (&mut TrackNode, &mut TrackNode) {
    if a < b {
        let (head, tail) = tracks.split_at_mut(b);
        (&mut head[a], &mut tail[0])
    } else {
        let (head, tail) = tracks.split_at_mut(a);
        (&mut tail[0], &mut head[b])
    }
}

/// Read a sidechain's source, through its delay, into the bus its device reads.
fn fetch_sidechain(buses: &mut [taps::TapBus], sc: &SidechainIn, frames: usize) {
    let (src, feed) = (sc.read.bus as usize, sc.feed as usize);
    if src == feed || src.max(feed) >= buses.len() {
        return;
    }
    if src < feed {
        let (head, tail) = buses.split_at_mut(feed);
        tail[0].fetch_from(&mut head[src], sc.read.reader, frames);
    } else {
        let (head, tail) = buses.split_at_mut(src);
        head[feed].fetch_from(&mut tail[0], sc.read.reader, frames);
    }
}

/// Put a stereo signal on a hardware output. A stereo signal into a mono output is summed and
/// attenuated by 6 dB (manual p. 375). `write` replaces what is there rather than adding to it.
fn put_output(
    outputs: &mut [&mut [f32]],
    start: usize,
    frames: usize,
    pair: OutputPair,
    left: &[f32],
    right: &[f32],
    write: bool,
) {
    let channel = pair.channel as usize;
    let mut store = |at: usize, from: &[f32], gain: Option<&[f32]>| {
        let Some(out) = outputs.get_mut(at) else {
            return;
        };
        let out = &mut out[start..start + frames];
        match (write, gain) {
            (true, None) => out.copy_from_slice(&from[..frames]),
            (false, None) => out.iter_mut().zip(from).for_each(|(o, s)| *o += s),
            (_, Some(other)) => {
                for ((o, a), b) in out.iter_mut().zip(from).zip(other) {
                    let s = (a + b) * 0.5;
                    *o = if write { s } else { *o + s };
                }
            }
        }
    };
    if pair.stereo {
        store(channel, left, None);
        store(channel + 1, right, None);
    } else {
        store(channel, left, Some(right));
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

/// A jump of the playhead that happens at the next launch point.
#[derive(Clone, Copy)]
struct PendingJump {
    at: f64,
    to: f64,
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

impl Engine {
    /// Where the metronome may click from, or `None` if it may not. With Enable Only While
    /// Recording it sounds only while recording, and not before Punch-In.
    fn click_from(&self) -> Option<f64> {
        if !self.metronome.config.only_while_recording {
            return Some(f64::NEG_INFINITY);
        }
        let recording = self.recording || self.tracks.iter().any(|t| t.slot_rec.is_some());
        recording.then(|| {
            if self.recording {
                self.punch_in.unwrap_or(f64::NEG_INFINITY)
            } else {
                f64::NEG_INFINITY
            }
        })
    }
}

impl ClickDelay {
    fn new(max_block: usize) -> Self {
        Self {
            line: None,
            delay: FadingDelay::default(),
            buf: [vec![0.0; max_block], vec![0.0; max_block]],
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render(
        &mut self,
        metronome: &mut Metronome,
        time: BlockTime,
        bars: &BarMap,
        left: &mut [f32],
        right: &mut [f32],
        playing: bool,
        from: f64,
    ) {
        let Some(line) = &mut self.line else {
            if playing {
                metronome.render(time, bars, left, right, from);
            }
            return;
        };
        let frames = left.len();
        let [bl, br] = &mut self.buf;
        let (bl, br) = (&mut bl[..frames], &mut br[..frames]);
        bl.fill(0.0);
        br.fill(0.0);
        if playing {
            metronome.render(time, bars, bl, br, from);
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
