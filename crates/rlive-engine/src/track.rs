//! Tracks, device chains and the mixer stage.
#![allow(clippy::vec_box)]

use crate::clip::{BlockTime, ClipData, ClipPlayer, Pending};
use crate::lane::{ArrangementLane, LanePlayer};
use crate::params::{
    BusLayout, CellTarget, ControlHooks, ControlStep, ParamBus, TrackRoutes, CONTROL_STEP,
};
use crate::shared::{PeakMeter, TrackShared, NO_SLOT};
use rlive_core::param::{mixer, ParamId};
use rlive_core::{
    db_to_gain, AudioBuffer, DeviceId, EventQueue, MidiEvent, ParamEvent, ParamOutQueue,
    ProcessContext, Processor, TrackId, TransportInfo, ViewChannel, ViewWriter,
};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

pub const MAX_DEVICES: usize = 64;
pub const MAX_SCENES: usize = 256;
pub const MAX_EVENTS: usize = 2048;

/// Room in a device's parameter queue when nothing says how much it needs.
const DEFAULT_PARAM_EVENTS: usize = 1024;
/// A device's parameter queue never needs more than this; past it, changes get merged.
const MAX_PARAM_EVENTS: usize = 8192;

/// One device's parameter changes, in block order, waiting for the device to run.
///
/// Fixed capacity. When it fills up, only the newest change of each parameter is kept, so a burst
/// can blur the timing of a change but can't lose its final value.
struct ParamQueue {
    items: Vec<ParamEvent>,
    /// Events already handed to the device this block.
    taken: usize,
}

impl ParamQueue {
    fn with_capacity(cap: usize) -> Self {
        Self {
            items: Vec::with_capacity(cap),
            taken: 0,
        }
    }

    /// Queue a change. Offsets must not decrease within a block.
    fn push(&mut self, event: ParamEvent) {
        if self.items.len() == self.items.capacity() {
            self.compact();
        }
        if self.items.len() < self.items.capacity() {
            self.items.push(event);
        }
    }

    fn compact(&mut self) {
        self.items.drain(..self.taken);
        self.taken = 0;
        self.items.sort_unstable_by_key(|e| (e.id, e.offset));
        let n = self.items.len();
        let mut kept = 0;
        for i in 0..n {
            if i + 1 == n || self.items[i + 1].id != self.items[i].id {
                self.items[kept] = self.items[i];
                kept += 1;
            }
        }
        self.items.truncate(kept);
        self.items.sort_unstable_by_key(|e| e.offset);
    }

    /// The changes due before `to`, with offsets relative to `from`, marked as handed over.
    /// Anything held back from an earlier stretch arrives at the start of this one.
    fn take_segment(&mut self, from: u32, to: u32) -> &[ParamEvent] {
        let start = self.taken;
        let end = start + self.items[start..].partition_point(|e| e.offset < to);
        if from > 0 {
            for e in &mut self.items[start..end] {
                e.offset = e.offset.saturating_sub(from);
            }
        }
        self.taken = end;
        &self.items[start..end]
    }

    /// Forget what the device heard. A bypassed device hears the rest at its next block.
    fn end_block(&mut self) {
        self.items.drain(..self.taken);
        self.taken = 0;
        for e in &mut self.items {
            e.offset = 0;
        }
    }
}

/// A processor in a device chain.
pub struct DeviceSlot {
    pub id: DeviceId,
    pub processor: Box<dyn Processor>,
    pub bypass: bool,
    /// The device replaces the MIDI stream with its `events_out`, for the devices after it.
    pub midi_effect: bool,
    /// The device's latency in frames, shared with the UI.
    pub latency: Arc<AtomicU32>,
    /// One value per declared modulation source. Holds its value between blocks.
    pub mod_out: Box<[f32]>,
    /// Where the device's view data goes. `None` for a device with nothing to show.
    pub view: Option<Arc<ViewChannel>>,
    params: ParamQueue,
}

impl DeviceSlot {
    pub fn new(id: DeviceId, processor: Box<dyn Processor>) -> Self {
        Self {
            id,
            processor,
            bypass: false,
            midi_effect: false,
            latency: Arc::new(AtomicU32::new(0)),
            mod_out: Box::default(),
            view: None,
            params: ParamQueue::with_capacity(DEFAULT_PARAM_EVENTS),
        }
    }

