//! Tracks, device chains and the mixer stage.
#![allow(clippy::vec_box)]

use crate::clip::{BlockTime, ClipData, ClipPlayer, Pending};
use crate::graph::{BusIn, Tap};
use crate::lane::{ArrangementLane, LanePlayer};
use crate::params::{mix_cell, MAX_SENDS};
use crate::params::{BusLayout, ControlHooks, ControlStep, ParamBus, TrackRoutes, CONTROL_STEP};
use crate::rack::RackNode;
use crate::shared::{TrackShared, NO_SLOT};
use rlive_core::param::{mixer, ParamId};
use rlive_core::time::BarMap;
use rlive_core::warp::StretchBank;
use rlive_core::{
    db_to_gain, AudioBuffer, DeviceId, EventQueue, MidiEvent, ParamOutQueue, Ports, ProcessContext,
    Processor, Resource, TrackId, TransportInfo, ViewChannel, ViewWriter,
};
use std::ops::Range;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

pub const MAX_DEVICES: usize = 64;
pub const MAX_SCENES: usize = 256;
pub const MAX_EVENTS: usize = 2048;

/// What a device in a chain actually is.
pub enum DeviceBody {
    Proc(Box<dyn Processor>),
    Rack(Box<RackNode>),
}

/// A node in a device chain: a processor, or a rack of further chains.
pub struct DeviceSlot {
    pub id: DeviceId,
    pub body: DeviceBody,
    /// The cells this device owns in its track's bus, its activator first. The engine keeps this
    /// up to date with the bus, so it finds the device's inbox in O(1) at any depth.
    pub cells: Range<u32>,
    pub ports: Ports,
    pub bypass: bool,
    /// The device replaces the MIDI stream with its `events_out`, for the devices after it.
    pub midi_effect: bool,
    /// The device's latency in frames, shared with the UI.
    pub latency: Arc<AtomicU32>,
    /// One value per declared modulation source. Holds its value between blocks.
    pub mod_out: Box<[f32]>,
    /// Where the device's view data goes. `None` for a device with nothing to show.
    pub view: Option<Arc<ViewChannel>>,
}

impl DeviceSlot {
    fn with_body(id: DeviceId, body: DeviceBody) -> Self {
        Self {
            id,
            body,
            cells: 0..0,
            ports: Ports::default(),
            bypass: false,
            midi_effect: false,
            latency: Arc::new(AtomicU32::new(0)),
            mod_out: Box::default(),
            view: None,
        }
    }

    pub fn new(id: DeviceId, processor: Box<dyn Processor>) -> Self {
        Self::with_body(id, DeviceBody::Proc(processor))
    }

    /// A rack. A MIDI rack replaces the MIDI stream, as a MIDI effect does.
    pub fn rack(id: DeviceId, rack: Box<RackNode>) -> Self {
        let midi_effect = rack.kind == crate::rack::RackKind::Midi;
        let mut slot = Self::with_body(id, DeviceBody::Rack(rack));
        slot.midi_effect = midi_effect;
        slot
    }

    /// This slot's rack, if it is one.
    pub fn as_rack_mut(&mut self) -> Option<&mut RackNode> {
        match &mut self.body {
            DeviceBody::Rack(r) => Some(r),
            DeviceBody::Proc(_) => None,
        }
    }

    /// How many racks deep this device goes: 0 for a processor.
    pub(crate) fn nesting(&self) -> usize {
        match &self.body {
            DeviceBody::Proc(_) => 0,
            DeviceBody::Rack(r) => {
                let inner = r.chains.iter().chain(&r.returns);
                1 + inner
                    .flat_map(|c| c.devices.iter())
                    .map(|d| d.nesting())
                    .max()
                    .unwrap_or(0)
            }
        }
    }

    fn for_each_processor(&mut self, f: &mut dyn FnMut(&mut dyn Processor)) {
        match &mut self.body {
            DeviceBody::Proc(p) => f(&mut **p),
            DeviceBody::Rack(r) => r
                .chains
                .iter_mut()
                .chain(r.returns.iter_mut())
                .flat_map(|c| c.devices.iter_mut())
                .for_each(|d| d.for_each_processor(f)),
        }
    }

    /// Tell every processor in this slot it is about to leave the engine.
    pub(crate) fn stop_processing(&mut self) {
        self.for_each_processor(&mut |p| p.stop_processing());
    }

