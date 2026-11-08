//! Simpler (manual pp. 791-802): one sample, played as an instrument, a one-shot or in slices,
//! through a filter, three envelopes and an LFO, optionally warped to the Set's tempo.
//!
//! The manual gives no units or ranges for most of the region, filter, envelope, spread and glide
//! controls, so those here are our own. It does give: Transpose ±48 st, Detune ±50 ct, LFO
//! 0.01-30 Hz, a Sensitivity reaching at most 64 slices, and a ±5 st pitch bend.
//!
//! **Warping.** [`WarpVoice`] is the seam for the warp modes: it plays a source range against
//! the timeline through `rlive-stretch`. Only Beats and Re-Pitch follow the timeline so far
//! ([`StretchKind::follows_timeline`]); the others fall back to native-speed reading until
//! milestone A10 builds them, which only has to add them in [`WarpVoice::stretcher`].

use super::common::*;
use crate::dsp::envelope::{Envelope, EnvelopeSettings, LoopMode};
use crate::dsp::filter::{Filter, FilterSettings};
use crate::dsp::lfo::{Lfo, LfoRate, LfoSettings, LfoShape};
use crate::dsp::tempo::{BeatClock, SyncRate};
use crate::dsp::util::{db_to_gain, Rng};
use crate::dsp::voice::{
    GlideMode, StealMode, VoiceAllocator, VoiceEvent, VoiceMode, VoiceSettings,
};
use crate::{BuiltinInfo, DeviceInit, DeviceRole};
use rlive_core::warp::{
    BeatsLoop, BeatsPreserve, Passthrough, StretchKind, StretchSpan, Stretcher, WarpMap,
    WarpMarker, WarpMode,
};
use rlive_core::{
    BlockStep, DeviceDesc, Item, MidiMessage, ParamId, ParamInfo, ProcessContext, Processor,
    Resource, SampleBuffer, SampleSlot, Section, Status, Tail, ViewId, PPQ,
};
use rlive_stretch::{Beats, RePitch};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// The most slices one sample can have: the manual's limit for Transient mode, used by them all.
pub const MAX_SLICES: usize = 64;

/// The key the first slice answers. The manual doesn't say.
pub const FIRST_SLICE_KEY: u8 = 36;

/// The key that plays a sample at its own pitch.
pub const ROOT_KEY: f32 = 60.0;

/// Frames between updates of everything that doesn't need to change every sample.
const CONTROL_FRAMES: usize = 32;

/// The longest voices can be asked for. Spread takes two per note.
const MAX_NOTES: usize = 24;
const MAX_VOICES: usize = MAX_NOTES * 2;

pub const MODE: u32 = 0;
pub const START_FLAG: u32 = 1;
pub const END_FLAG: u32 = 2;
pub const START: u32 = 3;
pub const LENGTH: u32 = 4;
pub const LOOP_ON: u32 = 5;
pub const LOOP: u32 = 6;
pub const SNAP: u32 = 7;
pub const FADE: u32 = 8;
pub const GAIN: u32 = 9;
pub const VOICES: u32 = 10;
pub const RETRIG: u32 = 11;
pub const GATE: u32 = 12;
pub const FADE_IN: u32 = 13;
pub const FADE_OUT: u32 = 14;
pub const SLICE_BY: u32 = 15;
pub const SENSITIVITY: u32 = 16;
pub const DIVISION: u32 = 17;
pub const REGIONS: u32 = 18;
pub const PLAYBACK: u32 = 19;
pub const WARP: u32 = 20;
pub const WARP_MODE: u32 = 21;
pub const WARP_BEATS: u32 = 22;
pub const WARP_PRESERVE: u32 = 23;
pub const WARP_LOOP: u32 = 24;
pub const WARP_ENVELOPE: u32 = 25;
pub const F_ON: u32 = 26;
pub const F_TYPE: u32 = 27;
pub const F_CIRCUIT: u32 = 28;
pub const F_SLOPE: u32 = 29;
pub const F_FREQ: u32 = 30;
pub const F_RES: u32 = 31;
pub const F_DRIVE: u32 = 32;
pub const F_MORPH: u32 = 33;
pub const F_VEL: u32 = 34;
pub const F_KEY: u32 = 35;
pub const F_ENV: u32 = 36;
pub const A_ATTACK: u32 = 37;
pub const A_DECAY: u32 = 38;
pub const A_SUSTAIN: u32 = 39;
pub const A_RELEASE: u32 = 40;
pub const A_LOOP: u32 = 41;
pub const A_TIME: u32 = 42;
pub const A_RATE: u32 = 43;
pub const FE_ATTACK: u32 = 44;
pub const FE_DECAY: u32 = 45;
pub const FE_SUSTAIN: u32 = 46;
pub const FE_RELEASE: u32 = 47;
pub const PE_ATTACK: u32 = 48;
pub const PE_DECAY: u32 = 49;
pub const PE_SUSTAIN: u32 = 50;
pub const PE_RELEASE: u32 = 51;
pub const PE_AMOUNT: u32 = 52;
pub const L_ON: u32 = 53;
pub const L_TYPE: u32 = 54;
pub const L_SYNC: u32 = 55;
pub const L_FREQ: u32 = 56;
pub const L_BEATS: u32 = 57;
pub const L_ATTACK: u32 = 58;
pub const L_RETRIG: u32 = 59;
pub const L_OFFSET: u32 = 60;
pub const L_KEY: u32 = 61;
pub const L_VOLUME: u32 = 62;
pub const L_PITCH: u32 = 63;
pub const L_PAN: u32 = 64;
pub const L_FILTER: u32 = 65;
pub const PAN: u32 = 66;
pub const RANDOM_PAN: u32 = 67;
pub const SPREAD: u32 = 68;
pub const VOLUME: u32 = 69;
pub const VEL_VOL: u32 = 70;
pub const TRANSPOSE: u32 = 71;
pub const DETUNE: u32 = 72;
pub const GLIDE: u32 = 73;
pub const GLIDE_TIME: u32 = 74;
pub const CONST_POWER: u32 = 75;
pub const REVERSE: u32 = 76;
pub const PARAM_COUNT: usize = 77;

pub const MODE_CLASSIC: usize = 0;
pub const MODE_ONE_SHOT: usize = 1;
pub const MODE_SLICING: usize = 2;

pub const SLICE_TRANSIENT: usize = 0;
pub const SLICE_BEAT: usize = 1;
pub const SLICE_REGION: usize = 2;
pub const SLICE_MANUAL: usize = 3;

pub const PLAY_MONO: usize = 0;
pub const PLAY_POLY: usize = 1;
pub const PLAY_THRU: usize = 2;

