//! DS Snare (manual p. 837): a pitched oscillator and filtered noise.

use super::common::*;
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{ParamInfo, Section, Status};

use crate::dsp::filter::{Filter, FilterKind, FilterSettings};
use std::f32::consts::TAU;

pub const COLOR: u32 = 0;
pub const TONE: u32 = 1;
pub const FILTER: u32 = 2;
pub const DECAY: u32 = 3;
pub const TUNE: u32 = 4;
pub const VOLUME: u32 = 5;

fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::continuous(COLOR, "Color", 0.0, 100.0, 50.0, "%"),
        ParamInfo::continuous(TONE, "Tone", 0.0, 100.0, 50.0, "%"),
        ParamInfo::stepped(
            FILTER,
            "Filter Type",
            &["Low-Pass", "High-Pass", "Band-Pass"],
            1,
        ),
        decay_param(DECAY, 200.0),
        ParamInfo::continuous(TUNE, "Tune", -24.0, 24.0, 0.0, "st"),
        volume_param(VOLUME),
    ]
}

fn layout() -> Vec<Section> {
    vec![
        display_section(),
        Section {
            title: "Snare",
            items: vec![
                knob(COLOR),
                knob(TONE),
                menu(FILTER),
                knob(DECAY),
                knob(TUNE),
                knob(VOLUME),
            ],
        },
    ]
}

pub struct Snare {
    color: f32,
    tone: f32,
    kind: FilterKind,
    decay: f32,
    tune: f32,
    volume: f32,
    phase: f32,
    amp: OneShot,
    noise_env: OneShot,
    pitch_env: f32,
    pitch_coef: f32,
    filter: Filter,
    noise: Noise,
    velocity: f32,
    dirty: bool,
}

impl Default for Snare {
    fn default() -> Self {
        Self {
            color: 0.5,
            tone: 0.5,
            kind: FilterKind::HighPass,
            decay: 0.2,
            tune: 0.0,
            volume: 1.0,
            phase: 0.0,
            amp: OneShot::default(),
            noise_env: OneShot::default(),
            pitch_env: 0.0,
            pitch_coef: 0.0,
            filter: Filter::new(),
            noise: Noise::new(0x1dea),
            velocity: 1.0,
            dirty: true,
        }
    }
}

impl DrumVoice for Snare {
    fn set(&mut self, id: u32, value: f64) {
        match id {
            COLOR => self.color = (value * 0.01) as f32,
            TONE => self.tone = (value * 0.01) as f32,
            FILTER => {
                self.kind = match value.round() as i32 {
                    0 => FilterKind::LowPass,
                    1 => FilterKind::HighPass,
                    _ => FilterKind::BandPass,
                };
                self.dirty = true;
            }
            DECAY => self.decay = value as f32 * 0.001,
            TUNE => self.tune = value as f32,
            VOLUME => self.volume = gain(value),
            _ => {}
        }
    }

    fn trigger(&mut self, velocity: f32, sample_rate: f32) {
        self.amp.trigger(0.0, self.decay * 0.35, sample_rate);
        self.noise_env.trigger(0.0, self.decay, sample_rate);
        self.phase = 0.0;
        self.velocity = velocity;
        self.pitch_env = 1.0;
        self.pitch_coef = decay_coefficient(0.03, sample_rate);
        if self.dirty {
            self.filter.prepare(f64::from(sample_rate));
            self.filter.set(&FilterSettings {
                kind: self.kind,
                cutoff: match self.kind {
                    FilterKind::LowPass => 6_000.0,
                    FilterKind::HighPass => 1_800.0,
                    _ => 3_500.0,
                },
                resonance: 0.2,
                ..Default::default()
            });
            self.dirty = false;
        }
    }

    fn next(&mut self, sample_rate: f32) -> [f32; 2] {
        let a = self.amp.next();
        let n = self.noise_env.next();
        self.pitch_env *= self.pitch_coef;
        let semis = self.tune + self.pitch_env * 6.0;
        let hz = 180.0 * 2f32.powf(semis / 12.0);
        self.phase = (self.phase + hz / sample_rate).fract();
        // Color moves the pitched part from a pure sine towards a harder triangle-rich tone.
        let sine = (self.phase * TAU).sin();
        let harsh = (sine * (1.0 + self.color * 5.0)).clamp(-1.0, 1.0);
        let tone = sine + (harsh - sine) * self.color;
        let noisy = self.filter.process(self.noise.white());
        let out = (tone * a * 0.6 + noisy * n * self.tone * 1.1) * self.velocity;
        [out, out]
    }

    fn is_active(&self) -> bool {
        self.amp.is_active() || self.noise_env.is_active()
    }

    fn silence(&mut self) {
        self.amp.off();
        self.noise_env.off();
        self.filter.reset();
    }

    fn volume(&self) -> f32 {
        self.volume
    }

    fn tail_seconds(&self) -> f32 {
        self.decay
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.ds_snare",
    name: "DS Snare",
    role: DeviceRole::Instrument,
    category: "Drums",
    status: Status::Beta,
    desc: || desc(params(), layout()),
    create: |_| Box::new(DrumSynth::new(Snare::default())),
};
