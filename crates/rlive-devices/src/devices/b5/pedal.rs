//! Pedal: three distortion types, then a three-band adaptive EQ, a Sub switch and a level.
//!
//! From the manual: Gain with 0% still distorting, Output, the three types, Bass as a peak at
//! 100 Hz, Mid with centres at 500 Hz, 1 kHz and 2 kHz, narrower at the low setting and wider at
//! the high, Treble as a 3.3 kHz shelf, Q rising with boost, the Sub low shelf below 250 Hz,
//! Dry/Wet and Hi-Quality. The manual gives no gain ranges, Q values or curves; those are ours.

use super::common::{DcBlock, DryLine, Ramp, CONTROL_FRAMES, HI_QUALITY_FACTOR};
use crate::dsp::follower::EnvelopeFollower;
use crate::dsp::oversample::Oversampler;
use crate::dsp::{Biquad, BiquadKind};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    db_to_gain, AudioBuffer, BlockStep, DeviceDesc, Item, ParamId, ParamInfo, ProcessContext,
    Processor, Section, Status,
};

pub const TYPE: u32 = 0;
pub const GAIN: u32 = 1;
pub const OUTPUT: u32 = 2;
pub const BASS: u32 = 3;
pub const MID: u32 = 4;
pub const MID_FREQ: u32 = 5;
pub const TREBLE: u32 = 6;
pub const SUB: u32 = 7;
pub const MIX: u32 = 8;
pub const HI_QUALITY: u32 = 9;

const PARAMS: usize = 10;

/// From the manual.
pub const BASS_HZ: f32 = 100.0;
pub const MID_HZ: [f32; 3] = [500.0, 1_000.0, 2_000.0];
pub const TREBLE_HZ: f32 = 3_300.0;
pub const SUB_HZ: f32 = 250.0;

/// Ours; the manual doesn't say.
pub const BASS_RANGE_DB: f32 = 15.0;
pub const MID_RANGE_DB: f32 = 15.0;
pub const TREBLE_RANGE_DB: f32 = 15.0;
pub const SUB_DB: f32 = 9.0;
pub const MID_Q: [f32; 3] = [1.6, 1.0, 0.6];
pub const GAIN_MIN_DB: f32 = 18.0;
pub const GAIN_MAX_DB: f32 = 54.0;
/// How each type treats the same Gain: Overdrive keeps more of the signal's dynamics.
pub const TYPE_TRIM_DB: [f32; 3] = [-12.0, 0.0, 6.0];

fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::stepped(TYPE, "Type", &["Overdrive", "Distortion", "Fuzz"], 0),
        ParamInfo::continuous(GAIN, "Gain", 0.0, 1.0, 0.0, ""),
        ParamInfo::continuous(OUTPUT, "Output", -36.0, 12.0, 0.0, "dB"),
        ParamInfo::continuous(BASS, "Bass", -1.0, 1.0, 0.0, ""),
        ParamInfo::continuous(MID, "Mid", -1.0, 1.0, 0.0, ""),
        ParamInfo::stepped(MID_FREQ, "Mid Freq", &["500 Hz", "1 kHz", "2 kHz"], 1),
        ParamInfo::continuous(TREBLE, "Treble", -1.0, 1.0, 0.0, ""),
        ParamInfo::toggle(SUB, "Sub", false),
        ParamInfo::continuous(MIX, "Dry/Wet", 0.0, 1.0, 1.0, ""),
        ParamInfo::toggle(HI_QUALITY, "Hi-Quality", false),
    ]
}

fn layout() -> Vec<Section> {
    let knob = |id| Item::Knob(ParamId(id));
    vec![
        Section {
            title: "Pedal",
            items: vec![Item::Menu(ParamId(TYPE)), knob(GAIN), knob(OUTPUT)],
        },
        Section {
            title: "EQ",
            items: vec![
                knob(BASS),
                knob(MID),
                Item::Menu(ParamId(MID_FREQ)),
                knob(TREBLE),
                Item::Toggle(ParamId(SUB)),
            ],
        },
        Section {
            title: "Mix",
            items: vec![knob(MIX), Item::Toggle(ParamId(HI_QUALITY))],
        },
    ]
}

/// The adaptive Q: resonance grows with boost and is left alone when cutting.
#[inline]
pub fn adaptive_q(base: f32, amount: f32) -> f32 {
    base + 1.2 * amount.max(0.0)
}

