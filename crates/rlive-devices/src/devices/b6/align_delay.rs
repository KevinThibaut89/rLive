//! Align Delay: delays each channel by a time, a sample count, or a distance through air.
//!
//! The manual gives the three modes, Link L/R and the temperature with its Celsius or Fahrenheit
//! choice. It gives no ranges and no formula: ours are the ranges below and the dry-air speed of
//! sound `c = 331.3 * sqrt(1 + T / 273.15)` m/s, which gives 343.2 m/s at 20 degrees C.

use super::common::{knob, menu, toggle, values, DelayHead, Transition};
use crate::dsp::delay::{InterpDelay, Interpolation};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    AudioBuffer, BlockStep, DeviceDesc, ParamInfo, ProcessContext, Processor, Section, Status,
};

pub const MODE: u32 = 0;
pub const LINK: u32 = 1;
pub const TIME_L: u32 = 2;
pub const TIME_R: u32 = 3;
pub const SAMPLES_L: u32 = 4;
pub const SAMPLES_R: u32 = 5;
pub const DISTANCE_L: u32 = 6;
pub const DISTANCE_R: u32 = 7;
pub const DISTANCE_UNIT: u32 = 8;
pub const TEMPERATURE: u32 = 9;
pub const TEMPERATURE_UNIT: u32 = 10;
const PARAMS: usize = 11;

pub const MODE_TIME: usize = 0;
pub const MODE_SAMPLES: usize = 1;
pub const MODE_DISTANCE: usize = 2;

const MODES: [&str; 3] = ["Time", "Samples", "Distance"];

/// Longest setting in any mode: enough for 343 m at the coldest temperature.
const MAX_SECONDS: f64 = 2.5;
pub const METERS_PER_FOOT: f64 = 0.3048;

/// The speed of sound in dry air at `celsius`, in metres per second.
pub fn speed_of_sound(celsius: f64) -> f64 {
    331.3 * (1.0 + celsius.max(-273.0) / 273.15).sqrt()
}

/// A temperature read in degrees Fahrenheit, in Celsius.
pub fn fahrenheit_to_celsius(f: f64) -> f64 {
    (f - 32.0) * 5.0 / 9.0
}

fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::stepped(MODE, "Delay Mode", &MODES, 0),
        ParamInfo::toggle(LINK, "Link L/R", true),
        ParamInfo::skewed(TIME_L, "Delay L", 0.0, 1_000.0, 0.0, "ms", 2.0),
        ParamInfo::skewed(TIME_R, "Delay R", 0.0, 1_000.0, 0.0, "ms", 2.0),
        ParamInfo::continuous(SAMPLES_L, "Samples L", 0.0, 96_000.0, 0.0, "smp"),
        ParamInfo::continuous(SAMPLES_R, "Samples R", 0.0, 96_000.0, 0.0, "smp"),
        ParamInfo::skewed(DISTANCE_L, "Distance L", 0.0, 340.0, 0.0, "", 2.0),
        ParamInfo::skewed(DISTANCE_R, "Distance R", 0.0, 340.0, 0.0, "", 2.0),
        ParamInfo::stepped(DISTANCE_UNIT, "Distance Unit", &["Meters", "Feet"], 0),
        ParamInfo::continuous(TEMPERATURE, "Temperature", -30.0, 130.0, 20.0, ""),
        ParamInfo::stepped(TEMPERATURE_UNIT, "Temperature Unit", &["C", "F"], 0),
    ]
}

fn layout() -> Vec<Section> {
    vec![
        Section {
            title: "Mode",
            items: vec![menu(MODE), toggle(LINK)],
        },
        Section {
            title: "Time",
            items: vec![knob(TIME_L), knob(TIME_R)],
        },
        Section {
            title: "Samples",
            items: vec![knob(SAMPLES_L), knob(SAMPLES_R)],
        },
        Section {
            title: "Distance",
            items: vec![
                knob(DISTANCE_L),
                knob(DISTANCE_R),
                menu(DISTANCE_UNIT),
                knob(TEMPERATURE),
                menu(TEMPERATURE_UNIT),
            ],
        },
    ]
}

pub struct AlignDelay {
    v: [f64; PARAMS],
    sr: f32,
    lines: [InterpDelay; 2],
    heads: [DelayHead; 2],
}

impl Default for AlignDelay {
    fn default() -> Self {
        let mut heads = [DelayHead::default(); 2];
        heads
            .iter_mut()
            .for_each(|h| h.set_mode(Transition::Fade, false));
        Self {
            v: values(&params()),
            sr: 48_000.0,
            lines: [InterpDelay::new(16), InterpDelay::new(16)],
            heads,
        }
    }
}

impl AlignDelay {
    fn delay_samples(&self, channel: usize) -> f64 {
        let offset = if channel == 1 && self.v[LINK as usize] < 0.5 {
            1
        } else {
            0
        };
        let at = |id: u32| self.v[id as usize + offset];
        match self.v[MODE as usize].round() as usize {
            MODE_TIME => at(TIME_L) * 0.001 * self.sr as f64,
            MODE_SAMPLES => at(SAMPLES_L).round(),
            _ => {
                let meters = if self.v[DISTANCE_UNIT as usize].round() as i32 == 1 {
                    at(DISTANCE_L) * METERS_PER_FOOT
                } else {
                    at(DISTANCE_L)
                };
                let temperature = self.v[TEMPERATURE as usize];
                let celsius = if self.v[TEMPERATURE_UNIT as usize].round() as i32 == 1 {
                    fahrenheit_to_celsius(temperature)
                } else {
                    temperature
                };
                meters / speed_of_sound(celsius) * self.sr as f64
            }
        }
    }

    fn render(&mut self, audio: AudioBuffer) {
        for ch in 0..2 {
            let d = self.delay_samples(ch) as f32;
            self.heads[ch].set_target(d);
        }
        let (left, right) = (&mut *audio.left, &mut *audio.right);
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            self.lines[0].write(*l);
            self.lines[1].write(*r);
            let lines = &self.lines;
            *l = self.heads[0].read(|d| lines[0].read(d, Interpolation::Cubic));
            *r = self.heads[1].read(|d| lines[1].read(d, Interpolation::Cubic));
        }
    }
}

impl Processor for AlignDelay {
    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sr = sample_rate as f32;
        let capacity = (sample_rate * MAX_SECONDS) as usize;
        self.lines = [InterpDelay::new(capacity), InterpDelay::new(capacity)];
        self.heads.iter_mut().for_each(|h| h.prepare(self.sr));
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
        self.lines.iter_mut().for_each(InterpDelay::clear);
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.align_delay",
    name: "Align Delay",
    role: DeviceRole::AudioEffect,
    category: "Delay & Modulation",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::<AlignDelay>::default(),
};
