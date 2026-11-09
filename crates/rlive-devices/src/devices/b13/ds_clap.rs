//! DS Clap (manual p. 832): filtered noise and an impulse through panned delays.

use super::common::*;
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{ParamInfo, Section, Status};

use crate::dsp::filter::{Filter, FilterKind, FilterSettings};

pub const SLOPPY: u32 = 0;
pub const TAIL: u32 = 1;
pub const SPREAD: u32 = 2;
pub const TONE: u32 = 3;
pub const TUNE: u32 = 4;
pub const DECAY: u32 = 5;
pub const VOLUME: u32 = 6;

fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::continuous(SLOPPY, "Sloppy", 0.0, 100.0, 25.0, "%"),
        ParamInfo::continuous(TAIL, "Tail", 0.0, 100.0, 50.0, "%"),
        ParamInfo::continuous(SPREAD, "Spread", 0.0, 100.0, 50.0, "%"),
        ParamInfo::continuous(TONE, "Tone", 0.0, 100.0, 50.0, "%"),
        ParamInfo::continuous(TUNE, "Tune", -24.0, 24.0, 0.0, "st"),
        decay_param(DECAY, 250.0),
        volume_param(VOLUME),
    ]
}

fn layout() -> Vec<Section> {
    vec![
        display_section(),
        Section {
            title: "Clap",
            items: vec![
                knob(SLOPPY),
                knob(TAIL),
                knob(SPREAD),
                knob(TONE),
                knob(TUNE),
                knob(DECAY),
                knob(VOLUME),
            ],
        },
    ]
}

/// How many hand claps make up the attack.
const BURSTS: usize = 4;

pub struct Clap {
    sloppy: f32,
    tail: f32,
    spread: f32,
    tone: f32,
    tune: f32,
    decay: f32,
    volume: f32,
    /// Samples since the hit, and the longest it runs.
    t: f32,
    length: f32,
    burst: [OneShot; 2],
    next_burst: [f32; 2],
    bursts_left: [usize; 2],
    tail_env: OneShot,
    filters: [Filter; 2],
    noise: [Noise; 2],
    velocity: f32,
    active: bool,
    prepared: bool,
}

impl Default for Clap {
    fn default() -> Self {
        Self {
            sloppy: 0.25,
            tail: 0.5,
            spread: 0.5,
            tone: 0.5,
            tune: 0.0,
            decay: 0.25,
            volume: 1.0,
            t: 0.0,
            length: 0.0,
            burst: [OneShot::default(); 2],
            next_burst: [0.0; 2],
            bursts_left: [0; 2],
            tail_env: OneShot::default(),
            filters: [Filter::new(), Filter::new()],
            noise: [Noise::new(0x9001), Noise::new(0x5e7a)],
            velocity: 1.0,
            active: false,
            prepared: false,
        }
    }
}

impl Clap {
    fn tune_filters(&mut self, sample_rate: f32) {
        let ratio = 2f32.powf(self.tune / 12.0);
        let settings = FilterSettings {
            kind: FilterKind::BandPass,
            cutoff: (600.0 + self.tone * 3_000.0) * ratio,
            resonance: 0.35,
            ..Default::default()
        };
        for f in &mut self.filters {
            f.prepare(f64::from(sample_rate));
            f.set(&settings);
        }
        self.prepared = true;
    }
}

impl DrumVoice for Clap {
    fn set(&mut self, id: u32, value: f64) {
        let unit = (value * 0.01) as f32;
        match id {
            SLOPPY => self.sloppy = unit,
            TAIL => self.tail = unit,
            SPREAD => self.spread = unit,
            TONE => {
                self.tone = unit;
                self.prepared = false;
            }
            TUNE => {
                self.tune = value as f32;
                self.prepared = false;
            }
            DECAY => self.decay = value as f32 * 0.001,
            VOLUME => self.volume = gain(value),
            _ => {}
        }
    }

    fn trigger(&mut self, velocity: f32, sample_rate: f32) {
        if !self.prepared {
            self.tune_filters(sample_rate);
        }
        self.velocity = velocity;
        self.t = 0.0;
        // The right-hand line runs Sloppy later than the left.
        let lag = self.sloppy * 0.03 * sample_rate;
        self.next_burst = [0.0, lag];
        self.bursts_left = [BURSTS; 2];
        self.burst = [OneShot::default(); 2];
        self.tail_env
            .trigger(0.002, self.decay.max(0.02), sample_rate);
        self.active = true;
        self.length = (self.decay + 0.1) * sample_rate;
    }

    fn next(&mut self, sample_rate: f32) -> [f32; 2] {
        let gap = 0.009 * sample_rate;
        let mut hands = [0.0f32; 2];
        for (ch, hand) in hands.iter_mut().enumerate() {
            if self.bursts_left[ch] > 0 && self.t >= self.next_burst[ch] {
                self.burst[ch].trigger(0.0005, 0.02, sample_rate);
                self.bursts_left[ch] -= 1;
                self.next_burst[ch] += gap;
            }
            *hand = self.burst[ch].next();
        }
        let tail_level = self.tail_env.next() * self.tail;
        let mut out = [0.0f32; 2];
        for (ch, o) in out.iter_mut().enumerate() {
            let source = self.noise[ch].white();
            *o = self.filters[ch].process(source) * (hands[ch] + tail_level) * 2.5;
        }
        self.t += 1.0;
        if self.t > self.length {
            self.active = false;
        }
        let m = (out[0] + out[1]) * 0.5;
        let s = (out[0] - out[1]) * 0.5 * self.spread;
        [(m + s) * self.velocity, (m - s) * self.velocity]
    }

    fn is_active(&self) -> bool {
        self.active
    }

    fn silence(&mut self) {
        self.active = false;
        self.tail_env.off();
        self.burst = [OneShot::default(); 2];
        self.bursts_left = [0; 2];
        for f in &mut self.filters {
            f.reset();
        }
    }

    fn volume(&self) -> f32 {
        self.volume
    }

    fn tail_seconds(&self) -> f32 {
        self.decay + 0.1
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.ds_clap",
    name: "DS Clap",
    role: DeviceRole::Instrument,
    category: "Drums",
    status: Status::Beta,
    desc: || desc(params(), layout()),
    create: |_| Box::new(DrumSynth::new(Clap::default())),
};
