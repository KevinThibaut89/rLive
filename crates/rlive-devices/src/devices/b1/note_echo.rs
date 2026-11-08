//! Note Echo: repeats notes with decreasing velocity.

use super::common::*;
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    DeviceDesc, ExprKind, Item, MidiMessage, ParamId, ParamInfo, Ports, Section, Status,
};

pub const SYNC: u32 = 0;
pub const DELAY_SIXTEENTHS: u32 = 1;
pub const DELAY_OFFSET: u32 = 2;
pub const DELAY_TIME: u32 = 3;
pub const INPUT: u32 = 4;
pub const PITCH: u32 = 5;
pub const DELAY: u32 = 6;
pub const FEEDBACK: u32 = 7;
pub const MPE: u32 = 8;
pub const PRESS: u32 = 9;
pub const SLIDE: u32 = 10;
pub const NOTE_PB: u32 = 11;

/// Echoes one note can spawn.
const MAX_ECHOES: usize = 16;
/// Echoing notes one device can have going.
const MAX_SLOTS: usize = 64;
const SIXTEENTH_BEATS: f64 = 0.25;
/// Velocities below this aren't echoed: they would make a note-on nothing can hear.
const MIN_VELOCITY: f32 = 1.0 / 127.0;

pub fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::toggle(SYNC, "Sync", true),
        int_param(DELAY_SIXTEENTHS, "Delay Time", 1.0, 8.0, 4.0),
        percent_param(DELAY_OFFSET, "Delay Offset", -33.3, 33.3, 0.0),
        ParamInfo::skewed(
            DELAY_TIME,
            "Delay Time Free",
            1.0,
            10_000.0,
            250.0,
            "ms",
            3.0,
        ),
        ParamInfo::stepped(INPUT, "Input", &["Thru", "Mute"], 0),
        int_param(PITCH, "Pitch", -24.0, 24.0, 0.0),
        percent_param(DELAY, "Delay", 0.0, 100.0, 75.0),
        percent_param(FEEDBACK, "Fback", 0.0, 100.0, 50.0),
        ParamInfo::toggle(MPE, "MPE", false),
        percent_param(PRESS, "Press", 0.0, 100.0, 100.0),
        percent_param(SLIDE, "Slide", 0.0, 100.0, 100.0),
        percent_param(NOTE_PB, "Note PB", 0.0, 100.0, 100.0),
    ]
}

fn layout() -> Vec<Section> {
    let id = ParamId;
    vec![
        Section {
            title: "Delay",
            items: vec![
                Item::Toggle(id(SYNC)),
                Item::Knob(id(DELAY_SIXTEENTHS)),
                Item::Knob(id(DELAY_OFFSET)),
                Item::Knob(id(DELAY_TIME)),
            ],
        },
        Section {
            title: "Echo",
            items: vec![
                Item::Menu(id(INPUT)),
                Item::Knob(id(PITCH)),
                Item::Knob(id(DELAY)),
                Item::Knob(id(FEEDBACK)),
            ],
        },
        Section {
            title: "MPE",
            items: vec![
                Item::Toggle(id(MPE)),
                Item::Knob(id(PRESS)),
                Item::Knob(id(SLIDE)),
                Item::Knob(id(NOTE_PB)),
            ],
        },
    ]
}

/// One echo of a note.
#[derive(Clone, Copy)]
struct Echo {
    out: NoteRef,
    delay: u64,
}

#[derive(Clone, Copy)]
struct Slot {
    src: NoteRef,
    started: u64,
    echoes: [Echo; MAX_ECHOES],
    count: usize,
}

pub struct NoteEcho {
    core: Core,
    synced: bool,
    sixteenths: f64,
    offset: f64,
    delay_ms: f64,
    mute_input: bool,
    pitch: i32,
    velocity: f32,
    feedback: f32,
    mpe: bool,
    press: f32,
    slide: f32,
    note_pb: f32,
    slots: Vec<Slot>,
}

impl Default for NoteEcho {
    fn default() -> Self {
        Self {
            core: Core::default(),
            synced: true,
            sixteenths: 4.0,
            offset: 0.0,
            delay_ms: 250.0,
            mute_input: false,
            pitch: 0,
            velocity: 0.75,
            feedback: 0.5,
            mpe: false,
            press: 1.0,
            slide: 1.0,
            note_pb: 1.0,
            slots: Vec::with_capacity(MAX_SLOTS),
        }
    }
}

