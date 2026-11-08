//! Chorus-Ensemble: two or three modulated delay lines per channel, or a single vibrato line.
//!
//! The manual gives the modes, High-Pass (20 Hz to 2 kHz), Width (0 to 200%), Delay Time (Auto or
//! a fixed value), the Taps toggle, Vibrato's Offset and Shape, Rate, Amount, Feedback and its
//! invert, Output, Warmth and Dry/Wet. It gives no delay times or depths: ours are in the
//! constants below, chosen for the classic chorus range.

use super::common::{guard, knob, menu, toggle, values, BandSplit, Warmth, CONTROL_FRAMES};
use crate::dsp::delay::{InterpDelay, Interpolation};
use crate::dsp::lfo::{Lfo, LfoRate, LfoSettings};
use crate::dsp::util::sanitize;
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    db_to_gain, AudioBuffer, BlockStep, DeviceDesc, ParamInfo, ProcessContext, Processor, Section,
    Status, Tail,
};
use std::f32::consts::TAU;

pub const MODE: u32 = 0;
pub const HP_ON: u32 = 1;
pub const HP_FREQ: u32 = 2;
pub const WIDTH: u32 = 3;
pub const DELAY_TIME: u32 = 4;
pub const TWO_TAPS: u32 = 5;
pub const OFFSET: u32 = 6;
pub const SHAPE: u32 = 7;
pub const RATE: u32 = 8;
pub const AMOUNT: u32 = 9;
pub const FEEDBACK: u32 = 10;
pub const FB_INVERT: u32 = 11;
pub const OUTPUT: u32 = 12;
pub const WARMTH: u32 = 13;
pub const MIX: u32 = 14;
const PARAMS: usize = 15;

pub const MODE_CHORUS: usize = 0;
pub const MODE_ENSEMBLE: usize = 1;
pub const MODE_VIBRATO: usize = 2;

const DELAY_CHOICES: [&str; 8] = [
    "Auto", "1 ms", "2 ms", "3 ms", "5 ms", "8 ms", "12 ms", "20 ms",
];
const DELAY_MS: [f32; 8] = [0.0, 1.0, 2.0, 3.0, 5.0, 8.0, 12.0, 20.0];

/// At full Amount, Auto mode's delay for each tap, in ms. It shrinks with Amount.
pub const AUTO_DELAY_MS: [f32; 3] = [7.0, 9.0, 11.0];
/// Peak modulation of a tap at full Amount, in ms.
pub const CHORUS_DEPTH_MS: f32 = 2.5;
/// Peak modulation of Vibrato at full Amount, in ms.
pub const VIBRATO_DEPTH_MS: f32 = 5.0;
const MIN_DELAY_MS: f32 = 0.3;
const TAP_GAIN: [f32; 3] = [1.0, std::f32::consts::FRAC_1_SQRT_2, 0.577_350_3];
const MAX_TAPS: usize = 3;

fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::stepped(MODE, "Mode", &["Chorus", "Ensemble", "Vibrato"], 0),
        ParamInfo::toggle(HP_ON, "High-Pass", false),
        ParamInfo::skewed(
            HP_FREQ,
            "High-Pass Frequency",
            20.0,
            2_000.0,
            120.0,
            "Hz",
            3.0,
        ),
        ParamInfo::continuous(WIDTH, "Width", 0.0, 200.0, 100.0, "%"),
        ParamInfo::stepped(DELAY_TIME, "Delay Time", &DELAY_CHOICES, 0),
        ParamInfo::toggle(TWO_TAPS, "Two Taps", true),
        ParamInfo::continuous(OFFSET, "Offset", 0.0, 180.0, 0.0, "deg"),
        ParamInfo::continuous(SHAPE, "Shape", 0.0, 100.0, 0.0, "%"),
        ParamInfo::skewed(RATE, "Rate", 0.02, 10.0, 1.0, "Hz", 3.0),
        ParamInfo::continuous(AMOUNT, "Amount", 0.0, 100.0, 50.0, "%"),
        ParamInfo::continuous(FEEDBACK, "Feedback", 0.0, 100.0, 0.0, "%"),
        ParamInfo::toggle(FB_INVERT, "Feedback Invert", false),
        ParamInfo::continuous(OUTPUT, "Output", -24.0, 12.0, 0.0, "dB"),
        ParamInfo::continuous(WARMTH, "Warmth", 0.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(MIX, "Dry/Wet", 0.0, 100.0, 50.0, "%"),
    ]
}