    /// Silence every processor in this slot.
    pub(crate) fn reset(&mut self) {
        self.for_each_processor(&mut |p| p.reset());
    }

    /// Hand a processor a shared resource. A rack takes none and gives it straight back.
    pub(crate) fn set_resource(&mut self, slot: u32, resource: Resource) -> Option<Resource> {
        match &mut self.body {
            DeviceBody::Proc(p) => p.set_resource(slot, resource),
            DeviceBody::Rack(_) => Some(resource),
        }
    }

    /// Find this device's cells again after the bus or the tree changed, all the way down.
    pub(crate) fn bind_cells(&mut self, bus: &ParamBus) {
        self.cells = bus.cells_of(self.id).unwrap_or(0..0);
        if let DeviceBody::Rack(r) = &mut self.body {
            for chain in r.chains.iter_mut().chain(r.returns.iter_mut()) {
                bind_chain(&mut chain.devices, bus);
            }
        }
    }
}

/// Find the cells of every device in a chain, all the way down.
pub(crate) fn bind_chain(devices: &mut [Box<DeviceSlot>], bus: &ParamBus) {
    for d in devices.iter_mut() {
        d.bind_cells(bus);
    }
}

/// Mixer settings of a track.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MixSettings {
    pub volume_db: f32,
    /// -1 (left) ..= 1 (right).
    pub pan: f32,
    pub mute: bool,
    pub solo: bool,
}

impl Default for MixSettings {
    fn default() -> Self {
        Self {
            volume_db: 0.0,
            pan: 0.0,
            mute: false,
            solo: false,
        }
    }
}

impl MixSettings {
    /// Target (left, right) gains with a -3 dB-compensated sine pan law (unity at centre).
    pub fn gains(&self) -> (f32, f32) {
        let g = db_to_gain(self.volume_db);
        let theta = (self.pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
        let l = (theta.cos() * std::f32::consts::SQRT_2).min(1.0);
        let r = (theta.sin() * std::f32::consts::SQRT_2).min(1.0);
        (g * l, g * r)
    }

    /// Take a resolved mixer value.
    fn apply(&mut self, control: ParamId, plain: f64) {
        match control {
            mixer::VOLUME => self.volume_db = plain as f32,
            mixer::PAN => self.pan = plain as f32,
            mixer::MUTE => self.mute = plain >= 0.5,
            mixer::SOLO => self.solo = plain >= 0.5,
            _ => {}
        }
    }
}

/// Smoothed stereo gain to avoid zipper noise.
pub(crate) struct GainSmoother {
    l: f32,
    r: f32,
}

impl GainSmoother {
    fn new() -> Self {
        Self { l: 1.0, r: 1.0 }
    }

    fn apply(&mut self, left: &mut [f32], right: &mut [f32], target: (f32, f32), coeff: f32) {
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            self.l += (target.0 - self.l) * coeff;
            self.r += (target.1 - self.r) * coeff;
            *l *= self.l;
            *r *= self.r;
        }
    }

    /// Mix `src` into `dst` at the smoothed gain.
    fn add(
        &mut self,
        src: (&[f32], &[f32]),
        dst: (&mut [f32], &mut [f32]),
        target: (f32, f32),
        coeff: f32,
    ) {
        let (sl, sr) = src;
        let (dl, dr) = dst;
        for i in 0..sl.len().min(dl.len()) {
            self.l += (target.0 - self.l) * coeff;
            self.r += (target.1 - self.r) * coeff;
            dl[i] += sl[i] * self.l;
            dr[i] += sr[i] * self.r;
        }
    }
}

/// A gain whose target changes at control steps inside a block.
struct GainStage {
    smoother: GainSmoother,
    /// The target in force at the start of the next block.
    settled: (f32, f32),
    /// The target after the steps so far.
    target: (f32, f32),
    points: Vec<(u32, (f32, f32))>,
    /// The mixer values changed since the last target was set.
    dirty: bool,
}

impl GainStage {
    fn new(max_block: usize) -> Self {
        Self {
            smoother: GainSmoother::new(),
            settled: (1.0, 1.0),
            target: (1.0, 1.0),
            points: Vec::with_capacity(max_block / CONTROL_STEP + 2),
            dirty: true,
        }
    }

    fn set_target(&mut self, offset: usize, target: (f32, f32)) {
        self.dirty = false;
        if target != self.target && self.points.len() < self.points.capacity() {
            self.target = target;
            self.points.push((offset as u32, target));
        }
    }

