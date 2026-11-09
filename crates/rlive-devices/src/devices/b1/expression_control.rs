//! Expression Control: turns MIDI and MPE into modulation sources for mapped parameters.

use super::common::*;
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    DeviceDesc, ExprKind, Item, MidiMessage, ParamId, ParamInfo, Ports, Section, Status,
    ViewWriter, SLIDE_CC,
};

pub const SLOTS: usize = 5;
/// Slot `n`'s parameters are numbered from `n * SLOT_PARAMS`.
pub const SLOT_PARAMS: u32 = 20;

pub const SOURCE: u32 = 0;
pub const MODULATION: u32 = 1;
pub const POLARITY: u32 = 2;
pub const OUT_MAX: u32 = 3;
pub const OUT_MIN: u32 = 4;
pub const CURVE_TYPE: u32 = 5;
pub const CURVE_A: u32 = 6;
pub const CURVE_B: u32 = 7;
pub const CURVE_LINK: u32 = 8;
pub const MIN: u32 = 9;
pub const MAX: u32 = 10;
pub const X: u32 = 11;
pub const Y: u32 = 12;
pub const INCREMENT_STEPS: u32 = 13;
pub const RANDOM_AMOUNT: u32 = 14;
pub const SMOOTHING_TYPE: u32 = 15;
pub const RISE: u32 = 16;
pub const FALL: u32 = 17;

const SOURCE_LABELS: [&str; 10] = [
    "Velocity",
    "Modwheel",
    "Pitchbend",
    "Pressure",
    "Keytrack",
    "Expression",
    "Random",
    "Increment",
    "Slide",
    "Sustain",
];

/// Where a slot gets its value from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceKind {
    Velocity,
    Modwheel,
    Pitchbend,
    Pressure,
    Keytrack,
    Expression,
    Random,
    Increment,
    Slide,
    Sustain,
}

const SOURCES: [SourceKind; 10] = [
    SourceKind::Velocity,
    SourceKind::Modwheel,
    SourceKind::Pitchbend,
    SourceKind::Pressure,
    SourceKind::Keytrack,
    SourceKind::Expression,
    SourceKind::Random,
    SourceKind::Increment,
    SourceKind::Slide,
    SourceKind::Sustain,
];

/// The id of parameter `p` of slot `slot`.
pub const fn slot_param(slot: usize, p: u32) -> u32 {
    slot as u32 * SLOT_PARAMS + p
}

pub fn params() -> Vec<ParamInfo> {
    let mut out = Vec::new();
    for slot in 0..SLOTS {
        let id = |p| slot_param(slot, p);
        let name = |s: &str| format!("{} {s}", slot + 1);
        out.extend([
            ParamInfo::stepped(id(SOURCE), &name("Source"), &SOURCE_LABELS, 0),
            ParamInfo::stepped(
                id(MODULATION),
                &name("Modulation"),
                &["Modulate", "Remote Control"],
                0,
            ),
            ParamInfo::stepped(id(POLARITY), &name("Polarity"), &["Bipolar", "Unipolar"], 1),
            percent_param(id(OUT_MAX), &name("Output Max"), -100.0, 100.0, 100.0),
            percent_param(id(OUT_MIN), &name("Output Min"), 0.0, 100.0, 0.0),
            ParamInfo::stepped(
                id(CURVE_TYPE),
                &name("Curve Type"),
                &["Linear", "S-Shaped"],
                0,
            ),
            percent_param(id(CURVE_A), &name("Curve A"), -100.0, 100.0, 0.0),
            percent_param(id(CURVE_B), &name("Curve B"), -100.0, 100.0, 0.0),
            ParamInfo::toggle(id(CURVE_LINK), &name("Curve Link"), true),
            percent_param(id(MIN), &name("Min"), 0.0, 100.0, 0.0),
            percent_param(id(MAX), &name("Max"), 0.0, 100.0, 100.0),
            percent_param(id(X), &name("X"), 0.0, 100.0, 50.0),
            percent_param(id(Y), &name("Y"), 0.0, 100.0, 50.0),
            int_param(
                id(INCREMENT_STEPS),
                &name("Increment Steps"),
                1.0,
                32.0,
                4.0,
            ),
            percent_param(id(RANDOM_AMOUNT), &name("Random Amount"), 0.0, 100.0, 100.0),
            ParamInfo::stepped(
                id(SMOOTHING_TYPE),
                &name("Smoothing Type"),
                &["Linear", "Logarithmic"],
                0,
            ),
            ParamInfo::skewed(id(RISE), &name("Rise"), 0.0, 1000.0, 0.0, "ms", 3.0),
            ParamInfo::skewed(id(FALL), &name("Fall"), 0.0, 1000.0, 0.0, "ms", 3.0),
        ]);
    }
    out
}