fn layout() -> Vec<Section> {
    vec![
        Section {
            title: "Mode",
            items: vec![menu(MODE), toggle(HP_ON), knob(HP_FREQ)],
        },
        Section {
            title: "Chorus",
            items: vec![knob(WIDTH), menu(DELAY_TIME), toggle(TWO_TAPS)],
        },
        Section {
            title: "Vibrato",
            items: vec![knob(OFFSET), knob(SHAPE)],
        },
        Section {
            title: "Modulation",
            items: vec![knob(RATE), knob(AMOUNT), knob(FEEDBACK), toggle(FB_INVERT)],
        },
        Section {
            title: "Global",
            items: vec![knob(OUTPUT), knob(WARMTH), knob(MIX)],
        },
    ]
}

/// Sine at 0, triangle at 1, by blending the two.
#[inline]
pub fn vibrato_wave(phase: f32, shape: f32) -> f32 {
    let p = phase.fract();
    let tri = if p < 0.25 {
        4.0 * p
    } else if p < 0.75 {
        2.0 - 4.0 * p
    } else {
        4.0 * p - 4.0
    };
    let sine = (p * TAU).sin();
    sine + (tri - sine) * shape.clamp(0.0, 1.0)
}

pub struct ChorusEnsemble {
    v: [f64; PARAMS],
    sr: f32,
    lines: [InterpDelay; 2],
    lfo: Lfo,
    split: [BandSplit; 2],
    split_ready: bool,
    warmth: [Warmth; 2],
    /// Each channel's delay per tap at the end of the last control step, in samples.
    delays: [[f32; MAX_TAPS]; 2],
    placed: bool,
    /// The wet output of each channel, for feedback.
    last: [f32; 2],
}

impl Default for ChorusEnsemble {
    fn default() -> Self {
        Self {
            v: values(&params()),
            sr: 48_000.0,
            lines: [InterpDelay::new(16), InterpDelay::new(16)],
            lfo: Lfo::new(0xC0),
            split: [BandSplit::default(); 2],
            split_ready: false,
            warmth: [Warmth::default(); 2],
            delays: [[0.0; MAX_TAPS]; 2],
            placed: false,
            last: [0.0; 2],
        }
    }
}

impl ChorusEnsemble {
    fn value(&self, id: u32) -> f32 {
        self.v[id as usize] as f32
    }

    fn flag(&self, id: u32) -> bool {
        self.v[id as usize] >= 0.5
    }

    /// The delays this control step should reach, in samples, per channel and tap.
    fn targets(
        &self,
        phases: [f32; 2],
        mode: usize,
        amount: f32,
        taps: usize,
    ) -> [[f32; MAX_TAPS]; 2] {
        let ms = self.sr * 0.001;
        let mut out = [[0.0; MAX_TAPS]; 2];
        for (ch, channel) in out.iter_mut().enumerate() {
            if mode == MODE_VIBRATO {
                let m = vibrato_wave(phases[ch], self.value(SHAPE) * 0.01);
                let depth = amount * VIBRATO_DEPTH_MS;
                channel[0] = (MIN_DELAY_MS + depth * (1.0 + m)).max(MIN_DELAY_MS) * ms;
                continue;
            }
            let fixed = DELAY_MS
                [(self.v[DELAY_TIME as usize].round().max(0.0) as usize).min(DELAY_MS.len() - 1)];
            for (tap, delay) in channel.iter_mut().enumerate().take(taps) {
                let base = if fixed > 0.0 {
                    fixed * (1.0 + 0.2 * tap as f32)
                } else {
                    amount * AUTO_DELAY_MS[tap]
                };
                let phase = phases[ch] + tap as f32 / taps as f32;
                let m = (phase * TAU).sin();
                *delay = (base + amount * CHORUS_DEPTH_MS * m).max(MIN_DELAY_MS) * ms;
            }
        }
        out
    }

