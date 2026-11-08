//! MPE Control: reshapes MPE pressure, slide and per-note pitch, and can turn them into global
//! MIDI for instruments without MPE.

use super::common::*;
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    DeviceDesc, ExprKind, Item, MidiMessage, ParamId, ParamInfo, Ports, Section, Status,
    MPE_BEND_RANGE,
};

pub const SOURCES: usize = 3;
/// Press parameters start here.
pub const PRESS: u32 = 0;
/// Slide parameters start here.
pub const SLIDE: u32 = 20;
/// NotePB parameters start here.
pub const NOTE_PB: u32 = 40;

// Offsets of each source's parameters.
pub const ON: u32 = 0;
pub const CURVE_TYPE: u32 = 1;
pub const CURVE_A: u32 = 2;
pub const CURVE_B: u32 = 3;
pub const CURVE_LINK: u32 = 4;
pub const MIN: u32 = 5;
pub const MAX: u32 = 6;
pub const X: u32 = 7;
pub const Y: u32 = 8;
pub const SMOOTH: u32 = 9;
pub const RISE: u32 = 10;
pub const FALL: u32 = 11;

// Press offsets.
pub const PRESS_DEFAULT: u32 = 12;
pub const PRESS_DEFAULT_VALUE: u32 = 13;
pub const SWAP_TO_SLIDE: u32 = 14;
pub const PRESS_TO_AT: u32 = 15;
// Slide offsets.
pub const SLIDE_DEFAULT: u32 = 12;
pub const SLIDE_DEFAULT_VALUE: u32 = 13;
pub const CENTERED: u32 = 14;
pub const SLIDE_MODE: u32 = 15;
pub const SLIDE_TO_MOD: u32 = 16;
// NotePB offsets.
pub const PITCH_RANGE: u32 = 12;
pub const NOTE_PB_TO_PB: u32 = 13;

const RANGE_LABELS: [&str; 9] = ["1x", "2x", "3x", "4x", "6x", "8x", "12x", "24x", "48x"];
const RANGE_VALUES: [f32; 9] = [1.0, 2.0, 3.0, 4.0, 6.0, 8.0, 12.0, 24.0, 48.0];

/// How many notes can be smoothed at once.
const MAX_NOTES: usize = 64;
/// How often smoothing sends out new values.
const SMOOTH_STEP_FRAMES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SlideMode {
    #[default]
    Abs,
    Rel,
    Ons,
}

fn source_params(base: u32, name: &str) -> Vec<ParamInfo> {
    let n = |s: &str| format!("{name} {s}");
    vec![
        ParamInfo::toggle(base + ON, &n("On"), true),
        ParamInfo::stepped(
            base + CURVE_TYPE,
            &n("Curve Type"),
            &["Linear", "S-Shaped"],
            0,
        ),
        percent_param(base + CURVE_A, &n("Curve"), -100.0, 100.0, 0.0),
        percent_param(base + CURVE_B, &n("Curve B"), -100.0, 100.0, 0.0),
        ParamInfo::toggle(base + CURVE_LINK, &n("Curve Link"), true),
        percent_param(base + MIN, &n("Min"), 0.0, 100.0, 0.0),
        percent_param(base + MAX, &n("Max"), 0.0, 100.0, 100.0),
        percent_param(base + X, &n("X"), 0.0, 100.0, 50.0),
        percent_param(base + Y, &n("Y"), 0.0, 100.0, 50.0),
        ParamInfo::toggle(base + SMOOTH, &n("Smooth"), false),
        ParamInfo::skewed(base + RISE, &n("Rise"), 0.0, 1000.0, 50.0, "ms", 3.0),
        ParamInfo::skewed(base + FALL, &n("Fall"), 0.0, 1000.0, 50.0, "ms", 3.0),
    ]
}

pub fn params() -> Vec<ParamInfo> {
    let mut out = source_params(PRESS, "Press");
    out.extend([
        ParamInfo::toggle(PRESS + PRESS_DEFAULT, "Press Default", false),
        percent_param(
            PRESS + PRESS_DEFAULT_VALUE,
            "Press Default Value",
            0.0,
            100.0,
            50.0,
        ),
        ParamInfo::toggle(PRESS + SWAP_TO_SLIDE, "Press Swap to Slide", false),
        ParamInfo::toggle(PRESS + PRESS_TO_AT, "Press to AT", false),
    ]);
    out.extend(source_params(SLIDE, "Slide"));
    out.extend([
        ParamInfo::toggle(SLIDE + SLIDE_DEFAULT, "Slide Default", false),
        percent_param(
            SLIDE + SLIDE_DEFAULT_VALUE,
            "Slide Default Value",
            0.0,
            100.0,
            50.0,
        ),
        ParamInfo::toggle(SLIDE + CENTERED, "Slide Centered", false),
        ParamInfo::stepped(SLIDE + SLIDE_MODE, "Slide Mode", &["Abs", "Rel", "Ons"], 0),
        ParamInfo::toggle(SLIDE + SLIDE_TO_MOD, "Slide to Mod", false),
    ]);
    out.extend(source_params(NOTE_PB, "NotePB"));
    out.extend([
        ParamInfo::stepped(
            NOTE_PB + PITCH_RANGE,
            "NotePB Pitch Range",
            &RANGE_LABELS,
            0,
        ),
        ParamInfo::toggle(NOTE_PB + NOTE_PB_TO_PB, "NotePB to PB", false),
    ]);
    out
}

