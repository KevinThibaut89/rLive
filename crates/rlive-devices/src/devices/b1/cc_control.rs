//! CC Control: sends MIDI CC, pitch bend and pressure from sixteen controls.

use super::common::*;
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{DeviceDesc, Item, MidiMessage, ParamId, ParamInfo, Ports, Section, Status};

pub const MOD_WHEEL: u32 = 0;
pub const PITCH_BEND: u32 = 1;
pub const PRESSURE: u32 = 2;
pub const CUSTOM_A: u32 = 3;
pub const CUSTOM_A_CC: u32 = 4;
/// The values of Custom B to M.
pub const FIRST_CUSTOM: u32 = 5;
/// The controller numbers of Custom B to M.
pub const FIRST_CUSTOM_CC: u32 = 17;
pub const LEARN: u32 = 29;
pub const SEND: u32 = 30;
/// Which control incoming CCs are learned onto while Learn is on: 0 is Custom A.
pub const LEARN_TARGET: u32 = 31;

const CUSTOM_DIALS: usize = 12;
/// Custom A's controller when nothing has been chosen: the sustain pedal.
const DEFAULT_A_CC: f64 = 64.0;
/// Where the dials' controllers start by default, clear of the common ones.
const DEFAULT_FIRST_CC: f64 = 20.0;
const LEARN_LABELS: [&str; 13] = [
    "Custom A", "Custom B", "Custom C", "Custom D", "Custom E", "Custom F", "Custom G", "Custom H",
    "Custom I", "Custom J", "Custom K", "Custom L", "Custom M",
];

pub fn params() -> Vec<ParamInfo> {
    let labels = cc_labels();
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    let mut out = vec![
        int_param(MOD_WHEEL, "Mod Wheel", 0.0, 127.0, 0.0),
        int_param(PITCH_BEND, "Pitch Bend", -8192.0, 8191.0, 0.0),
        int_param(PRESSURE, "Pressure", 0.0, 127.0, 0.0),
        ParamInfo::toggle(CUSTOM_A, "Custom A", false),
        ParamInfo::stepped(CUSTOM_A_CC, "Custom A CC", &labels, DEFAULT_A_CC as usize),
    ];
    for i in 0..CUSTOM_DIALS as u32 {
        let letter = (b'B' + i as u8) as char;
        out.push(int_param(
            FIRST_CUSTOM + i,
            &format!("Custom {letter}"),
            0.0,
            127.0,
            0.0,
        ));
    }
    for i in 0..CUSTOM_DIALS as u32 {
        let letter = (b'B' + i as u8) as char;
        out.push(ParamInfo::stepped(
            FIRST_CUSTOM_CC + i,
            &format!("Custom {letter} CC"),
            &labels,
            (DEFAULT_FIRST_CC + i as f64) as usize,
        ));
    }
    out.extend([
        ParamInfo::toggle(LEARN, "Learn", false),
        ParamInfo::toggle(SEND, "Send", false),
        ParamInfo::stepped(LEARN_TARGET, "Learn Target", &LEARN_LABELS, 0),
    ]);
    out
}

fn layout() -> Vec<Section> {
    let id = ParamId;
    let dial = |i: u32| Item::Knob(id(FIRST_CUSTOM + i));
    let cc = |i: u32| Item::Menu(id(FIRST_CUSTOM_CC + i));
    let group = |range: std::ops::Range<u32>| -> Vec<Item> {
        range.flat_map(|i| [dial(i), cc(i)]).collect()
    };
    vec![
        Section {
            title: "Controls 1-8",
            items: [
                vec![
                    Item::Knob(id(MOD_WHEEL)),
                    Item::Knob(id(PITCH_BEND)),
                    Item::Knob(id(PRESSURE)),
                    Item::Toggle(id(CUSTOM_A)),
                    Item::Menu(id(CUSTOM_A_CC)),
                ],
                group(0..4),
            ]
            .concat(),
        },
        Section {
            title: "Controls 9-16",
            items: group(4..12),
        },
        Section {
            title: "",
            items: vec![
                Item::Toggle(id(SEND)),
                Item::Toggle(id(LEARN)),
                Item::Menu(id(LEARN_TARGET)),
            ],
        },
    ]
}

pub struct CcControl {
    core: Core,
    mod_wheel: u8,
    pitch_bend: i32,
    pressure: u8,
    custom_a: bool,
    custom_a_cc: u8,
    custom: [u8; CUSTOM_DIALS],
    custom_cc: [u8; CUSTOM_DIALS],
    learn: bool,
    learn_target: usize,
    send_held: bool,
}