/// Beats (quarter notes) of each Division in Beat mode.
pub const DIVISION_BEATS: [f64; 11] = [
    0.25,
    1.0 / 6.0,
    0.5,
    1.0 / 3.0,
    1.0,
    2.0 / 3.0,
    2.0,
    4.0 / 3.0,
    4.0,
    8.0,
    16.0,
];
const DIVISION_LABELS: [&str; 11] = [
    "1/16", "1/16T", "1/8", "1/8T", "1/4", "1/4T", "1/2", "1/2T", "1 Bar", "2 Bars", "4 Bars",
];
const VOICE_COUNTS: [usize; 14] = [1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 20, 24];
const VOICE_LABELS: [&str; 14] = [
    "1", "2", "3", "4", "5", "6", "7", "8", "10", "12", "14", "16", "20", "24",
];
const LOOP_MODES: [&str; 5] = ["None", "Loop", "Beat", "Sync", "Trigger"];
const LFO_TYPES: [&str; 6] = ["Sine", "Square", "Triangle", "Saw Down", "Saw Up", "Random"];

pub fn params() -> Vec<ParamInfo> {
    let pct = |id, name, default: f64| ParamInfo::continuous(id, name, 0.0, 100.0, default, "%");
    let ms = |id, name: &str, default: f64| {
        ParamInfo::skewed(id, name, 0.0, 20_000.0, default, "ms", 3.5)
    };
    vec![
        ParamInfo::stepped(
            MODE,
            "Mode",
            &["Classic", "One-Shot", "Slicing"],
            MODE_CLASSIC,
        ),
        ParamInfo::continuous(START_FLAG, "Sample Start", 0.0, 1.0, 0.0, ""),
        ParamInfo::continuous(END_FLAG, "Sample End", 0.0, 1.0, 1.0, ""),
        pct(START, "Start", 0.0),
        pct(LENGTH, "Length", 100.0),
        ParamInfo::toggle(LOOP_ON, "Loop", false),
        pct(LOOP, "Loop Length", 100.0),
        ParamInfo::toggle(SNAP, "Snap", false),
        pct(FADE, "Fade", 0.0),
        ParamInfo::continuous(GAIN, "Gain", -36.0, 36.0, 0.0, "dB"),
        ParamInfo::stepped(VOICES, "Voices", &VOICE_LABELS, 7),
        ParamInfo::toggle(RETRIG, "Retrig", true),
        ParamInfo::stepped(GATE, "Trigger/Gate", &["Trigger", "Gate"], 0),
        ms(FADE_IN, "Fade In", 0.0),
        ms(FADE_OUT, "Fade Out", 0.0),
        ParamInfo::stepped(
            SLICE_BY,
            "Slice By",
            &["Transient", "Beat", "Region", "Manual"],
            0,
        ),
        pct(SENSITIVITY, "Sensitivity", 50.0),
        ParamInfo::stepped(DIVISION, "Division", &DIVISION_LABELS, 4),
        ParamInfo::continuous(REGIONS, "Regions", 2.0, 64.0, 8.0, ""),
        ParamInfo::stepped(PLAYBACK, "Playback", &["Mono", "Poly", "Thru"], PLAY_POLY),
        ParamInfo::toggle(WARP, "Warp", false),
        ParamInfo::stepped(
            WARP_MODE,
            "Warp Mode",
            &[
                "Beats",
                "Tones",
                "Texture",
                "Re-Pitch",
                "Complex",
                "Complex Pro",
            ],
            0,
        ),
        ParamInfo::continuous(WARP_BEATS, "Warp As", 0.25, 256.0, 4.0, "beats"),
        ParamInfo::stepped(WARP_PRESERVE, "Preserve", &["Transients", "Grid"], 0),
        ParamInfo::stepped(
            WARP_LOOP,
            "Transient Loop",
            &["Off", "Forward", "Back-and-Forth"],
            0,
        ),
        ParamInfo::continuous(WARP_ENVELOPE, "Transient Envelope", 0.0, 100.0, 100.0, ""),
        ParamInfo::toggle(F_ON, "Filter", false),
        ParamInfo::stepped(F_TYPE, "Filter Type", &FILTER_TYPES, 0),
        ParamInfo::stepped(F_CIRCUIT, "Circuit", &FILTER_CIRCUITS, 0),
        ParamInfo::stepped(F_SLOPE, "Slope", &FILTER_SLOPES, 0),
        ParamInfo::skewed(F_FREQ, "Frequency", 20.0, 20_000.0, 2_000.0, "Hz", 2.5),
        ParamInfo::continuous(F_RES, "Resonance", 0.0, 1.0, 0.0, ""),
        ParamInfo::continuous(F_DRIVE, "Drive", 0.0, 24.0, 0.0, "dB"),
        pct(F_MORPH, "Morph", 0.0),
        pct(F_VEL, "Filter < Vel", 0.0),
        pct(F_KEY, "Filter < Key", 0.0),
        ParamInfo::continuous(F_ENV, "Filter Env Amount", -72.0, 72.0, 0.0, "st"),
        ms(A_ATTACK, "Attack", 0.1),
        ms(A_DECAY, "Decay", 600.0),
        ParamInfo::continuous(A_SUSTAIN, "Sustain", 0.0, 1.0, 1.0, ""),
        ms(A_RELEASE, "Release", 50.0),
        ParamInfo::stepped(A_LOOP, "Loop Mode", &LOOP_MODES, 0),
        ms(A_TIME, "Time", 0.0),
        ParamInfo::stepped(A_RATE, "Rate", &SyncRate::LABELS, 12),
        ms(FE_ATTACK, "Filter Attack", 0.1),
        ms(FE_DECAY, "Filter Decay", 600.0),
        ParamInfo::continuous(FE_SUSTAIN, "Filter Sustain", 0.0, 1.0, 0.0, ""),
        ms(FE_RELEASE, "Filter Release", 50.0),
        ms(PE_ATTACK, "Pitch Attack", 0.1),
        ms(PE_DECAY, "Pitch Decay", 600.0),
        ParamInfo::continuous(PE_SUSTAIN, "Pitch Sustain", 0.0, 1.0, 0.0, ""),
        ms(PE_RELEASE, "Pitch Release", 50.0),
        ParamInfo::continuous(PE_AMOUNT, "Pitch Env Amount", -48.0, 48.0, 0.0, "st"),
        ParamInfo::toggle(L_ON, "LFO", false),
        ParamInfo::stepped(L_TYPE, "LFO Type", &LFO_TYPES, 0),
        ParamInfo::stepped(L_SYNC, "LFO Rate Mode", &["Hz", "Sync"], 0),
        ParamInfo::skewed(L_FREQ, "LFO Freq", 0.01, 30.0, 1.0, "Hz", 3.0),
        ParamInfo::stepped(L_BEATS, "LFO Beats", &SyncRate::LABELS, 12),
        ms(L_ATTACK, "LFO Attack", 0.0),
        ParamInfo::toggle(L_RETRIG, "LFO Retrigger", true),
        ParamInfo::continuous(L_OFFSET, "LFO Offset", 0.0, 1.0, 0.0, ""),
        ParamInfo::continuous(L_KEY, "LFO Key", 0.0, 1.0, 0.0, ""),
        pct(L_VOLUME, "LFO > Volume", 0.0),
        pct(L_PITCH, "LFO > Pitch", 0.0),
        pct(L_PAN, "LFO > Pan", 0.0),
        pct(L_FILTER, "LFO > Filter", 0.0),
        ParamInfo::continuous(PAN, "Pan", -1.0, 1.0, 0.0, ""),
        pct(RANDOM_PAN, "Random > Pan", 0.0),
        pct(SPREAD, "Spread", 0.0),
        ParamInfo::continuous(VOLUME, "Volume", -60.0, 12.0, -6.0, "dB"),
        pct(VEL_VOL, "Velocity > Volume", 80.0),
        ParamInfo::continuous(TRANSPOSE, "Transpose", -48.0, 48.0, 0.0, "st"),
        ParamInfo::continuous(DETUNE, "Detune", -50.0, 50.0, 0.0, "ct"),
        ParamInfo::stepped(GLIDE, "Glide Mode", &["Off", "Glide", "Portamento"], 0),
        ParamInfo::skewed(GLIDE_TIME, "Glide Time", 1.0, 2_000.0, 100.0, "ms", 3.0),
        ParamInfo::toggle(CONST_POWER, "Constant Power Fade", true),
        ParamInfo::toggle(REVERSE, "Reverse", false),
    ]
}