    /// Size the parameter queue for a device with `params` parameters.
    pub fn with_param_capacity(mut self, params: usize, max_block: usize) -> Self {
        let cap = params.max(8) * (max_block / CONTROL_STEP + 1);
        self.params = ParamQueue::with_capacity(cap.clamp(DEFAULT_PARAM_EVENTS, MAX_PARAM_EVENTS));
        self
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
}

/// Timing of the block being rendered, for chain segments.
#[derive(Clone, Copy)]
pub(crate) struct BlockCtx<'a> {
    pub transport: &'a TransportInfo,
    pub ticks_per_sample: f64,
    pub frames: usize,
}

impl BlockCtx<'_> {
    /// The transport as it stands `offset` frames into the block.
    fn at(&self, offset: usize) -> TransportInfo {
        let mut t = *self.transport;
        if t.playing {
            t.tick += offset as f64 * self.ticks_per_sample;
            t.sample_pos += offset as u64;
            t.bar_start = t.time_signature.bar_start(t.tick);
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

/// Where a resolved value reaches a device. The only place that hands parameter changes to a
/// processor: a sample-timed event for the device to hear at `offset`, or the engine's own switch.
fn deliver_to_device(device: &mut DeviceSlot, target: CellTarget, plain: f64, offset: u32) {
    match target {
        CellTarget::Activator { .. } => {
            let bypass = plain < 0.5;
            if bypass != device.bypass {
                device.bypass = bypass;
                if bypass {
                    device.processor.reset();
                }
            }
        }
        CellTarget::Device { param, .. } => device.params.push(ParamEvent {
            offset,
            id: param,
            value: plain,
        }),
        _ => {}
    }
}

/// Finds the device a resolved value belongs to in its chain and delivers it.
fn deliver_to_chain(devices: &mut [Box<DeviceSlot>], target: CellTarget, plain: f64, offset: u32) {
    let (CellTarget::Activator { device } | CellTarget::Device { device, .. }) = target else {
        return;
    };
    if let Some(d) = devices.iter_mut().find(|d| d.id == device) {
        deliver_to_device(d, target, plain, offset);
    }
}

/// One stretch of a block running through a chain.
struct ChainRun<'a> {
    left: &'a mut [f32],
    right: &'a mut [f32],
    /// The chain's MIDI for this stretch.
    events: &'a [MidiEvent],
    /// Holds what MIDI effects pass along.
    midi: &'a mut [EventQueue; 2],
    transport: &'a TransportInfo,
    param_out: &'a mut ParamOutQueue,
    /// The stretch in frames from the start of the block.
    from: u32,
    to: u32,
}

/// Run a device chain over a buffer.
fn run_chain(
    devices: &mut [Box<DeviceSlot>],
    run: ChainRun<'_>,
    mut on_param: impl FnMut(DeviceId, u32, f64),
) {
    let ChainRun {
        left,
        right,
        events,
        midi,
        transport,
        param_out,
        from,
        to,
    } = run;
    // Which queue holds the MIDI, once a MIDI effect has replaced `events`.
    let mut input = None;
    for dev in devices.iter_mut() {
        let dev = &mut **dev;
        if dev.bypass {
            continue;
        }
        let [a, b] = &mut *midi;
        let (held, out) = if input == Some(0) { (a, b) } else { (b, a) };
        out.clear();
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
            events: match input {
                Some(_) => held.as_slice(),
                None => events,
            },
            events_out: &mut *out,
            params: dev.params.take_segment(from, to),
            transport,
            param_out: &mut *param_out,
            mod_out: &mut dev.mod_out,
            view: &view,
        };
        dev.processor.process(&mut ctx);
        dev.latency
            .store(dev.processor.latency(), Ordering::Relaxed);
        if dev.midi_effect {
            out.sort();
            input = Some(if input == Some(0) { 1 } else { 0 });
        }
        for (id, v) in param_out.drain() {
            on_param(dev.id, id, v);
        }
    }
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
    /// Set by `ParamCmd::SetRoutes`; nothing reads it yet.
    pub routes: Arc<TrackRoutes>,
    /// Writes the bus at each control step. Unused until automation and modulators exist.
    pub hooks: Option<Box<dyn ControlHooks>>,

    pub(crate) player: ClipPlayer,
    pub(crate) lane: LanePlayer,
    /// Playing the lane. A launch turns this off until `BackToArrangement`.
    pub(crate) follow: bool,
    pub(crate) left: Vec<f32>,
    pub(crate) right: Vec<f32>,
    pub(crate) events: Vec<MidiEvent>,
    pub(crate) live: Vec<MidiEvent>,
    pub(crate) param_out: ParamOutQueue,
    midi: [EventQueue; 2],
    cursor: ChainCursor,
    gain: GainStage,
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
        }
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
                self.lane
                    .render(time, time.t0, until, &mut self.events, left, right);
                if let Some(at) = launch_at {
                    self.lane
                        .release_and_resync(time.event_offset(at), &mut self.events);
                    self.follow = false;
                }
            }
            self.player
                .render(time, &mut self.events, left, right, retire, &self.slots);
        }
        self.events.sort_unstable_by_key(|e| e.sort_key());
        self.cursor = ChainCursor::default();
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
        let bus = &mut self.bus;
        run_chain(
            &mut self.devices,
            ChainRun {
                left: &mut self.left[from..to],
                right: &mut self.right[from..to],
                events: segment,
                midi: &mut self.midi,
                transport: &transport,
                param_out: &mut self.param_out,
                from: from as u32,
                to: to as u32,
            },
            |dev, p, v| {
                bus.report_from_device(dev, p, v);
                on_param(id, dev, p, v);
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
        if let Some(hooks) = self.hooks.as_deref_mut() {
            let step = ControlStep {
                index,
                offset: offset as u32,
                tick: ctx.at(offset).tick,
            };
            hooks.macros(step, &mut self.bus);
            hooks.automation(step, &mut self.bus);
            hooks.modulation(step, &mut self.bus);
        }
        let resolved = self.bus.resolve();
        if resolved.activators {
            self.render_to(offset, ctx, on_param);
        }
        let (devices, mix) = (&mut self.devices, &mut self.mix);
        self.bus.drain_resolved(|target, plain| match target {
            CellTarget::Mixer(control) => mix.apply(control, plain),
            target => deliver_to_chain(devices, target, plain, offset as u32),
        });
        self.gain.dirty |= resolved.mixer;
    }

    /// Set the gain target from the mixer, at this step, when something affecting it changed.
    pub(crate) fn update_mix(&mut self, offset: usize, any_solo: bool, solo_changed: bool) {
        if !self.gain.dirty && !solo_changed {
            return;
        }
        let audible = !self.mix.mute && (!any_solo || self.mix.solo);
        let target = if audible {
            self.mix.gains()
        } else {
            (0.0, 0.0)
        };
        self.gain.set_target(offset, target);
    }

    /// Run what is left of the chain and publish the clip state.
    pub(crate) fn finish_block(
        &mut self,
        time: BlockTime,
        ctx: &BlockCtx,
        on_param: &mut dyn FnMut(TrackId, DeviceId, u32, f64),
    ) {
        self.render_to(time.frames, ctx, on_param);
        for d in self.devices.iter_mut() {
            d.params.end_block();
        }

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

/// The master bus.
pub struct MasterNode {
    pub devices: Vec<Box<DeviceSlot>>,
    pub volume_db: f32,
    /// Every parameter of the Master. Devices must be placed in the layout it was built from.
    pub bus: Box<ParamBus>,
    /// Set by `ParamCmd::SetRoutes`; nothing reads it yet.
    pub routes: Arc<TrackRoutes>,
    /// Writes the bus at each control step. Unused until automation and modulators exist.
    pub hooks: Option<Box<dyn ControlHooks>>,
    pub(crate) left: Vec<f32>,
    pub(crate) right: Vec<f32>,
    pub(crate) param_out: ParamOutQueue,
    midi: [EventQueue; 2],
    cursor: ChainCursor,
    gain: GainStage,
}

impl MasterNode {
    pub fn new(max_block: usize) -> Self {
        Self {
            devices: Vec::with_capacity(MAX_DEVICES),
            volume_db: 0.0,
            bus: BusLayout::for_master(0.0).build(),
            routes: Arc::new(TrackRoutes::default()),
            hooks: None,
            left: vec![0.0; max_block],
            right: vec![0.0; max_block],
            param_out: ParamOutQueue::with_capacity(256),
            midi: [EventQueue::with_capacity(0), EventQueue::with_capacity(0)],
            cursor: ChainCursor::default(),
            gain: GainStage::new(max_block),
        }
    }

    fn render_to(
        &mut self,
        to: usize,
        ctx: &BlockCtx,
        on_param: &mut dyn FnMut(DeviceId, u32, f64),
    ) {
        let from = self.cursor.frames;
        if to <= from {
            return;
        }
        let transport = ctx.at(from);
        let bus = &mut self.bus;
        run_chain(
            &mut self.devices,
            ChainRun {
                left: &mut self.left[from..to],
                right: &mut self.right[from..to],
                events: &[],
                midi: &mut self.midi,
                transport: &transport,
                param_out: &mut self.param_out,
                from: from as u32,
                to: to as u32,
            },
            |dev, p, v| {
                bus.report_from_device(dev, p, v);
                on_param(dev, p, v);
            },
        );
        self.cursor.frames = to;
    }

    /// Resolve the Master's parameters at a step, once its input is ready.
    pub(crate) fn control_step(
        &mut self,
        index: u32,
        offset: usize,
        ctx: &BlockCtx,
        on_param: &mut dyn FnMut(DeviceId, u32, f64),
    ) {
        if let Some(hooks) = self.hooks.as_deref_mut() {
            let step = ControlStep {
                index,
                offset: offset as u32,
                tick: ctx.at(offset).tick,
            };
            hooks.macros(step, &mut self.bus);
            hooks.automation(step, &mut self.bus);
            hooks.modulation(step, &mut self.bus);
        }
        let resolved = self.bus.resolve();
        if resolved.activators {
            self.render_to(offset, ctx, on_param);
        }
        let (devices, volume) = (&mut self.devices, &mut self.volume_db);
        self.bus.drain_resolved(|target, plain| match target {
            CellTarget::Mixer(mixer::VOLUME) => *volume = plain as f32,
            target => deliver_to_chain(devices, target, plain, offset as u32),
        });
        if resolved.mixer || self.gain.dirty {
            let g = db_to_gain(self.volume_db);
            self.gain.set_target(offset, (g, g));
        }
    }

    pub(crate) fn begin_block(&mut self) {
        self.cursor = ChainCursor::default();
    }

    pub(crate) fn finish(
        &mut self,
        ctx: &BlockCtx,
        meter: &PeakMeter,
        coeff: f32,
        on_param: &mut dyn FnMut(DeviceId, u32, f64),
    ) {
        let frames = ctx.frames;
        self.render_to(frames, ctx, on_param);
        for d in self.devices.iter_mut() {
            d.params.end_block();
        }
        let left = &mut self.left[..frames];
        let right = &mut self.right[..frames];
        self.gain.apply(left, right, frames, coeff);
        meter.update(left, right);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(offset: u32, id: u32, value: f64) -> ParamEvent {
        ParamEvent {
            offset,
            id: ParamId(id),
            value,
        }
    }

    #[test]
    fn a_full_queue_keeps_the_newest_value_of_every_parameter() {
        let mut q = ParamQueue::with_capacity(4);
        q.push(event(0, 1, 0.1));
        q.push(event(0, 2, 0.2));
        q.push(event(32, 1, 0.3));
        q.push(event(32, 2, 0.4));
        q.push(event(64, 3, 0.5));
        assert_eq!(
            q.take_segment(0, 128),
            [event(32, 1, 0.3), event(32, 2, 0.4), event(64, 3, 0.5)]
        );
    }

    #[test]
    fn segments_get_their_own_offsets() {
        let mut q = ParamQueue::with_capacity(8);
        q.push(event(32, 1, 0.1));
        q.push(event(96, 1, 0.2));
        assert_eq!(q.take_segment(0, 64), [event(32, 1, 0.1)]);
        assert_eq!(q.take_segment(64, 128), [event(32, 1, 0.2)]);
        q.end_block();
        assert_eq!(q.take_segment(0, 128), []);
    }

    #[test]
    fn what_a_bypassed_device_missed_waits_for_its_next_block() {
        let mut q = ParamQueue::with_capacity(8);
        q.push(event(32, 1, 0.1));
        q.end_block();
        q.push(event(64, 1, 0.2));
        q.end_block();
        assert_eq!(
            q.take_segment(0, 256),
            [event(0, 1, 0.1), event(0, 1, 0.2)],
            "held back and delivered at the start, in order"
        );
    }

    #[test]
    fn compacting_and_taking_do_not_allocate() {
        let mut q = ParamQueue::with_capacity(16);
        rlive_testkit::assert_no_alloc(|| {
            for i in 0..1000u32 {
                q.push(event(i, i % 3, f64::from(i)));
            }
            let taken = q.take_segment(0, 1000).len();
            std::hint::black_box(taken);
            q.end_block();
        });
    }
}
