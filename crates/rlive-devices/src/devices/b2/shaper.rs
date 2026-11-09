//! Shaper (manual pp. 846-848): a breakpoint envelope as modulation, looped, fired or scrubbed.
//!
//! The manual gives no ranges for Rate, Jitter and so on; those are ours, matching the LFO. Its
//! six presets are unnamed, and [`PRESETS`](super::breakpoints::PRESETS) are ours as well.

use super::breakpoints::ParsedShape;
use super::common::*;
use crate::devices::b10::common::STATE_SLOT;
use crate::dsp::tempo::{BeatClock, SyncRate};
use crate::{BuiltinInfo, DeviceInit, DeviceRole};
use rlive_core::{
    BlockStep, DeviceDesc, Item, ParamInfo, Ports, ProcessContext, Processor, Resource, Section,
    Status,
};
use std::sync::Arc;

pub const MODE: u32 = 0;
pub const RATE: u32 = 1;
pub const TIME_MODE: u32 = 2;
pub const SYNC: u32 = 3;
pub const DEPTH: u32 = 4;
pub const OFFSET: u32 = 5;
pub const PHASE: u32 = 6;
pub const JITTER: u32 = 7;
pub const SMOOTH: u32 = 8;
pub const MANUAL: u32 = 9;
pub const TRIGGER: u32 = 10;
pub const RETRIGGER: u32 = 11;
pub const GRID: u32 = 12;
pub const SNAP: u32 = 13;

const CONTROL: usize = 32;

