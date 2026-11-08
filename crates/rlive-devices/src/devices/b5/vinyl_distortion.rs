//! Vinyl Distortion: even harmonics from a tracing model, odd harmonics from a pinch model, and
//! a crackle generator.
//!
//! From the manual: Tracing Model and Pinch Effect each with drive, frequency and Q, the Pinch's
//! 180-degree stereo option, the overall Drive, Soft and Hard modes, and Crackle's Density and
//! Volume. The manual gives no numbers, so the curves, ranges and crackle statistics are ours.
//! The X-Y displays aren't built; their controls are ordinary parameters.

use crate::dsp::util::{sanitize, Rng};
use crate::dsp::{Biquad, BiquadKind};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    db_to_gain, AudioBuffer, BlockStep, DeviceDesc, Item, ParamId, ParamInfo, ProcessContext,
    Processor, Section, Status,
};

pub const TRACING_DRIVE: u32 = 0;
pub const TRACING_FREQ: u32 = 1;
pub const TRACING_Q: u32 = 2;
pub const PINCH_DRIVE: u32 = 3;
pub const PINCH_FREQ: u32 = 4;
pub const PINCH_Q: u32 = 5;
pub const DRIVE: u32 = 6;
pub const MODE: u32 = 7;
pub const PINCH_STEREO: u32 = 8;
pub const CRACKLE_DENSITY: u32 = 9;
pub const CRACKLE_VOLUME: u32 = 10;

const PARAMS: usize = 11;

fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::continuous(TRACING_DRIVE, "Tracing Drive", 0.0, 1.0, 0.0, ""),
        ParamInfo::skewed(
            TRACING_FREQ,
            "Tracing Freq",
            50.0,
            18_000.0,
            1_000.0,
            "Hz",
            2.5,
        ),
        ParamInfo::continuous(TRACING_Q, "Tracing Width", 0.1, 4.0, 1.0, ""),
        ParamInfo::continuous(PINCH_DRIVE, "Pinch Drive", 0.0, 1.0, 0.0, ""),
        ParamInfo::skewed(PINCH_FREQ, "Pinch Freq", 50.0, 18_000.0, 3_000.0, "Hz", 2.5),
        ParamInfo::continuous(PINCH_Q, "Pinch Width", 0.1, 4.0, 1.0, ""),
        ParamInfo::continuous(DRIVE, "Drive", -24.0, 24.0, 0.0, "dB"),
        ParamInfo::stepped(MODE, "Mode", &["Soft", "Hard"], 0),
        ParamInfo::toggle(PINCH_STEREO, "Pinch Stereo", true),
        ParamInfo::continuous(CRACKLE_DENSITY, "Crackle Density", 0.0, 1.0, 0.0, ""),
        ParamInfo::continuous(CRACKLE_VOLUME, "Crackle Volume", -70.0, 0.0, -30.0, "dB"),
    ]
}

fn layout() -> Vec<Section> {
    let knob = |id| Item::Knob(ParamId(id));
    vec![
        Section {
            title: "Tracing Model",
            items: vec![knob(TRACING_DRIVE), knob(TRACING_FREQ), knob(TRACING_Q)],
        },
        Section {
            title: "Pinch Effect",
            items: vec![
                knob(PINCH_DRIVE),
                knob(PINCH_FREQ),
                knob(PINCH_Q),
                Item::Toggle(ParamId(PINCH_STEREO)),
            ],
        },
        Section {
            title: "Output",
            items: vec![knob(DRIVE), Item::Menu(ParamId(MODE))],
        },
        Section {
            title: "Crackle",
            items: vec![knob(CRACKLE_DENSITY), knob(CRACKLE_VOLUME)],
        },
    ]
}

/// Tracing: even harmonics, always pushing the same way. Soft saturates early; Hard doesn't.
#[inline]
pub fn tracing(band: f32, hard: bool) -> f32 {
    let even = band * band;
    if hard {
        even.min(2.0)
    } else {
        even.tanh()
    }
}

/// Pinch: odd harmonics.
#[inline]
pub fn pinch(band: f32, hard: bool) -> f32 {
    let odd = band * band * band;
    if hard {
        odd.clamp(-2.0, 2.0)
    } else {
        odd.tanh()
    }
}

#[derive(Clone, Copy)]
struct Crackle {
    rng: Rng,
    level: f32,
    decay: f32,
    sign: f32,
}