    /// Smooth towards each target from the frame it took effect.
    fn apply(&mut self, left: &mut [f32], right: &mut [f32], frames: usize, coeff: f32) {
        let mut target = self.settled;
        let mut from = 0;
        for &(at, next) in &self.points {
            let at = (at as usize).min(frames);
            self.smoother
                .apply(&mut left[from..at], &mut right[from..at], target, coeff);
            target = next;
            from = at;
        }
        self.smoother.apply(
            &mut left[from..frames],
            &mut right[from..frames],
            target,
            coeff,
        );
        self.settled = target;
        self.points.clear();
    }

    /// [`apply`](Self::apply), but mixing `src` into `dst` and leaving `src` alone.
    fn add(
        &mut self,
        src: (&[f32], &[f32]),
        dst: (&mut [f32], &mut [f32]),
        frames: usize,
        coeff: f32,
    ) {
        let silent = self.points.is_empty()
            && self.settled == (0.0, 0.0)
            && self.smoother.l.abs() < 1e-6
            && self.smoother.r.abs() < 1e-6;
        if silent {
            self.smoother.l = 0.0;
            self.smoother.r = 0.0;
            return;
        }
        let (sl, sr) = (&src.0[..frames], &src.1[..frames]);
        let (dl, dr) = (&mut dst.0[..frames], &mut dst.1[..frames]);
        let mut target = self.settled;
        let mut from = 0;
        for &(at, next) in &self.points {
            let at = (at as usize).min(frames);
            self.smoother.add(
                (&sl[from..at], &sr[from..at]),
                (&mut dl[from..at], &mut dr[from..at]),
                target,
                coeff,
            );
            target = next;
            from = at;
        }
        self.smoother.add(
            (&sl[from..], &sr[from..]),
            (&mut dl[from..], &mut dr[from..]),
            target,
            coeff,
        );
        self.settled = target;
        self.points.clear();
    }

    /// Forget targets that nothing rendered this block.
    fn settle(&mut self) {
        if let Some(&(_, target)) = self.points.last() {
            self.settled = target;
        }
        self.points.clear();
    }

    /// Start exactly at a gain, with nothing to smooth towards.
    fn seed(&mut self, gain: (f32, f32)) {
        self.smoother.l = gain.0;
        self.smoother.r = gain.1;
        self.settled = gain;
        self.target = gain;
        self.points.clear();
        self.dirty = false;
    }
}

/// Signal a track makes available to its sends. Only filled when the plan has one.
struct SendTaps {
    /// Before the mixer stage, gated by solo.
    pre: [Vec<f32>; 2],
    /// After the mixer stage, gated by solo.
    post: [Vec<f32>; 2],
    /// Open unless solo silences this track's sends.
    gate: GainStage,
    /// Volume, pan and the activator, as the main output has them, without the solo stage.
    mix: GainStage,
    /// The level of each send, in the sender's own bus.
    levels: Vec<GainStage>,
    /// What the plan wants of this track this block.
    wants_pre: bool,
    wants_post: bool,
    gate_dirty: bool,
    feedback: Option<FeedbackRing>,
}

impl SendTaps {
    fn new(max_block: usize) -> Self {
        Self {
            pre: [vec![0.0; max_block], vec![0.0; max_block]],
            post: [vec![0.0; max_block], vec![0.0; max_block]],
            gate: GainStage::new(max_block),
            mix: GainStage::new(max_block),
            levels: (0..MAX_SENDS)
                .map(|_| {
                    let mut level = GainStage::new(max_block);
                    level.seed((0.0, 0.0));
                    level
                })
                .collect(),
            wants_pre: false,
            wants_post: false,
            gate_dirty: false,
            feedback: None,
        }
    }

    fn set_level(&mut self, offset: usize, control: ParamId, plain: f64) {
        let n = (control.0 - mixer::FIRST_SEND.0) as usize;
        if let Some(level) = self.levels.get_mut(n) {
            let gain = db_to_gain(plain as f32);
            level.set_target(offset, (gain, gain));
        }
    }
}

/// What a return track fed its sends, a fixed time ago. A feedback send reads this: a track can
/// only hear itself, or one rendering after it, from the past.
struct FeedbackRing {
    pre: [Vec<f32>; 2],
    post: [Vec<f32>; 2],
    /// Where the next block goes, which is also the oldest audio in the ring.
    head: usize,
}