fn source_section(title: &'static str, base: u32, extras: Vec<Item>) -> Section {
    let id = |p: u32| ParamId(base + p);
    let mut items = vec![
        Item::Toggle(id(ON)),
        Item::Menu(id(CURVE_TYPE)),
        Item::Knob(id(CURVE_A)),
        Item::Knob(id(CURVE_B)),
        Item::Toggle(id(CURVE_LINK)),
        Item::Knob(id(MIN)),
        Item::Knob(id(MAX)),
        Item::Knob(id(X)),
        Item::Knob(id(Y)),
        Item::Toggle(id(SMOOTH)),
        Item::Knob(id(RISE)),
        Item::Knob(id(FALL)),
    ];
    items.extend(extras);
    Section { title, items }
}

fn layout() -> Vec<Section> {
    let p = |id: u32| ParamId(id);
    vec![
        source_section(
            "Press",
            PRESS,
            vec![
                Item::Toggle(p(PRESS + PRESS_DEFAULT)),
                Item::Knob(p(PRESS + PRESS_DEFAULT_VALUE)),
                Item::Toggle(p(PRESS + SWAP_TO_SLIDE)),
                Item::Toggle(p(PRESS + PRESS_TO_AT)),
            ],
        ),
        source_section(
            "Slide",
            SLIDE,
            vec![
                Item::Toggle(p(SLIDE + SLIDE_DEFAULT)),
                Item::Knob(p(SLIDE + SLIDE_DEFAULT_VALUE)),
                Item::Toggle(p(SLIDE + CENTERED)),
                Item::Menu(p(SLIDE + SLIDE_MODE)),
                Item::Toggle(p(SLIDE + SLIDE_TO_MOD)),
            ],
        ),
        source_section(
            "NotePB",
            NOTE_PB,
            vec![
                Item::Menu(p(NOTE_PB + PITCH_RANGE)),
                Item::Toggle(p(NOTE_PB + NOTE_PB_TO_PB)),
            ],
        ),
    ]
}

#[derive(Clone, Copy, Default)]
struct Source {
    curve: Curve,
    on: bool,
    smooth: bool,
    rise_ms: f64,
    fall_ms: f64,
}

/// One note's per-source smoothing and the Rel/Ons slide state.
#[derive(Clone, Copy)]
struct Note {
    src: NoteRef,
    smoothers: [Smoother; SOURCES],
    /// The first slide value this note had, for Rel and Ons.
    slide_origin: Option<f32>,
    active: bool,
}

pub struct MpeControl {
    core: Core,
    sources: [Source; SOURCES],
    press_default: bool,
    press_default_value: f32,
    swap_to_slide: bool,
    press_to_at: bool,
    slide_default: bool,
    slide_default_value: f32,
    centered: bool,
    slide_mode: SlideMode,
    slide_to_mod: bool,
    pitch_range: f32,
    note_pb_to_pb: bool,
    notes: Vec<Note>,
    smooth_counter: usize,
}

impl Default for MpeControl {
    fn default() -> Self {
        let source = Source {
            curve: Curve::default(),
            on: true,
            smooth: false,
            rise_ms: 50.0,
            fall_ms: 50.0,
        };
        Self {
            core: Core::default(),
            sources: [source; SOURCES],
            press_default: false,
            press_default_value: 0.5,
            swap_to_slide: false,
            press_to_at: false,
            slide_default: false,
            slide_default_value: 0.5,
            centered: false,
            slide_mode: SlideMode::Abs,
            slide_to_mod: false,
            pitch_range: 1.0,
            note_pb_to_pb: false,
            notes: Vec::with_capacity(MAX_NOTES),
            smooth_counter: 0,
        }
    }
}

impl MpeControl {
    fn note_index(&self, src: NoteRef) -> Option<usize> {
        self.notes.iter().position(|n| n.active && n.src.is(src))
    }

