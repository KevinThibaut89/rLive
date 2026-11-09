//! DS Clang (manual p. 831): two cowbell tones and noise through a filter, or a clave.

use super::common::*;
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{ParamInfo, Section, Status};

use crate::dsp::filter::{Filter, FilterKind, FilterSettings};
use std::f32::consts::TAU;

pub const TONE_A: u32 = 0;
pub const TONE_B: u32 = 1;
pub const FILTER: u32 = 2;
pub const NOISE: u32 = 3;
pub const CLAVE: u32 = 4;
pub const REPEAT: u32 = 5;
pub const PITCH: u32 = 6;
pub const DECAY: u32 = 7;
pub const VOLUME: u32 = 8;

fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::continuous(TONE_A, "Tone A", 0.0, 100.0, 80.0, "%"),
        ParamInfo::continuous(TONE_B, "Tone B", 0.0, 100.0, 80.0, "%"),
        ParamInfo::continuous(FILTER, "Filter", 0.0, 100.0, 60.0, "%"),
        ParamInfo::continuous(NOISE, "Noise", 0.0, 100.0, 0.0, "%"),
        ParamInfo::toggle(CLAVE, "Clave", false),
        ParamInfo::stepped(REPEAT, "Repeat", &["0", "1", "2", "3", "4"], 0),
        ParamInfo::continuous(PITCH, "Pitch", -24.0, 24.0, 0.0, "st"),
        decay_param(DECAY, 200.0),
        volume_param(VOLUME),
    ]
}

fn layout() -> Vec<Section> {
    vec![
        display_section(),
        Section {
            title: "Clang",
            items: vec![
                knob(TONE_A),
                knob(TONE_B),
                knob(FILTER),
                knob(NOISE),
                toggle(CLAVE),
                knob(REPEAT),
                knob(PITCH),
                knob(DECAY),
                knob(VOLUME),
            ],
        },
    ]
}

pub struct Clang {
    tone_a: f32,
    tone_b: f32,
    filter: f32,
    noise_amount: f32,
    clave: bool,
    repeat: usize,
    pitch: f32,
    decay: f32,
    volume: f32,
    phase: [f32; 2],
    amp: OneShot,
    hp: Filter,
    bp: Filter,
    noise: Noise,
    velocity: f32,
    /// Clave repeats still to come, and samples to the next.
    repeats_left: usize,
    next_repeat: f32,
    repeat_gain: f32,
    active_until: f32,
    t: f32,
}

impl Default for Clang {
    fn default() -> Self {
        Self {
            tone_a: 0.8,
            tone_b: 0.8,
            filter: 0.6,
            noise_amount: 0.0,
            clave: false,
            repeat: 0,
            pitch: 0.0,
            decay: 0.2,
            volume: 1.0,
            phase: [0.0; 2],
            amp: OneShot::default(),
            hp: Filter::new(),
            bp: Filter::new(),
            noise: Noise::new(0xc10a),
            velocity: 1.0,
            repeats_left: 0,
            next_repeat: 0.0,
            repeat_gain: 1.0,
            active_until: 0.0,
            t: 0.0,
        }
    }
}

const REPEAT_GAP: f32 = 0.06;

impl DrumVoice for Clang {
    fn set(&mut self, id: u32, value: f64) {
        let unit = (value * 0.01) as f32;
        match id {
            TONE_A => self.tone_a = unit,
            TONE_B => self.tone_b = unit,
            FILTER => self.filter = unit,
            NOISE => self.noise_amount = unit,
            CLAVE => self.clave = value >= 0.5,
            REPEAT => self.repeat = value.round().clamp(0.0, 4.0) as usize,
            PITCH => self.pitch = value as f32,
            DECAY => self.decay = value as f32 * 0.001,
            VOLUME => self.volume = gain(value),
            _ => {}
        }
    }

    fn trigger(&mut self, velocity: f32, sample_rate: f32) {
        let decay = if self.clave {
            self.decay.min(0.08)
        } else {
            self.decay
        };
        self.amp.trigger(0.0, decay, sample_rate);
        self.phase = [0.0; 2];
        self.velocity = velocity;
        self.t = 0.0;
        self.repeats_left = if self.clave { self.repeat } else { 0 };
        self.repeat_gain = 1.0;
        self.next_repeat = REPEAT_GAP * sample_rate;
        self.active_until = (decay + self.repeat as f32 * REPEAT_GAP + 0.02) * sample_rate;
        let cutoff = 400.0 + self.filter * self.filter * 7_000.0;
        for (f, kind) in [
            (&mut self.hp, FilterKind::HighPass),
            (&mut self.bp, FilterKind::BandPass),
        ] {
            f.prepare(f64::from(sample_rate));
            f.set(&FilterSettings {
                kind,
                cutoff,
                resonance: 0.4,
                ..Default::default()
            });
        }
    }

    fn next(&mut self, sample_rate: f32) -> [f32; 2] {
        if self.repeats_left > 0 && self.t >= self.next_repeat {
            self.repeats_left -= 1;
            self.next_repeat += REPEAT_GAP * sample_rate;
            self.repeat_gain *= 0.7;
            self.amp.trigger(0.0, self.decay.min(0.08), sample_rate);
        }
        let a = self.amp.next();
        let ratio = 2f32.powf(self.pitch / 12.0);
        let (hz_a, hz_b) = (540.0 * ratio, 800.0 * ratio);
        self.phase[0] = (self.phase[0] + hz_a / sample_rate).fract();
        self.phase[1] = (self.phase[1] + hz_b / sample_rate).fract();
        let voice = |p: f32, clave: bool| {
            if clave {
                (p * TAU).sin()
            } else if p < 0.5 {
                1.0
            } else {
                -1.0
            }
        };
        let tones = voice(self.phase[0], self.clave) * self.tone_a
            + voice(self.phase[1], self.clave) * self.tone_b;
        let source = tones * 0.5 + self.noise.white() * self.noise_amount;
        let hit = self.hp.process(source) * 0.5 + self.bp.process(source);
        self.t += 1.0;
        let out = hit * a * self.velocity * self.repeat_gain;
        [out, out]
    }

    fn is_active(&self) -> bool {
        self.t < self.active_until || self.amp.is_active()
    }

    fn silence(&mut self) {
        self.amp.off();
        self.repeats_left = 0;
        self.active_until = 0.0;
        self.t = 0.0;
        self.hp.reset();
        self.bp.reset();
    }

    fn volume(&self) -> f32 {
        self.volume
    }

    fn tail_seconds(&self) -> f32 {
        self.decay + self.repeat as f32 * REPEAT_GAP
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.ds_clang",
    name: "DS Clang",
    role: DeviceRole::Instrument,
    category: "Drums",
    status: Status::Beta,
    desc: || desc(params(), layout()),
    create: |_| Box::new(DrumSynth::new(Clang::default())),
};
