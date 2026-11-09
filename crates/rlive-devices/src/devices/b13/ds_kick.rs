//! DS Kick (manual p. 836): a modulated sine with drive, overtones and a click.

use super::common::*;
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{ParamInfo, Section, Status};

use std::f32::consts::TAU;

pub const PITCH: u32 = 0;
pub const DRIVE: u32 = 1;
pub const OT: u32 = 2;
pub const ATTACK: u32 = 3;
pub const CLICK: u32 = 4;
pub const DECAY: u32 = 5;
pub const ENV: u32 = 6;
pub const VOLUME: u32 = 7;

fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::skewed(PITCH, "Pitch", 20.0, 200.0, 55.0, "Hz", 2.0),
        ParamInfo::continuous(DRIVE, "Drive", 0.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(OT, "OT", 0.0, 100.0, 0.0, "%"),
        ParamInfo::skewed(ATTACK, "Attack", 0.0, 50.0, 0.0, "ms", 2.0),
        ParamInfo::toggle(CLICK, "Click", true),
        decay_param(DECAY, 300.0),
        ParamInfo::continuous(ENV, "Env", 0.0, 100.0, 50.0, "%"),
        volume_param(VOLUME),
    ]
}

fn layout() -> Vec<Section> {
    vec![
        display_section(),
        Section {
            title: "Kick",
            items: vec![
                knob(PITCH),
                knob(DRIVE),
                knob(OT),
                knob(ATTACK),
                toggle(CLICK),
                knob(DECAY),
                knob(ENV),
                knob(VOLUME),
            ],
        },
    ]
}

pub struct Kick {
    hz: f32,
    drive: f32,
    ot: f32,
    attack: f32,
    click: bool,
    decay: f32,
    env_amount: f32,
    volume: f32,
    phase: f32,
    phase2: f32,
    amp: OneShot,
    pitch_env: f32,
    pitch_coef: f32,
    click_env: f32,
    click_coef: f32,
    velocity: f32,
    noise: Noise,
}

impl Default for Kick {
    fn default() -> Self {
        Self {
            hz: 55.0,
            drive: 0.0,
            ot: 0.0,
            attack: 0.0,
            click: true,
            decay: 0.3,
            env_amount: 0.5,
            volume: 1.0,
            phase: 0.0,
            phase2: 0.0,
            amp: OneShot::default(),
            pitch_env: 0.0,
            pitch_coef: 0.0,
            click_env: 0.0,
            click_coef: 0.0,
            velocity: 1.0,
            noise: Noise::new(0x4b1c),
        }
    }
}

impl DrumVoice for Kick {
    fn set(&mut self, id: u32, value: f64) {
        match id {
            PITCH => self.hz = value as f32,
            DRIVE => self.drive = (value * 0.01) as f32,
            OT => self.ot = (value * 0.01) as f32,
            ATTACK => self.attack = value as f32 * 0.001,
            CLICK => self.click = value >= 0.5,
            DECAY => self.decay = value as f32 * 0.001,
            ENV => self.env_amount = (value * 0.01) as f32,
            VOLUME => self.volume = gain(value),
            _ => {}
        }
    }

    fn trigger(&mut self, velocity: f32, sample_rate: f32) {
        self.amp.trigger(self.attack, self.decay, sample_rate);
        self.phase = 0.0;
        self.phase2 = 0.0;
        self.velocity = velocity;
        self.pitch_env = 1.0;
        self.pitch_coef = decay_coefficient(0.06, sample_rate);
        self.click_env = if self.click { 1.0 } else { 0.0 };
        self.click_coef = decay_coefficient(0.004, sample_rate);
    }

    fn next(&mut self, sample_rate: f32) -> [f32; 2] {
        let a = self.amp.next();
        self.pitch_env *= self.pitch_coef;
        let hz = self.hz * (1.0 + self.pitch_env * self.env_amount * 12.0);
        self.phase = (self.phase + hz / sample_rate).fract();
        self.phase2 = (self.phase2 + hz * 2.76 / sample_rate).fract();
        let body = (self.phase * TAU).sin() + self.ot * 0.5 * (self.phase2 * TAU).sin();
        let driven = if self.drive > 0.0 {
            let k = 1.0 + self.drive * 9.0;
            (body * k).tanh() / k.tanh()
        } else {
            body
        };
        self.click_env *= self.click_coef;
        let click = self.noise.white() * self.click_env * 0.5;
        let out = (driven * a + click) * self.velocity;
        [out, out]
    }

    fn is_active(&self) -> bool {
        self.amp.is_active()
    }

    fn silence(&mut self) {
        self.amp.off();
        self.click_env = 0.0;
    }

    fn volume(&self) -> f32 {
        self.volume
    }

    fn tail_seconds(&self) -> f32 {
        self.decay
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.ds_kick",
    name: "DS Kick",
    role: DeviceRole::Instrument,
    category: "Drums",
    status: Status::Beta,
    desc: || desc(params(), layout()),
    create: |_| Box::new(DrumSynth::new(Kick::default())),
};