fn layout() -> Vec<Section> {
    const TITLES: [&str; SLOTS] = ["Mod 1", "Mod 2", "Mod 3", "Mod 4", "Mod 5"];
    let mut sections = vec![Section {
        title: "Map",
        items: vec![Item::Panel("expression_map")],
    }];
    sections.extend((0..SLOTS).map(|slot| {
        let id = |p| ParamId(slot_param(slot, p));
        Section {
            title: TITLES[slot],
            items: vec![
                Item::Menu(id(SOURCE)),
                Item::Menu(id(MODULATION)),
                Item::Menu(id(POLARITY)),
                Item::Knob(id(OUT_MAX)),
                Item::Knob(id(OUT_MIN)),
                Item::Menu(id(CURVE_TYPE)),
                Item::Knob(id(CURVE_A)),
                Item::Knob(id(CURVE_B)),
                Item::Toggle(id(CURVE_LINK)),
                Item::Knob(id(MIN)),
                Item::Knob(id(MAX)),
                Item::Knob(id(X)),
                Item::Knob(id(Y)),
                Item::Knob(id(INCREMENT_STEPS)),
                Item::Knob(id(RANDOM_AMOUNT)),
                Item::Menu(id(SMOOTHING_TYPE)),
                Item::Knob(id(RISE)),
                Item::Knob(id(FALL)),
            ],
        }
    }));
    sections
}

#[derive(Clone, Copy)]
struct Slot {
    source: SourceKind,
    remote: bool,
    bipolar: bool,
    out_max: f32,
    out_min: f32,
    curve: Curve,
    increment_steps: usize,
    random_amount: f32,
    logarithmic: bool,
    rise_ms: f32,
    fall_ms: f32,
    smoother: Smoother,
    last_random: f32,
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            source: SourceKind::Velocity,
            remote: false,
            bipolar: false,
            out_max: 1.0,
            out_min: 0.0,
            curve: Curve::default(),
            increment_steps: 4,
            random_amount: 1.0,
            logarithmic: false,
            rise_ms: 0.0,
            fall_ms: 0.0,
            smoother: Smoother::default(),
            last_random: 0.5,
        }
    }
}

pub struct ExpressionControl {
    core: Core,
    slots: [Slot; SLOTS],
    velocity: f32,
    modwheel: f32,
    pitchbend: f32,
    pressure: f32,
    keytrack: f32,
    expression: f32,
    slide: f32,
    sustain: f32,
    increment: usize,
    rng: Rng,
}

impl ExpressionControl {
    pub fn with_seed(seed: u64) -> Self {
        Self {
            core: Core::default(),
            slots: [Slot::default(); SLOTS],
            velocity: 0.0,
            modwheel: 0.0,
            pitchbend: 0.5,
            pressure: 0.0,
            keytrack: 0.0,
            expression: 0.0,
            slide: 0.0,
            sustain: 0.0,
            increment: 0,
            rng: Rng::new(seed),
        }
    }

    fn note_triggered(&mut self) {
        for slot in &mut self.slots {
            let value = self.rng.unit();
            slot.last_random = 0.5 + (value - 0.5) * slot.random_amount;
        }
        self.increment = self.increment.wrapping_add(1);
    }

    /// A slot's source value right now, 0..=1.
    fn input_of(&self, slot: &Slot) -> f32 {
        match slot.source {
            SourceKind::Velocity => self.velocity,
            SourceKind::Modwheel => self.modwheel,
            SourceKind::Pitchbend => self.pitchbend,
            SourceKind::Pressure => self.pressure,
            SourceKind::Keytrack => self.keytrack,
            SourceKind::Expression => self.expression,
            SourceKind::Random => slot.last_random,
            SourceKind::Increment => {
                if slot.increment_steps <= 1 || self.increment == 0 {
                    0.0
                } else {
                    let index = (self.increment - 1) % slot.increment_steps;
                    index as f32 / (slot.increment_steps - 1) as f32
                }
            }
            SourceKind::Slide => self.slide,
            SourceKind::Sustain => self.sustain,
        }
    }

