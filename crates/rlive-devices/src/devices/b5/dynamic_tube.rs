//! Dynamic Tube: three tube curves, a Bias that an envelope follower can push around, and a
//! Tone control that only colours the distortion.
//!
//! From the manual: tubes A, B and C and how each behaves, Tone, Drive, Bias, Envelope (positive
//! and negative), Attack, Release, Output and Hi-Quality. The manual gives no numbers, so the
//! curves and every range are ours.

use super::common::{DcBlock, HI_QUALITY_FACTOR};
use crate::dsp::follower::EnvelopeFollower;
use crate::dsp::oversample::Oversampler;
use crate::dsp::{Biquad, BiquadKind};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    db_to_gain, AudioBuffer, BlockStep, DeviceDesc, Item, ParamId, ParamInfo, ProcessContext,
    Processor, Section, Status,
};

pub const TUBE: u32 = 0;
pub const DRIVE: u32 = 1;
pub const BIAS: u32 = 2;
pub const TONE: u32 = 3;
pub const ENVELOPE: u32 = 4;
pub const ATTACK: u32 = 5;
pub const RELEASE: u32 = 6;
pub const OUTPUT: u32 = 7;
pub const HI_QUALITY: u32 = 8;

const PARAMS: usize = 9;

/// Ours: where Tone's tilt pivots and how far it reaches.
pub const TONE_HZ: f32 = 1_200.0;
pub const TONE_RANGE_DB: f32 = 15.0;

fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::stepped(TUBE, "Tube", &["A", "B", "C"], 0),
        ParamInfo::continuous(DRIVE, "Drive", -12.0, 36.0, 0.0, "dB"),
        ParamInfo::continuous(BIAS, "Bias", 0.0, 1.0, 0.2, ""),
        ParamInfo::continuous(TONE, "Tone", -1.0, 1.0, 0.0, ""),
        ParamInfo::continuous(ENVELOPE, "Envelope", -1.0, 1.0, 0.0, ""),
        ParamInfo::skewed(ATTACK, "Attack", 0.1, 100.0, 5.0, "ms", 2.5),
        ParamInfo::skewed(RELEASE, "Release", 10.0, 1_000.0, 100.0, "ms", 2.5),
        ParamInfo::continuous(OUTPUT, "Output", -36.0, 12.0, 0.0, "dB"),
        ParamInfo::toggle(HI_QUALITY, "Hi-Quality", false),
    ]
}

fn layout() -> Vec<Section> {
    let knob = |id| Item::Knob(ParamId(id));
    vec![
        Section {
            title: "Tube",
            items: vec![
                Item::Menu(ParamId(TUBE)),
                knob(DRIVE),
                knob(BIAS),
                knob(TONE),
            ],
        },
        Section {
            title: "Envelope",
            items: vec![knob(ENVELOPE), knob(ATTACK), knob(RELEASE)],
        },
        Section {
            title: "Output",
            items: vec![knob(OUTPUT), Item::Toggle(ParamId(HI_QUALITY))],
        },
    ]
}

/// Tube A's curve: exactly linear below its threshold, then a smooth clip.
#[inline]
fn knee(v: f32, threshold: f32) -> f32 {
    let t = threshold.clamp(0.05, 1.0);
    let a = v.abs();
    if a <= t {
        v
    } else {
        v.signum() * (t + (1.0 - t + 0.05) * ((a - t) / (1.0 - t + 0.05)).tanh())
    }
}

/// Tube C's curve: distorts everywhere, with more bite on one side.
#[inline]
fn always(v: f32) -> f32 {
    (1.6 * v + 0.6 * v * v).tanh() * 0.8
}

/// The transfer curve of tube `tube` (0 A, 1 B, 2 C) at an operating point `bias`, with the
/// resting output taken off so the bias itself isn't a DC step.
#[inline]
pub fn tube_curve(v: f32, tube: f32, bias: f32) -> f32 {
    let bias = bias.clamp(0.0, 1.5);
    let threshold = 1.0 - 0.85 * bias.min(1.0);
    let offset = bias * 0.6;
    let shape = |u: f32| {
        let a = knee(u, threshold);
        let c = always(u);
        a + (c - a) * (tube * 0.5)
    };
    shape(v + offset) - shape(offset)
}

#[derive(Clone)]
struct Chan {
    follower: EnvelopeFollower,
    os: Oversampler,
    tone_low: Biquad,
    tone_high: Biquad,
    dc: DcBlock,
}

impl Chan {
    fn new(max_block: usize) -> Self {
        Self {
            follower: EnvelopeFollower::new(),
            os: Oversampler::new(HI_QUALITY_FACTOR, max_block),
            tone_low: Biquad::default(),
            tone_high: Biquad::default(),
            dc: DcBlock::default(),
        }
    }
}