    /// Sends a press value as whatever Press is set to become.
    fn send_press(&mut self, src: NoteRef, value: f32) {
        if self.press_to_at {
            self.core.forward(MidiMessage::ChannelPressure {
                channel: src.channel,
                value,
            });
        } else {
            let kind = if self.swap_to_slide {
                ExprKind::Slide
            } else {
                ExprKind::Pressure
            };
            self.core.forward(MidiMessage::NoteExpr {
                channel: src.channel,
                key: src.key,
                note_id: src.id,
                kind,
                value,
            });
        }
    }

    fn send_slide(&mut self, src: NoteRef, value: f32) {
        if self.slide_to_mod {
            self.core.forward(MidiMessage::ControlChange {
                channel: src.channel,
                cc: 1,
                value: (value.clamp(0.0, 1.0) * 127.0).round() as u8,
            });
        } else {
            self.core.forward(MidiMessage::NoteExpr {
                channel: src.channel,
                key: src.key,
                note_id: src.id,
                kind: ExprKind::Slide,
                value,
            });
        }
    }

    fn send_bend(&mut self, src: NoteRef, value: f32) {
        if self.note_pb_to_pb {
            self.core.forward(MidiMessage::PitchBend {
                channel: src.channel,
                value: (value / MPE_BEND_RANGE).clamp(-1.0, 1.0),
            });
        } else {
            self.core.forward(MidiMessage::NoteExpr {
                channel: src.channel,
                key: src.key,
                note_id: src.id,
                kind: ExprKind::Pitch,
                value,
            });
        }
    }

    fn send_for(&mut self, source: usize, src: NoteRef, value: f32) {
        match source {
            0 => self.send_press(src, value),
            1 => self.send_slide(src, value),
            _ => self.send_bend(src, value),
        }
    }

    fn press(&mut self, src: NoteRef, value: f32) {
        let s = self.sources[0];
        if !s.on {
            self.core.forward(MidiMessage::NoteExpr {
                channel: src.channel,
                key: src.key,
                note_id: src.id,
                kind: ExprKind::Pressure,
                value,
            });
            return;
        }
        self.dispatch(0, src, s.curve.apply(value), 0.0);
    }

    fn slide(&mut self, src: NoteRef, value: f32) {
        let s = self.sources[1];
        if !s.on {
            self.core.forward(MidiMessage::NoteExpr {
                channel: src.channel,
                key: src.key,
                note_id: src.id,
                kind: ExprKind::Slide,
                value,
            });
            return;
        }
        let mut input = value;
        if let Some(i) = self.note_index(src) {
            match self.slide_mode {
                SlideMode::Abs => {}
                SlideMode::Rel => {
                    let origin = *self.notes[i].slide_origin.get_or_insert(value);
                    input = (value - origin + 0.5).clamp(0.0, 1.0);
                }
                SlideMode::Ons => {
                    if self.notes[i].slide_origin.is_some() {
                        return;
                    }
                    self.notes[i].slide_origin = Some(value);
                }
            }
        }
        if self.centered {
            input = ((input - 0.5).abs() * 2.0).min(1.0);
        }
        let output = s.curve.apply(input);
        // Onset values are one-shots: smoothing would only blur the value it just set.
        let smoothing = s.smooth && self.slide_mode != SlideMode::Ons;
        self.dispatch(1, src, output, if smoothing { 1.0 } else { 0.0 });
    }

    fn bend(&mut self, src: NoteRef, value: f32) {
        let s = self.sources[2];
        if !s.on {
            self.core.forward(MidiMessage::NoteExpr {
                channel: src.channel,
                key: src.key,
                note_id: src.id,
                kind: ExprKind::Pitch,
                value,
            });
            return;
        }
        let magnitude = s.curve.apply(value.abs() / MPE_BEND_RANGE) * MPE_BEND_RANGE;
        let scaled = (magnitude * self.pitch_range).min(MPE_BEND_RANGE);
        self.dispatch(2, src, scaled.copysign(value), 0.0);
    }

    /// Sends a shaped value out, through its source's smoother when that's on. The press and
    /// pitch sources decide smoothing from their own Smooth toggle; slide passes `force`.
    fn dispatch(&mut self, source: usize, src: NoteRef, value: f32, force: f32) {
        let s = self.sources[source];
        let smoothing = if source == 1 { force > 0.5 } else { s.smooth };
        let sr = self.core.sample_rate();
        let index = if smoothing {
            self.note_index(src)
        } else {
            None
        };
        match index {
            Some(i) => {
                let frames = |ms: f64| ms * 0.001 * sr;
                self.notes[i].smoothers[source].set_target(
                    value,
                    frames(s.rise_ms),
                    frames(s.fall_ms),
                    true,
                );
                // Nothing to glide to with a time of zero: it has already arrived.
                if !self.notes[i].smoothers[source].moving() {
                    let value = self.notes[i].smoothers[source].value;
                    self.send_for(source, src, value);
                }
            }
            None => self.send_for(source, src, value),
        }
    }
}