    fn render(&mut self, audio: AudioBuffer) {
        if !self.split_ready {
            let freq = self.value(HP_FREQ);
            for s in self.split.iter_mut() {
                s.set(freq, self.sr);
            }
            self.split_ready = true;
        }
        let mode = (self.v[MODE as usize].round() as usize).min(MODE_VIBRATO);
        let vibrato = mode == MODE_VIBRATO;
        let taps = match mode {
            MODE_CHORUS => {
                if self.flag(TWO_TAPS) {
                    2
                } else {
                    1
                }
            }
            MODE_ENSEMBLE => 3,
            _ => 1,
        };
        let amount = self.value(AMOUNT) * 0.01;
        let feedback = if vibrato {
            0.0
        } else {
            let f = self.value(FEEDBACK) * 0.0099;
            if self.flag(FB_INVERT) {
                -f
            } else {
                f
            }
        };
        let hp_on = self.flag(HP_ON);
        let warmth = self.value(WARMTH) * 0.01;
        let output = db_to_gain(self.value(OUTPUT));
        let width = if vibrato {
            1.0
        } else {
            self.value(WIDTH) * 0.01
        };
        let mix = if vibrato { 1.0 } else { self.value(MIX) * 0.01 };
        self.lfo.set(&LfoSettings {
            rate: LfoRate::Hz(self.value(RATE)),
            phase: if vibrato {
                self.value(OFFSET) / 360.0
            } else {
                0.25
            },
            ..Default::default()
        });
        let tap_gain = TAP_GAIN[taps - 1];
        let (left, right) = (&mut *audio.left, &mut *audio.right);
        for (lc, rc) in left
            .chunks_mut(CONTROL_FRAMES)
            .zip(right.chunks_mut(CONTROL_FRAMES))
        {
            let n = lc.len();
            let phases = self.lfo.advance_phases(n);
            let target = self.targets(phases, mode, amount, taps);
            if !self.placed {
                self.delays = target;
                self.placed = true;
            }
            for (i, (l, r)) in lc.iter_mut().zip(rc.iter_mut()).enumerate() {
                let t = (i + 1) as f32 / n as f32;
                let inputs = [*l, *r];
                let mut wet = [0.0f32; 2];
                for ch in 0..2 {
                    let (low, high) = if hp_on {
                        self.split[ch].split(inputs[ch])
                    } else {
                        (0.0, inputs[ch])
                    };
                    let mut out = 0.0;
                    for (from, to) in self.delays[ch].iter().zip(&target[ch]).take(taps) {
                        let d = from + (to - from) * t;
                        out += self.lines[ch].read(d - 1.0, Interpolation::Cubic);
                    }
                    out *= tap_gain;
                    self.lines[ch].write(guard(high + self.last[ch] * feedback));
                    self.last[ch] = sanitize(out);
                    wet[ch] = low + out;
                }
                for (w, heat) in wet.iter_mut().zip(self.warmth.iter_mut()) {
                    *w = heat.process(*w, warmth, self.sr) * output;
                }
                // Mid and side levels: the side at 100% matches the mid, at 200% twice it.
                let mid = (wet[0] + wet[1]) * 0.5;
                let side = (wet[0] - wet[1]) * 0.5 * width;
                *l = inputs[0] * (1.0 - mix) + (mid + side) * mix;
                *r = inputs[1] * (1.0 - mix) + (mid - side) * mix;
            }
            self.delays = target;
        }
    }
}

impl Processor for ChorusEnsemble {
    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sr = sample_rate as f32;
        let capacity = (sample_rate * 0.06) as usize;
        self.lines = [InterpDelay::new(capacity), InterpDelay::new(capacity)];
        self.lfo.prepare(sample_rate);
        self.split_ready = false;
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
        if id == HP_FREQ {
            self.split_ready = false;
        }
    }

    fn reset(&mut self) {
        self.lines.iter_mut().for_each(InterpDelay::clear);
        self.split.iter_mut().for_each(BandSplit::reset);
        self.warmth.iter_mut().for_each(Warmth::reset);
        self.last = [0.0; 2];
        self.placed = false;
    }

    fn tail(&self) -> Tail {
        Tail::Infinite
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.chorus_ensemble",
    name: "Chorus-Ensemble",
    role: DeviceRole::AudioEffect,
    category: "Delay & Modulation",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::<ChorusEnsemble>::default(),
};
