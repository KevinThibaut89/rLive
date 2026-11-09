//! LFO (manual pp. 843-845): a low-frequency oscillator for mapped parameters.
//!
//! The manual names the waveforms and says what Shape, Steps, Jitter and Smooth are for, but gives
//! no ranges or formulas. Random, Bin, Stray and Glider are a value per cycle held, held at one of
//! two levels, reached in a straight line and reached in a raised-cosine glide. The rest of the
//! figures are ours too: 0 to 40 Hz (and x10), Steps up to 24 (that one the manual gives).

use super::common::*;
use crate::dsp::tempo::{BeatClock, SyncRate};
use crate::dsp::util::Rng;
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    BlockStep, DeviceDesc, Item, ParamInfo, Ports, ProcessContext, Processor, Section, Status,
};
use std::f32::consts::TAU;

pub const WAVEFORM: u32 = 0;
pub const SHAPE: u32 = 1;
pub const STEPS: u32 = 2;
pub const JITTER: u32 = 3;
pub const SMOOTH: u32 = 4;
pub const RATE: u32 = 5;
pub const TIME_MODE: u32 = 6;
pub const TIMES_TEN: u32 = 7;
pub const SYNC: u32 = 8;
pub const DEPTH: u32 = 9;
pub const OFFSET: u32 = 10;
pub const PHASE: u32 = 11;
pub const HOLD: u32 = 12;
pub const RETRIGGER: u32 = 13;

pub const WAVES: [&str; 9] = [
    "Sine", "Up", "Down", "Triangle", "Square", "Random", "Bin", "Stray", "Glider",
];

const CONTROL: usize = 32;

