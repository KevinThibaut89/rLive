//! Pitch: transposes notes in semitones or scale degrees and limits them to a range.

use super::common::*;
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    DeviceDesc, Item, MidiMessage, ParamId, ParamInfo, Ports, Section, Status, TransportInfo,
    ViewId, ViewWriter,
};

pub const PITCH: u32 = 0;
pub const STEP_WIDTH: u32 = 1;
pub const STEP_UP: u32 = 2;
pub const STEP_DOWN: u32 = 3;
pub const LOWEST: u32 = 4;
pub const RANGE: u32 = 5;
pub const MODE: u32 = 6;
pub const USE_SCALE: u32 = 7;

pub const LED: ViewId = ViewId(0);
pub const LED_COUNT: usize = 1;

/// The most scale degrees the Pitch control reaches.
const MAX_DEGREES: f64 = 30.0;
const MAX_SEMITONES: f64 = 128.0;
/// The most degrees the Step Width reaches.
const MAX_WIDTH_DEGREES: f64 = 30.0;

/// What happens to notes outside the range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RangeMode {
    #[default]
    Block,
    Fold,
    Limit,
}

impl RangeMode {
    fn from_index(index: f64) -> Self {
        match index.round() as i32 {
            1 => Self::Fold,
            2 => Self::Limit,
            _ => Self::Block,
        }
    }
}

pub fn params() -> Vec<ParamInfo> {
    vec![
        int_param(PITCH, "Pitch", -MAX_SEMITONES, MAX_SEMITONES, 0.0),
        int_param(STEP_WIDTH, "Step Width", 1.0, 48.0, 1.0),
        ParamInfo::toggle(STEP_UP, "Step Up", false),
        ParamInfo::toggle(STEP_DOWN, "Step Down", false),
        int_param(LOWEST, "Lowest", 0.0, 127.0, 0.0),
        int_param(RANGE, "Range", 0.0, 127.0, 127.0),
        ParamInfo::stepped(MODE, "Mode", &["Block", "Fold", "Limit"], 0),
        ParamInfo::toggle(USE_SCALE, "Use Current Scale", false),
    ]
}

fn layout() -> Vec<Section> {
    let id = ParamId;
    vec![
        Section {
            title: "Pitch",
            items: vec![
                Item::Knob(id(PITCH)),
                Item::Knob(id(STEP_WIDTH)),
                Item::Row(vec![Item::Toggle(id(STEP_UP)), Item::Toggle(id(STEP_DOWN))]),
                Item::Toggle(id(USE_SCALE)),
            ],
        },
        Section {
            title: "Range",
            items: vec![
                Item::Knob(id(LOWEST)),
                Item::Knob(id(RANGE)),
                Item::Menu(id(MODE)),
                Item::Panel("pitch_led"),
            ],
        },
    ]
}

pub struct Pitch {
    core: Core,
    pitch: f64,
    step_width: f64,
    lowest: i32,
    range: i32,
    mode: RangeMode,
    use_scale: bool,
    transposer: Transposer,
    up: bool,
    down: bool,
    led: f32,
}

impl Default for Pitch {
    fn default() -> Self {
        Self {
            core: Core::default(),
            pitch: 0.0,
            step_width: 1.0,
            lowest: 0,
            range: 127,
            mode: RangeMode::Block,
            use_scale: false,
            transposer: Transposer::Chromatic,
            up: false,
            down: false,
            led: 0.0,
        }
    }
}

impl Pitch {
    /// The range that passes, inclusive at both ends.
    fn bounds(&self) -> (i32, i32) {
        let high = if self.range >= 127 {
            127
        } else {
            (self.lowest + self.range).min(127)
        };
        (self.lowest, high.max(self.lowest))
    }

    /// Where a key ends up, or `None` if it doesn't pass.
    fn map(&self, key: u8) -> Option<u8> {
        let degrees = matches!(self.transposer, Transposer::Scale(_));
        let amount = if degrees {
            self.pitch.clamp(-MAX_DEGREES, MAX_DEGREES)
        } else {
            self.pitch
        };
        let moved = self.transposer.apply(key as i32, amount.round() as i32);
        let (low, high) = self.bounds();
        let out = if moved < low || moved > high {
            match self.mode {
                RangeMode::Block => return None,
                RangeMode::Limit => moved.clamp(low, high),
                RangeMode::Fold => fold(moved, low, high),
            }
        } else {
            moved
        };
        (0..=127).contains(&out).then_some(out as u8)
    }

    fn step(&mut self, sign: f64) {
        let degrees = matches!(self.transposer, Transposer::Scale(_));
        let (limit, width) = if degrees {
            (MAX_DEGREES, self.step_width.min(MAX_WIDTH_DEGREES))
        } else {
            (MAX_SEMITONES, self.step_width)
        };
        self.pitch = (self.pitch + sign * width).clamp(-limit, limit);
        self.core.report(PITCH, self.pitch);
    }
}

/// Octaves until the note fits, or the nearest edge if the range is narrower than an octave.
fn fold(mut key: i32, low: i32, high: i32) -> i32 {
    while key < low {
        key += 12;
    }
    while key > high {
        key -= 12;
    }
    if key < low {
        low
    } else {
        key
    }
}

impl MidiFx for Pitch {
    fn core(&mut self) -> &mut Core {
        &mut self.core
    }

    fn apply(&mut self, id: u32, value: f64) {
        match id {
            PITCH => self.pitch = value,
            STEP_WIDTH => self.step_width = value.max(1.0),
            STEP_UP => self.up = value >= 0.5,
            STEP_DOWN => self.down = value >= 0.5,
            LOWEST => self.lowest = value as i32,
            RANGE => self.range = value as i32,
            MODE => self.mode = RangeMode::from_index(value),
            USE_SCALE => self.use_scale = value >= 0.5,
            _ => {}
        }
    }

    fn param_changed(&mut self, id: u32, value: f64) {
        let on = value >= 0.5;
        let was_up = self.up;
        let was_down = self.down;
        self.apply(id, value);
        match id {
            STEP_UP if on && !was_up => {
                self.step(1.0);
                // The button springs back, so the next press is a new one.
                self.up = false;
                self.core.report(STEP_UP, 0.0);
            }
            STEP_DOWN if on && !was_down => {
                self.step(-1.0);
                self.down = false;
                self.core.report(STEP_DOWN, 0.0);
            }
            _ => {}
        }
    }

    fn begin(&mut self, transport: &TransportInfo) {
        self.transposer = Transposer::resolve(self.use_scale, transport);
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
                match self.map(key) {
                    Some(out) => {
                        self.core
                            .note_on(src, NoteRef { key: out, ..src }, velocity, tuning)
                    }
                    None => {
                        self.led = 1.0;
                        self.core.block(src);
                    }
                }
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

midi_fx_processor!(Pitch);

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.pitch",
    name: "Pitch",
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
    create: |_| Box::<Pitch>::default(),
};
