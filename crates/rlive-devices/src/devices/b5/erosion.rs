//! Erosion: a short delay modulated by a sine and by band-passed noise.
//!
//! From the manual: the structure, Freq driving both the sine and the noise filter's centre,
//! Amount, Noise Blend (0% sine, 100% noise), Filter Width and Stereo Width. The manual gives no
//! delay times or ranges. Ours: the base delay of 0.4 ms, which is the device's latency, and
//! that Amount at full swings the delay by nearly all of it. The X-Y display isn't built; its
//! controls are ordinary parameters.

use super::common::{Ramp, CONTROL_FRAMES};
use crate::dsp::delay::{InterpDelay, Interpolation};
use crate::dsp::util::Rng;
use crate::dsp::{FilterMode, Svf};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    AudioBuffer, BlockStep, DeviceDesc, Item, ParamId, ParamInfo, ProcessContext, Processor,
    Section, Status,
};
use std::f32::consts::TAU;

pub const FREQ: u32 = 0;
pub const AMOUNT: u32 = 1;
pub const NOISE_BLEND: u32 = 2;
pub const FILTER_WIDTH: u32 = 3;
pub const STEREO_WIDTH: u32 = 4;

const PARAMS: usize = 5;

fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::skewed(FREQ, "Freq", 30.0, 18_000.0, 1_000.0, "Hz", 2.5),
        ParamInfo::continuous(AMOUNT, "Amount", 0.0, 1.0, 0.0, ""),
        ParamInfo::continuous(NOISE_BLEND, "Noise Blend", 0.0, 1.0, 0.5, ""),
        ParamInfo::continuous(FILTER_WIDTH, "Filter Width", 0.0, 1.0, 0.5, ""),
        ParamInfo::continuous(STEREO_WIDTH, "Stereo Width", 0.0, 1.0, 0.0, ""),
    ]
}

fn layout() -> Vec<Section> {
    let knob = |id| Item::Knob(ParamId(id));
    vec![Section {
        title: "Erosion",
        items: vec![
            knob(FREQ),
            knob(AMOUNT),
            knob(NOISE_BLEND),
            knob(FILTER_WIDTH),
            knob(STEREO_WIDTH),
        ],
    }]
}

struct Chan {
    delay: InterpDelay,
    noise: Rng,
    band: Svf,
}

impl Chan {
    fn new(seed: u32) -> Self {
        Self {
            delay: InterpDelay::new(256),
            noise: Rng::new(seed),
            band: Svf::default(),
        }
    }

    /// One sample of band-passed noise, in about `-1..1`.
    #[inline]
    fn noise(&mut self, gain: f32) -> f32 {
        let white = self.noise.bipolar();
        (self.band.process(white, FilterMode::BandPass) * gain).clamp(-1.0, 1.0)
    }
}

pub struct Erosion {
    v: [f64; PARAMS],
    sr: f32,
    chans: [Chan; 2],
    base: usize,
    phase: f32,
    freq: Ramp,
    width: Ramp,
    depth: Ramp,
    fresh: bool,
}

impl Default for Erosion {
    fn default() -> Self {
        let mut v = [0.0; PARAMS];
        for p in params() {
            v[p.id as usize] = p.default;
        }
        Self {
            v,
            sr: 48_000.0,
            chans: [Chan::new(1), Chan::new(2)],
            base: 19,
            phase: 0.0,
            freq: Ramp::new(1_000f32.log2()),
            width: Ramp::new(0.5),
            depth: Ramp::new(0.0),
            fresh: true,
        }
    }
}

impl Erosion {
    fn base_for(sample_rate: f32) -> usize {
        ((sample_rate * 0.0004).round() as usize).max(2)
    }

    fn render(&mut self, audio: AudioBuffer) {
        self.freq.target = (self.v[FREQ as usize] as f32).log2();
        self.width.target = self.v[FILTER_WIDTH as usize] as f32;
        self.depth.target = self.v[AMOUNT as usize] as f32 * (self.base - 1) as f32;
        let mut stale = false;
        if self.fresh {
            self.fresh = false;
            for ramp in [&mut self.freq, &mut self.width, &mut self.depth] {
                ramp.snap(ramp.target);
            }
            stale = true;
        }
        let blend = self.v[NOISE_BLEND as usize] as f32;
        let stereo = self.v[STEREO_WIDTH as usize] as f32;
        let base = self.base as f32;
        let frames = audio.frames();
        let (left, right) = (audio.left, audio.right);
        let mut pos = 0;
        while pos < frames {
            let n = (frames - pos).min(CONTROL_FRAMES);
            let mut moved = stale;
            stale = false;
            moved |= self.freq.tick(n, 0.02, self.sr);
            moved |= self.width.tick(n, 0.02, self.sr);
            self.depth.tick(n, 0.02, self.sr);
            let hz = 2f32.powf(self.freq.value);
            if moved {
                let width = self.width.value;
                let res = 0.94 * (1.0 - width);
                for chan in &mut self.chans {
                    chan.band.set(hz, res, self.sr);
                }
            }
            let depth = self.depth.value;
            // A narrow filter passes less of the noise, so it's lifted to match.
            let gain = 2.0 + (1.0 - self.width.value) * 6.0;
            let dt = hz / self.sr;
            let [cl, cr] = &mut self.chans;
            for i in pos..pos + n {
                let sine_l = (self.phase * TAU).sin();
                let sine_r = ((self.phase + stereo * 0.25) * TAU).sin();
                self.phase = (self.phase + dt).fract();
                let noise_l = cl.noise(gain);
                let noise_other = cr.noise(gain);
                let noise_r = noise_l + (noise_other - noise_l) * stereo;
                let mod_l = sine_l + (noise_l - sine_l) * blend;
                let mod_r = sine_r + (noise_r - sine_r) * blend;
                cl.delay.write(left[i]);
                cr.delay.write(right[i]);
                left[i] = cl.delay.read(base + depth * mod_l, Interpolation::Linear);
                right[i] = cr.delay.read(base + depth * mod_r, Interpolation::Linear);
            }
            pos += n;
        }
    }
}

impl Processor for Erosion {
    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sr = sample_rate as f32;
        self.base = Self::base_for(self.sr);
        let capacity = self.base * 2 + 8;
        for (i, chan) in self.chans.iter_mut().enumerate() {
            chan.delay = InterpDelay::new(capacity);
            chan.noise = Rng::new(i as u32 + 1);
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
            chan.delay.clear();
            chan.band.reset();
        }
        self.phase = 0.0;
        self.fresh = true;
    }

    fn latency(&self) -> u32 {
        self.base as u32
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.erosion",
    name: "Erosion",
    role: DeviceRole::AudioEffect,
    category: "Distortion",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::<Erosion>::default(),
};
