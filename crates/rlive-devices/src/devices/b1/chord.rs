//! Chord: adds up to six notes to each incoming note, optionally strummed.

use super::common::*;
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    DeviceDesc, ExprKind, Item, MidiMessage, ParamId, ParamInfo, Ports, ScaleInfo, Section, Status,
    TransportInfo, ViewId, ViewWriter,
};

pub const FIRST_SHIFT: u32 = 0;
pub const FIRST_VELOCITY: u32 = 6;
pub const FIRST_CHANCE: u32 = 12;
pub const VELOCITY_ON: u32 = 18;
pub const CHANCE_ON: u32 = 19;
pub const STRUM: u32 = 20;
pub const TENSION: u32 = 21;
pub const CRESCENDO: u32 = 22;
pub const USE_SCALE: u32 = 23;
pub const PLAY_DUPLICATES: u32 = 24;
pub const PER_NOTE_EVENTS: u32 = 25;
pub const LEARN: u32 = 26;

pub const LEDS: ViewId = ViewId(0);
pub const LED_COUNT: usize = 6;

const SHIFTS: usize = 6;
/// The root and its six shifts.
const MAX_GENERATED: usize = SHIFTS + 1;
const MAX_SLOTS: usize = 64;

pub fn params() -> Vec<ParamInfo> {
    let mut out = Vec::new();
    for i in 0..SHIFTS as u32 {
        out.push(int_param(
            FIRST_SHIFT + i,
            &format!("Shift {}", i + 1),
            -36.0,
            36.0,
            0.0,
        ));
    }
    for i in 0..SHIFTS as u32 {
        out.push(percent_param(
            FIRST_VELOCITY + i,
            &format!("Velocity {}", i + 1),
            1.0,
            200.0,
            100.0,
        ));
    }
    for i in 0..SHIFTS as u32 {
        out.push(percent_param(
            FIRST_CHANCE + i,
            &format!("Chance {}", i + 1),
            0.0,
            100.0,
            100.0,
        ));
    }
    out.extend([
        ParamInfo::toggle(VELOCITY_ON, "Velocity On", false),
        ParamInfo::toggle(CHANCE_ON, "Chance On", false),
        ParamInfo::continuous(STRUM, "Strum", -400.0, 400.0, 0.0, "ms"),
        percent_param(TENSION, "Tension", -100.0, 100.0, 0.0),
        percent_param(CRESCENDO, "Crescendo", -100.0, 100.0, 0.0),
        ParamInfo::toggle(USE_SCALE, "Use Current Scale", false),
        ParamInfo::toggle(
            PLAY_DUPLICATES,
            "Play Duplicate Notes When Strumming",
            false,
        ),
        ParamInfo::toggle(
            PER_NOTE_EVENTS,
            "Send Per Note Events to Generated Notes",
            false,
        ),
        ParamInfo::toggle(LEARN, "Learn", false),
    ]);
    out
}

fn layout() -> Vec<Section> {
    let id = ParamId;
    let shifts: Vec<Item> = (0..SHIFTS as u32)
        .map(|i| Item::Knob(id(FIRST_SHIFT + i)))
        .collect();
    let velocities: Vec<Item> = (0..SHIFTS as u32)
        .map(|i| Item::Slider(id(FIRST_VELOCITY + i)))
        .collect();
    let chances: Vec<Item> = (0..SHIFTS as u32)
        .map(|i| Item::Slider(id(FIRST_CHANCE + i)))
        .collect();
    vec![
        Section {
            title: "Shift",
            items: vec![Item::Row(shifts), Item::Panel("chord_leds")],
        },
        Section {
            title: "Velocity",
            items: [vec![Item::Toggle(id(VELOCITY_ON))], velocities].concat(),
        },
        Section {
            title: "Chance",
            items: [vec![Item::Toggle(id(CHANCE_ON))], chances].concat(),
        },
        Section {
            title: "Strum",
            items: vec![
                Item::Knob(id(STRUM)),
                Item::Knob(id(TENSION)),
                Item::Knob(id(CRESCENDO)),
            ],
        },
        Section {
            title: "Options",
            items: vec![
                Item::Toggle(id(USE_SCALE)),
                Item::Toggle(id(PLAY_DUPLICATES)),
                Item::Toggle(id(PER_NOTE_EVENTS)),
                Item::Toggle(id(LEARN)),
            ],
        },
    ]
}

#[derive(Clone, Copy)]
struct Generated {
    out: NoteRef,
    delay: u64,
    /// The Shift this note came from: `None` for the note that was played.
    shift: Option<i32>,
}

#[derive(Clone, Copy)]
struct Slot {
    src: NoteRef,
    started: u64,
    notes: [Generated; MAX_GENERATED],
    count: usize,
}