impl Crackle {
    fn new(seed: u32) -> Self {
        Self {
            rng: Rng::new(seed),
            level: 0.0,
            decay: 0.0,
            sign: 1.0,
        }
    }

    /// `chance` is the probability of a tick starting on this sample.
    #[inline]
    fn next(&mut self, chance: f32) -> f32 {
        if self.rng.unipolar() < chance {
            self.level = 0.5 + 0.5 * self.rng.unipolar();
            self.decay = 0.2 + 0.7 * self.rng.unipolar();
            self.sign = if self.rng.unipolar() < 0.5 { 1.0 } else { -1.0 };
        }
        let out = self.level * self.sign;
        self.level = sanitize(self.level * self.decay);
        out
    }
}

#[derive(Clone, Copy)]
struct Chan {
    tracing_band: Biquad,
    pinch_band: Biquad,
    crackle: Crackle,
}

pub struct VinylDistortion {
    v: [f64; PARAMS],
    sr: f32,
    chans: [Chan; 2],
    tuned: [f32; 4],
}

impl Default for VinylDistortion {
    fn default() -> Self {
        let mut v = [0.0; PARAMS];
        for p in params() {
            v[p.id as usize] = p.default;
        }
        let chan = |seed| Chan {
            tracing_band: Biquad::default(),
            pinch_band: Biquad::default(),
            crackle: Crackle::new(seed),
        };
        Self {
            v,
            sr: 48_000.0,
            chans: [chan(1), chan(2)],
            tuned: [f32::NAN; 4],
        }
    }
}

impl VinylDistortion {
    fn val(&self, id: u32) -> f32 {
        self.v[id as usize] as f32
    }

    fn retune(&mut self) {
        let tuned = [
            self.val(TRACING_FREQ),
            self.val(TRACING_Q),
            self.val(PINCH_FREQ),
            self.val(PINCH_Q),
        ];
        if tuned == self.tuned {
            return;
        }
        self.tuned = tuned;
        for chan in &mut self.chans {
            chan.tracing_band
                .set(BiquadKind::BandPass, tuned[0], tuned[1], 0.0, self.sr);
            chan.pinch_band
                .set(BiquadKind::BandPass, tuned[2], tuned[3], 0.0, self.sr);
        }
    }

    fn render(&mut self, audio: AudioBuffer) {
        self.retune();
        let scale = db_to_gain(self.val(DRIVE));
        let tracing_drive = self.val(TRACING_DRIVE) * scale * TRACING_RANGE;
        let pinch_drive = self.val(PINCH_DRIVE) * scale * PINCH_RANGE;
        let hard = self.val(MODE) >= 0.5;
        let stereo = self.val(PINCH_STEREO) >= 0.5;
        let chance = self.val(CRACKLE_DENSITY).powi(3) * 0.02;
        let crackle = if chance > 0.0 {
            db_to_gain(self.val(CRACKLE_VOLUME))
        } else {
            0.0
        };
        let [cl, cr] = &mut self.chans;
        for (i, (chan, block)) in [(cl, &mut *audio.left), (cr, &mut *audio.right)]
            .into_iter()
            .enumerate()
        {
            let pinch_sign = if stereo && i == 1 { -1.0 } else { 1.0 };
            for s in block.iter_mut() {
                let x = *s;
                let mut y = x;
                if tracing_drive > 0.0 {
                    let band = chan.tracing_band.process(x);
                    y += tracing_drive * tracing(band * BAND_DRIVE, hard);
                }
                if pinch_drive > 0.0 {
                    let band = chan.pinch_band.process(x);
                    y += pinch_sign * pinch_drive * pinch(band * BAND_DRIVE, hard);
                }
                if crackle > 0.0 {
                    y += crackle * chan.crackle.next(chance);
                }
                *s = y;
            }
        }
    }
}

/// Ours: how much distortion full Drive adds, and the level into the curves.
pub const TRACING_RANGE: f32 = 0.5;
pub const PINCH_RANGE: f32 = 0.5;
pub const BAND_DRIVE: f32 = 2.0;

impl Processor for VinylDistortion {
    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sr = sample_rate as f32;
        self.tuned = [f32::NAN; 4];
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
            chan.tracing_band.reset();
            chan.pinch_band.reset();
            chan.crackle.level = 0.0;
        }
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.vinyl_distortion",
    name: "Vinyl Distortion",
    role: DeviceRole::AudioEffect,
    category: "Distortion",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::<VinylDistortion>::default(),
};
