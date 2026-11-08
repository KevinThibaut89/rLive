//! Phaser-Flanger: an all-pass cascade, or a modulated delay, in one chassis with two LFOs.
//!
//! The manual gives the three modes and every control, and which direction Flanger and Doubler
//! move the delay. It gives no ranges other than Safe Bass's: Notches 1 to 12, Center 50 Hz to
//! 18 kHz, Time ranges and depths below are ours. A notch is one second-order all-pass, so at a
//! 50% mix the display's notch count is the filter count.

use super::common::{
    guard, knob, menu, toggle, values, BandSplit, Warmth, Wave, WaveLfo, WaveSettings,
    CONTROL_FRAMES,
};
use crate::dsp::delay::{InterpDelay, Interpolation};
use crate::dsp::follower::EnvelopeFollower;
use crate::dsp::lfo::{Lfo, LfoRate, LfoSettings, LfoShape};
use crate::dsp::tempo::{BeatClock, SyncRate};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    db_to_gain, AudioBuffer, BlockStep, DeviceDesc, Item, ParamInfo, ProcessContext, Processor,
    Section, Status, Tail,
};
use std::f32::consts::PI;

pub const MODE: u32 = 0;
pub const NOTCHES: u32 = 1;
pub const CENTER: u32 = 2;
pub const SPREAD: u32 = 3;
pub const BLEND: u32 = 4;
pub const FLANGER_TIME: u32 = 5;
pub const DOUBLER_TIME: u32 = 6;
pub const SYNC: u32 = 7;
pub const FREQ: u32 = 8;
pub const RATE: u32 = 9;
pub const WAVEFORM: u32 = 10;
pub const STEREO_MODE: u32 = 11;
pub const PHASE: u32 = 12;
pub const SPIN: u32 = 13;
pub const DUTY: u32 = 14;
pub const LFO2_MIX: u32 = 15;
pub const LFO2_SYNC: u32 = 16;
pub const LFO2_FREQ: u32 = 17;
pub const LFO2_RATE: u32 = 18;
pub const AMOUNT: u32 = 19;
pub const FEEDBACK: u32 = 20;
pub const FB_INVERT: u32 = 21;
pub const ENV_ON: u32 = 22;
pub const ENV_AMOUNT: u32 = 23;
pub const ENV_ATTACK: u32 = 24;
pub const ENV_RELEASE: u32 = 25;
pub const SAFE_BASS: u32 = 26;
pub const OUTPUT: u32 = 27;
pub const WARMTH: u32 = 28;
pub const MIX: u32 = 29;
const PARAMS: usize = 30;

pub const MODE_PHASER: usize = 0;
pub const MODE_FLANGER: usize = 1;
pub const MODE_DOUBLER: usize = 2;

pub const MAX_NOTCHES: usize = 12;
const WAVES: [&str; 10] = [
    "Sine",
    "Triangle",
    "Saw Up",
    "Saw Down",
    "Rectangle",
    "Random",
    "Random S&H",
    "Triangle Analog",
    "Triangle 8",
    "Triangle 16",
];
/// The notch centre's swing at full Amount, in octaves either way.
pub const CENTER_OCTAVES: f32 = 3.0;
/// Doubler's bipolar swing at full Amount, as a fraction of its Time.
pub const DOUBLER_DEPTH: f32 = 0.2;
const MIN_DELAY_MS: f32 = 0.05;

/// The all-pass quality for a Spread of 0..1: narrow at 0, wide at 1.
pub fn spread_q(spread: f32) -> f32 {
    8.0 * 2f32.powf(-6.0 * spread.clamp(0.0, 1.0))
}

fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::stepped(MODE, "Mode", &["Phaser", "Flanger", "Doubler"], 0),
        ParamInfo::continuous(NOTCHES, "Notches", 1.0, MAX_NOTCHES as f64, 4.0, ""),
        ParamInfo::skewed(CENTER, "Center", 50.0, 18_000.0, 1_000.0, "Hz", 3.0),
        ParamInfo::continuous(SPREAD, "Spread", 0.0, 100.0, 30.0, "%"),
        ParamInfo::continuous(BLEND, "Blend", 0.0, 100.0, 0.0, "%"),
        ParamInfo::skewed(FLANGER_TIME, "Flanger Time", 0.1, 20.0, 2.0, "ms", 2.0),
        ParamInfo::skewed(DOUBLER_TIME, "Doubler Time", 1.0, 100.0, 15.0, "ms", 2.0),
        ParamInfo::toggle(SYNC, "LFO Sync", false),
        ParamInfo::skewed(FREQ, "LFO Freq", 0.01, 40.0, 0.5, "Hz", 3.0),
        ParamInfo::stepped(RATE, "LFO Rate", &SyncRate::LABELS, 12),
        ParamInfo::stepped(WAVEFORM, "LFO Waveform", &WAVES, 1),
        ParamInfo::stepped(STEREO_MODE, "LFO Stereo Mode", &["Phase", "Spin"], 0),
        ParamInfo::continuous(PHASE, "LFO Phase", 0.0, 360.0, 180.0, "deg"),
        ParamInfo::continuous(SPIN, "LFO Spin", 0.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(DUTY, "Duty Cycle", -100.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(LFO2_MIX, "LFO2 Mix", 0.0, 100.0, 0.0, "%"),
        ParamInfo::toggle(LFO2_SYNC, "LFO2 Sync", false),
        ParamInfo::skewed(LFO2_FREQ, "LFO2 Freq", 0.01, 40.0, 2.0, "Hz", 3.0),
        ParamInfo::stepped(LFO2_RATE, "LFO2 Rate", &SyncRate::LABELS, 10),
        ParamInfo::continuous(AMOUNT, "Amount", 0.0, 100.0, 40.0, "%"),
        ParamInfo::continuous(FEEDBACK, "Feedback", 0.0, 95.0, 20.0, "%"),
        ParamInfo::toggle(FB_INVERT, "Feedback Invert", false),
        ParamInfo::toggle(ENV_ON, "Env Fol", false),
        ParamInfo::continuous(ENV_AMOUNT, "Env Amount", -100.0, 100.0, 0.0, "%"),
        ParamInfo::skewed(ENV_ATTACK, "Attack", 0.1, 500.0, 5.0, "ms", 3.0),
        ParamInfo::skewed(ENV_RELEASE, "Release", 1.0, 2_000.0, 100.0, "ms", 3.0),
        ParamInfo::skewed(SAFE_BASS, "Safe Bass", 5.0, 3_000.0, 100.0, "Hz", 3.0),
        ParamInfo::continuous(OUTPUT, "Output", -24.0, 12.0, 0.0, "dB"),
        ParamInfo::continuous(WARMTH, "Warmth", 0.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(MIX, "Dry/Wet", 0.0, 100.0, 50.0, "%"),
    ]
}

fn layout() -> Vec<Section> {
    let row = Item::Row;
    vec![
        Section {
            title: "Mode",
            items: vec![
                menu(MODE),
                row(vec![knob(NOTCHES), knob(CENTER), knob(SPREAD), knob(BLEND)]),
                row(vec![knob(FLANGER_TIME), knob(DOUBLER_TIME)]),
            ],
        },
        Section {
            title: "LFO",
            items: vec![
                row(vec![toggle(SYNC), knob(FREQ), menu(RATE), menu(WAVEFORM)]),
                row(vec![menu(STEREO_MODE), knob(PHASE), knob(SPIN), knob(DUTY)]),
                row(vec![
                    knob(LFO2_MIX),
                    toggle(LFO2_SYNC),
                    knob(LFO2_FREQ),
                    menu(LFO2_RATE),
                ]),
            ],
        },
        Section {
            title: "Envelope",
            items: vec![
                toggle(ENV_ON),
                knob(ENV_AMOUNT),
                knob(ENV_ATTACK),
                knob(ENV_RELEASE),
            ],
        },
        Section {
            title: "Global",
            items: vec![
                knob(AMOUNT),
                knob(FEEDBACK),
                toggle(FB_INVERT),
                knob(SAFE_BASS),
                knob(OUTPUT),
                knob(WARMTH),
                knob(MIX),
            ],
        },
    ]
}

/// A second-order all-pass in direct form II transposed.
#[derive(Clone, Copy, Debug, Default)]
struct AllPass {
    z1: f32,
    z2: f32,
}

#[derive(Clone, Copy, Debug, Default)]
struct AllPassCoeffs {
    a1: f32,
    a2: f32,
}

impl AllPassCoeffs {
    fn tuned(hz: f32, q: f32, sample_rate: f32) -> Self {
        let w = 2.0 * PI * hz.clamp(10.0, sample_rate * 0.45) / sample_rate;
        let alpha = w.sin() / (2.0 * q.max(0.05));
        let a0 = 1.0 + alpha;
        Self {
            a1: -2.0 * w.cos() / a0,
            a2: (1.0 - alpha) / a0,
        }
    }
}

impl AllPass {
    #[inline]
    fn process(&mut self, x: f32, c: AllPassCoeffs) -> f32 {
        // With b0 = a2, b1 = a1 and b2 = 1.
        let y = c.a2 * x + self.z1;
        self.z1 = c.a1 * x - c.a1 * y + self.z2;
        self.z2 = x - c.a2 * y;
        y
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

/// The magnitude of the phaser's dry plus wet at `hz`, for the display and tests.
pub fn phaser_response(hz: f32, center: f32, q: f32, notches: usize, sample_rate: f32) -> f32 {
    let c = AllPassCoeffs::tuned(center, q, sample_rate);
    let w = 2.0 * PI * hz / sample_rate;
    let (cos1, sin1) = ((-w).cos(), (-w).sin());
    let (cos2, sin2) = ((-2.0 * w).cos(), (-2.0 * w).sin());
    // H(z) = (a2 + a1 z^-1 + z^-2) / (1 + a1 z^-1 + a2 z^-2)
    let (nr, ni) = (c.a2 + c.a1 * cos1 + cos2, c.a1 * sin1 + sin2);
    let (dr, di) = (1.0 + c.a1 * cos1 + c.a2 * cos2, c.a1 * sin1 + c.a2 * sin2);
    let den = dr * dr + di * di;
    let (hr, hi) = ((nr * dr + ni * di) / den, (ni * dr - nr * di) / den);
    let mut acc_r = 1.0f32;
    let mut acc_i = 0.0f32;
    for _ in 0..notches {
        (acc_r, acc_i) = (acc_r * hr - acc_i * hi, acc_r * hi + acc_i * hr);
    }
    let (s_r, s_i) = (1.0 + acc_r, acc_i);
    0.5 * (s_r * s_r + s_i * s_i).sqrt()
}

pub struct PhaserFlanger {
    v: [f64; PARAMS],
    sr: f32,
    lines: [InterpDelay; 2],
    filters: [[AllPass; MAX_NOTCHES]; 2],
    coeffs: [[AllPassCoeffs; 1]; 2],
    split: [BandSplit; 2],
    split_ready: bool,
    lfo1: WaveLfo,
    lfo2: Lfo,
    env: EnvelopeFollower,
    env_level: f32,
    warmth: [Warmth; 2],
    last: [f32; 2],
    delay_samples: [f32; 2],
}

impl Default for PhaserFlanger {
    fn default() -> Self {
        Self {
            v: values(&params()),
            sr: 48_000.0,
            lines: [InterpDelay::new(16), InterpDelay::new(16)],
            filters: [[AllPass::default(); MAX_NOTCHES]; 2],
            coeffs: [[AllPassCoeffs::default(); 1]; 2],
            split: [BandSplit::default(); 2],
            split_ready: false,
            lfo1: WaveLfo::new(0xF1),
            lfo2: Lfo::new(0xF2),
            env: EnvelopeFollower::new(),
            env_level: 0.0,
            warmth: [Warmth::default(); 2],
            last: [0.0; 2],
            delay_samples: [0.0; 2],
        }
    }
}

impl PhaserFlanger {
    fn value(&self, id: u32) -> f32 {
        self.v[id as usize] as f32
    }

    fn flag(&self, id: u32) -> bool {
        self.v[id as usize] >= 0.5
    }

    fn configure_lfos(&mut self) {
        let wave = match self.v[WAVEFORM as usize].round() as i32 {
            1 => Wave::Triangle,
            2 => Wave::SawUp,
            3 => Wave::SawDown,
            4 => Wave::Rectangle,
            5 => Wave::Random,
            6 => Wave::RandomHold,
            7 => Wave::TriangleAnalog,
            8 => Wave::Triangle8,
            9 => Wave::Triangle16,
            _ => Wave::Sine,
        };
        let spinning = self.v[STEREO_MODE as usize].round() as i32 == 1;
        let rate = if self.flag(SYNC) {
            LfoRate::Beats(SyncRate::from_index(self.v[RATE as usize]).beats())
        } else {
            LfoRate::Hz(self.value(FREQ))
        };
        self.lfo1.set(&WaveSettings {
            wave,
            rate,
            phase: if spinning {
                0.0
            } else {
                self.value(PHASE) / 360.0
            },
            spin: if spinning {
                self.value(SPIN) * 0.01
            } else {
                0.0
            },
            duty: self.value(DUTY) * 0.01,
            ..Default::default()
        });
        let rate2 = if self.flag(LFO2_SYNC) {
            LfoRate::Beats(SyncRate::from_index(self.v[LFO2_RATE as usize]).beats())
        } else {
            LfoRate::Hz(self.value(LFO2_FREQ))
        };
        self.lfo2.set(&LfoSettings {
            shape: LfoShape::Triangle,
            rate: rate2,
            ..Default::default()
        });
    }

    fn render(&mut self, audio: AudioBuffer) {
        if !self.split_ready {
            let freq = self.value(SAFE_BASS);
            for s in self.split.iter_mut() {
                s.set(freq, self.sr);
            }
            self.split_ready = true;
        }
        self.configure_lfos();
        let mode = (self.v[MODE as usize].round() as usize).min(MODE_DOUBLER);
        let notches = (self.value(NOTCHES).round() as usize).clamp(1, MAX_NOTCHES);
        let amount = self.value(AMOUNT) * 0.01;
        let blend = self.value(BLEND) * 0.01;
        let lfo2_mix = self.value(LFO2_MIX) * 0.01;
        let feedback = {
            let f = self.value(FEEDBACK) * 0.01;
            if self.flag(FB_INVERT) {
                -f
            } else {
                f
            }
        };
        let env_on = self.flag(ENV_ON);
        let env_amount = self.value(ENV_AMOUNT) * 0.01;
        self.env.set_times(
            self.value(ENV_ATTACK) * 0.001,
            self.value(ENV_RELEASE) * 0.001,
        );
        let output = db_to_gain(self.value(OUTPUT));
        let warmth = self.value(WARMTH) * 0.01;
        let mix = self.value(MIX) * 0.01;
        let ms = self.sr * 0.001;
        let time = match mode {
            MODE_FLANGER => self.value(FLANGER_TIME) * ms,
            _ => self.value(DOUBLER_TIME) * ms,
        };
        let center = self.value(CENTER);
        let base_spread = self.value(SPREAD) * 0.01;

        let (left, right) = (&mut *audio.left, &mut *audio.right);
        for (lc, rc) in left
            .chunks_mut(CONTROL_FRAMES)
            .zip(right.chunks_mut(CONTROL_FRAMES))
        {
            let n = lc.len();
            let lfo = self.lfo1.advance(n);
            let two = self.lfo2.advance(n);
            let m = [
                lfo[0] + (two[0] - lfo[0]) * lfo2_mix,
                lfo[1] + (two[1] - lfo[1]) * lfo2_mix,
            ];
            let mut modulation = [m[0] * amount, m[1] * amount];
            if env_on {
                for x in lc.iter().zip(rc.iter()).map(|(l, r)| (l + r) * 0.5) {
                    self.env_level = self.env.process(x);
                }
                let e = self.env_level * env_amount;
                modulation[0] += e;
                modulation[1] += e;
            }
            let mut targets = self.delay_samples;
            for ch in 0..2 {
                let c = modulation[ch];
                match mode {
                    MODE_PHASER => {
                        let hz = center * 2f32.powf(c * (1.0 - blend) * CENTER_OCTAVES);
                        let spread = (base_spread + c * blend).clamp(0.0, 1.0);
                        self.coeffs[ch][0] = AllPassCoeffs::tuned(hz, spread_q(spread), self.sr);
                    }
                    // From the Time value, modulation only ever shortens the delay: Time at
                    // the LFO's lowest point, down by Amount at its highest.
                    MODE_FLANGER => {
                        let unipolar = (m[ch] + 1.0) * 0.5 * amount + (c - m[ch] * amount);
                        let shorter = unipolar.clamp(0.0, 1.0);
                        targets[ch] = (time * (1.0 - shorter)).max(MIN_DELAY_MS * ms);
                    }
                    _ => {
                        targets[ch] = (time * (1.0 + c * DOUBLER_DEPTH)).max(MIN_DELAY_MS * ms);
                    }
                }
            }
            for (i, (l, r)) in lc.iter_mut().zip(rc.iter_mut()).enumerate() {
                let inputs = [*l, *r];
                let mut wet = [0.0f32; 2];
                for ch in 0..2 {
                    let (low, high) = self.split[ch].split(inputs[ch]);
                    let x = high + self.last[ch] * feedback;
                    let y = if mode == MODE_PHASER {
                        let c = self.coeffs[ch][0];
                        let mut y = x;
                        for f in self.filters[ch].iter_mut().take(notches) {
                            y = f.process(y, c);
                        }
                        y
                    } else {
                        let t = (i + 1) as f32 / n as f32;
                        let d = self.delay_samples[ch] + (targets[ch] - self.delay_samples[ch]) * t;
                        let y = self.lines[ch].read((d - 1.0).max(0.0), Interpolation::Cubic);
                        self.lines[ch].write(guard(x));
                        y
                    };
                    self.last[ch] = guard(y);
                    wet[ch] = low + y;
                }
                for (w, heat) in wet.iter_mut().zip(self.warmth.iter_mut()) {
                    *w = heat.process(*w, warmth, self.sr) * output;
                }
                *l = inputs[0] * (1.0 - mix) + wet[0] * mix;
                *r = inputs[1] * (1.0 - mix) + wet[1] * mix;
            }
            self.delay_samples = targets;
        }
    }
}

impl Processor for PhaserFlanger {
    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sr = sample_rate as f32;
        let capacity = (sample_rate * 0.15) as usize;
        self.lines = [InterpDelay::new(capacity), InterpDelay::new(capacity)];
        self.lfo1.prepare(sample_rate);
        self.lfo2.prepare(sample_rate);
        self.env.prepare(sample_rate);
        self.split_ready = false;
        self.coeffs = [[AllPassCoeffs::tuned(1_000.0, 1.0, self.sr); 1]; 2];
        self.delay_samples = [self.value(FLANGER_TIME) * self.sr * 0.001; 2];
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        let clock = BeatClock::from_transport(ctx.transport);
        self.lfo1.set_clock(&clock);
        self.lfo2.set_clock(&clock);
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
        if id == SAFE_BASS {
            self.split_ready = false;
        }
    }

    fn reset(&mut self) {
        self.lines.iter_mut().for_each(InterpDelay::clear);
        for f in self.filters.iter_mut().flatten() {
            f.reset();
        }
        self.split.iter_mut().for_each(BandSplit::reset);
        self.warmth.iter_mut().for_each(Warmth::reset);
        self.last = [0.0; 2];
        self.env.reset();
        self.env_level = 0.0;
    }

    fn tail(&self) -> Tail {
        Tail::Infinite
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.phaser_flanger",
    name: "Phaser-Flanger",
    role: DeviceRole::AudioEffect,
    category: "Delay & Modulation",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::<PhaserFlanger>::default(),
};
