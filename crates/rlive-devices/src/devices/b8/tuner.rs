//! Tuner: shows the incoming monophonic pitch and how far it is from the nearest semitone,
//! without touching the audio (manual pages 678-683).
//!
//! The manual gives the 410-480 Hz Reference, the Classic (Target, Strobe) and Histogram views,
//! the Hertz/Cents switch and the three note spellings. It gives no detection range; ours is
//! 30 Hz to 4.2 kHz. Pitch is measured only while a UI is reading.

use super::common::{hz_to_note, knob, menu, toggle};
use crate::dsp::pitch::PitchDetector;
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    AudioBuffer, BlockStep, DeviceDesc, Item, ParamInfo, ProcessContext, Processor, Section,
    Status, ViewId,
};

pub const REFERENCE: u32 = 0;
pub const VIEW_MODE: u32 = 1;
pub const CLASSIC_MODE: u32 = 2;
pub const UNIT: u32 = 3;
pub const SPELLING: u32 = 4;
pub const AUTO: u32 = 5;
const PARAMS: usize = 6;

pub const VIEW_CLASSIC: usize = 0;
pub const VIEW_HISTOGRAM: usize = 1;
pub const MODE_TARGET: usize = 0;
pub const MODE_STROBE: usize = 1;
pub const UNIT_HERTZ: usize = 0;
pub const UNIT_CENTS: usize = 1;
pub const SPELL_SHARPS: usize = 0;
pub const SPELL_FLATS: usize = 1;
pub const SPELL_BOTH: usize = 2;

/// `[hz, confidence, nearest MIDI note, cents from it]`. Hz is 0 when no pitch is heard.
pub const READING: ViewId = ViewId(0);
pub const VIEW_LEN: usize = 4;

const MIN_HZ: f32 = 30.0;
const MAX_HZ: f32 = 4_200.0;
const HOP: usize = 256;

/// `(nearest note, cents from it)` of a frequency against a reference for A.
pub fn nearest_note(hz: f32, reference: f32) -> (f32, f32) {
    let note = hz_to_note(hz * 440.0 / reference.max(1.0));
    let nearest = note.round();
    (nearest, (note - nearest) * 100.0)
}

fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::continuous(REFERENCE, "Reference", 410.0, 480.0, 440.0, "Hz"),
        ParamInfo::stepped(VIEW_MODE, "View", &["Classic", "Histogram"], VIEW_CLASSIC),
        ParamInfo::stepped(
            CLASSIC_MODE,
            "Classic Mode",
            &["Target", "Strobe"],
            MODE_TARGET,
        ),
        ParamInfo::stepped(UNIT, "Display", &["Hertz", "Cents"], UNIT_CENTS),
        ParamInfo::stepped(
            SPELLING,
            "Note Spelling",
            &["Sharps", "Flats", "Sharps and Flats"],
            SPELL_SHARPS,
        ),
        ParamInfo::toggle(AUTO, "Auto", true),
    ]
}

fn layout() -> Vec<Section> {
    vec![
        Section {
            title: "Tuner",
            items: vec![Item::Panel("tuner_display")],
        },
        Section {
            title: "Settings",
            items: vec![
                menu(VIEW_MODE),
                menu(CLASSIC_MODE),
                menu(UNIT),
                menu(SPELLING),
                toggle(AUTO),
                knob(REFERENCE),
            ],
        },
    ]
}

pub struct Tuner {
    v: [f64; PARAMS],
    detector: Option<PitchDetector>,
}

impl Default for Tuner {
    fn default() -> Self {
        let mut v = [0.0; PARAMS];
        for p in params() {
            v[p.id as usize] = p.default;
        }
        Self { v, detector: None }
    }
}

impl Processor for Tuner {
    fn activate(&mut self, sample_rate: f64, _max_block: usize) {
        self.detector = Some(PitchDetector::new(sample_rate, MIN_HZ, MAX_HZ, HOP));
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        let view = ctx.view;
        let active = view.is_active();
        let mut latest = None;
        let v = &mut self.v;
        let detector = &mut self.detector;
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => {
                if let Some(slot) = v.get_mut(id as usize) {
                    *slot = value;
                }
            }
            BlockStep::Audio(AudioBuffer { left, right }) => {
                if !active {
                    return;
                }
                let Some(detector) = detector.as_mut() else {
                    return;
                };
                for (l, r) in left.iter().zip(right.iter()) {
                    let mono = [0.5 * (l + r)];
                    latest = detector.process(&mono);
                }
            }
            BlockStep::Midi(_) => {}
        });
        if active {
            let reference = self.v[REFERENCE as usize] as f32;
            let reading = match latest {
                Some(p) => {
                    let (note, cents) = nearest_note(p.hz, reference);
                    [p.hz, p.confidence, note, cents]
                }
                None => [0.0, 0.0, 0.0, 0.0],
            };
            view.publish(READING, &reading);
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(slot) = self.v.get_mut(id as usize) {
            *slot = value;
        }
    }

    fn reset(&mut self) {
        if let Some(detector) = self.detector.as_mut() {
            detector.reset();
        }
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.tuner",
    name: "Tuner",
    role: DeviceRole::AudioEffect,
    category: "Spectral",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::new(Tuner::default()),
};
