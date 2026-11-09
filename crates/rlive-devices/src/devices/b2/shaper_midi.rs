//! Shaper MIDI (manual pp. 862-864): a breakpoint envelope fired by each note.
//!
//! The manual names Echo and Time but gives no ranges; those, and how long echoes are kept, are
//! ours. The sounding envelopes are summed and kept within -1 to 1.

use super::breakpoints::ParsedShape;
use super::common::*;
use crate::devices::b10::common::STATE_SLOT;
use crate::dsp::tempo::{BeatClock, SyncRate};
use crate::{BuiltinInfo, DeviceInit, DeviceRole};
use rlive_core::{
    BlockStep, DeviceDesc, Item, MidiMessage, ParamInfo, Ports, ProcessContext, Processor,
    Resource, Section, Status,
};
use std::sync::Arc;

pub const RATE: u32 = 0;
pub const TIME_MODE: u32 = 1;
pub const SYNC: u32 = 2;
pub const DEPTH: u32 = 3;
pub const OFFSET: u32 = 4;
pub const JITTER: u32 = 5;
pub const SMOOTH: u32 = 6;
pub const VELOCITY: u32 = 7;
pub const LOOP: u32 = 8;
pub const ECHO: u32 = 9;
pub const TIME: u32 = 10;
pub const GRID: u32 = 11;
pub const SNAP: u32 = 12;

const CONTROL: usize = 32;
/// How many envelopes can sound at once, the first and its echoes.
const VOICES: usize = 16;