pub struct Chord {
    core: Core,
    shifts: [i32; SHIFTS],
    velocities: [f32; SHIFTS],
    chances: [f32; SHIFTS],
    velocity_on: bool,
    chance_on: bool,
    strum_ms: f64,
    tension: f64,
    crescendo: f32,
    use_scale: bool,
    play_duplicates: bool,
    per_note: bool,
    learn: bool,
    transposer: Transposer,
    clip: Option<ScaleInfo>,
    rng: Rng,
    slots: Vec<Slot>,
    leds: [f32; LED_COUNT],
    learn_root: Option<u8>,
    learn_next: usize,
    /// Keys held since Learn was switched on, so the first one is known to be the root.
    learn_held: usize,
}

impl Chord {
    pub fn with_seed(seed: u64) -> Self {
        Self {
            core: Core::default(),
            shifts: [0; SHIFTS],
            velocities: [1.0; SHIFTS],
            chances: [1.0; SHIFTS],
            velocity_on: false,
            chance_on: false,
            strum_ms: 0.0,
            tension: 0.0,
            crescendo: 0.0,
            use_scale: false,
            play_duplicates: false,
            per_note: false,
            learn: false,
            transposer: Transposer::Chromatic,
            clip: None,
            rng: Rng::new(seed),
            slots: Vec::with_capacity(MAX_SLOTS),
            leds: [0.0; LED_COUNT],
            learn_root: None,
            learn_next: 0,
            learn_held: 0,
        }
    }

    fn duplicates_allowed(&self) -> bool {
        self.play_duplicates && self.strum_ms != 0.0
    }

    /// Multiplies gap `i` of `gaps` by this. Tension keeps the total length the same.
    fn gap_scale(&self, i: usize, gaps: usize) -> f64 {
        if gaps <= 1 {
            return 1.0;
        }
        let progress = i as f64 / (gaps - 1) as f64;
        1.0 + self.tension * (1.0 - 2.0 * progress)
    }

    fn learn_note(&mut self, key: u8) {
        if self.learn_held == 0 {
            self.learn_root = Some(key);
            self.learn_next = 0;
            for i in 0..SHIFTS {
                self.shifts[i] = 0;
                self.core.report(FIRST_SHIFT + i as u32, 0.0);
            }
        } else if let Some(root) = self.learn_root {
            let shift = (key as i32 - root as i32).clamp(-36, 36);
            if self.learn_next < SHIFTS && shift != 0 {
                let index = self.learn_next;
                self.shifts[index] = shift;
                self.core.report(FIRST_SHIFT + index as u32, shift as f64);
                self.learn_next += 1;
            }
        }
        self.learn_held += 1;
    }

    fn note_on(&mut self, src: NoteRef, velocity: f32, tuning: f32) {
        if self.learn {
            self.learn_note(src.key);
        }
        let now = self.core.now();
        let mut slot = Slot {
            src,
            started: now,
            notes: [Generated {
                out: src,
                delay: 0,
                shift: None,
            }; MAX_GENERATED],
            count: 1,
        };
        // The scale velocities apply before the strum changes the order.
        let mut velocities = [velocity; MAX_GENERATED];
        let allow_duplicates = self.duplicates_allowed();
        let mut used = [0; SHIFTS];
        let mut used_count = 0;
        for i in 0..SHIFTS {
            let shift = self.shifts[i];
            if shift == 0 {
                continue;
            }
            if !allow_duplicates && used[..used_count].contains(&shift) {
                continue;
            }
            used[used_count] = shift;
            used_count += 1;
            if self.chance_on && self.rng.unit() >= self.chances[i] {
                continue;
            }
            let moved = self.transposer.apply(src.key as i32, shift);
            if !(0..=127).contains(&moved) {
                continue;
            }
            let n = slot.count;
            slot.notes[n] = Generated {
                out: NoteRef {
                    channel: src.channel,
                    key: moved as u8,
                    id: device_note_id(),
                },
                delay: 0,
                shift: Some(shift),
            };
            velocities[n] = if self.velocity_on {
                velocity * self.velocities[i]
            } else {
                velocity
            };
            slot.count += 1;
            self.leds[i] = 1.0;
        }

        let count = slot.count;
        // In strum order, indexes into `slot.notes`.
        let mut order = [0usize; MAX_GENERATED];
        for (k, o) in order.iter_mut().enumerate().take(count) {
            *o = if self.strum_ms < 0.0 {
                count - 1 - k
            } else {
                k
            };
        }
        let gap = self.core.ms_to_frames(self.strum_ms.abs());
        let gaps = count.saturating_sub(1);
        let mut delay = 0.0;
        for (k, &index) in order.iter().enumerate().take(count) {
            slot.notes[index].delay = delay as u64;
            if self.strum_ms != 0.0 && self.crescendo != 0.0 && count > 1 {
                let progress = k as f32 / (count - 1) as f32;
                velocities[index] *= if self.crescendo > 0.0 {
                    1.0 - self.crescendo * (1.0 - progress)
                } else {
                    1.0 + self.crescendo * progress
                };
            }
            delay += gap * self.gap_scale(k, gaps);
        }

        for (n, velocity) in slot.notes.iter().zip(velocities).take(count) {
            self.core.note_on_after(
                n.delay,
                src,
                n.out,
                velocity.clamp(1.0 / 127.0, 1.0),
                tuning,
            );
        }
        if self.slots.len() < MAX_SLOTS {
            self.slots.push(slot);
        } else {
            for index in 0..count {
                let n = slot.notes[index];
                self.core.note_off_after(n.delay + 1, n.out, 0.0);
            }
        }
    }

