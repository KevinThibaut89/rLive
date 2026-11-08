//! Scale: remaps each incoming note through a 13x13 note matrix.

use super::common::*;
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    DeviceDesc, Item, MidiMessage, ParamId, ParamInfo, Ports, ScaleInfo, Section, Status,
    TransportInfo,
};

pub const BASE: u32 = 0;
pub const SCALE_NAME: u32 = 1;
pub const TRANSPOSE: u32 = 2;
pub const FOLD: u32 = 3;
pub const LOWEST: u32 = 4;
pub const RANGE: u32 = 5;
pub const USE_SCALE: u32 = 6;
/// The user matrix, one parameter for each of the first twelve columns.
pub const FIRST_MAP: u32 = 7;
/// The value of a matrix column whose note is removed.
pub const REMOVED: i32 = -1;

pub const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// The index of the User entry in the Scale Name menu.
pub fn user_scale_index() -> usize {
    SCALES.len()
}

pub fn params() -> Vec<ParamInfo> {
    let names: Vec<&str> = SCALES
        .iter()
        .map(|(n, _)| *n)
        .chain(std::iter::once("User"))
        .collect();
    let mut out = vec![
        ParamInfo::stepped(BASE, "Base", &NOTE_NAMES, 0),
        ParamInfo::stepped(SCALE_NAME, "Scale Name", &names, user_scale_index()),
        int_param(TRANSPOSE, "Transpose", -36.0, 36.0, 0.0),
        ParamInfo::toggle(FOLD, "Fold", false),
        int_param(LOWEST, "Lowest", 0.0, 127.0, 0.0),
        int_param(RANGE, "Range", 0.0, 127.0, 127.0),
        ParamInfo::toggle(USE_SCALE, "Use Current Scale", false),
    ];
    for column in 0..12 {
        out.push(int_param(
            FIRST_MAP + column,
            &format!("Map {column}"),
            REMOVED as f64,
            12.0,
            column as f64,
        ));
    }
    out
}

fn layout() -> Vec<Section> {
    let id = ParamId;
    vec![
        Section {
            title: "Note Matrix",
            items: vec![Item::Panel("scale_matrix")],
        },
        Section {
            title: "Scale",
            items: vec![
                Item::Menu(id(BASE)),
                Item::Menu(id(SCALE_NAME)),
                Item::Knob(id(TRANSPOSE)),
                Item::Toggle(id(FOLD)),
                Item::Toggle(id(USE_SCALE)),
            ],
        },
        Section {
            title: "Range",
            items: vec![Item::Knob(id(LOWEST)), Item::Knob(id(RANGE))],
        },
    ]
}

/// The offsets a named scale's matrix holds: each column maps to the nearest scale note, ties
/// going up. Columns `0..=12` are semitones above the base.
pub fn named_matrix(mask: u16) -> [i32; 13] {
    let mut map = [0; 13];
    for (column, out) in map.iter_mut().enumerate() {
        let c = column as i32;
        let mut best = c;
        for distance in 0..12 {
            let up = c + distance;
            if up <= 12 && mask & (1 << (up % 12)) != 0 {
                best = up;
                break;
            }
            let down = c - distance;
            if down >= 0 && mask & (1 << (down % 12)) != 0 {
                best = down;
                break;
            }
        }
        *out = best;
    }
    map
}

/// The matrix in play and what to do with it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mapping {
    /// One entry for each of the 13 columns.
    pub matrix: [i32; 13],
    pub base: u8,
}

impl Mapping {
    /// Where `key` goes. `None` if its column has been removed.
    pub fn apply(&self, key: u8) -> Option<i32> {
        let from_base = key as i32 - self.base as i32;
        let column = from_base.rem_euclid(12);
        let mapped = self.matrix[column as usize];
        (mapped != REMOVED).then_some(key as i32 - column + mapped)
    }
}

/// Everything the mapping depends on, as the panel and the device both need it.
pub fn mapping_for(
    base: u8,
    scale_name: usize,
    user: &[i32; 12],
    clip: Option<ScaleInfo>,
) -> Mapping {
    if let Some(scale) = clip {
        return Mapping {
            matrix: named_matrix(scale.mask),
            base: scale.root,
        };
    }
    let matrix = if let Some((_, mask)) = SCALES.get(scale_name) {
        named_matrix(*mask)
    } else {
        let mut m = [0; 13];
        m[..12].copy_from_slice(user);
        m[12] = if user[0] == REMOVED {
            REMOVED
        } else {
            user[0] + 12
        };
        m
    };
    Mapping { matrix, base }
}

pub struct Scale {
    core: Core,
    base: u8,
    scale_name: usize,
    user: [i32; 12],
    transpose: i32,
    fold: bool,
    lowest: i32,
    range: i32,
    use_scale: bool,
    clip: Option<ScaleInfo>,
}

impl Default for Scale {
    fn default() -> Self {
        let mut user = [0; 12];
        for (i, u) in user.iter_mut().enumerate() {
            *u = i as i32;
        }
        Self {
            core: Core::default(),
            base: 0,
            scale_name: user_scale_index(),
            user,
            transpose: 0,
            fold: false,
            lowest: 0,
            range: 127,
            use_scale: false,
            clip: None,
        }
    }
}

impl Scale {
    fn is_in_range(&self, key: u8) -> bool {
        if self.range >= 127 {
            key as i32 >= self.lowest
        } else {
            (self.lowest..self.lowest + self.range).contains(&(key as i32))
        }
    }

    fn map(&self, key: u8) -> Option<u8> {
        if !self.is_in_range(key) {
            return Some(key);
        }
        let clip = self.clip.filter(|_| self.use_scale);
        let mapping = mapping_for(self.base, self.scale_name, &self.user, clip);
        let mut out = mapping.apply(key)?;
        // Fold is a property of the User scale.
        if self.fold && clip.is_none() && self.scale_name == user_scale_index() {
            let distance = out - key as i32;
            if distance > 6 {
                out -= 12;
            } else if distance < -6 {
                out += 12;
            }
        }
        out += self.transpose;
        Some(out.clamp(0, 127) as u8)
    }
}

impl MidiFx for Scale {
    fn core(&mut self) -> &mut Core {
        &mut self.core
    }

    fn apply(&mut self, id: u32, value: f64) {
        match id {
            BASE => self.base = value as u8 % 12,
            SCALE_NAME => self.scale_name = value.round() as usize,
            TRANSPOSE => self.transpose = value as i32,
            FOLD => self.fold = value >= 0.5,
            LOWEST => self.lowest = value as i32,
            RANGE => self.range = value as i32,
            USE_SCALE => self.use_scale = value >= 0.5,
            id if (FIRST_MAP..FIRST_MAP + 12).contains(&id) => {
                self.user[(id - FIRST_MAP) as usize] = value.round() as i32;
            }
            _ => {}
        }
    }

    fn begin(&mut self, transport: &TransportInfo) {
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
                match self.map(key) {
                    Some(out) => {
                        self.core
                            .note_on(src, NoteRef { key: out, ..src }, velocity, tuning)
                    }
                    None => self.core.block(src),
                }
            }
            other => self.core.pass_other(other),
        }
    }
}

midi_fx_processor!(Scale);

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.scale",
    name: "Scale",
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
    create: |_| Box::<Scale>::default(),
};