fn params() -> Vec<ParamInfo> {
    let mut out = vec![
        ParamInfo::skewed(RATE, "Rate", 0.01, 40.0, 1.0, "Hz", 3.0),
        ParamInfo::stepped(TIME_MODE, "Time Mode", &["Hz", "Sync"], 0),
        ParamInfo::stepped(SYNC, "Sync Rate", &SyncRate::LABELS, 12),
        ParamInfo::continuous(DEPTH, "Depth", 0.0, 100.0, 100.0, "%"),
        ParamInfo::continuous(OFFSET, "Offset", -100.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(JITTER, "Jitter", 0.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(SMOOTH, "Smooth", 0.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(VELOCITY, "Velocity", 0.0, 100.0, 0.0, "%"),
        ParamInfo::toggle(LOOP, "Loop", false),
        ParamInfo::continuous(ECHO, "Echo", 0.0, 100.0, 0.0, "%"),
        ParamInfo::skewed(TIME, "Time", 10.0, 2000.0, 250.0, "ms", 2.0),
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
                knob(VELOCITY),
                knob(OFFSET),
            ],
        },
        Section {
            title: "Play",
            items: vec![
                toggle(LOOP),
                knob(RATE),
                menu(TIME_MODE),
                menu(SYNC),
                knob(DEPTH),
                knob(ECHO),
                knob(TIME),
            ],
        },
    ]
}

#[derive(Clone, Copy, Default)]
struct Voice {
    time: f64,
    gain: f32,
    running: bool,
    /// An echo: it plays to its end, and doesn't wait at the sustain point.
    echo: bool,
}

pub struct ShaperMidi {
    sample_rate: f64,
    hz: f32,
    sync: bool,
    sync_index: f64,
    velocity_amount: f32,
    looped: bool,
    echo: f32,
    time_ms: f32,
    clock: Clock,
    voices: [Voice; VOICES],
    held: u32,
    /// Samples until the next echo, and its level.
    echo_in: Option<(f64, f32)>,
    shape: Arc<ParsedShape>,
    cond: Conditioner,
    position: f32,
    map: MapOutput,
    scope: ScopeFeed,
    sub: usize,
    through: ThroughNotes,
}

impl ShaperMidi {
    pub fn new(state: Option<&str>, seed: u32) -> Self {
        Self {
            sample_rate: 48_000.0,
            hz: 1.0,
            sync: false,
            sync_index: 12.0,
            velocity_amount: 0.0,
            looped: false,
            echo: 0.0,
            time_ms: 250.0,
            clock: Clock::default(),
            voices: [Voice::default(); VOICES],
            held: 0,
            echo_in: None,
            shape: Arc::new(ParsedShape::parse(state)),
            cond: Conditioner::new(seed),
            position: 0.5,
            map: MapOutput::new(true),
            scope: ScopeFeed::default(),
            sub: 0,
            through: ThroughNotes::default(),
        }
    }

    fn rate(&self) -> Rate {
        Rate::of(self.sync, self.hz, self.sync_index)
    }

    fn apply(&mut self, id: u32, value: f64) {
        if self.map.apply(id, value) {
            return;
        }
        let unit = (value * 0.01) as f32;
        match id {
            RATE => self.hz = value as f32,
            TIME_MODE => self.sync = value >= 0.5,
            SYNC => self.sync_index = value,
            DEPTH => self.cond.depth = unit,
            OFFSET => self.cond.offset = unit,
            JITTER => self.cond.jitter = unit,
            SMOOTH => self.cond.smooth = unit,
            VELOCITY => self.velocity_amount = unit,
            LOOP => self.looped = value >= 0.5,
            ECHO => self.echo = unit,
            TIME => self.time_ms = value as f32,
            _ => {}
        }
    }

    fn start(&mut self, gain: f32, echo: bool) {
        let free = self
            .voices
            .iter()
            .position(|v| !v.running)
            .unwrap_or(VOICES - 1);
        self.voices[free] = Voice {
            time: 0.0,
            gain,
            running: true,
            echo,
        };
        self.scope.trigger();
    }

    fn note_on(&mut self, velocity: f32) {
        self.held += 1;
        // Velocity scales the swing down, and at 0% has no effect (manual p. 863).
        let gain = 1.0 - self.velocity_amount * (1.0 - velocity);
        // A new note takes over from what was playing.
        self.voices = [Voice::default(); VOICES];
        self.start(gain, false);
        self.echo_in =
            (self.echo > 0.0).then_some((self.echo_samples(), gain * self.echo.clamp(0.0, 0.98)));
    }

    fn note_off(&mut self) {
        self.held = self.held.saturating_sub(1);
    }

    fn echo_samples(&self) -> f64 {
        f64::from(self.time_ms) * 0.001 * self.sample_rate
    }

    fn control_step(&mut self, n: usize) {
        if let Some((left, gain)) = self.echo_in {
            let left = left - n as f64;
            if left <= 0.0 {
                self.start(gain, true);
                let next = gain * self.echo.clamp(0.0, 0.98);
                self.echo_in = (next > 1e-3).then_some((self.echo_samples(), next));
            } else {
                self.echo_in = Some((left, gain));
            }
        }
        let rate = self.rate();
        let step = self.clock.increment(n, self.sample_rate, rate);
        let sustain_time = self
            .shape
            .0
            .sustain
            .and_then(|i| self.shape.0.points.get(i))
            .map(|p| f64::from(p.time));
        let shape = self.shape.clone();
        let mut sum = None;
        let held = self.held > 0;
        let looped = self.looped;
        for v in &mut self.voices {
            if !v.running {
                continue;
            }
            // Loop bypasses the sustain point (manual p. 864).
            let hold_at = sustain_time.filter(|_| held && !looped && !v.echo);
            match hold_at {
                Some(at) if v.time >= at => v.time = at,
                _ => v.time += step,
            }
            if v.time >= 1.0 {
                if looped && held && !v.echo {
                    v.time = v.time.fract();
                } else {
                    v.time = 1.0;
                    v.running = false;
                }
            }
            let value = shape.0.value_at(v.time as f32) * v.gain;
            sum = Some(sum.unwrap_or(0.0) + value);
        }
        let shape_end = self.shape.0.points.last().map_or(0.0, |p| p.value);
        let wave = sum.map_or(shape_end, |v: f32| v.clamp(-1.0, 1.0));
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

impl Processor for ShaperMidi {
    fn activate(&mut self, sample_rate: f64, _max_block: usize) {
        self.sample_rate = sample_rate.max(1.0);
        self.clock.clock = BeatClock::new(120.0, self.sample_rate);
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        self.clock.clock = BeatClock::from_transport(ctx.transport);
        let mut seen = 0;
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => self.apply(id, value),
            BlockStep::Midi(message) => {
                self.through.note(message);
                match message {
                    MidiMessage::NoteOn { velocity, .. } => self.note_on(velocity),
                    MidiMessage::NoteOff { .. } => self.note_off(),
                    MidiMessage::AllNotesOff => self.held = 0,
                    _ => {}
                }
            }
            BlockStep::Audio(a) => {
                seen += a.frames();
                self.advance(a.frames());
            }
        });
        ctx.events_out.extend(ctx.events);
        self.through.flush(ctx.transport.playing, ctx.events_out);
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

    fn reset(&mut self) {
        self.held = 0;
        self.through.reset();
    }
}

fn create(init: &DeviceInit) -> Box<dyn Processor> {
    Box::new(ShaperMidi::new(init.state.as_deref(), next_seed()))
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.shaper_midi",
    name: "Shaper MIDI",
    role: DeviceRole::MidiEffect,
    category: "Modulators",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        ports: Ports {
            midi_out: true,
            mod_out: MOD_PORTS.to_vec(),
            ..Default::default()
        },
        layout: layout(),
        ..Default::default()
    },
    create,
};