fn layout() -> Vec<Section> {
    let knob = |id| Item::Knob(ParamId(id));
    let menu = |id| Item::Menu(ParamId(id));
    let toggle = |id| Item::Toggle(ParamId(id));
    vec![
        Section {
            title: "Sample",
            items: vec![
                Item::Panel("simpler_wave"),
                Item::Row(vec![menu(MODE), toggle(WARP), menu(WARP_MODE)]),
                Item::Row(vec![knob(GAIN), knob(WARP_BEATS)]),
            ],
        },
        Section {
            title: "Classic",
            items: vec![
                Item::Row(vec![knob(START), knob(LENGTH), knob(LOOP), knob(FADE)]),
                Item::Row(vec![
                    toggle(LOOP_ON),
                    toggle(SNAP),
                    menu(VOICES),
                    toggle(RETRIG),
                ]),
            ],
        },
        Section {
            title: "One-Shot",
            items: vec![Item::Row(vec![knob(FADE_IN), knob(FADE_OUT), menu(GATE)])],
        },
        Section {
            title: "Slicing",
            items: vec![
                Item::Row(vec![menu(SLICE_BY), menu(PLAYBACK), knob(SENSITIVITY)]),
                Item::Row(vec![menu(DIVISION), knob(REGIONS)]),
            ],
        },
        Section {
            title: "Filter",
            items: vec![
                Item::Row(vec![
                    toggle(F_ON),
                    menu(F_TYPE),
                    menu(F_CIRCUIT),
                    menu(F_SLOPE),
                ]),
                Item::Row(vec![
                    knob(F_FREQ),
                    knob(F_RES),
                    knob(F_DRIVE),
                    knob(F_MORPH),
                ]),
                Item::Row(vec![knob(F_VEL), knob(F_KEY), knob(F_ENV)]),
            ],
        },
        Section {
            title: "Envelopes",
            items: vec![
                Item::Row(vec![
                    knob(A_ATTACK),
                    knob(A_DECAY),
                    knob(A_SUSTAIN),
                    knob(A_RELEASE),
                ]),
                Item::Row(vec![menu(A_LOOP), knob(A_TIME), menu(A_RATE)]),
                Item::Row(vec![
                    knob(FE_ATTACK),
                    knob(FE_DECAY),
                    knob(FE_SUSTAIN),
                    knob(FE_RELEASE),
                ]),
                Item::Row(vec![
                    knob(PE_ATTACK),
                    knob(PE_DECAY),
                    knob(PE_SUSTAIN),
                    knob(PE_RELEASE),
                    knob(PE_AMOUNT),
                ]),
            ],
        },
        Section {
            title: "LFO",
            items: vec![
                Item::Row(vec![
                    toggle(L_ON),
                    menu(L_TYPE),
                    menu(L_SYNC),
                    knob(L_FREQ),
                    menu(L_BEATS),
                ]),
                Item::Row(vec![
                    knob(L_ATTACK),
                    toggle(L_RETRIG),
                    knob(L_OFFSET),
                    knob(L_KEY),
                ]),
                Item::Row(vec![
                    knob(L_VOLUME),
                    knob(L_PITCH),
                    knob(L_PAN),
                    knob(L_FILTER),
                ]),
            ],
        },
        Section {
            title: "Global",
            items: vec![
                Item::Row(vec![knob(PAN), knob(RANDOM_PAN), knob(SPREAD)]),
                Item::Row(vec![
                    knob(VOLUME),
                    knob(VEL_VOL),
                    knob(TRANSPOSE),
                    knob(DETUNE),
                ]),
                Item::Row(vec![
                    menu(GLIDE),
                    knob(GLIDE_TIME),
                    toggle(CONST_POWER),
                    toggle(REVERSE),
                ]),
            ],
        },
    ]
}

/// A view: the sample overview, the slices, and where the first voices are.
pub const VIEW_WAVE: ViewId = ViewId(0);
pub const VIEW_SLICES: ViewId = ViewId(WAVE_VALUES as u32);
/// Count, then `MAX_SLICES + 1` positions in `0..1`.
pub const SLICE_VALUES: usize = MAX_SLICES + 2;
pub const VIEW_HEADS: ViewId = ViewId((WAVE_VALUES + SLICE_VALUES) as u32);
pub const HEAD_VALUES: usize = 4;
/// The sample's length in frames, then its peak.
pub const VIEW_INFO: ViewId = ViewId((WAVE_VALUES + SLICE_VALUES + HEAD_VALUES) as u32);
pub const INFO_VALUES: usize = 2;
pub const VIEW_LEN: usize = WAVE_VALUES + SLICE_VALUES + HEAD_VALUES + INFO_VALUES;

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.simpler",
    name: "Simpler",
    role: DeviceRole::Instrument,
    category: "Samplers",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        samples: vec![SampleSlot { name: "Sample" }],
        ..Default::default()
    },
    create: |init| Box::new(Simpler::new(init)),
};

/// The part of a Simpler that isn't parameters. Saved as RON in the device's `state`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SimplerState {
    pub version: u32,
    /// Slices made by hand, in frames. Survive any change of Sensitivity.
    #[serde(default)]
    pub manual: Vec<f64>,
    /// Automatic slices the user deleted, in frames.
    #[serde(default)]
    pub removed: Vec<f64>,
    /// `(source frame, tick)` Warp Markers, copied from a clip dragged in.
    #[serde(default)]
    pub warp_markers: Vec<(f64, f64)>,
}

/// The version of [`SimplerState`] this build writes.
pub const STATE_VERSION: u32 = 1;

/// A parsed [`SimplerState`], ready for the audio thread. Built off it.
#[derive(Debug, Default)]
pub struct ParsedState {
    pub state: SimplerState,
    pub warp_map: Option<WarpMap>,
}