fn params() -> Vec<ParamInfo> {
    let mut out = vec![
        ParamInfo::stepped(MODE, "Trigger Mode", &["Loop", "1-Shot", "Manual"], 0),
        ParamInfo::skewed(RATE, "Rate", 0.01, 40.0, 1.0, "Hz", 3.0),
        ParamInfo::stepped(TIME_MODE, "Time Mode", &["Hz", "Sync"], 1),
        ParamInfo::stepped(SYNC, "Sync Rate", &SyncRate::LABELS, 16),
        ParamInfo::continuous(DEPTH, "Depth", 0.0, 100.0, 100.0, "%"),
        ParamInfo::continuous(OFFSET, "Offset", -100.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(PHASE, "Phase", 0.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(JITTER, "Jitter", 0.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(SMOOTH, "Smooth", 0.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(MANUAL, "Manual", 0.0, 100.0, 0.0, "%"),
        ParamInfo::toggle(TRIGGER, "T", false),
        ParamInfo::toggle(RETRIGGER, "R", false),
        ParamInfo::stepped(
            GRID,
            "Grid",
            &["2", "3", "4", "6", "8", "12", "16", "24", "32"],
            4,
        ),
        ParamInfo::toggle(SNAP, "Snap", true),
    ];
    out.extend(map_info(true));
    out
}

fn layout() -> Vec<Section> {
    vec![
        map_section(),
        Section {
            title: "Shape",
            items: vec![Item::Panel("shaper_editor"), menu(GRID), toggle(SNAP)],
        },
        Section {
            title: "Scope",
            items: vec![
                Item::Panel("modulator_scope"),
                knob(JITTER),
                knob(SMOOTH),
                knob(OFFSET),
                knob(PHASE),
                toggle(RETRIGGER),
            ],
        },
        Section {
            title: "Play",
            items: vec![
                menu(MODE),
                toggle(TRIGGER),
                knob(MANUAL),
                knob(RATE),
                menu(TIME_MODE),
                menu(SYNC),
                knob(DEPTH),
            ],
        },
    ]
}

pub struct Shaper {
    sample_rate: f64,
    mode: usize,
    hz: f32,
    sync: bool,
    sync_index: f64,
    phase: f64,
    manual: f32,
    retrigger: bool,
    fire: bool,
    clock: Clock,
    one_shot: f64,
    one_shot_running: bool,
    shape: Arc<ParsedShape>,
    cond: Conditioner,
    position: f32,
    map: MapOutput,
    scope: ScopeFeed,
    sub: usize,
}

impl Shaper {
    pub fn new(state: Option<&str>, seed: u32) -> Self {
        Self {
            sample_rate: 48_000.0,
            mode: 0,
            hz: 1.0,
            sync: true,
            sync_index: 16.0,
            phase: 0.0,
            manual: 0.0,
            retrigger: false,
            fire: false,
            clock: Clock::default(),
            one_shot: 1.0,
            one_shot_running: false,
            shape: Arc::new(ParsedShape::parse(state)),
            cond: Conditioner::new(seed),
            position: 0.5,
            map: MapOutput::new(true),
            scope: ScopeFeed::default(),
            sub: 0,
        }
    }

    fn rate(&self) -> Rate {
        Rate::of(self.sync, self.hz, self.sync_index)
    }

    fn apply(&mut self, id: u32, value: f64) {
        if self.map.apply(id, value) {
            return;
        }
        match id {
            MODE => self.mode = value.round().clamp(0.0, 2.0) as usize,
            RATE => self.hz = value as f32,
            TIME_MODE => self.sync = value >= 0.5,
            SYNC => self.sync_index = value,
            DEPTH => self.cond.depth = (value * 0.01) as f32,
            OFFSET => self.cond.offset = (value * 0.01) as f32,
            PHASE => self.phase = value * 0.01,
            JITTER => self.cond.jitter = (value * 0.01) as f32,
            SMOOTH => self.cond.smooth = (value * 0.01) as f32,
            MANUAL => self.manual = (value * 0.01) as f32,
            TRIGGER => self.fire = true,
            RETRIGGER => self.retrigger = true,
            _ => {}
        }
    }

    fn control_step(&mut self, n: usize) {
        let rate = self.rate();
        if self.retrigger {
            self.retrigger = false;
            // Greyed out while Sync is on (manual p. 847).
            if !self.sync {
                self.clock.retrigger(rate);
            }
        }
        let fire = std::mem::take(&mut self.fire);
        let time = match self.mode {
            1 => {
                if fire {
                    self.one_shot = 0.0;
                    self.one_shot_running = true;
                    self.scope.trigger();
                }
                if self.one_shot_running {
                    self.one_shot += self.clock.increment(n, self.sample_rate, rate);
                    if self.one_shot >= 1.0 {
                        self.one_shot = 1.0;
                        self.one_shot_running = false;
                    }
                }
                self.one_shot as f32
            }
            2 => self.manual,
            _ => {
                self.clock.advance(n, self.sample_rate, rate);
                ((self.clock.pos + self.phase).fract() as f32).min(0.999_999)
            }
        };
        let wave = self.shape.0.value_at(time);
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

    pub fn position(&self) -> f32 {
        self.position
    }
}

impl Processor for Shaper {
    fn activate(&mut self, sample_rate: f64, _max_block: usize) {
        self.sample_rate = sample_rate.max(1.0);
        self.clock.clock = BeatClock::new(120.0, self.sample_rate);
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        self.clock.clock = BeatClock::from_transport(ctx.transport);
        let mut seen = 0;
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => self.apply(id, value),
            BlockStep::Midi(_) => {}
            BlockStep::Audio(a) => {
                seen += a.frames();
                self.advance(a.frames());
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

    fn set_resource(&mut self, slot: u32, resource: Resource) -> Option<Resource> {
        if slot != STATE_SLOT {
            return Some(resource);
        }
        match resource.downcast::<ParsedShape>() {
            Ok(parsed) => Some(std::mem::replace(&mut self.shape, parsed) as Resource),
            Err(other) => Some(other),
        }
    }
}

fn create(init: &DeviceInit) -> Box<dyn Processor> {
    Box::new(Shaper::new(init.state.as_deref(), next_seed()))
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.shaper",
    name: "Shaper",
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
    create,
};