    fn note_off(&mut self, src: NoteRef, velocity: f32, message: MidiMessage) {
        let Some(i) = self.slots.iter().position(|s| s.src.is(src)) else {
            self.core.pass_other(message);
            return;
        };
        let slot = self.slots.swap_remove(i);
        let now = self.core.now();
        for n in slot.notes[..slot.count].iter() {
            // A note can't end on the frame it starts, even if it was only just played.
            let off_at = (now + n.delay).max(slot.started + n.delay + 1);
            self.core.note_off_after(off_at - now, n.out, velocity);
        }
    }

    /// The bend a generated note gets, so a bend of the played note lands in the same place.
    fn follow_bend(&self, src: NoteRef, shift: i32, out_key: u8, value: f32) -> f32 {
        match (self.transposer, self.clip) {
            (Transposer::Scale(scale), _) => {
                transpose_degrees_fractional(src.key as f32 + value, shift, scale) - out_key as f32
            }
            _ => value,
        }
    }

    fn expr(&mut self, src: NoteRef, kind: ExprKind, value: f32, message: MidiMessage) {
        let Some(i) = self.slots.iter().position(|s| s.src.is(src)) else {
            self.core.pass_other(message);
            return;
        };
        let slot = self.slots[i];
        for n in slot.notes[..slot.count].iter() {
            let target_value = match (kind, n.shift) {
                (_, None) => value,
                (ExprKind::Pitch, Some(shift)) if self.per_note => {
                    self.follow_bend(src, shift, n.out.key, value)
                }
                (_, Some(_)) if self.per_note => value,
                _ => continue,
            };
            if self.core.is_sounding(n.out) {
                self.core.forward(MidiMessage::NoteExpr {
                    channel: n.out.channel,
                    key: n.out.key,
                    note_id: n.out.id,
                    kind,
                    value: target_value,
                });
            }
        }
    }
}

impl MidiFx for Chord {
    fn core(&mut self) -> &mut Core {
        &mut self.core
    }

    fn apply(&mut self, id: u32, value: f64) {
        match id {
            id if (FIRST_SHIFT..FIRST_SHIFT + SHIFTS as u32).contains(&id) => {
                self.shifts[(id - FIRST_SHIFT) as usize] = value.round() as i32;
            }
            id if (FIRST_VELOCITY..FIRST_VELOCITY + SHIFTS as u32).contains(&id) => {
                self.velocities[(id - FIRST_VELOCITY) as usize] = (value * 0.01) as f32;
            }
            id if (FIRST_CHANCE..FIRST_CHANCE + SHIFTS as u32).contains(&id) => {
                self.chances[(id - FIRST_CHANCE) as usize] = (value * 0.01) as f32;
            }
            VELOCITY_ON => self.velocity_on = value >= 0.5,
            CHANCE_ON => self.chance_on = value >= 0.5,
            STRUM => self.strum_ms = value,
            TENSION => self.tension = value * 0.01,
            CRESCENDO => self.crescendo = (value * 0.01) as f32,
            USE_SCALE => self.use_scale = value >= 0.5,
            PLAY_DUPLICATES => self.play_duplicates = value >= 0.5,
            PER_NOTE_EVENTS => self.per_note = value >= 0.5,
            LEARN => {
                let on = value >= 0.5;
                if on && !self.learn {
                    self.learn_root = None;
                    self.learn_next = 0;
                    self.learn_held = 0;
                }
                self.learn = on;
            }
            _ => {}
        }
    }

    fn begin(&mut self, transport: &TransportInfo) {
        self.transposer = Transposer::resolve(self.use_scale, transport);
        self.clip = transport.scale;
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
                self.note_on(src, velocity, tuning);
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
                if self.learn {
                    self.learn_held = self.learn_held.saturating_sub(1);
                }
                self.note_off(src, velocity, message);
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
                self.expr(src, kind, value, message);
            }
            other => self.core.pass_other(other),
        }
    }

    fn end(&mut self, _mod_out: &mut [f32], view: &ViewWriter) {
        if view.is_active() {
            view.publish(LEDS, &self.leds);
        }
        for led in &mut self.leds {
            *led *= 0.85;
        }
    }

    fn clear(&mut self) {
        self.slots.clear();
    }
}

midi_fx_processor!(Chord);

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.chord",
    name: "Chord",
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
    create: |_| Box::new(Chord::with_seed(entropy_seed())),
};
