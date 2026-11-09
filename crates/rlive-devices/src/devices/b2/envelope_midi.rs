//! Envelope MIDI (manual pp. 848-850): an ADSR fired by notes, as modulation.
//!
//! The manual gives no ranges, or how the Loop mode's rate relates to Global Time beyond naming
//! it; the times here are ours, with Loop repeating each attack-decay at the scaled stage times.

use super::common::*;
use crate::dsp::envelope::{Envelope, EnvelopeSettings, LoopMode};
use crate::dsp::tempo::{BeatClock, SyncRate};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    BlockStep, DeviceDesc, Item, MidiMessage, ParamInfo, Ports, ProcessContext, Processor, Section,
    Status,
};

pub const ATTACK: u32 = 0;
pub const DECAY: u32 = 1;
pub const SUSTAIN: u32 = 2;
pub const RELEASE: u32 = 3;
pub const ATTACK_SLOPE: u32 = 4;
pub const DECAY_SLOPE: u32 = 5;
pub const RELEASE_SLOPE: u32 = 6;
pub const LOOP_MODE: u32 = 7;
pub const SYNC: u32 = 8;
pub const GLOBAL_TIME: u32 = 9;
pub const ECHO_TIME: u32 = 10;
pub const FEEDBACK: u32 = 11;
pub const AMOUNT: u32 = 12;
pub const VELOCITY: u32 = 13;
pub const SUSTAIN_ON: u32 = 14;

pub const LOOP_MODES: [&str; 4] = ["Free", "Sync", "Loop", "Echo"];

const CONTROL: usize = 32;

fn params() -> Vec<ParamInfo> {
    let mut out = vec![
        ParamInfo::skewed(ATTACK, "Attack", 0.0, 2000.0, 5.0, "ms", 3.0),
        ParamInfo::skewed(DECAY, "Decay", 0.0, 5000.0, 200.0, "ms", 3.0),
        ParamInfo::continuous(SUSTAIN, "Sustain", 0.0, 100.0, 50.0, "%"),
        ParamInfo::skewed(RELEASE, "Release", 0.0, 5000.0, 200.0, "ms", 3.0),
        ParamInfo::continuous(ATTACK_SLOPE, "Attack Slope", -100.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(DECAY_SLOPE, "Decay Slope", -100.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(RELEASE_SLOPE, "Release Slope", -100.0, 100.0, 0.0, "%"),
        ParamInfo::stepped(LOOP_MODE, "Loop Mode", &LOOP_MODES, 0),
        ParamInfo::stepped(SYNC, "Sync Rate", &SyncRate::LABELS, 12),
        ParamInfo::skewed(GLOBAL_TIME, "Global Time", 0.1, 10.0, 1.0, "", 2.0),
        ParamInfo::skewed(ECHO_TIME, "Env Echo Time", 10.0, 2000.0, 250.0, "ms", 2.0),
        ParamInfo::continuous(FEEDBACK, "Env Feedback", 0.0, 100.0, 50.0, "%"),
        ParamInfo::continuous(AMOUNT, "Amount", 0.0, 100.0, 100.0, "%"),
        ParamInfo::toggle(VELOCITY, "Velocity", true),
        ParamInfo::toggle(SUSTAIN_ON, "Sustain On", true),
    ];
    out.extend(map_info(false));
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
            title: "Mode",
            items: vec![
                menu(LOOP_MODE),
                menu(SYNC),
                knob(GLOBAL_TIME),
                knob(ECHO_TIME),
                knob(FEEDBACK),
                knob(AMOUNT),
                toggle(VELOCITY),
                toggle(SUSTAIN_ON),
            ],
        },
        Section {
            title: "ADSR",
            items: vec![
                knob(ATTACK),
                knob(DECAY),
                knob(SUSTAIN),
                knob(RELEASE),
                knob(ATTACK_SLOPE),
                knob(DECAY_SLOPE),
                knob(RELEASE_SLOPE),
            ],
        },
    ]
}

pub struct EnvelopeMidi {
    sample_rate: f64,
    attack: f32,
    decay: f32,
    sustain: f32,
    release: f32,
    slopes: [f32; 3],
    mode: usize,
    sync_index: f64,
    global: f32,
    echo_ms: f32,
    feedback: f32,
    amount: f32,
    velocity_on: bool,
    sustain_on: bool,
    env: Envelope,
    /// How many keys are down, so the envelope only releases when the last one is up.
    held: u32,
    gain: f32,
    velocity: f32,
    /// Samples until the next echo fires, while echoes are running.
    echo_in: Option<f64>,
    position: f32,
    map: MapOutput,
    scope: ScopeFeed,
    sub: usize,
    through: ThroughNotes,
}

