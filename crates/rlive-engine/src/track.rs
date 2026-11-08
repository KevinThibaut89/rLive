//! Tracks, device chains and the mixer stage.
#![allow(clippy::vec_box)]

use crate::clip::{BlockTime, ClipData, ClipPlayer, Pending};
use crate::lane::{ArrangementLane, LanePlayer};
use crate::shared::{PeakMeter, TrackShared, NO_SLOT};
use rlive_core::{
    db_to_gain, AudioBuffer, DeviceId, MidiEvent, ParamOutQueue, ProcessContext, Processor,
    TrackId, TransportInfo,
};
use std::sync::Arc;

pub const MAX_DEVICES: usize = 64;
pub const MAX_SCENES: usize = 256;
pub const MAX_EVENTS: usize = 2048;

/// A processor in a device chain.
pub struct DeviceSlot {
    pub id: DeviceId,
    pub processor: Box<dyn Processor>,
    pub bypass: bool,
}

impl DeviceSlot {
    pub fn new(id: DeviceId, processor: Box<dyn Processor>) -> Self {
        Self {
            id,
            processor,
            bypass: false,
        }
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

/// Run a device chain over a buffer.
pub(crate) fn run_chain(
    devices: &mut [Box<DeviceSlot>],
    left: &mut [f32],
    right: &mut [f32],
    events: &[MidiEvent],
    transport: &TransportInfo,
    param_out: &mut ParamOutQueue,
    mut on_param: impl FnMut(DeviceId, u32, f64),
) {
    for dev in devices.iter_mut() {
        if dev.bypass {
            continue;
        }
        let mut ctx = ProcessContext {
            audio: AudioBuffer {
                left: &mut *left,
                right: &mut *right,
            },
            events,
            transport,
            param_out: &mut *param_out,
        };
        dev.processor.process(&mut ctx);
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

    pub(crate) player: ClipPlayer,
    pub(crate) lane: LanePlayer,
    /// Playing the lane. A launch turns this off until `BackToArrangement`.
    pub(crate) follow: bool,
    pub(crate) left: Vec<f32>,
    pub(crate) right: Vec<f32>,
    pub(crate) events: Vec<MidiEvent>,
    pub(crate) live: Vec<MidiEvent>,
    pub(crate) param_out: ParamOutQueue,
    smoother: GainSmoother,
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
            player: ClipPlayer::new(),
            lane: LanePlayer::new(Arc::new(ArrangementLane::default())),
            follow: true,
            left: vec![0.0; max_block],
            right: vec![0.0; max_block],
            events: Vec::with_capacity(MAX_EVENTS),
            live: Vec::with_capacity(256),
            param_out: ParamOutQueue::with_capacity(256),
            smoother: GainSmoother::new(),
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

    pub(crate) fn render(
        &mut self,
        time: BlockTime,
        transport: &TransportInfo,
        retire: &mut dyn FnMut(Arc<ClipData>),
        on_param: &mut dyn FnMut(TrackId, DeviceId, u32, f64),
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

        let id = self.id;
        run_chain(
            &mut self.devices,
            left,
            right,
            &self.events,
            transport,
            &mut self.param_out,
            |dev, p, v| on_param(id, dev, p, v),
        );

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

    /// Apply volume/pan (mute/solo resolved by the caller) and update meters.
    pub(crate) fn apply_mix(&mut self, frames: usize, audible: bool, coeff: f32) {
        let target = if audible {
            self.mix.gains()
        } else {
            (0.0, 0.0)
        };
        let left = &mut self.left[..frames];
        let right = &mut self.right[..frames];
        self.smoother.apply(left, right, target, coeff);
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
    pub(crate) left: Vec<f32>,
    pub(crate) right: Vec<f32>,
    pub(crate) param_out: ParamOutQueue,
    smoother: GainSmoother,
}

impl MasterNode {
    pub fn new(max_block: usize) -> Self {
        Self {
            devices: Vec::with_capacity(MAX_DEVICES),
            volume_db: 0.0,
            left: vec![0.0; max_block],
            right: vec![0.0; max_block],
            param_out: ParamOutQueue::with_capacity(256),
            smoother: GainSmoother::new(),
        }
    }

    pub(crate) fn finish(
        &mut self,
        frames: usize,
        transport: &TransportInfo,
        meter: &PeakMeter,
        coeff: f32,
        on_param: &mut dyn FnMut(DeviceId, u32, f64),
    ) {
        let left = &mut self.left[..frames];
        let right = &mut self.right[..frames];
        run_chain(
            &mut self.devices,
            left,
            right,
            &[],
            transport,
            &mut self.param_out,
            on_param,
        );
        let g = db_to_gain(self.volume_db);
        self.smoother.apply(left, right, (g, g), coeff);
        meter.update(left, right);
    }
}