fn params() -> Vec<ParamInfo> {
    let mut out = vec![
        ParamInfo::stepped(WAVEFORM, "Waveform", &WAVES, 0),
        ParamInfo::continuous(SHAPE, "Shape", -100.0, 100.0, 0.0, "%"),
        ParamInfo::stepped(
            STEPS,
            "Steps",
            &[
                "Off", "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14",
                "15", "16", "17", "18", "19", "20", "21", "22", "23", "24",
            ],
            0,
        ),
        ParamInfo::continuous(JITTER, "Jitter", 0.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(SMOOTH, "Smooth", 0.0, 100.0, 0.0, "%"),
        ParamInfo::skewed(RATE, "Rate", 0.01, 40.0, 1.0, "Hz", 3.0),
        ParamInfo::stepped(TIME_MODE, "Time Mode", &["Hz", "Sync"], 0),
        ParamInfo::toggle(TIMES_TEN, "x10", false),
        ParamInfo::stepped(SYNC, "Sync Rate", &SyncRate::LABELS, 12),
        ParamInfo::continuous(DEPTH, "Depth", 0.0, 100.0, 100.0, "%"),
        ParamInfo::continuous(OFFSET, "Offset", -100.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(PHASE, "Phase", 0.0, 100.0, 0.0, "%"),
        ParamInfo::toggle(HOLD, "Hold", false),
        ParamInfo::toggle(RETRIGGER, "R", false),
    ];
    out.extend(map_info(true));
    out
}

fn layout() -> Vec<Section> {
    vec![
        map_section(),
        Section {
            title: "Display",
            items: vec![Item::Panel("modulator_scope")],
        },
        Section {
            title: "Wave",
            items: vec![
                menu(WAVEFORM),
                knob(SHAPE),
                knob(STEPS),
                knob(JITTER),
                knob(SMOOTH),
            ],
        },
        Section {
            title: "Rate",
            items: vec![
                knob(RATE),
                menu(TIME_MODE),
                toggle(TIMES_TEN),
                menu(SYNC),
                knob(DEPTH),
                knob(OFFSET),
                knob(PHASE),
                toggle(HOLD),
                toggle(RETRIGGER),
            ],
        },
    ]
}

/// Bends the time within a cycle. Zero leaves it alone.
#[inline]
fn warp(p: f32, shape: f32) -> f32 {
    if shape == 0.0 {
        p
    } else if shape > 0.0 {
        p.powf(1.0 + 3.0 * shape)
    } else {
        1.0 - (1.0 - p).powf(1.0 - 3.0 * shape)
    }
}

pub struct Lfo {
    sample_rate: f64,
    wave: usize,
    shape: f32,
    steps: u32,
    hz: f32,
    sync: bool,
    x10: bool,
    sync_index: f64,
    phase: f32,
    hold: bool,
    held_pos: f64,
    pending_retrigger: bool,
    clock: Clock,
    rng: Rng,
    prev: f32,
    cur: f32,
    cond: Conditioner,
    position: f32,
    map: MapOutput,
    scope: ScopeFeed,
    sub: usize,
}

impl Lfo {
    pub fn with_seed(seed: u32) -> Self {
        let mut rng = Rng::new(seed);
        let (prev, cur) = (rng.bipolar(), rng.bipolar());
        Self {
            sample_rate: 48_000.0,
            wave: 0,
            shape: 0.0,
            steps: 0,
            hz: 1.0,
            sync: false,
            x10: false,
            sync_index: 12.0,
            phase: 0.0,
            hold: false,
            held_pos: 0.0,
            pending_retrigger: false,
            clock: Clock::default(),
            rng,
            prev,
            cur,
            cond: Conditioner::new(seed ^ 0x55aa),
            position: 0.5,
            map: MapOutput::new(true),
            scope: ScopeFeed::default(),
            sub: 0,
        }
    }

    fn rate(&self) -> Rate {
        let hz = if self.x10 { self.hz * 10.0 } else { self.hz };
        Rate::of(self.sync, hz, self.sync_index)
    }

    fn apply(&mut self, id: u32, value: f64) {
        if self.map.apply(id, value) {
            return;
        }
        match id {
            WAVEFORM => self.wave = (value.round() as usize).min(WAVES.len() - 1),
            SHAPE => self.shape = (value * 0.01) as f32,
            STEPS => self.steps = value.round().clamp(0.0, 24.0) as u32,
            JITTER => self.cond.jitter = (value * 0.01) as f32,
            SMOOTH => self.cond.smooth = (value * 0.01) as f32,
            RATE => self.hz = value as f32,
            TIME_MODE => self.sync = value >= 0.5,
            TIMES_TEN => self.x10 = value >= 0.5,
            SYNC => self.sync_index = value,
            DEPTH => self.cond.depth = (value * 0.01) as f32,
            OFFSET => self.cond.offset = (value * 0.01) as f32,
            PHASE => self.phase = (value * 0.01) as f32,
            HOLD => {
                let hold = value >= 0.5;
                if hold != self.hold {
                    if hold {
                        self.held_pos = self.clock.pos;
                    } else {
                        // Carry on from where it froze (manual p. 845).
                        let (pos, rate) = (self.held_pos, self.rate());
                        self.clock.resume_at(pos, rate);
                    }
                    self.hold = hold;
                }
            }
            RETRIGGER => self.pending_retrigger = true,
            _ => {}
        }
    }

    /// The wave at a position in its cycle, in -1..1.
    #[inline]
    fn wave_at(&self, p: f32) -> f32 {
        let random_only = matches!(self.wave, 5 | 6);
        let stepped = self.steps > 0 && !random_only && self.wave != 4;
        let p = if stepped {
            (p * self.steps as f32).floor() / self.steps as f32
        } else {
            p
        };
        let shaped = if self.wave <= 3 {
            warp(p, self.shape)
        } else {
            p
        };
        match self.wave {
            0 => (shaped * TAU).sin(),
            1 => 2.0 * shaped - 1.0,
            2 => 1.0 - 2.0 * shaped,
            3 => {
                if shaped < 0.25 {
                    4.0 * shaped
                } else if shaped < 0.75 {
                    2.0 - 4.0 * shaped
                } else {
                    4.0 * shaped - 4.0
                }
            }
            4 => {
                let duty = 0.5 + 0.45 * self.shape;
                if p < duty {
                    1.0
                } else {
                    -1.0
                }
            }
            5 => self.cur,
            6 => {
                if self.cur >= 0.0 {
                    1.0
                } else {
                    -1.0
                }
            }
            7 => self.prev + (self.cur - self.prev) * p,
            _ => {
                let t = 0.5 - 0.5 * (p * std::f32::consts::PI).cos();
                self.prev + (self.cur - self.prev) * t
            }
        }
    }

    fn control_step(&mut self, n: usize) {
        if self.pending_retrigger {
            self.pending_retrigger = false;
            let rate = self.rate();
            self.clock.retrigger(rate);
            self.prev = self.cur;
            self.cur = self.rng.bipolar();
        }
        if self.hold {
            self.scope.push(self.position);
            return;
        }
        let rate = self.rate();
        if self.clock.advance(n, self.sample_rate, rate) {
            self.prev = self.cur;
            self.cur = self.rng.bipolar();
        }
        let p = ((self.clock.pos + f64::from(self.phase)).fract() as f32).min(0.999_999);
        let wave = self.wave_at(p);
        self.position = self.cond.step(wave, (n as f64 / self.sample_rate) as f32);
        self.scope.push(self.position);
    }

    fn advance(&mut self, mut frames: usize) {
        while frames > 0 {
            let n = (CONTROL - self.sub).min(frames);
            self.sub += n;
            frames -= n;
            if self.sub == CONTROL {
                self.sub = 0;
                self.control_step(CONTROL);
            }
        }
    }

    /// The current position, 0 to 1.
    pub fn position(&self) -> f32 {
        self.position
    }
}

impl Processor for Lfo {
    fn activate(&mut self, sample_rate: f64, _max_block: usize) {
        self.sample_rate = sample_rate.max(1.0);
        self.clock.clock = BeatClock::new(120.0, self.sample_rate);
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        let tempo_clock = BeatClock::from_transport(ctx.transport);
        self.clock.clock = tempo_clock;
        let mut seen = 0;
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => self.apply(id, value),
            BlockStep::Midi(_) => {}
            BlockStep::Audio(a) => {
                let f = a.frames();
                seen += f;
                self.advance(f);
            }
        });
        let map = self.map;
        map.write(self.position, ctx.mod_out);
        if ctx.view.is_active() {
            self.scope
                .publish(ctx.view, seen as f32 / self.sample_rate as f32);
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        self.apply(id, value);
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.lfo",
    name: "LFO",
    role: DeviceRole::AudioEffect,
    category: "Modulators",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        ports: Ports {
            mod_out: MOD_PORTS.to_vec(),
            ..Default::default()
        },
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::new(Lfo::with_seed(next_seed())),
};