/// The Overdrive curve: warm, smooth and slightly asymmetric.
#[inline]
pub fn overdrive(v: f32) -> f32 {
    const O: f32 = 0.1;
    ((v + O).tanh() - O.tanh()) * 0.9
}

/// The Distortion curve: a hard knee, for a tight and aggressive sound.
#[inline]
pub fn distortion(v: f32) -> f32 {
    (v * 2.0).tanh().clamp(-0.85, 0.85)
}

/// The Fuzz curve at a bias point that drifts with level, which makes it sputter.
#[inline]
pub fn fuzz(v: f32, bias: f32) -> f32 {
    ((v * 3.0 + bias).tanh() - bias.tanh()) * 0.75
}

#[derive(Clone)]
struct Chan {
    os: Oversampler,
    dc: DcBlock,
    /// Runs before the curve; tightens the low end.
    tight: Biquad,
    follower: EnvelopeFollower,
    bass: Biquad,
    mid: Biquad,
    treble: Biquad,
    sub: Biquad,
}

impl Chan {
    fn new(max_block: usize) -> Self {
        Self {
            os: Oversampler::new(HI_QUALITY_FACTOR, max_block),
            dc: DcBlock::default(),
            tight: Biquad::default(),
            follower: EnvelopeFollower::new(),
            bass: Biquad::default(),
            mid: Biquad::default(),
            treble: Biquad::default(),
            sub: Biquad::default(),
        }
    }
}

pub struct Pedal {
    v: [f64; PARAMS],
    sr: f32,
    max_block: usize,
    chans: [Chan; 2],
    dry_line: DryLine,
    dry: [Vec<f32>; 2],
    latency: u32,
    bass: Ramp,
    mid: Ramp,
    treble: Ramp,
    tuned_static: (i32, i32, f32),
    fresh: bool,
}

impl Default for Pedal {
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
            dry_line: DryLine::new(0),
            dry: [Vec::new(), Vec::new()],
            latency: 0,
            bass: Ramp::new(0.0),
            mid: Ramp::new(0.0),
            treble: Ramp::new(0.0),
            tuned_static: (-1, -1, 0.0),
            fresh: true,
        }
    }
}

impl Pedal {
    fn on(&self, id: u32) -> bool {
        self.v[id as usize] >= 0.5
    }

    fn retune_eq(&mut self) {
        let (bass, mid, treble) = (self.bass.value, self.mid.value, self.treble.value);
        let slot = (self.v[MID_FREQ as usize].round() as usize).min(2);
        let sr = self.sr;
        for chan in &mut self.chans {
            chan.bass.set(
                BiquadKind::Peak,
                BASS_HZ,
                adaptive_q(0.7, bass),
                bass * BASS_RANGE_DB,
                sr,
            );
            chan.mid.set(
                BiquadKind::Peak,
                MID_HZ[slot],
                adaptive_q(MID_Q[slot], mid),
                mid * MID_RANGE_DB,
                sr,
            );
            chan.treble.set(
                BiquadKind::HighShelf,
                TREBLE_HZ,
                adaptive_q(0.7, treble),
                treble * TREBLE_RANGE_DB,
                sr,
            );
        }
    }

    fn retune_static(&mut self) {
        let tuned = (
            self.v[TYPE as usize].round() as i32,
            self.v[MID_FREQ as usize].round() as i32,
            self.sr,
        );
        if tuned == self.tuned_static {
            return;
        }
        self.tuned_static = tuned;
        let sr = self.sr;
        let tight = [120.0, 320.0, 40.0][(tuned.0 as usize).min(2)];
        for chan in &mut self.chans {
            chan.tight.set(BiquadKind::HighPass, tight, 0.707, 0.0, sr);
            chan.sub.set(BiquadKind::LowShelf, SUB_HZ, 0.7, SUB_DB, sr);
        }
        self.retune_eq();
    }

