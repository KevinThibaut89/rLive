//! The Gain, Mix and Mute of a plug-in's sidechain input (manual p. 485).

use rlive_core::param::plugin_sidechain as sc;
use rlive_core::{
    db_to_gain, AudioInput, ParamEvent, ParamInfo, ProcessContext, Processor, Resource, Tail,
};

/// The controls of a plug-in's sidechain block, as parameters of the device.
pub fn sidechain_params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::continuous(sc::GAIN.0, "SC Gain", -24.0, 24.0, 0.0, "dB"),
        ParamInfo::continuous(sc::MIX.0, "SC Mix", 0.0, 100.0, 100.0, "%"),
        ParamInfo::toggle(sc::MUTE.0, "SC Mute", false),
    ]
}

/// Wraps a plug-in with a sidechain input, turning what is routed to it into what it hears.
///
/// * Gain sets the level of the key. It's a trigger, so it is never heard.
/// * Mix blends the key with the device's own input: at 0% the key isn't used at all.
/// * Mute leaves the plug-in with no key.
///
/// With everything at its default the key goes through untouched.
pub struct KeyedProcessor {
    inner: Box<dyn Processor>,
    gain: f32,
    mix: f32,
    mute: bool,
    left: Vec<f32>,
    right: Vec<f32>,
    /// This block's events minus the controls above.
    forwarded: Vec<ParamEvent>,
}

impl KeyedProcessor {
    pub fn new(inner: Box<dyn Processor>, max_block: usize) -> Self {
        Self {
            inner,
            gain: 1.0,
            mix: 1.0,
            mute: false,
            left: vec![0.0; max_block],
            right: vec![0.0; max_block],
            forwarded: Vec::with_capacity(2048),
        }
    }

    fn set(&mut self, id: u32, value: f64) -> bool {
        match id {
            i if i == sc::GAIN.0 => self.gain = db_to_gain(value as f32),
            i if i == sc::MIX.0 => self.mix = (value as f32 / 100.0).clamp(0.0, 1.0),
            i if i == sc::MUTE.0 => self.mute = value >= 0.5,
            _ => return false,
        }
        true
    }

    fn untouched(&self) -> bool {
        self.gain == 1.0 && self.mix == 1.0 && !self.mute
    }
}

impl Processor for KeyedProcessor {
    fn activate(&mut self, sample_rate: f64, max_block: usize) {
        self.inner.activate(sample_rate, max_block);
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        self.forwarded.clear();
        for e in ctx.params {
            if !self.set(e.id.0, e.value) && self.forwarded.len() < self.forwarded.capacity() {
                self.forwarded.push(*e);
            }
        }
        let (params, sidechain) = (ctx.params, ctx.sidechain);
        // SAFETY: both only live in `ctx` for the call below, and are put back after it. Nothing
        // touches `forwarded`, `left` or `right` until then.
        unsafe {
            ctx.params = std::slice::from_raw_parts(self.forwarded.as_ptr(), self.forwarded.len());
            ctx.sidechain = match sidechain {
                Some(sc) if self.untouched() => Some(sc),
                Some(_) if self.mute => None,
                Some(sc) => {
                    let n = sc.frames().min(ctx.audio.frames()).min(self.left.len());
                    let (gain, mix) = (self.gain, self.mix);
                    for i in 0..n {
                        self.left[i] = sc.left[i] * gain * mix + ctx.audio.left[i] * (1.0 - mix);
                        self.right[i] = sc.right[i] * gain * mix + ctx.audio.right[i] * (1.0 - mix);
                    }
                    Some(AudioInput {
                        left: std::slice::from_raw_parts(self.left.as_ptr(), n),
                        right: std::slice::from_raw_parts(self.right.as_ptr(), n),
                    })
                }
                None => None,
            };
        }
        self.inner.process(ctx);
        ctx.params = params;
        ctx.sidechain = sidechain;
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if !self.set(id, value) {
            self.inner.set_param(id, value);
        }
    }

    fn stop_processing(&mut self) {
        self.inner.stop_processing();
    }

    fn set_aux_routed(&mut self, mask: u32) {
        self.inner.set_aux_routed(mask);
    }

    fn reset(&mut self) {
        self.inner.reset();
    }

    fn latency(&self) -> u32 {
        self.inner.latency()
    }

    fn sleeps_when_silent(&self) -> bool {
        self.inner.sleeps_when_silent()
    }

    fn thread_policy(&self) -> rlive_core::ThreadPolicy {
        self.inner.thread_policy()
    }

    fn tail(&self) -> Tail {
        self.inner.tail()
    }

    fn set_resource(&mut self, slot: u32, resource: Resource) -> Option<Resource> {
        self.inner.set_resource(slot, resource)
    }
}
