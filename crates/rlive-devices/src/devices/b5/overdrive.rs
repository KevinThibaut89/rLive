//! Overdrive: a band-pass, a drive stage, a tone control, and internal compression that makes
//! up for the level the drive adds.
//!
//! From the manual: the pre-distortion band-pass with centre and bandwidth, Drive at 0% still
//! distorting, Tone as a post-distortion EQ, Dynamics reducing the compression as it rises, and
//! Dry/Wet. The manual gives no numbers. The curve, the gain range, the bandwidth in octaves and
//! the compression law are ours.

use super::common::{Ramp, CONTROL_FRAMES};
use crate::dsp::follower::EnvelopeFollower;
use crate::dsp::{Biquad, BiquadKind};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    AudioBuffer, BlockStep, DeviceDesc, Item, ParamId, ParamInfo, ProcessContext, Processor,
    Section, Status,
};

pub const FREQ: u32 = 0;
pub const WIDTH: u32 = 1;
pub const DRIVE: u32 = 2;
pub const TONE: u32 = 3;
pub const DYNAMICS: u32 = 4;
pub const MIX: u32 = 5;

const PARAMS: usize = 6;

/// Ours: the gain into the curve at 0% and 100% Drive.
pub const MIN_GAIN_DB: f32 = 6.0;
pub const MAX_GAIN_DB: f32 = 48.0;
/// Ours: where Tone's shelf sits and how far it moves.
pub const TONE_HZ: f32 = 3_000.0;
pub const TONE_RANGE_DB: f32 = 12.0;

fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::skewed(FREQ, "Frequency", 50.0, 20_000.0, 1_000.0, "Hz", 2.5),
        ParamInfo::continuous(WIDTH, "Width", 0.5, 9.0, 6.0, "oct"),
        ParamInfo::continuous(DRIVE, "Drive", 0.0, 1.0, 0.5, ""),
        ParamInfo::continuous(TONE, "Tone", 0.0, 1.0, 0.5, ""),
        ParamInfo::continuous(DYNAMICS, "Dynamics", 0.0, 1.0, 0.5, ""),
        ParamInfo::continuous(MIX, "Dry/Wet", 0.0, 1.0, 1.0, ""),
    ]
}

fn layout() -> Vec<Section> {
    let knob = |id| Item::Knob(ParamId(id));
    vec![
        Section {
            title: "Band",
            items: vec![knob(FREQ), knob(WIDTH)],
        },
        Section {
            title: "Overdrive",
            items: vec![knob(DRIVE), knob(TONE), knob(DYNAMICS), knob(MIX)],
        },
    ]
}

/// The drive curve at a gain of `g`: a soft clip with a little asymmetry, so a small even
/// component comes with the odd ones. Never reaches zero distortion, whatever the Drive.
#[inline]
pub fn curve(x: f32, g: f32) -> f32 {
    const OFFSET: f32 = 0.08;
    ((x * g + OFFSET).tanh() - OFFSET.tanh()) / (1.0 + 0.1 * g.ln().max(0.0))
}

#[derive(Clone, Copy, Default)]
struct Chan {
    hp: Biquad,
    lp: Biquad,
    tone: Biquad,
    follower: Follower,
}

#[derive(Clone, Copy, Default)]
struct Follower(EnvelopeFollower);

pub struct Overdrive {
    v: [f64; PARAMS],
    sr: f32,
    chans: [Chan; 2],
    freq: Ramp,
    width: Ramp,
    tone: Ramp,
    fresh: bool,
}

impl Default for Overdrive {
    fn default() -> Self {
        let mut v = [0.0; PARAMS];
        for p in params() {
            v[p.id as usize] = p.default;
        }
        Self {
            v,
            sr: 48_000.0,
            chans: [Chan::default(); 2],
            freq: Ramp::new(1_000f32.log2()),
            width: Ramp::new(6.0),
            tone: Ramp::new(0.5),
            fresh: true,
        }
    }
}

impl Overdrive {
    fn retune(&mut self) {
        let centre = 2f32.powf(self.freq.value);
        let half = 2f32.powf(self.width.value * 0.5);
        let (low, high) = (centre / half, (centre * half).min(self.sr * 0.45));
        let tone_db = (self.tone.value - 0.5) * 2.0 * TONE_RANGE_DB;
        for chan in &mut self.chans {
            chan.hp.set(BiquadKind::HighPass, low, 0.707, 0.0, self.sr);
            chan.lp.set(BiquadKind::LowPass, high, 0.707, 0.0, self.sr);
            chan.tone
                .set(BiquadKind::HighShelf, TONE_HZ, 0.707, tone_db, self.sr);
        }
    }

    fn render(&mut self, audio: AudioBuffer) {
        self.freq.target = (self.v[FREQ as usize] as f32).log2();
        self.width.target = self.v[WIDTH as usize] as f32;
        self.tone.target = self.v[TONE as usize] as f32;
        let mut stale = false;
        if self.fresh {
            self.fresh = false;
            for ramp in [&mut self.freq, &mut self.width, &mut self.tone] {
                ramp.snap(ramp.target);
            }
            stale = true;
        }
        let drive = self.v[DRIVE as usize] as f32;
        let gain = rlive_core::db_to_gain(MIN_GAIN_DB + (MAX_GAIN_DB - MIN_GAIN_DB) * drive);
        let squash = 1.0 - self.v[DYNAMICS as usize] as f32;
        let mix = self.v[MIX as usize] as f32;
        let frames = audio.frames();
        let (left, right) = (audio.left, audio.right);
        let mut pos = 0;
        while pos < frames {
            let n = (frames - pos).min(CONTROL_FRAMES);
            let mut moved = stale;
            stale = false;
            for ramp in [&mut self.freq, &mut self.width, &mut self.tone] {
                moved |= ramp.tick(n, 0.02, self.sr);
            }
            if moved {
                self.retune();
            }
            let [cl, cr] = &mut self.chans;
            for (chan, block) in [
                (cl, &mut left[pos..pos + n]),
                (cr, &mut right[pos..pos + n]),
            ] {
                for s in block.iter_mut() {
                    let dry = *s;
                    let band = chan.lp.process(chan.hp.process(dry));
                    let shaped = curve(band, gain);
                    let level = chan.follower.0.process(shaped);
                    // At low Dynamics the louder it gets, the more it's pulled back.
                    let squashed = shaped / (1.0 + squash * 1.5 * level);
                    let wet = chan.tone.process(squashed);
                    *s = dry * (1.0 - mix) + wet * mix;
                }
            }
            pos += n;
        }
    }
}

impl Processor for Overdrive {
    fn sleeps_when_silent(&self) -> bool {
        true
    }

    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sr = sample_rate as f32;
        for chan in &mut self.chans {
            chan.follower.0.prepare(sample_rate);
            chan.follower.0.set_times(0.002, 0.08);
        }
        self.fresh = true;
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
            chan.hp.reset();
            chan.lp.reset();
            chan.tone.reset();
            chan.follower.0.reset();
        }
        self.fresh = true;
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.overdrive",
    name: "Overdrive",
    role: DeviceRole::AudioEffect,
    category: "Distortion",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::<Overdrive>::default(),
};
