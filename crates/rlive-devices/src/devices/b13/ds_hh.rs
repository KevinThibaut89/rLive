//! DS HH (manual p. 835): noise and sines, with a resonant high-pass on the pitched part.

use super::common::*;
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{ParamInfo, Section, Status};

use crate::dsp::filter::{Filter, FilterKind, FilterSettings, FilterSlope};
use std::f32::consts::TAU;

pub const NOISE: u32 = 0;
pub const TONE: u32 = 1;
pub const FILTER: u32 = 2;
pub const ATTACK: u32 = 3;
pub const PITCH: u32 = 4;
pub const DECAY: u32 = 5;
pub const VOLUME: u32 = 6;

fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::stepped(NOISE, "Noise", &["White", "Pink"], 0),
        ParamInfo::continuous(TONE, "Tone", 0.0, 100.0, 60.0, "%"),
        ParamInfo::stepped(FILTER, "Filter", &["12 dB", "24 dB"], 1),
        ParamInfo::skewed(ATTACK, "Attack", 0.0, 50.0, 0.0, "ms", 2.0),
        ParamInfo::continuous(PITCH, "Pitch", -24.0, 24.0, 0.0, "st"),
        decay_param(DECAY, 120.0),
        volume_param(VOLUME),
    ]
}

fn layout() -> Vec<Section> {
    vec![
        display_section(),
        Section {
            title: "Hat",
            items: vec![
                menu(NOISE),
                knob(TONE),
                menu(FILTER),
                knob(ATTACK),
                knob(PITCH),
                knob(DECAY),
                knob(VOLUME),
            ],
        },
    ]
}

const PARTIALS: [f32; 6] = [205.3, 304.4, 369.6, 522.7, 540.0, 800.0];

pub struct HiHat {
    pink: bool,
    tone: f32,
    slope: FilterSlope,
    attack: f32,
    pitch: f32,
    decay: f32,
    volume: f32,
    phase: [f32; 6],
    amp: OneShot,
    pitched_hp: Filter,
    noise_hp: Filter,
    noise: Noise,
    velocity: f32,
}

impl Default for HiHat {
    fn default() -> Self {
        Self {
            pink: false,
            tone: 0.6,
            slope: FilterSlope::Db24,
            attack: 0.0,
            pitch: 0.0,
            decay: 0.12,
            volume: 1.0,
            phase: [0.0; 6],
            amp: OneShot::default(),
            pitched_hp: Filter::new(),
            noise_hp: Filter::new(),
            noise: Noise::new(0xbead),
            velocity: 1.0,
        }
    }
}

impl DrumVoice for HiHat {
    fn set(&mut self, id: u32, value: f64) {
        match id {
            NOISE => self.pink = value >= 0.5,
            TONE => self.tone = (value * 0.01) as f32,
            FILTER => {
                self.slope = if value >= 0.5 {
                    FilterSlope::Db24
                } else {
                    FilterSlope::Db12
                }
            }
            ATTACK => self.attack = value as f32 * 0.001,
            PITCH => self.pitch = value as f32,
            DECAY => self.decay = value as f32 * 0.001,
            VOLUME => self.volume = gain(value),
            _ => {}
        }
    }

    fn trigger(&mut self, velocity: f32, sample_rate: f32) {
        self.amp.trigger(self.attack, self.decay, sample_rate);
        self.velocity = velocity;
        let cutoff = 2_000.0 + self.tone * self.tone * 11_000.0;
        self.noise_hp.prepare(f64::from(sample_rate));
        self.noise_hp.set(&FilterSettings {
            kind: FilterKind::HighPass,
            cutoff,
            resonance: 0.1,
            ..Default::default()
        });
        self.pitched_hp.prepare(f64::from(sample_rate));
        self.pitched_hp.set(&FilterSettings {
            kind: FilterKind::HighPass,
            slope: self.slope,
            cutoff: 5_000.0 * 2f32.powf(self.pitch / 12.0),
            resonance: 0.55,
            ..Default::default()
        });
    }

    fn next(&mut self, sample_rate: f32) -> [f32; 2] {
        let a = self.amp.next();
        let ratio = 2f32.powf(self.pitch / 12.0) * 8.0;
        let mut pitched = 0.0;
        for (i, hz) in PARTIALS.iter().enumerate() {
            self.phase[i] = (self.phase[i] + hz * ratio / sample_rate).fract();
            pitched += (self.phase[i] * TAU).sin();
        }
        let tone = self.pitched_hp.process(pitched / PARTIALS.len() as f32);
        let source = if self.pink {
            self.noise.pink()
        } else {
            self.noise.white()
        };
        let hiss = self.noise_hp.process(source);
        let out = (hiss * 0.8 + tone * 1.2) * a * self.velocity;
        [out, out]
    }

    fn is_active(&self) -> bool {
        self.amp.is_active()
    }

    fn silence(&mut self) {
        self.amp.off();
        self.pitched_hp.reset();
        self.noise_hp.reset();
        self.noise.reset();
    }

    fn volume(&self) -> f32 {
        self.volume
    }

    fn tail_seconds(&self) -> f32 {
        self.decay + self.attack
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.ds_hh",
    name: "DS HH",
    role: DeviceRole::Instrument,
    category: "Drums",
    status: Status::Beta,
    desc: || desc(params(), layout()),
    create: |_| Box::new(DrumSynth::new(HiHat::default())),
};
