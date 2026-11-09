//! Redux: a jittered sample-and-hold, then a quantizer with a companding curve.
//!
//! From the manual: Rate, Jitter, the Pre and Post filters, the Post filter's octave offset from
//! half the Rate, Bits, Shape, DC Shift and Dry/Wet. The manual gives no ranges. Ours: the 30 Hz
//! to 48 kHz Rate range, with its top meaning no downsampling at all, the companding law (mu-law)
//! and the amount of jitter.

use super::common::DcBlock;
use crate::dsp::util::Rng;
use crate::dsp::{Biquad, BiquadKind};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    AudioBuffer, BlockStep, DeviceDesc, Item, ParamId, ParamInfo, ProcessContext, Processor,
    Section, Status,
};

pub const RATE: u32 = 0;
pub const JITTER: u32 = 1;
pub const PRE: u32 = 2;
pub const POST: u32 = 3;
pub const OCTAVE: u32 = 4;
pub const BITS: u32 = 5;
pub const SHAPE: u32 = 6;
pub const DC_SHIFT: u32 = 7;
pub const MIX: u32 = 8;

const PARAMS: usize = 9;

/// At this Rate, the downsampler is out of the signal.
pub const RATE_OFF: f64 = 48_000.0;

fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::skewed(RATE, "Rate", 30.0, RATE_OFF, RATE_OFF, "Hz", 3.0),
        ParamInfo::continuous(JITTER, "Jitter", 0.0, 1.0, 0.0, ""),
        ParamInfo::toggle(PRE, "Pre-Filter", false),
        ParamInfo::toggle(POST, "Post-Filter", false),
        ParamInfo::continuous(OCTAVE, "Post-Filter Octave", -2.0, 2.0, 0.0, "oct"),
        ParamInfo::continuous(BITS, "Bits", 1.0, 16.0, 16.0, ""),
        ParamInfo::continuous(SHAPE, "Shape", 0.0, 1.0, 0.0, ""),
        ParamInfo::toggle(DC_SHIFT, "DC Shift", false),
        ParamInfo::continuous(MIX, "Dry/Wet", 0.0, 1.0, 1.0, ""),
    ]
}

fn layout() -> Vec<Section> {
    let knob = |id| Item::Knob(ParamId(id));
    let toggle = |id| Item::Toggle(ParamId(id));
    vec![
        Section {
            title: "Downsample",
            items: vec![
                knob(RATE),
                knob(JITTER),
                toggle(PRE),
                toggle(POST),
                knob(OCTAVE),
            ],
        },
        Section {
            title: "Bits",
            items: vec![knob(BITS), knob(SHAPE), toggle(DC_SHIFT), knob(MIX)],
        },
    ]
}

/// The compander for a Shape of `shape`: `None` when the quantizer is linear.
#[inline]
fn mu_for(shape: f32) -> Option<f32> {
    (shape > 0.0).then_some(shape * shape * 255.0)
}

#[inline]
fn compress(x: f32, mu: f32) -> f32 {
    x.signum() * (1.0 + mu * x.abs()).ln() / (1.0 + mu).ln()
}

#[inline]
fn expand(y: f32, mu: f32) -> f32 {
    y.signum() * ((1.0 + mu).powf(y.abs()) - 1.0) / mu
}

/// The quantizer: `bits` of resolution with a `shape` compander, optionally with DC Shift's half
/// step of offset first. At higher Shape, small values land on finer steps than large ones.
#[inline]
pub fn quantize(x: f32, bits: f32, shape: f32, dc_shift: bool) -> f32 {
    let levels = 2f32.powf(bits.clamp(1.0, 16.0) - 1.0);
    let offset = if dc_shift { 0.5 / levels } else { 0.0 };
    let x = x.clamp(-1.0, 1.0);
    match mu_for(shape) {
        Some(mu) => {
            let c = compress(x, mu);
            expand(
                (((c + offset) * levels).round() / levels).clamp(-1.0, 1.0),
                mu,
            )
        }
        None => (((x + offset) * levels).round() / levels).clamp(-1.0, 1.0),
    }
}

#[derive(Clone, Copy)]
struct Chan {
    phase: f32,
    held: f32,
    pre: [Biquad; 2],
    post: [Biquad; 2],
    dc: DcBlock,
    rng: Rng,
}

