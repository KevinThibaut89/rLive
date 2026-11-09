//! DS Tom (manual p. 838): an impulse and pitched oscillators through resonant filters.

use super::common::*;
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{ParamInfo, Section, Status};

use crate::dsp::filter::{Filter, FilterKind, FilterSettings};
use std::f32::consts::TAU;

pub const PITCH: u32 = 0;
pub const COLOR: u32 = 1;
pub const TONE: u32 = 2;
pub const BEND: u32 = 3;
pub const DECAY: u32 = 4;
pub const VOLUME: u32 = 5;

fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::skewed(PITCH, "Pitch", 40.0, 400.0, 120.0, "Hz", 2.0),
        ParamInfo::continuous(COLOR, "Color", 0.0, 100.0, 50.0, "%"),
        ParamInfo::continuous(TONE, "Tone", 0.0, 100.0, 30.0, "%"),
        ParamInfo::continuous(BEND, "Bend", 0.0, 100.0, 35.0, "%"),
        decay_param(DECAY, 350.0),
        volume_param(VOLUME),
    ]
}

fn layout() -> Vec<Section> {
    vec![
        display_section(),
        Section {
            title: "Tom",
            items: vec![
                knob(PITCH),
                knob(COLOR),
                knob(TONE),
                knob(BEND),
                knob(DECAY),
                knob(VOLUME),
            ],
        },
    ]
}

pub struct Tom {
    hz: f32,
    color: f32,
    tone: f32,
    bend: f32,
    decay: f32,
    volume: f32,
    phase: [f32; 2],
    amp: OneShot,
    pitch_env: f32,
    pitch_coef: f32,
    click: f32,
    click_coef: f32,
    low_pass: Filter,
    membrane: [Filter; 2],
    noise: Noise,
    velocity: f32,
}

impl Default for Tom {
    fn default() -> Self {
        Self {
            hz: 120.0,
            color: 0.5,
            tone: 0.3,
            bend: 0.35,
            decay: 0.35,
            volume: 1.0,
            phase: [0.0; 2],
            amp: OneShot::default(),
            pitch_env: 0.0,
            pitch_coef: 0.0,
            click: 0.0,
            click_coef: 0.0,
            low_pass: Filter::new(),
            membrane: [Filter::new(), Filter::new()],
            noise: Noise::new(0x70ad),
            velocity: 1.0,
        }
    }
}

impl DrumVoice for Tom {
    fn set(&mut self, id: u32, value: f64) {
        let unit = (value * 0.01) as f32;
        match id {
            PITCH => self.hz = value as f32,
            COLOR => self.color = unit,
            TONE => self.tone = unit,
            BEND => self.bend = unit,
            DECAY => self.decay = value as f32 * 0.001,
            VOLUME => self.volume = gain(value),
            _ => {}
        }
    }

    fn trigger(&mut self, velocity: f32, sample_rate: f32) {
        self.amp.trigger(0.0, self.decay, sample_rate);
        self.phase = [0.0; 2];
        self.velocity = velocity;
        self.pitch_env = 1.0;
        self.pitch_coef = decay_coefficient(0.08, sample_rate);
        self.click = 1.0;
        self.click_coef = decay_coefficient(0.003, sample_rate);
        // Color sets the filter's cutoff and how hard the signal hits it.
        self.low_pass.prepare(f64::from(sample_rate));
        self.low_pass.set(&FilterSettings {
            kind: FilterKind::LowPass,
            cutoff: 300.0 + self.color * 6_000.0,
            resonance: 0.1,
            drive: self.color * 12.0,
            ..Default::default()
        });
        for (i, ratio) in [1.59f32, 2.14].into_iter().enumerate() {
            self.membrane[i].prepare(f64::from(sample_rate));
            self.membrane[i].set(&FilterSettings {
                kind: FilterKind::BandPass,
                cutoff: self.hz * ratio,
                resonance: 0.82,
                ..Default::default()
            });
        }
    }

    fn next(&mut self, sample_rate: f32) -> [f32; 2] {
        let a = self.amp.next();
        self.pitch_env *= self.pitch_coef;
        let hz = self.hz * (1.0 + self.pitch_env * self.bend * 3.0);
        self.phase[0] = (self.phase[0] + hz / sample_rate).fract();
        self.phase[1] = (self.phase[1] + hz * 1.5 / sample_rate).fract();
        let body = (self.phase[0] * TAU).sin() + 0.35 * (self.phase[1] * TAU).sin();
        self.click *= self.click_coef;
        let impulse = self.noise.white() * self.click;
        let excited = body * a + impulse * 0.6;
        let shaped = self.low_pass.process(excited);
        let ring = self.membrane[0].process(excited) + self.membrane[1].process(excited);
        let out = (shaped + ring * self.tone) * self.velocity * 0.9;
        [out, out]
    }

    fn is_active(&self) -> bool {
        self.amp.is_active()
    }

    fn silence(&mut self) {
        self.amp.off();
        self.click = 0.0;
        self.low_pass.reset();
        self.membrane.iter_mut().for_each(Filter::reset);
    }

    fn volume(&self) -> f32 {
        self.volume
    }

    fn tail_seconds(&self) -> f32 {
        self.decay
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.ds_tom",
    name: "DS Tom",
    role: DeviceRole::Instrument,
    category: "Drums",
    status: Status::Beta,
    desc: || desc(params(), layout()),
    create: |_| Box::new(DrumSynth::new(Tom::default())),
};