impl ParsedState {
    pub fn parse(text: Option<&str>) -> Self {
        Self::from_state(
            text.and_then(|t| ron::from_str::<SimplerState>(t).ok())
                .filter(|s| s.version <= STATE_VERSION)
                .unwrap_or_default(),
        )
    }

    pub fn from_state(state: SimplerState) -> Self {
        let warp_map = WarpMap::new(
            state
                .warp_markers
                .iter()
                .map(|(src, tick)| WarpMarker::new(*src, *tick)),
        );
        Self { state, warp_map }
    }

    pub fn serialize(state: &SimplerState) -> String {
        ron::to_string(&SimplerState {
            version: STATE_VERSION,
            ..state.clone()
        })
        .unwrap_or_default()
    }
}

/// What determines how a region is cut into slices.
#[derive(Clone, Copy, Debug)]
pub struct SliceSpec {
    pub by: usize,
    pub sensitivity: f32,
    pub division_beats: f64,
    pub regions: usize,
    /// The sample region, in frames.
    pub start: f64,
    pub end: f64,
    /// How many beats the region lasts, for Beat mode.
    pub beats: f64,
}

const SAME_SLICE: f64 = 16.0;

/// Cuts a region into slices. `out` receives their start frames, ascending, starting with the
/// start of the region; the return value counts them. `out[count]` is the end of the region.
/// Does not allocate: Simpler re-cuts on the audio thread when a parameter moves.
pub fn compute_slices(
    spec: SliceSpec,
    analysis: Option<&Analysis>,
    state: &SimplerState,
    out: &mut [f64; MAX_SLICES + 1],
) -> usize {
    let (start, end) = (spec.start, spec.end.max(spec.start));
    let mut count = 0usize;
    let mut push = |frame: f64, out: &mut [f64; MAX_SLICES + 1]| {
        if count < MAX_SLICES && frame >= start && frame < end {
            out[count] = frame;
            count += 1;
        }
    };
    let manual = state.manual.iter().copied().filter(|m| m.is_finite());
    match spec.by {
        SLICE_TRANSIENT => {
            let threshold = 1.0 - spec.sensitivity.clamp(0.0, 1.0);
            let mut kept = [(0.0f32, 0.0f64); MAX_TRANSIENTS];
            let mut n = 0;
            for t in analysis.map_or(&[][..], |a| &a.transients) {
                let gone = state
                    .removed
                    .iter()
                    .any(|r| (r - t.frame).abs() < SAME_SLICE);
                if t.frame >= start
                    && t.frame < end
                    && t.strength >= threshold
                    && !gone
                    && n < kept.len()
                {
                    kept[n] = (t.strength, t.frame);
                    n += 1;
                }
            }
            // Keep the strongest if there are too many.
            let room = MAX_SLICES - 1;
            if n > room {
                let (strongest, _, _) =
                    kept[..n].select_nth_unstable_by(room, |a, b| b.0.total_cmp(&a.0));
                strongest.sort_unstable_by(|a, b| a.1.total_cmp(&b.1));
                n = room;
            } else {
                kept[..n].sort_unstable_by(|a, b| a.1.total_cmp(&b.1));
            }
            push(start, out);
            for (_, frame) in &kept[..n] {
                push(*frame, out);
            }
            for m in manual {
                push(m, out);
            }
        }
        SLICE_BEAT => {
            let per_beat = (end - start) / spec.beats.max(1e-6);
            let step = (spec.division_beats * per_beat).max(1.0);
            let mut frame = start;
            while frame < end && count_of(&frame, start, step) < MAX_SLICES {
                push(frame, out);
                frame += step;
            }
        }
        SLICE_REGION => {
            let regions = spec.regions.clamp(1, MAX_SLICES);
            for i in 0..regions {
                push(start + (end - start) * i as f64 / regions as f64, out);
            }
        }
        _ => {
            push(start, out);
            for m in manual {
                push(m, out);
            }
        }
    }
    out[..count].sort_unstable_by(f64::total_cmp);
    // Merge anything that landed on top of something else.
    let mut w = usize::from(count > 0);
    for r in 1..count {
        if out[r] - out[w - 1] >= SAME_SLICE {
            out[w] = out[r];
            w += 1;
        }
    }
    out[w] = end;
    w
}

fn count_of(frame: &f64, start: f64, step: f64) -> usize {
    ((frame - start) / step) as usize
}

fn map_lfo_type(i: f64) -> LfoShape {
    match i.round() as i32 {
        1 => LfoShape::Square,
        2 => LfoShape::Triangle,
        3 => LfoShape::SawDown,
        4 => LfoShape::SawUp,
        5 => LfoShape::SampleHold,
        _ => LfoShape::Sine,
    }
}

fn map_loop(i: f64) -> LoopMode {
    match i.round() as i32 {
        1 => LoopMode::Loop,
        2 => LoopMode::Beat,
        3 => LoopMode::Sync,
        4 => LoopMode::Trigger,
        _ => LoopMode::Off,
    }
}

fn adsr(a: f64, d: f64, s: f64, r: f64) -> EnvelopeSettings {
    EnvelopeSettings {
        attack: (a * 0.001) as f32,
        decay: (d * 0.001) as f32,
        sustain: s as f32,
        release: (r * 0.001) as f32,
        ..Default::default()
    }
}

/// The warp seam, one per voice: the same stretchers an audio clip plays through. See the
/// module docs.
#[derive(Default)]
struct WarpVoice {
    beats: Beats,
    repitch: RePitch,
    native: Passthrough,
    kind: Option<StretchKind>,
    tick: f64,
    active: bool,
}

impl WarpVoice {
    fn prepare(&mut self, sample_rate: f64) {
        self.beats.prepare(sample_rate);
        self.repitch.prepare(sample_rate);
        self.native.prepare(sample_rate);
    }

    /// A10: add each new mode's stretcher here, and it follows the timeline from then on.
    fn stretcher(&mut self, kind: StretchKind) -> &mut dyn Stretcher {
        match kind {
            StretchKind::Beats => &mut self.beats,
            StretchKind::RePitch => &mut self.repitch,
            _ => &mut self.native,
        }
    }
}

struct Voice {
    active: bool,
    key: f32,
    velocity: f32,
    released: bool,
    player: Player,
    /// Where the fades of this voice are measured, in frames.
    fade_from: f64,
    fade_to: f64,
    /// Playback frames this voice has been running.
    played: u64,
    /// Output frames a Gate release has left, when one has started.
    gate_left: f32,
    amp: Envelope,
    filter_env: Envelope,
    pitch_env: Envelope,
    lfo: Lfo,
    lfo_ramp: f32,
    filter: [Filter; 2],
    filter_on: bool,
    pan: f32,
    pan_random: f32,
    detune: f32,
    spread: f32,
    gain: f32,
    step: f64,
    ctl: usize,
    /// Smoothed per block values.
    lfo_volume: f32,
    lfo_pan: f32,
    env_shape: bool,
    warp: Box<WarpVoice>,
}