    /// Converts a curve output to the slot's modulation value, per Modulate or Remote Control.
    fn output(slot: &Slot, value: f32) -> f32 {
        if slot.remote {
            slot.out_min + value * (slot.out_max - slot.out_min)
        } else if slot.bipolar {
            (value * 2.0 - 1.0) * slot.out_max
        } else {
            value * slot.out_max
        }
    }
}

impl MidiFx for ExpressionControl {
    fn core(&mut self) -> &mut Core {
        &mut self.core
    }

    fn apply(&mut self, id: u32, value: f64) {
        let slot_index = (id / SLOT_PARAMS) as usize;
        let Some(slot) = self.slots.get_mut(slot_index) else {
            return;
        };
        let unit = (value * 0.01) as f32;
        match id % SLOT_PARAMS {
            SOURCE => slot.source = SOURCES[(value.round() as usize).min(SOURCES.len() - 1)],
            MODULATION => slot.remote = value >= 0.5,
            POLARITY => slot.bipolar = value < 0.5,
            OUT_MAX => slot.out_max = unit,
            OUT_MIN => slot.out_min = unit,
            CURVE_TYPE => slot.curve.s_shaped = value >= 0.5,
            CURVE_A => slot.curve.a = unit,
            CURVE_B => slot.curve.b = unit,
            CURVE_LINK => slot.curve.link = value >= 0.5,
            MIN => slot.curve.min = unit,
            MAX => slot.curve.max = unit,
            X => slot.curve.x = unit,
            Y => slot.curve.y = unit,
            INCREMENT_STEPS => slot.increment_steps = (value.round() as usize).max(1),
            RANDOM_AMOUNT => slot.random_amount = unit,
            SMOOTHING_TYPE => slot.logarithmic = value >= 0.5,
            RISE => slot.rise_ms = value as f32,
            FALL => slot.fall_ms = value as f32,
            _ => {}
        }
    }

    fn midi(&mut self, message: MidiMessage) {
        match message {
            MidiMessage::NoteOn { key, velocity, .. } => {
                self.velocity = velocity;
                self.keytrack = key as f32 / 127.0;
                self.note_triggered();
            }
            MidiMessage::ControlChange { cc, value, .. } => {
                let v = value as f32 / 127.0;
                match cc {
                    1 => self.modwheel = v,
                    11 => self.expression = v,
                    64 => self.sustain = v,
                    SLIDE_CC => self.slide = v,
                    _ => {}
                }
            }
            MidiMessage::PitchBend { value, .. } => self.pitchbend = (value + 1.0) * 0.5,
            MidiMessage::ChannelPressure { value, .. } => self.pressure = value,
            MidiMessage::NoteExpr { kind, value, .. } => match kind {
                ExprKind::Pressure => self.pressure = value,
                ExprKind::Slide => self.slide = value,
                ExprKind::Pitch => {}
            },
            _ => {}
        }
        self.core.pass_through(message);
    }

    fn advance(&mut self, frames: usize) {
        self.core.advance(frames as u64);
        for slot in &mut self.slots {
            slot.smoother.tick(frames);
        }
    }

    fn end(&mut self, mod_out: &mut [f32], _view: &ViewWriter) {
        for i in 0..SLOTS {
            let input = self.input_of(&self.slots[i]);
            let sr = self.core.sample_rate();
            let slot = &mut self.slots[i];
            let target = slot.curve.apply(input);
            slot.smoother.set_target(
                target,
                slot.rise_ms as f64 * 0.001 * sr,
                slot.fall_ms as f64 * 0.001 * sr,
                !slot.logarithmic,
            );
            if let Some(out) = mod_out.get_mut(i) {
                *out = Self::output(slot, slot.smoother.value);
            }
            if let Some(flag) = mod_out.get_mut(SLOTS + i) {
                *flag = slot.remote as u8 as f32;
            }
        }
    }

    fn clear(&mut self) {
        self.increment = 0;
    }
}

midi_fx_processor!(ExpressionControl);

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.expression_control",
    name: "Expression Control",
    role: DeviceRole::MidiEffect,
    category: "MIDI Effects",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        ports: Ports {
            midi_out: true,
            mod_out: vec![
                "Mod 1",
                "Mod 2",
                "Mod 3",
                "Mod 4",
                "Mod 5",
                "Mod 1 Remote",
                "Mod 2 Remote",
                "Mod 3 Remote",
                "Mod 4 Remote",
                "Mod 5 Remote",
            ],
            ..Default::default()
        },
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::new(ExpressionControl::with_seed(entropy_seed())),
};
