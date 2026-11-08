//! Velocity: reshapes incoming velocities through a curve.

use super::common::*;
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    DeviceDesc, Item, MidiMessage, ParamId, ParamInfo, Ports, Section, Status, ViewId, ViewWriter,
};

pub const DRIVE: u32 = 0;
pub const COMPAND: u32 = 1;
pub const OUT_HI: u32 = 2;
pub const OUT_LOW: u32 = 3;
pub const RANGE: u32 = 4;
pub const LOWEST: u32 = 5;
pub const MODE: u32 = 6;
pub const OPERATION: u32 = 7;
pub const RANDOM: u32 = 8;

pub const LED: ViewId = ViewId(0);
pub const LED_COUNT: usize = 1;

/// What happens to velocities outside the input range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Clip,
    Gate,
    Fixed,
}

/// Which velocities the device changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Operation {
    #[default]
    Velocity,
    Release,
    Both,
}

pub fn params() -> Vec<ParamInfo> {
    vec![
        percent_param(DRIVE, "Drive", -100.0, 100.0, 0.0),
        percent_param(COMPAND, "Compand", -100.0, 100.0, 0.0),
        int_param(OUT_HI, "Out Hi", 0.0, 127.0, 127.0),
        int_param(OUT_LOW, "Out Low", 0.0, 127.0, 0.0),
        int_param(RANGE, "Range", 0.0, 127.0, 127.0),
        int_param(LOWEST, "Lowest", 0.0, 127.0, 0.0),
        ParamInfo::stepped(MODE, "Mode", &["Clip", "Gate", "Fixed"], 0),
        ParamInfo::stepped(
            OPERATION,
            "Operation",
            &["Velocity", "Rel. Vel.", "Velocity + Rel. Vel."],
            0,
        ),
        int_param(RANDOM, "Random", 0.0, 64.0, 0.0),
    ]
}

fn layout() -> Vec<Section> {
    let id = ParamId;
    vec![
        Section {
            title: "Velocity Curve",
            items: vec![
                Item::Panel("velocity_curve"),
                Item::Row(vec![Item::Knob(id(DRIVE)), Item::Knob(id(COMPAND))]),
            ],
        },
        Section {
            title: "Range",
            items: vec![
                Item::Knob(id(LOWEST)),
                Item::Knob(id(RANGE)),
                Item::Knob(id(OUT_LOW)),
                Item::Knob(id(OUT_HI)),
            ],
        },
        Section {
            title: "Mode",
            items: vec![
                Item::Menu(id(MODE)),
                Item::Menu(id(OPERATION)),
                Item::Knob(id(RANDOM)),
                Item::Panel("velocity_led"),
            ],
        },
    ]
}

/// The velocity settings the curve depends on. Pure, so the panel can draw from it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CurveSettings {
    pub lowest: f32,
    pub range: f32,
    pub out_low: f32,
    pub out_hi: f32,
    pub drive: f32,
    pub compand: f32,
}

impl Default for CurveSettings {
    fn default() -> Self {
        Self {
            lowest: 0.0,
            range: 127.0,
            out_low: 0.0,
            out_hi: 127.0,
            drive: 0.0,
            compand: 0.0,
        }
    }
}

impl CurveSettings {
    /// The input range, inclusive at both ends.
    fn bounds(&self) -> (f32, f32) {
        (self.lowest, (self.lowest + self.range).min(127.0))
    }

    /// Output velocity (0..=127) for input velocity (0..=127). Inputs outside the range are held
    /// at its ends: this is what Clip mode gives.
    pub fn curve(&self, input: f32) -> f32 {
        let (low, high) = self.bounds();
        let span = (high - low).max(1.0);
        let mut x = ((input - low) / span).clamp(0.0, 1.0);
        x = shape(x, self.drive);
        if self.compand != 0.0 {
            // Positive pushes values away from the middle, negative pulls them towards it.
            let amount = (self.compand * 1.4).exp();
            let centred = 2.0 * x - 1.0;
            x = 0.5 + 0.5 * centred.abs().powf(1.0 / amount).copysign(centred);
        }
        self.out_low + x * (self.out_hi - self.out_low)
    }
}

