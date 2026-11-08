//! MIDI Monitor: shows the MIDI and MPE passing through it.

use super::common::*;
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    DeviceDesc, ExprKind, Item, MidiMessage, ParamId, ParamInfo, Ports, Section, Status, ViewId,
    ViewWriter,
};

pub const DISPLAY: u32 = 0;
pub const FREEZE: u32 = 1;

/// The velocity of each sounding key, 0 for off. Keyboard display.
pub const NOTES: ViewId = ViewId(0);
pub const NOTE_COUNT: usize = 128;
/// How many log entries have ever been written. The UI reads what's new from [`LOG`].
pub const LOG_COUNT: ViewId = ViewId(NOTE_COUNT as u32);
/// The newest log entries, in a ring: `[kind, channel, a, b]` each.
pub const LOG: ViewId = ViewId(NOTE_COUNT as u32 + 1);
pub const LOG_ENTRIES: usize = 64;
pub const LOG_ENTRY_VALUES: usize = 4;
/// MPE voices: `[key, velocity, slide, pressure, bend]` each, with key `-1` for an empty row.
pub const MPE: ViewId = ViewId(NOTE_COUNT as u32 + 1 + (LOG_ENTRIES * LOG_ENTRY_VALUES) as u32);
pub const MPE_VOICES: usize = 16;
pub const MPE_VALUES: usize = 5;
/// Everything the view holds.
pub const VIEW_LEN: usize =
    NOTE_COUNT + 1 + LOG_ENTRIES * LOG_ENTRY_VALUES + MPE_VOICES * MPE_VALUES;

/// What a log entry describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogKind {
    NoteOn = 0,
    NoteOff = 1,
    Control = 2,
    Bend = 3,
    Pressure = 4,
    Program = 5,
    Expr = 6,
}

impl LogKind {
    pub fn from_value(v: f32) -> Option<Self> {
        Some(match v as i32 {
            0 => Self::NoteOn,
            1 => Self::NoteOff,
            2 => Self::Control,
            3 => Self::Bend,
            4 => Self::Pressure,
            5 => Self::Program,
            6 => Self::Expr,
            _ => return None,
        })
    }
}

pub fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::stepped(DISPLAY, "Display", &["Note", "Flow", "MPE"], 0),
        ParamInfo::toggle(FREEZE, "Freeze", false),
    ]
}

fn layout() -> Vec<Section> {
    let id = ParamId;
    vec![
        Section {
            title: "",
            items: vec![Item::Panel("midi_monitor")],
        },
        Section {
            title: "Display",
            items: vec![Item::Menu(id(DISPLAY)), Item::Toggle(id(FREEZE))],
        },
    ]
}

/// The chord the held `keys` make, by the intervals above the lowest note. `None` when it isn't
/// a chord this knows, or fewer than two pitch classes are held.
pub fn chord_name(keys: &[u8]) -> Option<String> {
    const CHORDS: &[(u16, &str)] = &[
        (0b0000_1001_0001, "maj"),
        (0b0000_1000_1001, "min"),
        (0b0000_0100_1001, "dim"),
        (0b0001_0001_0001, "aug"),
        (0b0000_1000_0101, "sus2"),
        (0b0000_1010_0001, "sus4"),
        (0b1000_1001_0001, "maj7"),
        (0b0100_1001_0001, "7"),
        (0b0100_1000_1001, "min7"),
        (0b0100_0100_1001, "m7b5"),
        (0b0000_1000_0001, "5"),
    ];
    let lowest = *keys.iter().min()?;
    let mut mask = 0u16;
    for key in keys {
        mask |= 1 << ((key - lowest) % 12);
    }
    let name = CHORDS.iter().find(|(m, _)| *m == mask)?.1;
    Some(name.to_string())
}

#[derive(Debug, Clone, Copy)]
struct Voice {
    key: u8,
    id: u32,
    velocity: f32,
    slide: f32,
    pressure: f32,
    bend: f32,
    active: bool,
}

impl Default for Voice {
    fn default() -> Self {
        Self {
            key: 0,
            id: 0,
            velocity: 0.0,
            slide: 0.0,
            pressure: 0.0,
            bend: 0.0,
            active: false,
        }
    }
}

pub struct MidiMonitor {
    core: Core,
    notes: [f32; NOTE_COUNT],
    log: [f32; LOG_ENTRIES * LOG_ENTRY_VALUES],
    log_count: u32,
    voices: [Voice; MPE_VOICES],
    frozen: bool,
    changed: bool,
    published_count: u32,
}

impl Default for MidiMonitor {
    fn default() -> Self {
        Self {
            core: Core::default(),
            notes: [0.0; NOTE_COUNT],
            log: [0.0; LOG_ENTRIES * LOG_ENTRY_VALUES],
            log_count: 0,
            voices: [Voice::default(); MPE_VOICES],
            frozen: false,
            changed: false,
            published_count: 0,
        }
    }
}