impl FeedbackRing {
    fn new(max_block: usize) -> Self {
        Self {
            pre: [vec![0.0; max_block], vec![0.0; max_block]],
            post: [vec![0.0; max_block], vec![0.0; max_block]],
            head: 0,
        }
    }

    /// Read the oldest `frames` into `out`, without moving. Delays by the ring's full length.
    fn read(buf: &[f32], head: usize, frames: usize, out: &mut [f32]) {
        let wrap = (buf.len() - head).min(frames);
        out[..wrap].copy_from_slice(&buf[head..head + wrap]);
        out[wrap..frames].copy_from_slice(&buf[..frames - wrap]);
    }

    /// Store a block. A value that blew up never comes back round the loop.
    fn write(buf: &mut [f32], head: usize, input: &[f32]) {
        let n = buf.len();
        for (i, &s) in input.iter().enumerate() {
            buf[(head + i) % n] = if s.is_finite() { s } else { 0.0 };
        }
    }
}

/// Timing of the block being rendered, for chain segments.
#[derive(Clone, Copy)]
pub(crate) struct BlockCtx<'a> {
    pub transport: &'a TransportInfo,
    pub time: BlockTime,
    pub bars: &'a BarMap,
    pub frames: usize,
}

impl BlockCtx<'_> {
    /// The timeline position `offset` frames into the block.
    pub(crate) fn tick_at(&self, offset: usize) -> f64 {
        if self.transport.playing {
            self.time.tick_at(offset).max(0.0)
        } else {
            self.transport.tick
        }
    }

    /// The transport as it stands `offset` frames into the block.
    fn at(&self, offset: usize) -> TransportInfo {
        let mut t = *self.transport;
        if t.playing {
            t.tick = self.time.tick_at(offset).max(0.0);
            t.sample_pos += offset as u64;
            let bar = self.bars.bar_at(t.tick);
            t.bar_start = bar.start;
            t.time_signature = bar.signature;
        }
        t
    }
}

/// How far a block has rendered through a device chain.
#[derive(Default)]
struct ChainCursor {
    frames: usize,
    next_event: usize,
}

/// What every chain of a track shares while one stretch of a block runs through it.
pub(crate) struct Stretch<'a> {
    pub transport: &'a TransportInfo,
    pub param_out: &'a mut ParamOutQueue,
    pub bus: &'a mut ParamBus,
    pub on_param: &'a mut dyn FnMut(DeviceId, u32, f64),
    /// The stretch in frames from the start of the block.
    pub from: u32,
    pub to: u32,
}

/// One stretch of a block running through a device chain.
pub(crate) struct ChainRun<'a, 's> {
    pub left: &'a mut [f32],
    pub right: &'a mut [f32],
    /// The chain's MIDI for this stretch.
    pub events: &'a [MidiEvent],
    /// Holds what MIDI effects pass along.
    pub midi: &'a mut [EventQueue; 2],
    pub at: &'a mut Stretch<'s>,
}

/// Run a device chain over a buffer. Returns which of `midi` holds the chain's final MIDI, if a
/// MIDI effect replaced `events`.
pub(crate) fn run_chain(devices: &mut [Box<DeviceSlot>], run: ChainRun<'_, '_>) -> Option<usize> {
    let ChainRun {
        left,
        right,
        events,
        midi,
        at,
    } = run;
    // Which queue holds the MIDI, once a MIDI effect has replaced `events`.
    let mut input = None;
    for dev in devices.iter_mut() {
        let dev = &mut **dev;
        if let Some(bypass) = at.bus.take_switch(dev.cells.start) {
            if bypass != dev.bypass {
                dev.bypass = bypass;
                if bypass {
                    dev.reset();
                }
            }
        }
        if dev.bypass {
            continue;
        }
        let [a, b] = &mut *midi;
        let (held, out) = if input == Some(0) { (a, b) } else { (b, a) };
        out.clear();
        let heard = match input {
            Some(_) => held.as_slice(),
            None => events,
        };
        match &mut dev.body {
            DeviceBody::Proc(processor) => {
                let view = dev
                    .view
                    .as_deref()
                    .map_or(ViewWriter::none(), ViewWriter::new);
                let mut ctx = ProcessContext {
                    audio: AudioBuffer {
                        left: &mut *left,
                        right: &mut *right,
                    },
                    sidechain: None,
                    aux_out: &mut [],
                    events: heard,
                    events_out: &mut *out,
                    params: at.bus.take_segment(dev.cells.start, at.from, at.to),
                    transport: at.transport,
                    param_out: &mut *at.param_out,
                    mod_out: &mut dev.mod_out,
                    view: &view,
                };
                processor.process(&mut ctx);
                dev.latency.store(processor.latency(), Ordering::Relaxed);
            }
            DeviceBody::Rack(rack) => rack.run(left, right, heard, out, at),
        }
        if dev.midi_effect {
            out.sort();
            input = Some(if input == Some(0) { 1 } else { 0 });
        }
        for (id, v) in at.param_out.drain() {
            at.bus.report_from_device(dev.id, id, v);
            (at.on_param)(dev.id, id, v);
        }
    }
    input
}