impl Default for Voice {
    fn default() -> Self {
        Self {
            active: false,
            key: 60.0,
            velocity: 0.0,
            released: false,
            player: Player::default(),
            fade_from: 0.0,
            fade_to: 0.0,
            played: 0,
            gate_left: -1.0,
            amp: Envelope::new(),
            filter_env: Envelope::new(),
            pitch_env: Envelope::new(),
            lfo: Lfo::new(1),
            lfo_ramp: 1.0,
            filter: [Filter::new(); 2],
            filter_on: false,
            pan: 0.0,
            pan_random: 0.0,
            detune: 0.0,
            spread: 0.0,
            gain: 1.0,
            step: 1.0,
            ctl: 0,
            lfo_volume: 1.0,
            lfo_pan: 0.0,
            env_shape: true,
            warp: Box::default(),
        }
    }
}

/// See the [module docs](self).
pub struct Simpler {
    values: Vec<f64>,
    sr: f64,
    sample: Option<Arc<SampleBuffer>>,
    analysis: Option<Arc<Analysis>>,
    state: Arc<ParsedState>,
    slices: [f64; MAX_SLICES + 1],
    slice_count: usize,
    slices_dirty: bool,
    alloc: VoiceAllocator,
    voices: Box<[Voice]>,
    rng: Rng,
    warp_left: Vec<f32>,
    warp_right: Vec<f32>,
    clock: BeatClock,
    tempo: f64,
}

impl Simpler {
    pub fn new(init: &DeviceInit) -> Self {
        prepare_interpolation();
        let sample = init.samples.first().cloned().flatten();
        let analysis = sample.as_ref().map(|s| Arc::new(Analysis::new(s, None)));
        Self {
            values: params().iter().map(|p| p.default).collect(),
            sr: 48_000.0,
            sample,
            analysis,
            state: Arc::new(ParsedState::parse(init.state.as_deref())),
            slices: [0.0; MAX_SLICES + 1],
            slice_count: 0,
            slices_dirty: true,
            alloc: VoiceAllocator::new(MAX_VOICES),
            voices: (0..MAX_VOICES).map(|_| Voice::default()).collect(),
            rng: Rng::new(0x51_b1e2),
            warp_left: Vec::new(),
            warp_right: Vec::new(),
            clock: BeatClock::default(),
            tempo: 120.0,
        }
    }

    fn v(&self, id: u32) -> f64 {
        self.values[id as usize]
    }

    /// The active slices, as `[start, end)` frames of the sample: everything Slicing mode would
    /// play, in key order.
    pub fn slices(&self) -> &[f64] {
        &self.slices[..=self.slice_count]
    }

    /// The region between the flags, in frames.
    fn region(&self, n: f64) -> (f64, f64) {
        let start = self.v(START_FLAG).clamp(0.0, 1.0) * n;
        let end = self.v(END_FLAG).clamp(0.0, 1.0) * n;
        (start.min(end), start.max(end))
    }

    fn rebuild_slices(&mut self) {
        self.slices_dirty = false;
        let n = self.sample.as_ref().map_or(0.0, |s| s.len() as f64);
        let (start, end) = self.region(n);
        let spec = SliceSpec {
            by: self.v(SLICE_BY).round() as usize,
            sensitivity: (self.v(SENSITIVITY) * 0.01) as f32,
            division_beats: DIVISION_BEATS[(self.v(DIVISION).round() as usize).min(10)],
            regions: self.v(REGIONS).round() as usize,
            start,
            end,
            beats: self.v(WARP_BEATS),
        };
        self.slice_count = compute_slices(
            spec,
            self.analysis.as_deref(),
            &self.state.state,
            &mut self.slices,
        );
    }

    /// A voice's whole configuration at note-on.
    #[allow(clippy::too_many_arguments)]
    fn start_voice(
        voice: &mut Voice,
        v: &Values,
        sample: &SampleBuffer,
        slices: &[f64],
        spread: f32,
        key: u8,
        velocity: f32,
        rng: &mut Rng,
        clock: &BeatClock,
    ) {
        let n = sample.len() as f64;
        let mode = v.mode();
        let (flag_start, flag_end) = {
            let a = v.get(START_FLAG).clamp(0.0, 1.0) * n;
            let b = v.get(END_FLAG).clamp(0.0, 1.0) * n;
            (a.min(b), a.max(b))
        };
        let (mut start, mut end, mut fade_from, mut fade_to);
        let reverse = v.on(REVERSE);
        match mode {
            MODE_CLASSIC => {
                let avail = flag_end - flag_start;
                start = flag_start + avail * (v.get(START) * 0.01).clamp(0.0, 1.0);
                end = (start + avail * (v.get(LENGTH) * 0.01).clamp(0.0, 1.0)).min(flag_end);
            }
            MODE_ONE_SHOT => {
                start = flag_start;
                end = flag_end;
            }
            _ => {
                let index = (key as i32 - FIRST_SLICE_KEY as i32).max(0) as usize;
                let count = slices.len() - 1;
                let slice = index.min(count.saturating_sub(1));
                start = slices[slice];
                end = if v.playback() == PLAY_THRU {
                    slices[count]
                } else {
                    slices[slice + 1]
                };
            }
        }
        fade_from = start;
        fade_to = end;
        if v.on(SNAP) {
            start = snap_to_zero(&sample.left, start);
            end = snap_to_zero(&sample.left, end);
            fade_from = start;
            fade_to = end;
        }

        voice.player = Player::new(start, end.max(start + 1.0), reverse);
        if mode == MODE_CLASSIC && v.on(LOOP_ON) {
            let avail = (end - start).max(0.0);
            let mut loop_end = end;
            let mut loop_start = end - avail * (v.get(LOOP) * 0.01).clamp(0.0, 1.0);
            if v.on(SNAP) {
                loop_end = snap_to_zero(&sample.left, loop_end);
                loop_start = snap_to_zero(&sample.left, loop_start);
            }
            let len = (loop_end - loop_start).max(0.0);
            let crossfade = if v.on(WARP) {
                0.0 // The manual: no Fade while warping.
            } else {
                len * (v.get(FADE) * 0.01).clamp(0.0, 1.0)
            };
            voice.player.set_sustain(
                (loop_start, loop_end),
                LoopKind::Forward,
                crossfade,
                v.on(CONST_POWER),
                1.0,
            );
            voice.player.set_release(ReleaseMode::Off, 0.0);
        }

        voice.active = true;
        voice.released = false;
        voice.key = key as f32;
        voice.velocity = velocity;
        voice.fade_from = fade_from;
        voice.fade_to = fade_to;
        voice.played = 0;
        voice.gate_left = -1.0;
        voice.spread = spread;
        voice.detune = spread * (v.get(SPREAD) * 0.01) as f32 * 30.0;
        voice.pan_random = rng.bipolar() * (v.get(RANDOM_PAN) * 0.01) as f32;
        voice.pan = spread * (v.get(SPREAD) * 0.01) as f32;
        voice.gain = {
            let sense = (v.get(VEL_VOL) * 0.01) as f32;
            1.0 - sense + sense * velocity
        };
        voice.ctl = 0;
        voice.env_shape = mode == MODE_CLASSIC;
        if v.on(L_RETRIG) {
            voice.lfo.retrigger();
            voice.lfo_ramp = 0.0;
        }
        voice.amp.reset();
        voice.filter_env.reset();
        voice.pitch_env.reset();
        v.configure_envelopes(voice);
        voice.amp.gate_on();
        voice.filter_env.gate_on();
        voice.pitch_env.gate_on();
        for f in &mut voice.filter {
            f.reset();
        }
        voice.filter_on = false;
        voice.warp.active = false;
        if v.on(WARP) {
            let kind = v.warp_kind();
            voice.warp.kind = Some(kind);
            voice.warp.active = true;
            voice.warp.stretcher(kind).reset();
            voice.warp.stretcher(kind).seek(start);
            voice.warp.tick = 0.0;
        }
        voice.lfo.set_clock(clock);
    }
}