impl MidiMonitor {
    fn add_log(&mut self, kind: LogKind, channel: u8, a: f32, b: f32) {
        if self.frozen {
            return;
        }
        let at = (self.log_count as usize % LOG_ENTRIES) * LOG_ENTRY_VALUES;
        self.log[at] = kind as u8 as f32;
        self.log[at + 1] = channel as f32;
        self.log[at + 2] = a;
        self.log[at + 3] = b;
        self.log_count = self.log_count.wrapping_add(1);
        self.changed = true;
    }

    fn voice(&mut self, key: u8, id: u32) -> Option<&mut Voice> {
        self.voices
            .iter_mut()
            .find(|v| v.active && v.key == key && (id == 0 || v.id == id || v.id == 0))
    }
}

impl MidiFx for MidiMonitor {
    fn core(&mut self) -> &mut Core {
        &mut self.core
    }

    fn apply(&mut self, id: u32, value: f64) {
        if id == FREEZE {
            self.frozen = value >= 0.5;
        }
    }

    fn midi(&mut self, message: MidiMessage) {
        match message {
            MidiMessage::NoteOn {
                channel,
                key,
                velocity,
                note_id,
                ..
            } => {
                self.notes[key as usize] = velocity;
                if let Some(free) = self.voices.iter_mut().find(|v| !v.active) {
                    *free = Voice {
                        key,
                        id: note_id,
                        velocity,
                        active: true,
                        ..Voice::default()
                    };
                }
                self.add_log(LogKind::NoteOn, channel, key as f32, velocity);
                self.changed = true;
            }
            MidiMessage::NoteOff {
                channel,
                key,
                velocity,
                note_id,
            } => {
                self.notes[key as usize] = 0.0;
                if let Some(v) = self.voice(key, note_id) {
                    v.active = false;
                }
                self.add_log(LogKind::NoteOff, channel, key as f32, velocity);
                self.changed = true;
            }
            MidiMessage::NoteExpr {
                channel,
                key,
                note_id,
                kind,
                value,
            } => {
                if let Some(v) = self.voice(key, note_id) {
                    match kind {
                        ExprKind::Pitch => v.bend = value,
                        ExprKind::Slide => v.slide = value,
                        ExprKind::Pressure => v.pressure = value,
                    }
                }
                let which = match kind {
                    ExprKind::Pitch => 0.0,
                    ExprKind::Slide => 1.0,
                    ExprKind::Pressure => 2.0,
                };
                self.add_log(LogKind::Expr, channel, which, value);
                self.changed = true;
            }
            MidiMessage::ControlChange { channel, cc, value } => {
                self.add_log(LogKind::Control, channel, cc as f32, value as f32);
            }
            MidiMessage::PitchBend { channel, value } => {
                self.add_log(LogKind::Bend, channel, 0.0, value);
            }
            MidiMessage::ChannelPressure { channel, value } => {
                self.add_log(LogKind::Pressure, channel, 0.0, value);
            }
            MidiMessage::ProgramChange { channel, program } => {
                self.add_log(LogKind::Program, channel, program as f32, 0.0);
            }
            MidiMessage::AllNotesOff => {
                self.notes = [0.0; NOTE_COUNT];
                self.voices = [Voice::default(); MPE_VOICES];
                self.changed = true;
            }
        }
        self.core.pass_through(message);
    }

    fn end(&mut self, _mod_out: &mut [f32], view: &ViewWriter) {
        if !view.is_active() || (!self.changed && self.published_count == self.log_count) {
            return;
        }
        view.publish(NOTES, &self.notes);
        view.publish(LOG, &self.log);
        view.publish(LOG_COUNT, &[self.log_count as f32]);
        let mut voices = [-1.0f32; MPE_VOICES * MPE_VALUES];
        for (i, v) in self.voices.iter().filter(|v| v.active).enumerate() {
            let at = i * MPE_VALUES;
            voices[at] = v.key as f32;
            voices[at + 1] = v.velocity;
            voices[at + 2] = v.slide;
            voices[at + 3] = v.pressure;
            voices[at + 4] = v.bend;
        }
        view.publish(MPE, &voices);
        self.changed = false;
        self.published_count = self.log_count;
    }

    fn clear(&mut self) {
        self.notes = [0.0; NOTE_COUNT];
        self.voices = [Voice::default(); MPE_VOICES];
        self.changed = true;
    }
}

midi_fx_processor!(MidiMonitor);

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.midi_monitor",
    name: "MIDI Monitor",
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
    create: |_| Box::<MidiMonitor>::default(),
};