impl NoteEcho {
    fn delay_frames(&self) -> u64 {
        let frames = if self.synced {
            let beats = self.sixteenths * SIXTEENTH_BEATS * (1.0 + self.offset);
            self.core.beats_to_frames(beats)
        } else {
            self.core.ms_to_frames(self.delay_ms)
        };
        (frames.round() as u64).max(1)
    }

    fn start_echoes(&mut self, src: NoteRef, velocity: f32, tuning: f32) {
        let now = self.core.now();
        let delay = self.delay_frames();
        let mut slot = Slot {
            src,
            started: now,
            echoes: [Echo { out: src, delay: 0 }; MAX_ECHOES],
            count: 0,
        };
        let mut level = velocity * self.velocity;
        for n in 1..=MAX_ECHOES {
            if level < MIN_VELOCITY {
                break;
            }
            let key = src.key as i32 + self.pitch * n as i32;
            if !(0..=127).contains(&key) {
                break;
            }
            let out = NoteRef {
                channel: src.channel,
                key: key as u8,
                id: device_note_id(),
            };
            let at = delay * n as u64;
            self.core.note_on_after(at, GENERATED, out, level, tuning);
            slot.echoes[slot.count] = Echo { out, delay: at };
            slot.count += 1;
            level *= self.feedback;
        }
        if self.slots.len() < MAX_SLOTS {
            self.slots.push(slot);
        } else {
            for e in slot.echoes[..slot.count].iter() {
                self.core.note_off_after(e.delay + 1, e.out, 0.0);
            }
        }
    }

    fn end_echoes(&mut self, src: NoteRef) {
        if let Some(i) = self.slots.iter().position(|s| s.src.is(src)) {
            let slot = self.slots.swap_remove(i);
            let now = self.core.now();
            for e in slot.echoes[..slot.count].iter() {
                let off_at = (now + e.delay).max(slot.started + e.delay + 1);
                self.core.note_off_after(off_at - now, e.out, 0.0);
            }
        }
    }

    fn echo_expression(&mut self, src: NoteRef, kind: ExprKind, value: f32) {
        let Some(i) = self.slots.iter().position(|s| s.src.is(src)) else {
            return;
        };
        let amount = match kind {
            ExprKind::Pressure => self.press,
            ExprKind::Slide => self.slide,
            ExprKind::Pitch => self.note_pb,
        };
        let slot = self.slots[i];
        let mut scale = amount;
        for e in slot.echoes[..slot.count].iter() {
            let message = MidiMessage::NoteExpr {
                channel: e.out.channel,
                key: e.out.key,
                note_id: e.out.id,
                kind,
                value: value * scale,
            };
            self.core.schedule(e.delay, message, GENERATED);
            scale *= amount;
        }
    }
}

impl MidiFx for NoteEcho {
    fn core(&mut self) -> &mut Core {
        &mut self.core
    }

    fn apply(&mut self, id: u32, value: f64) {
        match id {
            SYNC => self.synced = value >= 0.5,
            DELAY_SIXTEENTHS => self.sixteenths = value.round().clamp(1.0, 8.0),
            DELAY_OFFSET => self.offset = value * 0.01,
            DELAY_TIME => self.delay_ms = value,
            INPUT => self.mute_input = value >= 0.5,
            PITCH => self.pitch = value.round() as i32,
            DELAY => self.velocity = (value * 0.01) as f32,
            FEEDBACK => self.feedback = (value * 0.01) as f32,
            MPE => self.mpe = value >= 0.5,
            PRESS => self.press = (value * 0.01) as f32,
            SLIDE => self.slide = (value * 0.01) as f32,
            NOTE_PB => self.note_pb = (value * 0.01) as f32,
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
                if !self.mute_input {
                    self.core.note_on(src, src, velocity, tuning);
                }
                self.start_echoes(src, velocity, tuning);
            }
            MidiMessage::NoteOff {
                channel,
                key,
                note_id,
                ..
            } => {
                let src = NoteRef {
                    channel,
                    key,
                    id: note_id,
                };
                self.end_echoes(src);
                self.core.pass_other(message);
            }
            MidiMessage::NoteExpr {
                channel,
                key,
                note_id,
                kind,
                value,
            } => {
                let src = NoteRef {
                    channel,
                    key,
                    id: note_id,
                };
                if self.mpe {
                    self.echo_expression(src, kind, value);
                }
                self.core.pass_other(message);
            }
            other => self.core.pass_other(other),
        }
    }

    fn clear(&mut self) {
        self.slots.clear();
    }
}

midi_fx_processor!(NoteEcho);

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.note_echo",
    name: "Note Echo",
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
    create: |_| Box::<NoteEcho>::default(),
};