/// Parameter values, as a voice wants them.
struct Values<'a>(&'a [f64]);

impl Values<'_> {
    #[inline]
    fn get(&self, id: u32) -> f64 {
        self.0[id as usize]
    }

    #[inline]
    fn on(&self, id: u32) -> bool {
        self.get(id) >= 0.5
    }

    fn mode(&self) -> usize {
        self.get(MODE).round() as usize
    }

    fn playback(&self) -> usize {
        self.get(PLAYBACK).round() as usize
    }

    fn warp_kind(&self) -> StretchKind {
        match self.get(WARP_MODE).round() as i32 {
            0 => StretchKind::Beats,
            1 => StretchKind::Tones,
            2 => StretchKind::Texture,
            4 => StretchKind::Complex,
            5 => StretchKind::ComplexPro,
            _ => StretchKind::RePitch,
        }
    }

    fn warp_mode(&self) -> WarpMode {
        match self.warp_kind() {
            StretchKind::Beats => WarpMode::Beats {
                preserve: if self.get(WARP_PRESERVE).round() as i32 == 1 {
                    BeatsPreserve::Grid(PPQ as u32 / 4)
                } else {
                    BeatsPreserve::Transients
                },
                beats_loop: match self.get(WARP_LOOP).round() as i32 {
                    1 => BeatsLoop::Forward,
                    2 => BeatsLoop::BackAndForth,
                    _ => BeatsLoop::Off,
                },
                transient_envelope: self.get(WARP_ENVELOPE) as f32,
            },
            StretchKind::Tones => WarpMode::Tones { grain_size: 40.0 },
            StretchKind::Texture => WarpMode::Texture {
                grain_size: 40.0,
                flux: 0.0,
            },
            StretchKind::Complex => WarpMode::Complex,
            StretchKind::ComplexPro => WarpMode::ComplexPro {
                formants: 1.0,
                envelope: 128.0,
            },
            _ => WarpMode::RePitch,
        }
    }

    fn configure_envelopes(&self, voice: &mut Voice) {
        let amp = EnvelopeSettings {
            loop_mode: map_loop(self.get(A_LOOP)),
            loop_time: (self.get(A_TIME) * 0.001) as f32,
            repeat_beats: SyncRate::from_index(self.get(A_RATE)).beats(),
            ..adsr(
                self.get(A_ATTACK),
                self.get(A_DECAY),
                self.get(A_SUSTAIN),
                self.get(A_RELEASE),
            )
        };
        voice.amp.set(&amp);
        voice.filter_env.set(&adsr(
            self.get(FE_ATTACK),
            self.get(FE_DECAY),
            self.get(FE_SUSTAIN),
            self.get(FE_RELEASE),
        ));
        voice.pitch_env.set(&adsr(
            self.get(PE_ATTACK),
            self.get(PE_DECAY),
            self.get(PE_SUSTAIN),
            self.get(PE_RELEASE),
        ));
    }

    fn lfo_settings(&self, key: f32) -> LfoSettings {
        let rate = if self.get(L_SYNC) >= 0.5 {
            LfoRate::Beats(SyncRate::from_index(self.get(L_BEATS)).beats())
        } else {
            let keyed = self.get(L_KEY) as f32 * (key - ROOT_KEY) / 12.0;
            LfoRate::Hz(self.get(L_FREQ) as f32 * 2f32.powf(keyed))
        };
        LfoSettings {
            shape: map_lfo_type(self.get(L_TYPE)),
            rate,
            offset: self.get(L_OFFSET) as f32,
            ..LfoSettings::default()
        }
    }

    fn filter_settings(&self, cutoff: f32) -> FilterSettings {
        filter_settings(
            self.get(F_TYPE),
            self.get(F_CIRCUIT),
            self.get(F_SLOPE),
            cutoff,
            self.get(F_RES) as f32,
            self.get(F_DRIVE) as f32,
            (self.get(F_MORPH) * 0.01) as f32,
        )
    }
}

impl Processor for Simpler {
    fn activate(&mut self, sample_rate: f64, max_block: usize) {
        self.sr = sample_rate;
        self.alloc.prepare(sample_rate);
        self.warp_left = vec![0.0; max_block];
        self.warp_right = vec![0.0; max_block];
        for v in self.voices.iter_mut() {
            v.amp.prepare(sample_rate);
            v.filter_env.prepare(sample_rate);
            v.pitch_env.prepare(sample_rate);
            v.lfo.prepare(sample_rate);
            for f in &mut v.filter {
                f.prepare(sample_rate);
            }
            v.warp.prepare(sample_rate);
        }
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        self.clock = BeatClock::from_transport(ctx.transport);
        self.tempo = ctx.transport.tempo;
        for v in self.voices.iter_mut().filter(|v| v.active) {
            v.amp.set_clock(&self.clock);
            v.filter_env.set_clock(&self.clock);
            v.pitch_env.set_clock(&self.clock);
            v.lfo.set_clock(&self.clock);
        }
        let this = &mut *self;
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => {
                if let Some(v) = this.values.get_mut(id as usize) {
                    *v = value;
                    if matches!(
                        id,
                        START_FLAG
                            | END_FLAG
                            | SLICE_BY
                            | SENSITIVITY
                            | DIVISION
                            | REGIONS
                            | WARP_BEATS
                    ) {
                        this.slices_dirty = true;
                    }
                }
            }
            BlockStep::Audio(audio) => this.render(audio.left, audio.right),
            BlockStep::Midi(message) => this.midi(message),
        });
        if ctx.view.is_active() {
            self.publish(ctx.view);
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.values.get_mut(id as usize) {
            *v = value;
            self.slices_dirty = true;
        }
    }

    fn reset(&mut self) {
        self.alloc.reset();
        for v in self.voices.iter_mut() {
            v.active = false;
        }
    }

    fn tail(&self) -> Tail {
        Tail::Infinite
    }

    fn set_resource(&mut self, slot: u32, resource: Resource) -> Option<Resource> {
        if slot == STATE_SLOT {
            return match resource.downcast::<ParsedState>() {
                Ok(state) => {
                    self.slices_dirty = true;
                    Some(std::mem::replace(&mut self.state, state) as Resource)
                }
                Err(refused) => Some(refused),
            };
        }
        if slot == ANALYSIS_SLOTS {
            return match resource.downcast::<Analysis>() {
                Ok(analysis) => {
                    self.slices_dirty = true;
                    self.analysis.replace(analysis).map(|old| old as Resource)
                }
                Err(refused) => Some(refused),
            };
        }
        if slot != 0 {
            return Some(resource);
        }
        match resource.downcast::<SampleBuffer>() {
            Ok(sample) => {
                self.slices_dirty = true;
                self.sample.replace(sample).map(|old| old as Resource)
            }
            Err(refused) => Some(refused),
        }
    }
}

