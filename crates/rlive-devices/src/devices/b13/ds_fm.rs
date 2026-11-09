//! DS FM (manual p. 834): two-operator FM with feedback.

use super::common::*;
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{ParamInfo, Section, Status};

use crate::dsp::filter::{Filter, FilterKind, FilterSettings};
use std::f32::consts::TAU;

pub const TONE: u32 = 0;
pub const FEEDBACK: u32 = 1;
pub const AMOUNT: u32 = 2;
pub const MOD: u32 = 3;
pub const PITCH: u32 = 4;
pub const DECAY: u32 = 5;
pub const VOLUME: u32 = 6;

fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::continuous(TONE, "Tone", 0.0, 100.0, 80.0, "%"),
        ParamInfo::continuous(FEEDBACK, "Feedb.", 0.0, 100.0, 20.0, "%"),
        ParamInfo::continuous(AMOUNT, "Amnt", 0.0, 100.0, 40.0, "%"),
        ParamInfo::continuous(MOD, "Mod", 0.0, 100.0, 0.0, "%"),
        ParamInfo::skewed(PITCH, "Pitch", 30.0, 1200.0, 110.0, "Hz", 3.0),
        decay_param(DECAY, 300.0),
        volume_param(VOLUME),
    ]
}

fn layout() -> Vec<Section> {
    vec![
        display_section(),
        Section {
            title: "FM",
            items: vec![
                knob(TONE),
                knob(FEEDBACK),
                knob(AMOUNT),
                knob(MOD),
                knob(PITCH),
                knob(DECAY),
                knob(VOLUME),
            ],
        },
    ]
}

pub struct Fm {
    tone: f32,
    feedback: f32,
    amount: f32,
    mod_blend: f32,
    hz: f32,
    decay: f32,
    volume: f32,
    carrier: f32,
    modulator: f32,
    last: f32,
    amp: OneShot,
    index_env: f32,
    index_coef: f32,
    lp: Filter,
    noise: Noise,
    velocity: f32,
}

impl Default for Fm {
    fn default() -> Self {
        Self {
            tone: 0.8,
            feedback: 0.2,
            amount: 0.4,
            mod_blend: 0.0,
            hz: 110.0,
            decay: 0.3,
            volume: 1.0,
            carrier: 0.0,
            modulator: 0.0,
            last: 0.0,
            amp: OneShot::default(),
            index_env: 0.0,
            index_coef: 0.0,
            lp: Filter::new(),
            noise: Noise::new(0xf33d),
            velocity: 1.0,
        }
    }
}

impl DrumVoice for Fm {
    fn set(&mut self, id: u32, value: f64) {
        let unit = (value * 0.01) as f32;
        match id {
            TONE => self.tone = unit,
            FEEDBACK => self.feedback = unit,
            AMOUNT => self.amount = unit,
            MOD => self.mod_blend = unit,
            PITCH => self.hz = value as f32,
            DECAY => self.decay = value as f32 * 0.001,
            VOLUME => self.volume = gain(value),
            _ => {}
        }
    }

    fn trigger(&mut self, velocity: f32, sample_rate: f32) {
        self.amp.trigger(0.0, self.decay, sample_rate);
        self.velocity = velocity;
        self.carrier = 0.0;
        self.modulator = 0.0;
        self.last = 0.0;
        self.index_env = 1.0;
        self.index_coef = decay_coefficient(self.decay.max(0.04), sample_rate);
        self.lp.prepare(f64::from(sample_rate));
        self.lp.set(&FilterSettings {
            kind: FilterKind::LowPass,
            cutoff: 200.0 + self.tone * self.tone * 18_000.0,
            resonance: 0.0,
            ..Default::default()
        });
    }

    fn next(&mut self, sample_rate: f32) -> [f32; 2] {
        let a = self.amp.next();
        self.index_env *= self.index_coef;
        let index = self.amount * 8.0 * self.index_env;
        // Mod blends between a fixed-ratio modulator and a noise one.
        let tuned = (self.modulator * TAU + self.feedback * self.last * TAU).sin();
        let pm = tuned + (self.noise.white() - tuned) * self.mod_blend;
        self.last = pm;
        self.carrier = (self.carrier + self.hz / sample_rate).fract();
        self.modulator = (self.modulator + self.hz * 1.41 / sample_rate).fract();
        let out = self.lp.process((self.carrier * TAU + pm * index).sin()) * a * self.velocity;
        [out, out]
    }

    fn is_active(&self) -> bool {
        self.amp.is_active()
    }

    fn silence(&mut self) {
        self.amp.off();
        self.lp.reset();
    }

    fn volume(&self) -> f32 {
        self.volume
    }

    fn tail_seconds(&self) -> f32 {
        self.decay
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.ds_fm",
    name: "DS FM",
    role: DeviceRole::Instrument,
    category: "Drums",
    status: Status::Beta,
    desc: || desc(params(), layout()),
    create: |_| Box::new(DrumSynth::new(Fm::default())),
};
