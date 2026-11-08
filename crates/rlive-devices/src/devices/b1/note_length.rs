//! Note Length: sets how long notes last, and can trigger them from note-offs instead.

use super::common::*;
use crate::dsp::tempo::SyncRate;
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{DeviceDesc, Item, MidiMessage, ParamId, ParamInfo, Ports, Section, Status};

pub const TRIGGER: u32 = 0;
pub const TIME_MODE: u32 = 1;
pub const LENGTH_MS: u32 = 2;
pub const LENGTH_SYNC: u32 = 3;
pub const GATE: u32 = 4;
pub const RELEASE_VELOCITY: u32 = 5;
pub const DECAY_TIME: u32 = 6;
pub const KEY_SCALE: u32 = 7;
pub const LATCH: u32 = 8;

/// A Decay Time at or above this does no decaying at all.
const MAX_DECAY_MS: f64 = 60_000.0;
/// Incoming notes one device can be waiting on.
const MAX_WAITING: usize = 64;
/// Notes one device can keep latched.
const MAX_LATCHED: usize = 64;
/// C3: where Key Scale leaves lengths alone.
const KEY_SCALE_CENTRE: f64 = 60.0;

pub fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::stepped(TRIGGER, "Trigger", &["Note On", "Note Off"], 0),
        ParamInfo::stepped(TIME_MODE, "Time Mode", &["Time", "Sync"], 1),
        ParamInfo::skewed(LENGTH_MS, "Length", 1.0, 60_000.0, 250.0, "ms", 3.0),
        ParamInfo::stepped(LENGTH_SYNC, "Sync Length", &SyncRate::LABELS, 12),
        percent_param(GATE, "Gate", 0.0, 1000.0, 100.0),
        percent_param(RELEASE_VELOCITY, "Release Velocity", 0.0, 100.0, 0.0),
        ParamInfo::skewed(
            DECAY_TIME,
            "Decay Time",
            1.0,
            MAX_DECAY_MS,
            MAX_DECAY_MS,
            "ms",
            3.0,
        ),
        percent_param(KEY_SCALE, "Key Scale", -100.0, 100.0, 0.0),
        ParamInfo::toggle(LATCH, "Latch", false),
    ]
}

fn layout() -> Vec<Section> {
    let id = ParamId;
    vec![
        Section {
            title: "Length",
            items: vec![
                Item::Menu(id(TRIGGER)),
                Item::Menu(id(TIME_MODE)),
                Item::Knob(id(LENGTH_MS)),
                Item::Menu(id(LENGTH_SYNC)),
                Item::Knob(id(GATE)),
                Item::Toggle(id(LATCH)),
            ],
        },
        Section {
            title: "Note Off",
            items: vec![
                Item::Knob(id(RELEASE_VELOCITY)),
                Item::Knob(id(DECAY_TIME)),
                Item::Knob(id(KEY_SCALE)),
            ],
        },
    ]
}

/// An incoming note in Note Off mode, waiting for its note-off.
#[derive(Clone, Copy)]
struct Waiting {
    note: NoteRef,
    velocity: f32,
    tuning: f32,
    started: u64,
}

/// A note held by Latch, and the frame it started at.
#[derive(Clone, Copy)]
struct Latched {
    note: NoteRef,
    started: u64,
}

pub struct NoteLength {
    core: Core,
    note_off_trigger: bool,
    synced: bool,
    length_ms: f64,
    length_sync: SyncRate,
    gate: f64,
    release_velocity: f32,
    decay_ms: f64,
    key_scale: f64,
    latch: bool,
    waiting: Vec<Waiting>,
    /// Keys still physically held, so Latch in Note Off mode knows when they've all gone up.
    down: usize,
    pedal: bool,
    latched: Vec<Latched>,
}

impl Default for NoteLength {
    fn default() -> Self {
        Self {
            core: Core::default(),
            note_off_trigger: false,
            synced: true,
            length_ms: 250.0,
            length_sync: SyncRate::from_index(12.0),
            gate: 1.0,
            release_velocity: 0.0,
            decay_ms: MAX_DECAY_MS,
            key_scale: 0.0,
            latch: false,
            waiting: Vec::with_capacity(MAX_WAITING),
            down: 0,
            pedal: false,
            latched: Vec::with_capacity(MAX_LATCHED),
        }
    }
}

impl NoteLength {
    /// How long notes last, in frames. Key Scale only shapes notes triggered by note-offs.
    fn length_frames(&self, key: u8, scale_by_key: bool) -> u64 {
        let base = if self.synced {
            self.core.beats_to_frames(self.length_sync.beats())
        } else {
            self.core.ms_to_frames(self.length_ms)
        };
        let factor = if scale_by_key {
            2f64.powf(-self.key_scale * (key as f64 - KEY_SCALE_CENTRE) / 12.0)
        } else {
            1.0
        };
        ((base * self.gate * factor).round() as u64).max(1)
    }