impl Simpler {
    fn midi(&mut self, message: MidiMessage) {
        if self.sample.is_none() {
            return;
        }
        if let MidiMessage::NoteOn { key, velocity, .. } = message {
            if velocity > 0.0 && self.v(MODE).round() as usize == MODE_SLICING {
                if self.slices_dirty {
                    self.rebuild_slices();
                }
                let index = (key as i32 - FIRST_SLICE_KEY as i32) as isize;
                if index < 0 || index as usize >= self.slice_count {
                    return; // No slice on this key.
                }
            }
            self.alloc.set(&self.voice_settings());
        }
        let values = Values(&self.values);
        let sr = self.sr as f32;
        let (Some(sample), voices, slices, rng, clock) = (
            self.sample.as_deref(),
            &mut self.voices,
            &self.slices[..=self.slice_count],
            &mut self.rng,
            &self.clock,
        ) else {
            return;
        };
        self.alloc.handle(message, |event| match event {
            VoiceEvent::Start { voice, note, .. } => {
                Self::start_voice(
                    &mut voices[voice],
                    &values,
                    sample,
                    slices,
                    note.spread,
                    note.key,
                    note.velocity,
                    rng,
                    clock,
                );
            }
            VoiceEvent::Legato { voice, note } => {
                voices[voice].key = note.key as f32;
            }
            VoiceEvent::Release { voice } => {
                let v = &mut voices[voice];
                if !v.active {
                    return;
                }
                let gated = values.get(GATE).round() as i32 == 1;
                match values.mode() {
                    MODE_CLASSIC => {
                        v.amp.gate_off();
                        v.filter_env.gate_off();
                        v.pitch_env.gate_off();
                        v.released = true;
                    }
                    _ if gated => {
                        v.released = true;
                        let frames = values.get(FADE_OUT) as f32 * 0.001 * sr;
                        v.gate_left = frames.max(1.0);
                    }
                    _ => {} // Trigger: keeps playing.
                }
            }
            VoiceEvent::Kill { voice } => {
                voices[voice].active = false;
            }
        });
        // A voice can run out before its release arrives: it no longer has anything to fade.
        for i in 0..self.voices.len() {
            if !self.voices[i].active {
                self.alloc.finished(i);
            }
        }
    }

    fn voice_settings(&self) -> VoiceSettings {
        let mode = self.v(MODE).round() as usize;
        let playback = self.v(PLAYBACK).round() as usize;
        let voices = VOICE_COUNTS[(self.v(VOICES).round() as usize).min(13)];
        let glide = self.v(GLIDE).round() as i32;
        let spread = self.v(SPREAD) > 0.0;
        let poly_voices = match mode {
            MODE_CLASSIC => true,
            MODE_SLICING => playback == PLAY_POLY,
            _ => false,
        };
        VoiceSettings {
            mode: if !poly_voices || glide == 1 {
                VoiceMode::Mono
            } else {
                VoiceMode::Poly
            },
            polyphony: if poly_voices { voices } else { 1 },
            unison: if spread { 2 } else { 1 },
            steal: StealMode::Oldest,
            retrigger: self.v(RETRIG) >= 0.5,
            glide: if glide > 0 {
                GlideMode::Always
            } else {
                GlideMode::Off
            },
            glide_time: (self.v(GLIDE_TIME) * 0.001) as f32,
            bend_range: 5.0,
            ..Default::default()
        }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let Some(sample) = self.sample.clone() else {
            return;
        };
        if sample.len() < 2 || self.warp_left.len() < left.len() {
            return;
        }
        if self.slices_dirty {
            self.rebuild_slices();
        }
        let frames = left.len();
        self.alloc.advance(frames);
        let values = Values(&self.values);
        let volume = db_to_gain(values.get(VOLUME) as f32) * db_to_gain(values.get(GAIN) as f32);
        let pan = values.get(PAN) as f32;
        let rate = sample.sample_rate / self.sr.max(1.0);
        let filter_on = values.on(F_ON);
        let lfo_on = values.on(L_ON);
        let constant_slices: &[f64] = self.analysis.as_ref().map_or(&[], |a| &a.transient_frames);
        let mut warp_buffer_left = std::mem::take(&mut self.warp_left);
        let mut warp_buffer_right = std::mem::take(&mut self.warp_right);
        for index in 0..self.voices.len() {
            let v = &mut self.voices[index];
            if !v.active {
                continue;
            }
            let warped = v.warp.active;
            let mut alive = !warped
                || Self::render_warp(
                    v,
                    &values,
                    &sample,
                    constant_slices,
                    self.state.warp_map.as_ref(),
                    self.tempo,
                    self.sr,
                    &mut warp_buffer_left[..frames],
                    &mut warp_buffer_right[..frames],
                );
            let mut i = 0;
            while i < frames && alive {
                if v.ctl == 0 {
                    Self::control(
                        v,
                        &values,
                        &self.alloc,
                        index,
                        rate,
                        lfo_on,
                        filter_on,
                        self.sr,
                    );
                }
                let n = (CONTROL_FRAMES - v.ctl).min(frames - i);
                v.ctl = (v.ctl + n) % CONTROL_FRAMES;
                let total_pan = (pan + v.pan + v.pan_random + v.lfo_pan).clamp(-1.0, 1.0);
                let (pl, pr) = ((1.0 - total_pan).min(1.0), (1.0 + total_pan).min(1.0));
                let classic = v.env_shape;
                let fade_in = values.get(FADE_IN) as f32 * 0.001 * self.sr as f32;
                let fade_out = values.get(FADE_OUT) as f32 * 0.001 * self.sr as f32;
                for frame in i..i + n {
                    if !alive {
                        break;
                    }
                    let [mut l, mut r] = if warped {
                        [warp_buffer_left[frame], warp_buffer_right[frame]]
                    } else {
                        v.player.sample(&sample, Interp::Good, v.released)
                    };
                    let shape = if classic {
                        v.amp.next()
                    } else {
                        Self::fades(v, fade_in, fade_out)
                    };
                    if filter_on {
                        l = v.filter[0].process(l);
                        r = v.filter[1].process(r);
                    }
                    let gain = shape * v.gain * volume * v.lfo_volume;
                    left[frame] += l * gain * pl;
                    right[frame] += r * gain * pr;
                    if !warped {
                        alive = v.player.advance(v.step, v.released);
                    }
                    v.played += 1;
                    if classic && !v.amp.is_active() {
                        alive = false;
                    }
                    if !classic && v.gate_left == 0.0 {
                        alive = false;
                    }
                }
                i += n;
            }
            if !alive {
                v.active = false;
                self.alloc.finished(index);
            }
        }
        self.warp_left = warp_buffer_left;
        self.warp_right = warp_buffer_right;
    }