impl Default for CcControl {
    fn default() -> Self {
        let mut custom_cc = [0; CUSTOM_DIALS];
        for (i, cc) in custom_cc.iter_mut().enumerate() {
            *cc = (DEFAULT_FIRST_CC as usize + i) as u8;
        }
        Self {
            core: Core::default(),
            mod_wheel: 0,
            pitch_bend: 0,
            pressure: 0,
            custom_a: false,
            custom_a_cc: DEFAULT_A_CC as u8,
            custom: [0; CUSTOM_DIALS],
            custom_cc,
            learn: false,
            learn_target: 0,
            send_held: false,
        }
    }
}

impl CcControl {
    fn send_cc(&mut self, cc: u8, value: u8) {
        self.core.forward(MidiMessage::ControlChange {
            channel: 0,
            cc: cc & 0x7f,
            value: value.min(127),
        });
    }

    fn send_bend(&mut self) {
        self.core.forward(MidiMessage::PitchBend {
            channel: 0,
            value: self.pitch_bend as f32 / 8192.0,
        });
    }

    fn send_pressure(&mut self) {
        self.core.forward(MidiMessage::ChannelPressure {
            channel: 0,
            value: self.pressure as f32 / 127.0,
        });
    }

    fn send_all(&mut self) {
        self.send_cc(1, self.mod_wheel);
        self.send_bend();
        self.send_pressure();
        self.send_cc(self.custom_a_cc, if self.custom_a { 127 } else { 0 });
        for i in 0..CUSTOM_DIALS {
            self.send_cc(self.custom_cc[i], self.custom[i]);
        }
    }

    fn learned(&mut self, cc: u8, value: u8) {
        if self.learn_target == 0 {
            self.custom_a_cc = cc;
            self.core.report(CUSTOM_A_CC, cc as f64);
        } else {
            let i = (self.learn_target - 1).min(CUSTOM_DIALS - 1);
            self.custom_cc[i] = cc;
            self.custom[i] = value;
            self.core.report(FIRST_CUSTOM_CC + i as u32, cc as f64);
            self.core.report(FIRST_CUSTOM + i as u32, value as f64);
        }
    }
}

impl MidiFx for CcControl {
    fn core(&mut self) -> &mut Core {
        &mut self.core
    }

    fn apply(&mut self, id: u32, value: f64) {
        match id {
            MOD_WHEEL => self.mod_wheel = value as u8,
            PITCH_BEND => self.pitch_bend = value as i32,
            PRESSURE => self.pressure = value as u8,
            CUSTOM_A => self.custom_a = value >= 0.5,
            CUSTOM_A_CC => self.custom_a_cc = value as u8,
            id if (FIRST_CUSTOM..FIRST_CUSTOM + CUSTOM_DIALS as u32).contains(&id) => {
                self.custom[(id - FIRST_CUSTOM) as usize] = value as u8;
            }
            id if (FIRST_CUSTOM_CC..FIRST_CUSTOM_CC + CUSTOM_DIALS as u32).contains(&id) => {
                self.custom_cc[(id - FIRST_CUSTOM_CC) as usize] = value as u8;
            }
            LEARN => self.learn = value >= 0.5,
            SEND => self.send_held = value >= 0.5,
            LEARN_TARGET => self.learn_target = value.round() as usize,
            _ => {}
        }
    }

    fn param_changed(&mut self, id: u32, value: f64) {
        let sends_now = value >= 0.5;
        let was_held = self.send_held;
        self.apply(id, value);
        match id {
            MOD_WHEEL => self.send_cc(1, self.mod_wheel),
            PITCH_BEND => self.send_bend(),
            PRESSURE => self.send_pressure(),
            CUSTOM_A => self.send_cc(self.custom_a_cc, if self.custom_a { 127 } else { 0 }),
            id if (FIRST_CUSTOM..FIRST_CUSTOM + CUSTOM_DIALS as u32).contains(&id) => {
                let i = (id - FIRST_CUSTOM) as usize;
                self.send_cc(self.custom_cc[i], self.custom[i]);
            }
            SEND if sends_now && !was_held => {
                self.send_all();
                // It springs back, so the next press is a new one.
                self.send_held = false;
                self.core.report(SEND, 0.0);
            }
            _ => {}
        }
    }

    fn midi(&mut self, message: MidiMessage) {
        if let (true, MidiMessage::ControlChange { cc, value, .. }) = (self.learn, message) {
            self.learned(cc, value);
        }
        self.core.pass_through(message);
    }
}

midi_fx_processor!(CcControl);

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.cc_control",
    name: "CC Control",
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
    create: |_| Box::<CcControl>::default(),
};