pub struct DynamicTube {
    v: [f64; PARAMS],
    sr: f32,
    max_block: usize,
    chans: [Chan; 2],
    tuned_tone: f32,
    tuned_rate: f32,
    latency: u32,
}

impl Default for DynamicTube {
    fn default() -> Self {
        let mut v = [0.0; PARAMS];
        for p in params() {
            v[p.id as usize] = p.default;
        }
        Self {
            v,
            sr: 48_000.0,
            max_block: 512,
            chans: [Chan::new(512), Chan::new(512)],
            tuned_tone: f32::NAN,
            tuned_rate: 0.0,
            latency: 0,
        }
    }
}

impl DynamicTube {
    fn on(&self, id: u32) -> bool {
        self.v[id as usize] >= 0.5
    }

    fn retune(&mut self, rate: f32) {
        let tone = self.v[TONE as usize] as f32;
        if tone == self.tuned_tone && rate == self.tuned_rate {
            return;
        }
        self.tuned_tone = tone;
        self.tuned_rate = rate;
        let db = tone * TONE_RANGE_DB;
        for chan in &mut self.chans {
            chan.dc.set(10.0, rate);
            chan.tone_low
                .set(BiquadKind::LowShelf, TONE_HZ, 0.707, -db, rate);
            chan.tone_high
                .set(BiquadKind::HighShelf, TONE_HZ, 0.707, db, rate);
        }
    }

    fn render(&mut self, audio: AudioBuffer) {
        let hq = self.on(HI_QUALITY);
        let rate = if hq {
            self.sr * HI_QUALITY_FACTOR.multiplier() as f32
        } else {
            self.sr
        };
        self.retune(rate);
        let drive = db_to_gain(self.v[DRIVE as usize] as f32);
        let bias = self.v[BIAS as usize] as f32;
        let envelope = self.v[ENVELOPE as usize] as f32;
        let tube = self.v[TUBE as usize].round() as f32;
        let out = db_to_gain(self.v[OUTPUT as usize] as f32);
        let attack = self.v[ATTACK as usize] as f32 / 1_000.0;
        let release = self.v[RELEASE as usize] as f32 / 1_000.0;

        let frames = audio.frames();
        let (left, right) = (audio.left, audio.right);
        let [cl, cr] = &mut self.chans;
        for (chan, block) in [(cl, &mut *left), (cr, &mut *right)] {
            chan.follower.set_times(attack, release);
            let mut pos = 0;
            while pos < frames {
                let n = (frames - pos).min(self.max_block.min(16));
                let chunk = &mut block[pos..pos + n];
                let level = chunk
                    .iter()
                    .map(|x| chan.follower.process(*x))
                    .fold(0.0f32, f32::max);
                let point = (bias + envelope * level).clamp(0.0, 1.5);
                let (low, high, dc) = (&mut chan.tone_low, &mut chan.tone_high, &mut chan.dc);
                let mut step = |x: f32| {
                    let v = x * drive;
                    let shaped = tube_curve(v, tube, point);
                    // Only what the tube added goes through Tone.
                    let colour = high.process(low.process(shaped - v));
                    dc.process(v + colour)
                };
                if hq {
                    chan.os.process_in_place(chunk, |hi| {
                        for s in hi.iter_mut() {
                            *s = step(*s);
                        }
                    });
                } else {
                    for s in chunk.iter_mut() {
                        *s = step(*s);
                    }
                }
                for s in chunk.iter_mut() {
                    *s *= out;
                }
                pos += n;
            }
        }
    }
}

impl Processor for DynamicTube {
    fn activate(&mut self, sample_rate: f64, max_block: usize) {
        self.sr = sample_rate as f32;
        self.max_block = max_block.max(1);
        self.chans = [Chan::new(self.max_block), Chan::new(self.max_block)];
        self.latency = self.chans[0].os.latency();
        for chan in &mut self.chans {
            chan.follower.prepare(sample_rate);
            chan.dc.set(10.0, self.sr);
        }
        self.tuned_tone = f32::NAN;
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
            chan.follower.reset();
            chan.os.reset();
            chan.tone_low.reset();
            chan.tone_high.reset();
            chan.dc.reset();
        }
    }

    fn latency(&self) -> u32 {
        if self.on(HI_QUALITY) {
            self.latency
        } else {
            0
        }
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.dynamic_tube",
    name: "Dynamic Tube",
    role: DeviceRole::AudioEffect,
    category: "Distortion",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::<DynamicTube>::default(),
};