    /// One Fade In/Out step: a linear ramp up from the start, and down over the last
    /// [`FADE_OUT`] of the region or after a Gate release.
    #[inline]
    fn fades(v: &mut Voice, fade_in: f32, fade_out: f32) -> f32 {
        let mut gain = 1.0;
        if fade_in > 0.0 {
            gain *= (v.played as f32 / fade_in).min(1.0);
        }
        if v.gate_left >= 0.0 {
            if fade_out > 0.0 {
                gain *= v.gate_left / fade_out;
                v.gate_left = (v.gate_left - 1.0).max(0.0);
            } else {
                v.gate_left = 0.0;
                return 0.0;
            }
        } else if fade_out > 0.0 {
            let remaining = ((v.fade_to - v.player.position()) / v.step.max(1e-6)).max(0.0) as f32;
            gain *= (remaining / fade_out).min(1.0);
        }
        gain.max(0.0)
    }

    #[allow(clippy::too_many_arguments)]
    fn control(
        v: &mut Voice,
        values: &Values,
        alloc: &VoiceAllocator,
        index: usize,
        rate: f64,
        lfo_on: bool,
        filter_on: bool,
        sr: f64,
    ) {
        values.configure_envelopes(v);
        let lfo = if lfo_on {
            let s = values.lfo_settings(v.key);
            v.lfo.set(&s);
            let raw = v.lfo.advance(CONTROL_FRAMES)[0];
            let attack = values.get(L_ATTACK) as f32 * 0.001 * sr as f32;
            if attack > 0.0 {
                v.lfo_ramp = (v.lfo_ramp + CONTROL_FRAMES as f32 / attack).min(1.0);
            } else {
                v.lfo_ramp = 1.0;
            }
            raw * v.lfo_ramp
        } else {
            0.0
        };
        let pitch_env = v.pitch_env.advance(CONTROL_FRAMES);
        let filter_env = v.filter_env.advance(CONTROL_FRAMES);
        let bend_key = alloc.pitch(index);
        let slicing = values.mode() == MODE_SLICING;
        let semis = (if slicing { 0.0 } else { bend_key - ROOT_KEY })
            + values.get(TRANSPOSE) as f32
            + values.get(DETUNE) as f32 * 0.01
            + v.detune * 0.01
            + pitch_env * values.get(PE_AMOUNT) as f32
            + lfo * values.get(L_PITCH) as f32 * 0.01 * 12.0;
        v.step = 2f64.powf(semis as f64 / 12.0) * rate;
        v.lfo_volume = (1.0 + lfo * (values.get(L_VOLUME) * 0.01) as f32).max(0.0);
        v.lfo_pan = lfo * (values.get(L_PAN) * 0.01) as f32;
        if filter_on {
            let key = (bend_key - ROOT_KEY) * (values.get(F_KEY) * 0.01) as f32;
            let vel = (v.velocity - 1.0) * (values.get(F_VEL) * 0.01) as f32 * 48.0;
            let env = filter_env * values.get(F_ENV) as f32;
            let lfo_part = lfo * (values.get(L_FILTER) * 0.01) as f32 * 24.0;
            let cutoff = values.get(F_FREQ) as f32 * 2f32.powf((key + vel + env + lfo_part) / 12.0);
            let settings = values.filter_settings(cutoff.clamp(20.0, 20_000.0));
            for f in &mut v.filter {
                f.set(&settings);
            }
        }
    }

    /// Renders a warped voice into `left`/`right`. Returns whether it's still playing.
    #[allow(clippy::too_many_arguments)]
    fn render_warp(
        v: &mut Voice,
        values: &Values,
        sample: &SampleBuffer,
        slices: &[f64],
        warp_map: Option<&WarpMap>,
        tempo: f64,
        sr: f64,
        left: &mut [f32],
        right: &mut [f32],
    ) -> bool {
        left.fill(0.0);
        right.fill(0.0);
        let Some(kind) = v.warp.kind else {
            return false;
        };
        let region_len = (v.fade_to - v.fade_from).max(1.0);
        let beats = values.get(WARP_BEATS).max(1e-6);
        let tick_start = v.warp.tick;
        let tick_end = tick_start + left.len() as f64 * tempo / 60.0 * PPQ as f64 / sr.max(1.0);
        // With a clip's Warp Markers the map says where the timeline is in the file; without
        // them the region is simply Warp As beats long.
        let (src_start, src_end) = match warp_map {
            Some(map) => {
                let first = map.src_to_tick(v.fade_from);
                (
                    map.tick_to_src(first + tick_start),
                    map.tick_to_src(first + tick_end),
                )
            }
            None => {
                let frames_per_tick = region_len / (beats * PPQ as f64);
                (
                    v.fade_from + tick_start * frames_per_tick,
                    v.fade_from + tick_end * frames_per_tick,
                )
            }
        };
        v.warp.tick = tick_end;
        v.player.seek(src_start.min(v.fade_to));
        let semis =
            v.key as f64 - ROOT_KEY as f64 + values.get(TRANSPOSE) + values.get(DETUNE) * 0.01;
        let span = StretchSpan {
            src_start,
            src_end,
            transpose: semis as f32,
            mode: values.warp_mode(),
            slices,
        };
        v.warp.stretcher(kind).render(sample, span, left, right);
        if v.released && v.gate_left < 0.0 && !v.amp.is_active() {
            return false;
        }
        src_start < v.fade_to
    }

    fn publish(&self, view: &rlive_core::ViewWriter) {
        if let Some(analysis) = &self.analysis {
            view.publish(VIEW_WAVE, &analysis.overview);
        }
        let n = self
            .sample
            .as_ref()
            .map_or(1.0, |s| (s.len() as f32).max(1.0));
        let mut slices = [0.0f32; SLICE_VALUES];
        slices[0] = self.slice_count as f32;
        for (out, s) in slices[1..]
            .iter_mut()
            .zip(&self.slices[..=self.slice_count.min(MAX_SLICES)])
        {
            *out = *s as f32 / n;
        }
        view.publish(VIEW_SLICES, &slices);
        let mut heads = [-1.0f32; HEAD_VALUES];
        for (out, v) in heads
            .iter_mut()
            .zip(self.voices.iter().filter(|v| v.active))
        {
            *out = v.player.position() as f32 / n;
        }
        view.publish(VIEW_HEADS, &heads);
        let info = [n, self.analysis.as_ref().map_or(0.0, |a| a.peak)];
        view.publish(VIEW_INFO, &info);
    }
}