impl MidiFx for MpeControl {
    fn core(&mut self) -> &mut Core {
        &mut self.core
    }

    fn apply(&mut self, id: u32, value: f64) {
        let on = value >= 0.5;
        let unit = (value * 0.01) as f32;
        let (source, offset) = match id {
            0..=19 => (0, id),
            20..=39 => (1, id - SLIDE),
            40..=59 => (2, id - NOTE_PB),
            _ => return,
        };
        {
            let s = &mut self.sources[source];
            match offset {
                ON => s.on = on,
                CURVE_TYPE => s.curve.s_shaped = on,
                CURVE_A => s.curve.a = unit,
                CURVE_B => s.curve.b = unit,
                CURVE_LINK => s.curve.link = on,
                MIN => s.curve.min = unit,
                MAX => s.curve.max = unit,
                X => s.curve.x = unit,
                Y => s.curve.y = unit,
                SMOOTH => s.smooth = on,
                RISE => s.rise_ms = value,
                FALL => s.fall_ms = value,
                _ => {}
            }
        }
        match (source, offset) {
            (0, PRESS_DEFAULT) => self.press_default = on,
            (0, PRESS_DEFAULT_VALUE) => self.press_default_value = unit,
            (0, SWAP_TO_SLIDE) => self.swap_to_slide = on,
            (0, PRESS_TO_AT) => self.press_to_at = on,
            (1, SLIDE_DEFAULT) => self.slide_default = on,
            (1, SLIDE_DEFAULT_VALUE) => self.slide_default_value = unit,
            (1, CENTERED) => self.centered = on,
            (1, SLIDE_MODE) => {
                self.slide_mode = match value.round() as i32 {
                    1 => SlideMode::Rel,
                    2 => SlideMode::Ons,
                    _ => SlideMode::Abs,
                }
            }
            (1, SLIDE_TO_MOD) => self.slide_to_mod = on,
            (2, PITCH_RANGE) => {
                self.pitch_range =
                    RANGE_VALUES[(value.round() as usize).min(RANGE_VALUES.len() - 1)]
            }
            (2, NOTE_PB_TO_PB) => self.note_pb_to_pb = on,
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
                self.core.note_on(src, src, velocity, tuning);
                if self.notes.len() < MAX_NOTES {
                    self.notes.push(Note {
                        src,
                        smoothers: [Smoother::default(); SOURCES],
                        slide_origin: None,
                        active: true,
                    });
                }
                if self.press_default {
                    let value = self.press_default_value;
                    self.send_press(src, value);
                }
                if self.slide_default {
                    let value = self.slide_default_value;
                    self.send_slide(src, value);
                }
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
                if let Some(i) = self.note_index(src) {
                    self.notes.swap_remove(i);
                }
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
                match kind {
                    ExprKind::Pressure => self.press(src, value),
                    ExprKind::Slide => self.slide(src, value),
                    ExprKind::Pitch => self.bend(src, value),
                }
            }
            other => self.core.pass_other(other),
        }
    }

    fn advance(&mut self, frames: usize) {
        // Steps through in short hops, so values land at the right time.
        let mut left = frames;
        while left > 0 {
            let hop = left.min(SMOOTH_STEP_FRAMES - self.smooth_counter);
            self.core.advance(hop as u64);
            self.smooth_counter += hop;
            left -= hop;
            if self.smooth_counter < SMOOTH_STEP_FRAMES {
                continue;
            }
            self.smooth_counter = 0;
            for i in 0..self.notes.len() {
                for source in 0..SOURCES {
                    if !self.sources[source].smooth {
                        continue;
                    }
                    if self.notes[i].smoothers[source].moving()
                        && self.notes[i].smoothers[source].tick(SMOOTH_STEP_FRAMES)
                    {
                        let value = self.notes[i].smoothers[source].value;
                        let src = self.notes[i].src;
                        self.send_for(source, src, value);
                    }
                }
            }
        }
    }

    fn clear(&mut self) {
        self.notes.clear();
        self.smooth_counter = 0;
    }
}

midi_fx_processor!(MpeControl);

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.mpe_control",
    name: "MPE Control",
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
    create: |_| Box::<MpeControl>::default(),
};
