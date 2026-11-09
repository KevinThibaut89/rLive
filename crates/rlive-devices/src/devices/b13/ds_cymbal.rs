//! DS Cymbal (manual p. 833): sines and pulses with high-passed noise.

use super::common::*;
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{ParamInfo, Section, Status};

use crate::dsp::filter::{Filter, FilterKind, FilterSettings};
use std::f32::consts::TAU;

pub const TONE: u32 = 0;
pub const PITCH: u32 = 1;
pub const DECAY: u32 = 2;
pub const VOLUME: u32 = 3;

fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::continuous(TONE, "Tone", 0.0, 100.0, 50.0, "%"),
        ParamInfo::continuous(PITCH, "Pitch", -24.0, 24.0, 0.0, "st"),
        decay_param(DECAY, 1200.0),
        volume_param(VOLUME),
    ]
}

fn layout() -> Vec<Section> {
    vec![
        display_section(),
        Section {
            title: "Cymbal",
            items: vec![knob(TONE), knob(PITCH), knob(DECAY), knob(VOLUME)],
        },
    ]
}

const RATIOS: [f32; 5] = [1.0, 1.483, 1.932, 2.546, 3.117];

pub struct Cymbal {
    tone: f32,
    pitch: f32,
    decay: f32,
    volume: f32,
    phase: [f32; 5],
    amp: OneShot,
    hp: Filter,
    noise: Noise,
    velocity: f32,
}

impl Default for Cymbal {
    fn default() -> Self {
        Self {
            tone: 0.5,
            pitch: 0.0,
            decay: 1.2,
            volume: 1.0,
            phase: [0.0; 5],
            amp: OneShot::default(),
            hp: Filter::new(),
            noise: Noise::new(0xc1ba),
            velocity: 1.0,
        }
    }
}

impl DrumVoice for Cymbal {
    fn set(&mut self, id: u32, value: f64) {
        match id {
            TONE => self.tone = (value * 0.01) as f32,
            PITCH => self.pitch = value as f32,
            DECAY => self.decay = value as f32 * 0.001,
            VOLUME => self.volume = gain(value),
            _ => {}
        }
    }

    fn trigger(&mut self, velocity: f32, sample_rate: f32) {
        self.amp.trigger(0.0, self.decay, sample_rate);
        self.velocity = velocity;
        self.hp.prepare(f64::from(sample_rate));
        self.hp.set(&FilterSettings {
            kind: FilterKind::HighPass,
            cutoff: 1_500.0 + self.tone * self.tone * 9_000.0,
            resonance: 0.15,
            ..Default::default()
        });
    }

    fn next(&mut self, sample_rate: f32) -> [f32; 2] {
        let a = self.amp.next();
        let base = 420.0 * 2f32.powf(self.pitch / 12.0);
        let mut partials = 0.0;
        for (i, r) in RATIOS.iter().enumerate() {
            self.phase[i] = (self.phase[i] + base * r / sample_rate).fract();
            partials += if i % 2 == 0 {
                (self.phase[i] * TAU).sin()
            } else if self.phase[i] < 0.5 {
                1.0
            } else {
                -1.0
            };
        }
        let out = self.hp.process(partials * 0.15 + self.noise.white() * 0.5) * a * self.velocity;
        [out, out]
    }

    fn is_active(&self) -> bool {
        self.amp.is_active()
    }

    fn silence(&mut self) {
        self.amp.off();
        self.hp.reset();
    }

    fn volume(&self) -> f32 {
        self.volume
    }

    fn tail_seconds(&self) -> f32 {
        self.decay
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.ds_cymbal",
    name: "DS Cymbal",
    role: DeviceRole::Instrument,
    category: "Drums",
    status: Status::Beta,
    desc: || desc(params(), layout()),
    create: |_| Box::new(DrumSynth::new(Cymbal::default())),
};