impl Default for EnvelopeMidi {
    fn default() -> Self {
        Self {
            sample_rate: 48_000.0,
            attack: 0.005,
            decay: 0.2,
            sustain: 0.5,
            release: 0.2,
            slopes: [0.0; 3],
            mode: 0,
            sync_index: 12.0,
            global: 1.0,
            echo_ms: 250.0,
            feedback: 0.5,
            amount: 1.0,
            velocity_on: true,
            sustain_on: true,
            env: Envelope::new(),
            held: 0,
            gain: 1.0,
            velocity: 1.0,
            echo_in: None,
            position: 0.0,
            map: MapOutput::new(false),
            scope: ScopeFeed::default(),
            sub: 0,
            through: ThroughNotes::default(),
        }
    }
}

impl EnvelopeMidi {
    fn settings(&self) -> EnvelopeSettings {
        let t = self.global;
        let loop_mode = match self.mode {
            1 => LoopMode::Sync,
            2 => LoopMode::Loop,
            _ if !self.sustain_on => LoopMode::Trigger,
            _ => LoopMode::Off,
        };
        EnvelopeSettings {
            initial: 0.0,
            peak: 1.0,
            sustain: self.sustain,
            end: 0.0,
            attack: self.attack * t,
            decay: self.decay * t,
            release: self.release * t,
            attack_slope: self.slopes[0],
            decay_slope: self.slopes[1],
            release_slope: self.slopes[2],
            loop_mode,
            repeat_beats: SyncRate::from_index(self.sync_index).beats(),
            ..EnvelopeSettings::default()
        }
    }

    fn update(&mut self) {
        let s = self.settings();
        self.env.set(&s);
    }

    fn apply(&mut self, id: u32, value: f64) {
        if self.map.apply(id, value) {
            return;
        }
        let ms = value as f32 * 0.001;
        let unit = (value * 0.01) as f32;
        match id {
            ATTACK => self.attack = ms,
            DECAY => self.decay = ms,
            SUSTAIN => self.sustain = unit,
            RELEASE => self.release = ms,
            ATTACK_SLOPE => self.slopes[0] = unit,
            DECAY_SLOPE => self.slopes[1] = unit,
            RELEASE_SLOPE => self.slopes[2] = unit,
            LOOP_MODE => self.mode = value.round().clamp(0.0, 3.0) as usize,
            SYNC => self.sync_index = value,
            GLOBAL_TIME => self.global = value as f32,
            ECHO_TIME => self.echo_ms = value as f32,
            FEEDBACK => self.feedback = unit,
            AMOUNT => self.amount = unit,
            VELOCITY => self.velocity_on = value >= 0.5,
            SUSTAIN_ON => self.sustain_on = value >= 0.5,
            _ => return,
        }
        self.update();
    }

    fn fire(&mut self) {
        self.env.gate_on();
        self.scope.trigger();
    }

    fn note_on(&mut self, velocity: f32) {
        self.held += 1;
        self.velocity = if self.velocity_on { velocity } else { 1.0 };
        self.gain = 1.0;
        self.echo_in = (self.mode == 3).then_some(self.echo_samples());
        self.fire();
    }

    fn note_off(&mut self) {
        self.held = self.held.saturating_sub(1);
        if self.held == 0 && self.mode != 3 {
            self.env.gate_off();
        }
    }

    fn echo_samples(&self) -> f64 {
        f64::from(self.echo_ms) * 0.001 * self.sample_rate
    }

    /// The next echo, or none once they have died away.
    fn echo(&mut self) {
        self.gain *= self.feedback;
        if self.gain < 1e-3 {
            self.gain = 0.0;
            self.echo_in = None;
            return;
        }
        self.echo_in = Some(self.echo_samples());
        self.fire();
    }

    fn control_step(&mut self, n: usize) {
        if let Some(left) = self.echo_in {
            let left = left - n as f64;
            if left <= 0.0 {
                self.echo();
            } else {
                self.echo_in = Some(left);
            }
        }
        let level = self.env.advance(n) * self.gain * self.velocity;
        self.position = (level * self.amount).clamp(0.0, 1.0);
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

impl Processor for EnvelopeMidi {
    fn activate(&mut self, sample_rate: f64, _max_block: usize) {
        self.sample_rate = sample_rate.max(1.0);
        self.env.prepare(self.sample_rate);
        self.env.set_clock(&BeatClock::new(120.0, self.sample_rate));
        self.update();
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        self.env
            .set_clock(&BeatClock::from_transport(ctx.transport));
        let mut seen = 0;
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => self.apply(id, value),
            BlockStep::Midi(message) => {
                self.through.note(message);
                match message {
                    MidiMessage::NoteOn { velocity, .. } => self.note_on(velocity),
                    MidiMessage::NoteOff { .. } => self.note_off(),
                    MidiMessage::AllNotesOff => {
                        self.held = 0;
                        self.env.gate_off();
                        self.echo_in = None;
                    }
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

    fn reset(&mut self) {
        self.held = 0;
        self.echo_in = None;
        self.env.gate_off();
        self.through.reset();
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.envelope_midi",
    name: "Envelope MIDI",
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
    create: |_| Box::<EnvelopeMidi>::default(),
};