impl Chan {
    fn new(seed: u32) -> Self {
        Self {
            // Latches the very first sample.
            phase: 1.0,
            held: 0.0,
            pre: [Biquad::default(); 2],
            post: [Biquad::default(); 2],
            dc: DcBlock::default(),
            rng: Rng::new(seed),
        }
    }
}

pub struct Redux {
    v: [f64; PARAMS],
    sr: f32,
    chans: [Chan; 2],
    /// The rate and octave the filters are tuned to.
    tuned: (f32, f32),
}

impl Default for Redux {
    fn default() -> Self {
        let mut v = [0.0; PARAMS];
        for p in params() {
            v[p.id as usize] = p.default;
        }
        Self {
            v,
            sr: 48_000.0,
            chans: [Chan::new(1), Chan::new(2)],
            tuned: (f32::NAN, 0.0),
        }
    }
}

impl Redux {
    fn on(&self, id: u32) -> bool {
        self.v[id as usize] >= 0.5
    }

    fn retune(&mut self) {
        let rate = self.v[RATE as usize] as f32;
        let octave = self.v[OCTAVE as usize] as f32;
        if (rate, octave) == self.tuned {
            return;
        }
        self.tuned = (rate, octave);
        let nyquist = self.sr * 0.49;
        let pre = (rate * 0.5).min(nyquist);
        let post = (rate * 0.5 * 2f32.powf(octave)).clamp(10.0, nyquist);
        for chan in &mut self.chans {
            for f in &mut chan.pre {
                f.set(BiquadKind::LowPass, pre, 0.707, 0.0, self.sr);
            }
            for f in &mut chan.post {
                f.set(BiquadKind::LowPass, post, 0.707, 0.0, self.sr);
            }
        }
    }

    fn render(&mut self, audio: AudioBuffer) {
        self.retune();
        let rate = self.v[RATE as usize] as f32;
        let bypass_rate = self.v[RATE as usize] >= RATE_OFF || rate >= self.sr;
        let step = rate / self.sr;
        let jitter = self.v[JITTER as usize] as f32;
        let (pre, post, dc_shift) = (self.on(PRE), self.on(POST), self.on(DC_SHIFT));
        let bits = self.v[BITS as usize] as f32;
        let shape = self.v[SHAPE as usize] as f32;
        let mix = self.v[MIX as usize] as f32;
        let [cl, cr] = &mut self.chans;
        for (chan, block) in [(cl, &mut *audio.left), (cr, &mut *audio.right)] {
            for s in block.iter_mut() {
                let dry = *s;
                let mut x = dry;
                if pre {
                    let first = chan.pre[0].process(x);
                    x = chan.pre[1].process(first);
                }
                if bypass_rate {
                    chan.held = x;
                } else {
                    chan.phase += step * (1.0 + jitter * 0.9 * chan.rng.bipolar());
                    if chan.phase >= 1.0 {
                        chan.phase = (chan.phase - 1.0).fract();
                        chan.held = x;
                    }
                }
                let mut y = quantize(chan.held, bits, shape, dc_shift);
                if post {
                    let first = chan.post[0].process(y);
                    y = chan.post[1].process(first);
                }
                if dc_shift {
                    // The offset changes the sound; it shouldn't leave the device as DC.
                    y = chan.dc.process(y);
                }
                *s = dry * (1.0 - mix) + y * mix;
            }
        }
    }
}

impl Processor for Redux {
    fn sleeps_when_silent(&self) -> bool {
        true
    }

    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sr = sample_rate as f32;
        for chan in &mut self.chans {
            chan.dc.set(10.0, self.sr);
        }
        self.tuned = (f32::NAN, 0.0);
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => self.set_param(id, value),
            BlockStep::Audio(audio) => self.render(audio),
            BlockStep::Midi(_) => {}
        });
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
        }
    }

    fn reset(&mut self) {
        for chan in &mut self.chans {
            chan.phase = 1.0;
            chan.held = 0.0;
            chan.pre
                .iter_mut()
                .chain(chan.post.iter_mut())
                .for_each(Biquad::reset);
            chan.dc.reset();
        }
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.redux",
    name: "Redux",
    role: DeviceRole::AudioEffect,
    category: "Distortion",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::<Redux>::default(),
};