    fn start_latched(&mut self, src: NoteRef, out: NoteRef, velocity: f32, tuning: f32) {
        let now = self.core.now();
        let earlier = self.latched.iter().any(|l| l.started != now);
        if earlier {
            self.release_latched();
        }
        self.core.note_on(src, out, velocity, tuning);
        if self.latched.len() < MAX_LATCHED {
            self.latched.push(Latched {
                note: out,
                started: now,
            });
        } else {
            self.core.note_off_after(1, out, 0.0);
        }
    }

    fn release_latched(&mut self) {
        for i in 0..self.latched.len() {
            self.core.note_off_after(0, self.latched[i].note, 0.0);
        }
        self.latched.clear();
    }

    /// Plays everything waiting: the moment all keys and the pedal have come up in Note Off
    /// Latch mode.
    fn fire_latched_group(&mut self) {
        self.release_latched();
        let now = self.core.now();
        for i in 0..self.waiting.len() {
            let w = self.waiting[i];
            let out = NoteRef {
                id: device_note_id(),
                ..w.note
            };
            self.core.note_on(GENERATED, out, w.velocity, w.tuning);
            if self.latched.len() < MAX_LATCHED {
                self.latched.push(Latched {
                    note: out,
                    started: now,
                });
            } else {
                self.core.note_off_after(1, out, 0.0);
            }
        }
        self.waiting.clear();
    }

    fn fire_note_off(&mut self, index: usize, release: f32) {
        let w = self.waiting.swap_remove(index);
        let mut velocity =
            w.velocity * (1.0 - self.release_velocity) + release * self.release_velocity;
        if self.decay_ms < MAX_DECAY_MS {
            let elapsed = (self.core.now() - w.started) as f64 * 1000.0 / self.core.sample_rate();
            let left = (1.0 - elapsed / self.decay_ms).max(0.0) as f32;
            velocity *= left;
            if left <= 0.0 {
                return;
            }
        }
        if velocity <= 0.0 {
            return;
        }
        let out = NoteRef {
            id: device_note_id(),
            ..w.note
        };
        self.core.note_on(GENERATED, out, velocity, w.tuning);
        let length = self.length_frames(w.note.key, true);
        self.core.note_off_after(length, out, 0.0);
    }

    fn maybe_fire_group(&mut self) {
        if self.latch && self.note_off_trigger && self.down == 0 && !self.pedal {
            self.fire_latched_group();
        }
    }
}

impl MidiFx for NoteLength {
    fn core(&mut self) -> &mut Core {
        &mut self.core
    }

    fn apply(&mut self, id: u32, value: f64) {
        match id {
            TRIGGER => self.note_off_trigger = value >= 0.5,
            TIME_MODE => self.synced = value >= 0.5,
            LENGTH_MS => self.length_ms = value,
            LENGTH_SYNC => self.length_sync = SyncRate::from_index(value),
            GATE => self.gate = value * 0.01,
            RELEASE_VELOCITY => self.release_velocity = (value * 0.01) as f32,
            DECAY_TIME => self.decay_ms = value,
            KEY_SCALE => self.key_scale = value * 0.01,
            LATCH => {
                let latch = value >= 0.5;
                if self.latch && !latch {
                    self.release_latched();
                }
                self.latch = latch;
            }
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
                if self.note_off_trigger {
                    self.down += 1;
                    if self.waiting.len() < MAX_WAITING {
                        self.waiting.push(Waiting {
                            note: src,
                            velocity,
                            tuning,
                            started: self.core.now(),
                        });
                    }
                } else if self.latch {
                    self.start_latched(src, src, velocity, tuning);
                } else {
                    self.core.note_on(src, src, velocity, tuning);
                    let length = self.length_frames(key, false);
                    self.core.note_off_after(length, src, 0.0);
                }
            }
            MidiMessage::NoteOff {
                channel,
                key,
                velocity,
                note_id,
            } => {
                let src = NoteRef {
                    channel,
                    key,
                    id: note_id,
                };
                if let Some(index) = self.waiting.iter().position(|w| w.note.is(src)) {
                    if self.note_off_trigger {
                        self.down = self.down.saturating_sub(1);
                        if self.latch {
                            self.maybe_fire_group();
                        } else {
                            self.fire_note_off(index, velocity);
                        }
                    } else {
                        // The trigger mode changed while it waited: it plays from here.
                        self.waiting.swap_remove(index);
                    }
                } else if !self.core.is_sounding_from(src) {
                    // Not one of ours, so it may belong to a note from before the device.
                    self.core.forward(message);
                }
                // Otherwise the length decides when this note ends, not its note-off.
            }
            MidiMessage::ControlChange { cc: 64, value, .. } => {
                self.pedal = value >= 64;
                self.core.forward(message);
                self.maybe_fire_group();
            }
            MidiMessage::NoteExpr { .. } | MidiMessage::AllNotesOff => {
                self.core.pass_other(message)
            }
            other => self.core.forward(other),
        }
    }

    fn clear(&mut self) {
        self.waiting.clear();
        self.latched.clear();
        self.down = 0;
        self.pedal = false;
    }
}

midi_fx_processor!(NoteLength);

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.note_length",
    name: "Note Length",
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
    create: |_| Box::<NoteLength>::default(),
};