pub struct Velocity {
    core: Core,
    curve: CurveSettings,
    mode: Mode,
    operation: Operation,
    random: f32,
    rng: Rng,
    led: f32,
}

impl Velocity {
    pub fn with_seed(seed: u64) -> Self {
        Self {
            core: Core::default(),
            curve: CurveSettings::default(),
            mode: Mode::Clip,
            operation: Operation::Velocity,
            random: 0.0,
            rng: Rng::new(seed),
            led: 0.0,
        }
    }

    fn is_outside(&self, input: f32) -> bool {
        let (low, high) = self.curve.bounds();
        input < low || input > high
    }

    /// The new velocity (0..=127) for `input` (0..=127) on a message of the kind the device
    /// reshapes. A note-off is never gated: its note has already started.
    fn remap(&mut self, input: f32, may_gate: bool) -> Option<f32> {
        if self.mode == Mode::Gate && may_gate && self.is_outside(input) {
            return None;
        }
        let mut out = match self.mode {
            Mode::Fixed => self.curve.out_hi,
            _ => self.curve.curve(input),
        };
        if self.random > 0.0 {
            out += (self.rng.unit() * 2.0 - 1.0) * self.random;
            let (low, high) = (
                self.curve.out_low.min(self.curve.out_hi),
                self.curve.out_low.max(self.curve.out_hi),
            );
            out = out.clamp(low, high);
        }
        Some(out)
    }
}

impl MidiFx for Velocity {
    fn core(&mut self) -> &mut Core {
        &mut self.core
    }

    fn apply(&mut self, id: u32, value: f64) {
        match id {
            DRIVE => self.curve.drive = (value * 0.01) as f32,
            COMPAND => self.curve.compand = (value * 0.01) as f32,
            OUT_HI => self.curve.out_hi = value as f32,
            OUT_LOW => self.curve.out_low = value as f32,
            RANGE => self.curve.range = value as f32,
            LOWEST => self.curve.lowest = value as f32,
            MODE => {
                self.mode = match value.round() as i32 {
                    1 => Mode::Gate,
                    2 => Mode::Fixed,
                    _ => Mode::Clip,
                }
            }
            OPERATION => {
                self.operation = match value.round() as i32 {
                    1 => Operation::Release,
                    2 => Operation::Both,
                    _ => Operation::Velocity,
                }
            }
            RANDOM => self.random = value as f32,
            _ => {}
        }
    }

    fn midi(&mut self, message: MidiMessage) {
        match message {
            MidiMessage::NoteOn {
                channel,
                key,
                velocity,
                note_id,
                tuning,
            } => {
                let src = NoteRef {
                    channel,
                    key,
                    id: note_id,
                };
                let new = if self.operation == Operation::Release {
                    Some(velocity)
                } else {
                    self.remap(velocity * 127.0, true).map(|v| v / 127.0)
                };
                match new {
                    Some(v) => self.core.note_on(src, src, v, tuning),
                    None => {
                        self.led = 1.0;
                        self.core.block(src);
                    }
                }
            }
            MidiMessage::NoteOff {
                channel,
                key,
                velocity,
                note_id,
            } => {
                let release = if self.operation == Operation::Velocity {
                    velocity
                } else {
                    self.remap(velocity * 127.0, false)
                        .map_or(velocity, |v| (v / 127.0).clamp(0.0, 1.0))
                };
                self.core.pass_other(MidiMessage::NoteOff {
                    channel,
                    key,
                    velocity: release,
                    note_id,
                });
            }
            other => self.core.pass_other(other),
        }
    }

    fn end(&mut self, _mod_out: &mut [f32], view: &ViewWriter) {
        if view.is_active() {
            view.publish(LED, &[self.led]);
        }
        self.led *= 0.85;
    }
}

midi_fx_processor!(Velocity);

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.velocity",
    name: "Velocity",
    role: DeviceRole::MidiEffect,
    category: "MIDI Effects",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        ports: Ports {
            midi_out: true,
            ..Default::default()
        },
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::new(Velocity::with_seed(entropy_seed())),
};