    fn render(&mut self, audio: AudioBuffer) {
        self.retune_static();
        self.bass.target = self.v[BASS as usize] as f32;
        self.mid.target = self.v[MID as usize] as f32;
        self.treble.target = self.v[TREBLE as usize] as f32;
        if self.fresh {
            self.fresh = false;
            for ramp in [&mut self.bass, &mut self.mid, &mut self.treble] {
                ramp.snap(ramp.target);
            }
            self.retune_eq();
        }
        let kind = self.v[TYPE as usize].round() as i32;
        let gain = db_to_gain(
            GAIN_MIN_DB
                + (GAIN_MAX_DB - GAIN_MIN_DB) * self.v[GAIN as usize] as f32
                + TYPE_TRIM_DB[(kind as usize).min(2)],
        );
        let out = db_to_gain(self.v[OUTPUT as usize] as f32);
        let mix = self.v[MIX as usize] as f32;
        let sub = self.on(SUB);
        let hq = self.on(HI_QUALITY);

        let frames = audio.frames();
        let (left, right) = (audio.left, audio.right);
        let mut pos = 0;
        while pos < frames {
            let n = (frames - pos).min(CONTROL_FRAMES).min(self.max_block);
            let mut moved = false;
            for ramp in [&mut self.bass, &mut self.mid, &mut self.treble] {
                moved |= ramp.tick(n, 0.02, self.sr);
            }
            if moved {
                self.retune_eq();
            }
            let mut channels = [&mut left[pos..pos + n], &mut right[pos..pos + n]];
            for (ch, block) in channels.iter_mut().enumerate() {
                self.dry[ch][..n].copy_from_slice(block);
                let chan = &mut self.chans[ch];
                let bias = if kind == 2 {
                    let level = block
                        .iter()
                        .map(|x| chan.follower.process(*x))
                        .fold(0.0f32, f32::max);
                    0.2 + 0.8 * level.min(1.0)
                } else {
                    0.0
                };
                for s in block.iter_mut() {
                    *s = chan.tight.process(*s) * gain;
                }
                let curve = |v: f32| match kind {
                    0 => overdrive(v),
                    1 => distortion(v),
                    _ => fuzz(v, bias),
                };
                if hq {
                    chan.os.process_in_place(block, |hi| {
                        for s in hi.iter_mut() {
                            *s = curve(*s);
                        }
                    });
                } else {
                    for s in block.iter_mut() {
                        *s = curve(*s);
                    }
                }
                for s in block.iter_mut() {
                    let mut x = chan.dc.process(*s);
                    x = chan.bass.process(x);
                    x = chan.mid.process(x);
                    x = chan.treble.process(x);
                    if sub {
                        x = chan.sub.process(x);
                    }
                    *s = x * out;
                }
            }
            let [wet_l, wet_r] = channels;
            let [dry_l, dry_r] = &mut self.dry;
            let line = &mut self.dry_line;
            for (y, x) in wet_l.iter_mut().zip(&dry_l[..n]) {
                let d = if hq { line.left(*x) } else { *x };
                *y = d * (1.0 - mix) + *y * mix;
            }
            for (y, x) in wet_r.iter_mut().zip(&dry_r[..n]) {
                let d = if hq { line.right(*x) } else { *x };
                *y = d * (1.0 - mix) + *y * mix;
            }
            pos += n;
        }
    }
}

impl Processor for Pedal {
    fn sleeps_when_silent(&self) -> bool {
        true
    }

    fn activate(&mut self, sample_rate: f64, max_block: usize) {
        self.sr = sample_rate as f32;
        self.max_block = max_block.max(1);
        self.chans = [Chan::new(self.max_block), Chan::new(self.max_block)];
        for chan in &mut self.chans {
            chan.dc.set(10.0, self.sr);
            chan.follower.prepare(sample_rate);
            chan.follower.set_times(0.005, 0.12);
        }
        self.latency = self.chans[0].os.latency();
        self.dry_line = DryLine::new(self.latency as usize);
        self.dry = [vec![0.0; self.max_block], vec![0.0; self.max_block]];
        self.tuned_static = (-1, -1, 0.0);
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
            chan.os.reset();
            chan.dc.reset();
            chan.follower.reset();
            for f in [
                &mut chan.tight,
                &mut chan.bass,
                &mut chan.mid,
                &mut chan.treble,
                &mut chan.sub,
            ] {
                f.reset();
            }
        }
        self.dry_line.clear();
        self.fresh = true;
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
    id: "rlive.pedal",
    name: "Pedal",
    role: DeviceRole::AudioEffect,
    category: "Distortion",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::<Pedal>::default(),
};