/// Everything that writes a bus at a control step, in resolve order.
fn run_writers(
    hooks: Option<&mut (dyn ControlHooks + 'static)>,
    routes: Option<&TrackRoutes>,
    bus: &mut ParamBus,
    index: u32,
    offset: usize,
    ctx: &BlockCtx,
) {
    let tick = ctx.tick_at(offset);
    let Some(hooks) = hooks else {
        if let Some(routes) = routes {
            routes.automate(tick, bus);
        }
        return;
    };
    let step = ControlStep {
        index,
        offset: offset as u32,
        tick,
    };
    hooks.macros(step, bus);
    if let Some(routes) = routes {
        routes.automate(tick, bus);
    }
    hooks.automation(step, bus);
    hooks.modulation(step, bus);
}

/// A track: clip slots → device chain → mixer.
pub struct TrackNode {
    pub id: TrackId,
    pub devices: Vec<Box<DeviceSlot>>,
    pub slots: Vec<Option<Arc<ClipData>>>,
    pub mix: MixSettings,
    pub shared: Arc<TrackShared>,
    /// Every parameter of this track. Devices must be placed in the layout it was built from.
    pub bus: Box<ParamBus>,
    /// Automation lanes, set by `ParamCmd::SetRoutes`.
    pub routes: Arc<TrackRoutes>,
    /// Writes the bus at each control step, around automation. Unused until modulators exist.
    pub hooks: Option<Box<dyn ControlHooks>>,

    pub(crate) player: ClipPlayer,
    pub(crate) lane: LanePlayer,
    /// Playing the lane. A launch turns this off until `BackToArrangement`.
    pub(crate) follow: bool,
    pub(crate) stretch: StretchBank,
    pub(crate) left: Vec<f32>,
    pub(crate) right: Vec<f32>,
    pub(crate) events: Vec<MidiEvent>,
    pub(crate) live: Vec<MidiEvent>,
    pub(crate) param_out: ParamOutQueue,
    midi: [EventQueue; 2],
    cursor: ChainCursor,
    gain: GainStage,
    sends: SendTaps,
    /// Room to read a feedback ring into, on a return track.
    scratch: [Vec<f32>; 2],
    /// Takes the Master's place in the plan: no pan law, and never silenced by solo.
    is_master: bool,
    /// This track's Solo switch as of the last control step.
    pub(crate) soloed: bool,
    /// Holds a soloed track, which keeps it audible.
    pub(crate) holds_solo: bool,
    /// Solo leaves this track's main output on.
    pub(crate) solo_open: bool,
    /// Solo leaves this track's sends on.
    pub(crate) sends_open: bool,
}

impl TrackNode {
    /// Create a track. Call on the UI thread: this allocates all buffers up front.
    pub fn new(id: TrackId, max_block: usize) -> Self {
        Self {
            id,
            devices: Vec::with_capacity(MAX_DEVICES),
            slots: Vec::with_capacity(MAX_SCENES),
            mix: MixSettings::default(),
            shared: Arc::new(TrackShared::default()),
            bus: BusLayout::for_track(&MixSettings::default()).build(),
            routes: Arc::new(TrackRoutes::default()),
            hooks: None,
            player: ClipPlayer::new(),
            lane: LanePlayer::new(Arc::new(ArrangementLane::default())),
            follow: true,
            stretch: StretchBank::new(max_block),
            left: vec![0.0; max_block],
            right: vec![0.0; max_block],
            events: Vec::with_capacity(MAX_EVENTS),
            live: Vec::with_capacity(256),
            param_out: ParamOutQueue::with_capacity(256),
            midi: [
                EventQueue::with_capacity(MAX_EVENTS),
                EventQueue::with_capacity(MAX_EVENTS),
            ],
            cursor: ChainCursor::default(),
            gain: GainStage::new(max_block),
            sends: SendTaps::new(max_block),
            scratch: [Vec::new(), Vec::new()],
            is_master: false,
            soloed: false,
            holds_solo: false,
            solo_open: true,
            sends_open: true,
        }
    }

    /// A track to render last, as the Set's Main output. Call on the UI thread.
    pub fn master(max_block: usize) -> Self {
        let mut node = Self::new(TrackId(0), max_block);
        node.is_master = true;
        node.bus = BusLayout::for_master(0.0).build();
        node
    }

    /// Give a return track somewhere to remember what it sent, so it can be fed back.
    pub fn with_feedback(mut self, max_block: usize) -> Self {
        self.sends.feedback = Some(FeedbackRing::new(max_block));
        self.scratch = [vec![0.0; max_block], vec![0.0; max_block]];
        self
    }

    /// Set the level of a send before the track is sent to the engine.
    pub fn seed_send(&mut self, n: usize, db: f32) {
        if let Some(level) = self.sends.levels.get_mut(n) {
            let gain = db_to_gain(db);
            level.seed((gain, gain));
        }
    }

    pub fn is_master(&self) -> bool {
        self.is_master
    }

    pub fn with_shared(mut self, shared: Arc<TrackShared>) -> Self {
        self.shared = shared;
        self
    }

    /// Set a clip on the UI thread before the track is sent to the engine.
    pub fn set_slot(&mut self, slot: usize, clip: Option<Arc<ClipData>>) {
        while self.slots.len() <= slot && self.slots.len() < MAX_SCENES {
            self.slots.push(None);
        }
        if let Some(s) = self.slots.get_mut(slot) {
            *s = clip;
        }
    }

    /// Set the Arrangement lane on the UI thread before the track is sent to the engine.
    pub fn set_lane(&mut self, lane: Arc<ArrangementLane>) {
        self.lane = LanePlayer::new(lane);
    }

    /// Prepare a block: render clips and collect its MIDI. The chain runs in [`Self::render_to`].
    pub(crate) fn begin_block(
        &mut self,
        time: BlockTime,
        transport: &TransportInfo,
        retire: &mut dyn FnMut(Arc<ClipData>),
    ) {
        let n = time.frames;
        let left = &mut self.left[..n];
        let right = &mut self.right[..n];
        left.fill(0.0);
        right.fill(0.0);
        self.events.clear();
        for e in self.live.drain(..) {
            if self.events.len() < self.events.capacity() {
                self.events.push(e);
            }
        }

        if transport.playing {
            if self.follow {
                // A launch due in this block ends the lane at its launch point.
                let launch_at = match self.player.pending {
                    Some(Pending::Launch { at, .. }) if at < time.t1() => Some(at.max(time.t0)),
                    _ => None,
                };
                let until = launch_at.unwrap_or(time.t1());
                self.lane.render(
                    time,
                    time.t0,
                    until,
                    &mut self.events,
                    left,
                    right,
                    &mut self.stretch,
                );
                if let Some(at) = launch_at {
                    self.lane
                        .release_and_resync(time.event_offset(at), &mut self.events);
                    self.follow = false;
                }
            }
            self.player.render(
                time,
                &mut self.events,
                left,
                right,
                retire,
                &self.slots,
                &mut self.stretch,
            );
        }
        self.events.sort_unstable_by_key(|e| e.sort_key());
        self.cursor = ChainCursor::default();
        // Anything no send consumed last block: there's no plan edge carrying it.
        for level in &mut self.sends.levels {
            level.settle();
        }
    }

    /// Clear what the sends carry over, as of a change of plan.
    pub(crate) fn plan_changed(&mut self) {
        self.sends.wants_pre = false;
        self.sends.wants_post = false;
    }

    /// Take the plan's demand for this track's send signals.
    pub(crate) fn want_send(&mut self, tap: &Tap) {
        match tap {
            Tap::PostFx => self.sends.wants_pre = true,
            _ => self.sends.wants_post = true,
        }
    }

    /// Run the chain up to `to` frames into the block, handing each device a segment of its own.
    fn render_to(
        &mut self,
        to: usize,
        ctx: &BlockCtx,
        on_param: &mut dyn FnMut(TrackId, DeviceId, u32, f64),
    ) {
        let from = self.cursor.frames;
        if to <= from {
            return;
        }
        // Later events must not be lost if a clip placed one at or past the block end.
        let end = if to >= ctx.frames {
            self.events.len()
        } else {
            self.cursor.next_event
                + self.events[self.cursor.next_event..]
                    .partition_point(|e| (e.offset as usize) < to)
        };
        let segment = &mut self.events[self.cursor.next_event..end];
        for e in segment.iter_mut() {
            e.offset -= from as u32;
        }
        let transport = ctx.at(from);
        let id = self.id;
        run_chain(
            &mut self.devices,
            ChainRun {
                left: &mut self.left[from..to],
                right: &mut self.right[from..to],
                events: segment,
                midi: &mut self.midi,
                at: &mut Stretch {
                    transport: &transport,
                    param_out: &mut self.param_out,
                    bus: &mut self.bus,
                    on_param: &mut |dev, p, v| on_param(id, dev, p, v),
                    from: from as u32,
                    to: to as u32,
                },
            },
        );
        self.cursor.frames = to;
        self.cursor.next_event = end;
    }

    /// Resolve this track's parameters at a step. Devices hear their changes as events at this
    /// offset; only a device being switched on or off makes the chain stop here.
    pub(crate) fn control_step(
        &mut self,
        index: u32,
        offset: usize,
        ctx: &BlockCtx,
        on_param: &mut dyn FnMut(TrackId, DeviceId, u32, f64),
    ) {
        run_writers(
            self.hooks.as_deref_mut(),
            // A launched Session clip punches a track's automation out until Back to Arrangement.
            self.follow.then_some(&*self.routes),
            &mut self.bus,
            index,
            offset,
            ctx,
        );
        let resolved = self.bus.resolve();
        if resolved.activators {
            self.render_to(offset, ctx, on_param);
        }
        let (mix, sends) = (&mut self.mix, &mut self.sends);
        self.bus.deliver_resolved(offset as u32, |control, plain| {
            if control >= mixer::FIRST_SEND {
                sends.set_level(offset, control, plain);
            } else {
                mix.apply(control, plain);
            }
        });
        self.gain.dirty |= resolved.mixer;
    }

    /// Say whether solo leaves this track's main output and sends on.
    pub(crate) fn set_solo_gates(&mut self, solo_open: bool, sends_open: bool) {
        self.gain.dirty |= solo_open != self.solo_open;
        self.sends.gate_dirty |= sends_open != self.sends_open;
        self.solo_open = solo_open;
        self.sends_open = sends_open;
    }

    /// Set the gain targets from the mixer, at this step, when something affecting them changed.
    pub(crate) fn update_mix(&mut self, offset: usize) {
        if self.is_master {
            if self.gain.dirty {
                let gain = db_to_gain(self.mix.volume_db);
                self.gain.set_target(offset, (gain, gain));
            }
            return;
        }
        if self.gain.dirty {
            let mix = if self.mix.mute {
                (0.0, 0.0)
            } else {
                self.mix.gains()
            };
            self.sends.mix.set_target(offset, mix);
            let target = if self.solo_open { mix } else { (0.0, 0.0) };
            self.gain.set_target(offset, target);
        }
        if self.sends.gate_dirty {
            self.sends.gate_dirty = false;
            let open = if self.sends_open { 1.0 } else { 0.0 };
            self.sends.gate.set_target(offset, (open, open));
        }
    }

    /// Run what is left of the chain and publish the clip state.
    pub(crate) fn finish_block(
        &mut self,
        time: BlockTime,
        ctx: &BlockCtx,
        on_param: &mut dyn FnMut(TrackId, DeviceId, u32, f64),
    ) {
        self.render_to(time.frames, ctx, on_param);
        self.bus.end_block();

        // Publish clip state.
        let shared = &self.shared;
        shared.playing_slot.store(
            self.player
                .playing
                .as_ref()
                .map_or(NO_SLOT, |p| p.slot as i32),
            std::sync::atomic::Ordering::Relaxed,
        );
        shared.queued_slot.store(
            match self.player.pending {
                Some(crate::clip::Pending::Launch { slot, .. }) => slot as i32,
                Some(crate::clip::Pending::Stop { .. }) => crate::shared::STOP_QUEUED,
                None => NO_SLOT,
            },
            std::sync::atomic::Ordering::Relaxed,
        );
        shared.clip_position.store(self.player.position(time.t1()));
        shared
            .follow_arrangement
            .store(self.follow, std::sync::atomic::Ordering::Relaxed);
    }

    /// Fill what this track's sends will tap, before the mixer stage changes its audio.
    pub(crate) fn prepare_sends(&mut self, frames: usize, coeff: f32) {
        let s = &mut self.sends;
        if !s.wants_pre && !s.wants_post {
            s.gate.settle();
            s.mix.settle();
            return;
        }
        s.pre[0][..frames].copy_from_slice(&self.left[..frames]);
        s.pre[1][..frames].copy_from_slice(&self.right[..frames]);
        let [pre_l, pre_r] = &mut s.pre;
        s.gate.apply(pre_l, pre_r, frames, coeff);
        if s.wants_post {
            s.post[0][..frames].copy_from_slice(&s.pre[0][..frames]);
            s.post[1][..frames].copy_from_slice(&s.pre[1][..frames]);
            let [post_l, post_r] = &mut s.post;
            s.mix.apply(post_l, post_r, frames, coeff);
        } else {
            s.mix.settle();
        }
        if let Some(ring) = &mut s.feedback {
            let head = ring.head;
            if s.wants_pre {
                FeedbackRing::write(&mut ring.pre[0], head, &s.pre[0][..frames]);
                FeedbackRing::write(&mut ring.pre[1], head, &s.pre[1][..frames]);
            }
            if s.wants_post {
                FeedbackRing::write(&mut ring.post[0], head, &s.post[0][..frames]);
                FeedbackRing::write(&mut ring.post[1], head, &s.post[1][..frames]);
            }
            ring.head = (head + frames) % ring.pre[0].len().max(1);
        }
    }

    /// Mix one of this track's sends into `dst`.
    pub(crate) fn send_into(
        &mut self,
        input: &BusIn,
        dst: (&mut [f32], &mut [f32]),
        frames: usize,
        coeff: f32,
    ) {
        let Some(n) = input.gain.and_then(mix_cell::send_index) else {
            return;
        };
        let s = &mut self.sends;
        let level = &mut s.levels[n];
        let tap = match input.tap {
            Tap::PostFx => &s.pre,
            _ => &s.post,
        };
        if input.feedback {
            let Some(ring) = &s.feedback else { return };
            let ring_tap = match input.tap {
                Tap::PostFx => &ring.pre,
                _ => &ring.post,
            };
            let [sl, sr] = &mut self.scratch;
            FeedbackRing::read(&ring_tap[0], ring.head, frames, sl);
            FeedbackRing::read(&ring_tap[1], ring.head, frames, sr);
            level.add((&sl[..frames], &sr[..frames]), dst, frames, coeff);
        } else {
            let (sl, sr) = (&tap[0][..frames], &tap[1][..frames]);
            level.add((sl, sr), dst, frames, coeff);
        }
    }

    /// Mix a feedback send from this track into itself.
    pub(crate) fn feedback_into_self(&mut self, input: &BusIn, frames: usize, coeff: f32) {
        let Some(n) = input.gain.and_then(mix_cell::send_index) else {
            return;
        };
        let s = &mut self.sends;
        let Some(ring) = &s.feedback else { return };
        let ring_tap = match input.tap {
            Tap::PostFx => &ring.pre,
            _ => &ring.post,
        };
        let [sl, sr] = &mut self.scratch;
        FeedbackRing::read(&ring_tap[0], ring.head, frames, sl);
        FeedbackRing::read(&ring_tap[1], ring.head, frames, sr);
        s.levels[n].add(
            (&sl[..frames], &sr[..frames]),
            (&mut self.left[..frames], &mut self.right[..frames]),
            frames,
            coeff,
        );
    }

    /// Apply volume/pan (mute/solo resolved at the control steps) and update meters.
    pub(crate) fn apply_mix(&mut self, frames: usize, coeff: f32) {
        let left = &mut self.left[..frames];
        let right = &mut self.right[..frames];
        self.gain.apply(left, right, frames, coeff);
        self.shared.meter.update(left, right);
    }

    pub(crate) fn output(&self, frames: usize) -> (&[f32], &[f32]) {
        (&self.left[..frames], &self.right[..frames])
    }

    pub fn find_device(&mut self, id: DeviceId) -> Option<&mut DeviceSlot> {
        self.devices
            .iter_mut()
            .find(|d| d.id == id)
            .map(|b| &mut **b)
    }
}
