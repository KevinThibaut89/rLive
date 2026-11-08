//! Random: randomises the pitch of incoming notes.

use super::common::*;
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    DeviceDesc, Item, MidiMessage, ParamId, ParamInfo, Ports, Section, Status, TransportInfo,
    ViewId, ViewWriter,
};

pub const CHANCE: u32 = 0;
pub const CHOICES: u32 = 1;
pub const INTERVAL: u32 = 2;
pub const MODE: u32 = 3;
pub const SIGN: u32 = 4;
pub const USE_SCALE: u32 = 5;

/// `+`, `0` and `-` LEDs, in that order.
pub const LEDS: ViewId = ViewId(0);
pub const LED_COUNT: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sign {
    #[default]
    Add,
    Sub,
    Bi,
}

pub fn params() -> Vec<ParamInfo> {
    vec![
        percent_param(CHANCE, "Chance", 0.0, 100.0, 50.0),
        int_param(CHOICES, "Choices", 1.0, 24.0, 1.0),
        int_param(INTERVAL, "Interval", 1.0, 24.0, 12.0),
        ParamInfo::stepped(MODE, "Mode", &["Random", "Alt"], 0),
        ParamInfo::stepped(SIGN, "Sign", &["Add", "Sub", "Bi"], 0),
        ParamInfo::toggle(USE_SCALE, "Use Current Scale", false),
    ]
}

fn layout() -> Vec<Section> {
    let id = ParamId;
    vec![
        Section {
            title: "Random",
            items: vec![
                Item::Knob(id(CHANCE)),
                Item::Panel("random_leds"),
                Item::Knob(id(CHOICES)),
                Item::Knob(id(INTERVAL)),
            ],
        },
        Section {
            title: "Mode",
            items: vec![
                Item::Menu(id(MODE)),
                Item::Menu(id(SIGN)),
                Item::Toggle(id(USE_SCALE)),
            ],
        },
    ]
}

pub struct Random {
    core: Core,
    chance: f32,
    choices: usize,
    interval: i32,
    alt: bool,
    sign: Sign,
    use_scale: bool,
    transposer: Transposer,
    /// How far through the round-robin Alt mode is.
    position: usize,
    rng: Rng,
    leds: [f32; LED_COUNT],
}

impl Random {
    pub fn with_seed(seed: u64) -> Self {
        Self {
            core: Core::default(),
            chance: 0.5,
            choices: 1,
            interval: 12,
            alt: false,
            sign: Sign::Add,
            use_scale: false,
            transposer: Transposer::Chromatic,
            position: 0,
            rng: Rng::new(seed),
            leds: [0.0; LED_COUNT],
        }
    }

    /// The transposition for the next note in the current mode, in intervals.
    fn next_steps(&mut self) -> i32 {
        if self.alt {
            if self.rng.unit() >= self.chance {
                return 0;
            }
            let k = self.position as i32;
            self.position = (self.position + 1) % self.choices;
            self.alt_steps(k)
        } else {
            if self.rng.unit() >= self.chance {
                return 0;
            }
            let k = (self.rng.below(self.choices) + 1) as i32;
            match self.sign {
                Sign::Add => k,
                Sign::Sub => -k,
                Sign::Bi if self.rng.unit() < 0.5 => k,
                Sign::Bi => -k,
            }
        }
    }

    /// Alt mode's fixed order. Bi works outward: 0, +1, -1, +2, -2...
    fn alt_steps(&self, k: i32) -> i32 {
        match self.sign {
            Sign::Add => k,
            Sign::Sub => -k,
            Sign::Bi => {
                if k % 2 == 1 {
                    (k + 1) / 2
                } else {
                    -(k / 2)
                }
            }
        }
    }
}

impl MidiFx for Random {
    fn core(&mut self) -> &mut Core {
        &mut self.core
    }

    fn apply(&mut self, id: u32, value: f64) {
        match id {
            CHANCE => self.chance = (value * 0.01) as f32,
            CHOICES => {
                self.choices = (value.round() as usize).max(1);
                self.position %= self.choices;
            }
            INTERVAL => self.interval = value.round() as i32,
            MODE => self.alt = value >= 0.5,
            SIGN => {
                self.sign = match value.round() as i32 {
                    1 => Sign::Sub,
                    2 => Sign::Bi,
                    _ => Sign::Add,
                }
            }
            USE_SCALE => self.use_scale = value >= 0.5,
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
                let steps = self.next_steps();
                let moved = self.transposer.apply(key as i32, steps * self.interval);
                // Notes pushed off the end of the keyboard stay where they were played.
                let out = if (0..=127).contains(&moved) {
                    moved as u8
                } else {
                    key
                };
                let led = match out.cmp(&key) {
                    std::cmp::Ordering::Greater => 0,
                    std::cmp::Ordering::Equal => 1,
                    std::cmp::Ordering::Less => 2,
                };
                self.leds[led] = 1.0;
                self.core
                    .note_on(src, NoteRef { key: out, ..src }, velocity, tuning);
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
        self.position = 0;
    }
}

midi_fx_processor!(Random);

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.random",
    name: "Random",
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
    create: |_| Box::new(Random::with_seed(entropy_seed())),
};
