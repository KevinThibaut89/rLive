//! Meld (manual pp. 745-755): two macro-oscillator engines, each with its filter, envelopes,
//! LFOs and modulation matrix, mixed per voice and driven and limited together.
//!
//! Named and specified by the manual: all oscillator and filter types, the envelope loop modes
//! (Trigger, Loop, AD Loop), LFO 1's six types, LFO 2's six shapes, key tracking, Phase Reset and
//! Spread, the two scale-awareness switches, Porta and Gliss, 2 to 12 voices. The manual doesn't
//! name LFO 1's eighteen FX, its macros, the Stacked Voices counts, or the depth of any
//! modulation, so those are ours.

use super::common::{
    nearest_in_scale, note_hz, pan_gains, soft_clip, transpose_degrees, CONTROL_INTERVAL,
};
use super::meld_filter::{MeldFilter, FILTER_MACROS, FILTER_TYPES, MEMBRANE, PLATE};
use super::meld_osc::{is_scale_aware, MeldOsc, OscControl, OSC_MACROS, OSC_TYPES};
use crate::dsp::envelope::{Envelope, EnvelopeSettings, LoopMode, LoopSpan};
use crate::dsp::lfo::{Lfo, LfoRate, LfoSettings};
use crate::dsp::modmatrix::{ModBus, ModMatrix, Route, RouteKind};
use crate::dsp::tempo::{BeatClock, SyncRate};
use crate::dsp::util::{db_to_gain, lerp, sanitize, Rng};
use crate::dsp::voice::{
    GlideMode, NoteInfo, VoiceAllocator, VoiceEvent, VoiceMode, VoiceSettings,
};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    BlockStep, DeviceDesc, Item, MidiMessage, ParamId, ParamInfo, ProcessContext, Processor,
    ScaleInfo, Section, Status, Tail,
};
use std::f32::consts::TAU;

pub const E_ON: u32 = 0;
pub const E_OCT: u32 = 1;
pub const E_SEMI: u32 = 2;
pub const E_CENTS: u32 = 3;
pub const E_TYPE: u32 = 4;
pub const E_M1: u32 = 5;
pub const E_M2: u32 = 6;
pub const AE_ATTACK: u32 = 7;
pub const AE_DECAY: u32 = 8;
pub const AE_SUSTAIN: u32 = 9;
pub const AE_RELEASE: u32 = 10;
pub const AE_A_SLOPE: u32 = 11;
pub const AE_D_SLOPE: u32 = 12;
pub const AE_R_SLOPE: u32 = 13;
pub const AE_LOOP: u32 = 14;
pub const ME_ATTACK: u32 = 15;
pub const ME_DECAY: u32 = 16;
pub const ME_SUSTAIN: u32 = 17;
pub const ME_RELEASE: u32 = 18;
pub const ME_A_SLOPE: u32 = 19;
pub const ME_D_SLOPE: u32 = 20;
pub const ME_R_SLOPE: u32 = 21;
pub const ME_LOOP: u32 = 22;
pub const ME_INITIAL: u32 = 23;
pub const ME_PEAK: u32 = 24;
pub const ME_FINAL: u32 = 25;
pub const L1_TYPE: u32 = 26;
pub const L1_RATE: u32 = 27;
pub const L1_SYNC: u32 = 28;
pub const L1_SYNC_RATE: u32 = 29;
pub const L1_PHASE: u32 = 30;
pub const L1_RETRIG: u32 = 31;
pub const L1_M1: u32 = 32;
pub const L1_M2: u32 = 33;
pub const L1_FX1: u32 = 34;
pub const L1_FX1_AMT: u32 = 35;
pub const L1_FX2: u32 = 36;
pub const L1_FX2_AMT: u32 = 37;
pub const L2_TYPE: u32 = 38;
pub const L2_RATE: u32 = 39;
pub const L2_SYNC: u32 = 40;
pub const L2_SYNC_RATE: u32 = 41;
pub const L2_PHASE: u32 = 42;
pub const L2_RETRIG: u32 = 43;
pub const KEY_TRACK: u32 = 44;
pub const PHASE_RESET: u32 = 45;
pub const PHASE_SPREAD: u32 = 46;
pub const OSC_SCALE: u32 = 47;
pub const FILTER_SCALE: u32 = 48;
pub const GLIDE_MODE: u32 = 49;
pub const GLIDE_TIME: u32 = 50;
pub const F_ON: u32 = 51;
pub const F_FREQ: u32 = 52;
pub const F_TYPE: u32 = 53;
pub const F_M1: u32 = 54;
pub const F_M2: u32 = 55;
pub const MIX_VOLUME: u32 = 56;
pub const MIX_PAN: u32 = 57;
pub const MIX_TONE: u32 = 58;
const ENGINE_BLOCK: u32 = 59;

pub const ENGINE_A: u32 = 0;
pub const ENGINE_B: u32 = ENGINE_BLOCK;
pub const MONO: u32 = ENGINE_B + ENGINE_BLOCK;
pub const LEGATO: u32 = MONO + 1;
pub const VOICES: u32 = MONO + 2;
pub const SPREAD: u32 = MONO + 3;
pub const STACKED: u32 = MONO + 4;
pub const DRIVE: u32 = MONO + 5;
pub const LIMIT: u32 = MONO + 6;
pub const VOLUME: u32 = MONO + 7;
pub const USE_SCALE: u32 = MONO + 8;
pub const LINK_ENV: u32 = MONO + 9;
pub const MATRIX_BASE: u32 = MONO + 10;

pub const SOURCES: [&str; 15] = [
    "Amp Env",
    "Mod Env",
    "LFO 1",
    "LFO 1 FX",
    "LFO 2",
    "Velocity",
    "Pitch",
    "Random",
    "Spread",
    "Pitch Bend",
    "Press",
    "Mod Wheel",
    "Note PB",
    "Slide",
    "MPE Press",
];
pub const TARGETS: [&str; 18] = [
    "Volume",
    "Pan",
    "Pitch",
    "Pitch Quant",
    "Osc Macro 1",
    "Osc Macro 2",
    "Filter Freq",
    "Filter Macro 1",
    "Filter Macro 2",
    "Tone Filter",
    "Amp Env Time",
    "Mod Env Time",
    "LFO 1 Rate",
    "LFO 1 Macro 1",
    "LFO 1 Macro 2",
    "LFO 1 FX 1",
    "LFO 1 FX 2",
    "LFO 2 Rate",
];
/// Multiplicative targets; the rest add.
const MULTIPLICATIVE: [bool; TARGETS.len()] = {
    let mut m = [false; TARGETS.len()];
    m[T_VOLUME] = true;
    m
};
const S_AMP: usize = 0;
const S_MOD: usize = 1;
const S_LFO1: usize = 2;
const S_LFO1FX: usize = 3;
const S_LFO2: usize = 4;
const S_VEL: usize = 5;
const S_PITCH: usize = 6;
const S_RANDOM: usize = 7;
const S_SPREAD: usize = 8;
const S_PB: usize = 9;
const S_PRESS: usize = 10;
const S_MW: usize = 11;
const S_NOTEPB: usize = 12;
const S_SLIDE: usize = 13;
const S_MPEPRESS: usize = 14;
const T_VOLUME: usize = 0;
const T_PAN: usize = 1;
const T_PITCH: usize = 2;
const T_QUANT: usize = 3;
const T_M1: usize = 4;
const T_M2: usize = 5;
const T_FREQ: usize = 6;
const T_FM1: usize = 7;
const T_FM2: usize = 8;
const T_TONE: usize = 9;
const T_AMP_TIME: usize = 10;
const T_MOD_TIME: usize = 11;
const T_L1_RATE: usize = 12;
const T_L1_M1: usize = 13;
const T_L1_M2: usize = 14;
const T_L1_FX1: usize = 15;
const T_L1_FX2: usize = 16;
const T_L2_RATE: usize = 17;

const CELLS: usize = SOURCES.len() * TARGETS.len();
pub const PARAM_COUNT: usize = MATRIX_BASE as usize + 2 * CELLS;

/// The id of one matrix cell.
pub const fn cell(engine: usize, source: usize, target: usize) -> u32 {
    MATRIX_BASE + (engine * CELLS + source * TARGETS.len() + target) as u32
}

pub const ENV_LOOPS: [&str; 4] = ["None", "Trigger", "Loop", "AD Loop"];
pub const LFO1_TYPES: [&str; 6] = [
    "Basic Shapes",
    "Ramp",
    "Wander",
    "Alternate",
    "Euclid",
    "Pulsate",
];
pub const LFO1_MACROS: [(&str, &str); 6] = [
    ("Shape", "Skew"),
    ("Curve", "Steps"),
    ("Depth", "Smooth"),
    ("Ratio", "Width"),
    ("Steps", "Pulses"),
    ("Count", "Decay"),
];
pub const LFO1_FX: [&str; 18] = [
    "Smooth",
    "Invert",
    "Quantize",
    "Clip",
    "Fold",
    "Skew",
    "Slew Up",
    "Slew Down",
    "Offset",
    "Scale",
    "Rectify",
    "Step Hold",
    "Decimate",
    "Tilt",
    "Delay",
    "Stutter",
    "Noise",
    "Chance",
];
pub const LFO2_TYPES: [&str; 6] = [
    "Sine",
    "Tri",
    "Saw Up",
    "Saw Down",
    "Rectangle",
    "Random S&H",
];
pub const GLIDE_MODES: [&str; 2] = ["Porta", "Gliss"];
pub const STACK_COUNTS: [usize; 6] = [1, 2, 3, 4, 6, 8];

fn engine_params(out: &mut Vec<ParamInfo>, base: u32, e: &str, is_a: bool) {
    let n = |s: &str| format!("{e} {s}");
    let pct =
        |id, s: &str, min, max, def| ParamInfo::continuous(base + id, &n(s), min, max, def, "%");
    let time =
        |id, s: &str, def| ParamInfo::skewed(base + id, &n(s), 0.5, 20_000.0, def, "ms", 4.0);
    let slope = |id, s: &str| ParamInfo::continuous(base + id, &n(s), -100.0, 100.0, 0.0, "%");
    out.push(ParamInfo::toggle(base + E_ON, &format!("Engine {e}"), is_a));
    out.push(ParamInfo::continuous(
        base + E_OCT,
        &n("Octaves"),
        -3.0,
        3.0,
        0.0,
        "",
    ));
    out.push(ParamInfo::continuous(
        base + E_SEMI,
        &n("Semitones"),
        -12.0,
        12.0,
        0.0,
        "",
    ));
    out.push(ParamInfo::continuous(
        base + E_CENTS,
        &n("Cents"),
        -100.0,
        100.0,
        0.0,
        "ct",
    ));
    out.push(ParamInfo::stepped(
        base + E_TYPE,
        &n("Oscillator Type"),
        &OSC_TYPES,
        0,
    ));
    out.push(pct(E_M1, "Osc Macro 1", 0.0, 100.0, 30.0));
    out.push(pct(E_M2, "Osc Macro 2", 0.0, 100.0, 20.0));
    out.push(time(AE_ATTACK, "Amp Attack", 2.0));
    out.push(time(AE_DECAY, "Amp Decay", 300.0));
    out.push(ParamInfo::continuous(
        base + AE_SUSTAIN,
        &n("Amp Sustain"),
        0.0,
        1.0,
        0.8,
        "",
    ));
    out.push(time(AE_RELEASE, "Amp Release", 150.0));
    out.push(slope(AE_A_SLOPE, "Amp Attack Slope"));
    out.push(slope(AE_D_SLOPE, "Amp Decay Slope"));
    out.push(slope(AE_R_SLOPE, "Amp Release Slope"));
    out.push(ParamInfo::stepped(
        base + AE_LOOP,
        &n("Amp Loop"),
        &ENV_LOOPS,
        0,
    ));
    out.push(time(ME_ATTACK, "Mod Attack", 2.0));
    out.push(time(ME_DECAY, "Mod Decay", 400.0));
    out.push(ParamInfo::continuous(
        base + ME_SUSTAIN,
        &n("Mod Sustain"),
        0.0,
        1.0,
        0.3,
        "",
    ));
    out.push(time(ME_RELEASE, "Mod Release", 200.0));
    out.push(slope(ME_A_SLOPE, "Mod Attack Slope"));
    out.push(slope(ME_D_SLOPE, "Mod Decay Slope"));
    out.push(slope(ME_R_SLOPE, "Mod Release Slope"));
    out.push(ParamInfo::stepped(
        base + ME_LOOP,
        &n("Mod Loop"),
        &ENV_LOOPS,
        0,
    ));
    out.push(ParamInfo::continuous(
        base + ME_INITIAL,
        &n("Mod Initial"),
        0.0,
        1.0,
        0.0,
        "",
    ));
    out.push(ParamInfo::continuous(
        base + ME_PEAK,
        &n("Mod Peak"),
        0.0,
        1.0,
        1.0,
        "",
    ));
    out.push(ParamInfo::continuous(
        base + ME_FINAL,
        &n("Mod Final"),
        0.0,
        1.0,
        0.0,
        "",
    ));
    out.push(ParamInfo::stepped(
        base + L1_TYPE,
        &n("LFO 1 Type"),
        &LFO1_TYPES,
        0,
    ));
    out.push(ParamInfo::skewed(
        base + L1_RATE,
        &n("LFO 1 Rate"),
        0.01,
        40.0,
        2.0,
        "Hz",
        3.0,
    ));
    out.push(ParamInfo::toggle(base + L1_SYNC, &n("LFO 1 Sync"), false));
    out.push(ParamInfo::stepped(
        base + L1_SYNC_RATE,
        &n("LFO 1 Beat Rate"),
        &SyncRate::LABELS,
        12,
    ));
    out.push(ParamInfo::continuous(
        base + L1_PHASE,
        &n("LFO 1 Phase"),
        0.0,
        360.0,
        0.0,
        "°",
    ));
    out.push(ParamInfo::toggle(
        base + L1_RETRIG,
        &n("LFO 1 Retrig"),
        false,
    ));
    out.push(pct(L1_M1, "LFO 1 Macro 1", 0.0, 100.0, 0.0));
    out.push(pct(L1_M2, "LFO 1 Macro 2", 0.0, 100.0, 50.0));
    out.push(ParamInfo::stepped(
        base + L1_FX1,
        &n("LFO 1 FX 1"),
        &LFO1_FX,
        0,
    ));
    out.push(pct(L1_FX1_AMT, "LFO 1 FX 1 Amount", 0.0, 100.0, 0.0));
    out.push(ParamInfo::stepped(
        base + L1_FX2,
        &n("LFO 1 FX 2"),
        &LFO1_FX,
        0,
    ));
    out.push(pct(L1_FX2_AMT, "LFO 1 FX 2 Amount", 0.0, 100.0, 0.0));
    out.push(ParamInfo::stepped(
        base + L2_TYPE,
        &n("LFO 2 Type"),
        &LFO2_TYPES,
        0,
    ));
    out.push(ParamInfo::skewed(
        base + L2_RATE,
        &n("LFO 2 Rate"),
        0.01,
        40.0,
        1.0,
        "Hz",
        3.0,
    ));
    out.push(ParamInfo::toggle(base + L2_SYNC, &n("LFO 2 Sync"), false));
    out.push(ParamInfo::stepped(
        base + L2_SYNC_RATE,
        &n("LFO 2 Beat Rate"),
        &SyncRate::LABELS,
        12,
    ));
    out.push(ParamInfo::continuous(
        base + L2_PHASE,
        &n("LFO 2 Phase"),
        0.0,
        360.0,
        0.0,
        "°",
    ));
    out.push(ParamInfo::toggle(
        base + L2_RETRIG,
        &n("LFO 2 Retrig"),
        false,
    ));
    out.push(ParamInfo::toggle(
        base + KEY_TRACK,
        &n("Osc Key Tracking"),
        true,
    ));
    out.push(ParamInfo::toggle(
        base + PHASE_RESET,
        &n("Phase Reset"),
        false,
    ));
    out.push(ParamInfo::toggle(
        base + PHASE_SPREAD,
        &n("Phase Spread"),
        false,
    ));
    out.push(ParamInfo::toggle(
        base + OSC_SCALE,
        &n("Oscillator Scale Awareness"),
        false,
    ));
    out.push(ParamInfo::toggle(
        base + FILTER_SCALE,
        &n("Filter Scale Awareness"),
        false,
    ));
    out.push(ParamInfo::stepped(
        base + GLIDE_MODE,
        &n("Glide Mode"),
        &GLIDE_MODES,
        0,
    ));
    out.push(ParamInfo::skewed(
        base + GLIDE_TIME,
        &n("Glide Time"),
        0.0,
        5_000.0,
        0.0,
        "ms",
        3.0,
    ));
    out.push(ParamInfo::toggle(base + F_ON, &format!("Filter {e}"), true));
    out.push(ParamInfo::skewed(
        base + F_FREQ,
        &n("Filter Frequency"),
        20.0,
        20_000.0,
        8_000.0,
        "Hz",
        2.5,
    ));
    out.push(ParamInfo::stepped(
        base + F_TYPE,
        &n("Filter Type"),
        &FILTER_TYPES,
        0,
    ));
    out.push(pct(F_M1, "Filter Macro 1", 0.0, 100.0, 20.0));
    out.push(pct(F_M2, "Filter Macro 2", 0.0, 100.0, 0.0));
    out.push(ParamInfo::continuous(
        base + MIX_VOLUME,
        &n("Volume"),
        -60.0,
        12.0,
        -6.0,
        "dB",
    ));
    out.push(ParamInfo::continuous(
        base + MIX_PAN,
        &n("Pan"),
        -1.0,
        1.0,
        0.0,
        "",
    ));
    out.push(ParamInfo::continuous(
        base + MIX_TONE,
        &n("Tone Filter"),
        -1.0,
        1.0,
        0.0,
        "",
    ));
}

/// The default of a matrix cell: only the bends are routed, so a keyboard works out of the box.
fn cell_default(source: usize, target: usize) -> f64 {
    match (source, target) {
        (S_PB, T_PITCH) => 2.0 / 48.0 * 100.0,
        (S_NOTEPB, T_PITCH) => 100.0,
        _ => 0.0,
    }
}

pub fn params() -> Vec<ParamInfo> {
    let mut out = Vec::with_capacity(PARAM_COUNT);
    engine_params(&mut out, ENGINE_A, "A", true);
    engine_params(&mut out, ENGINE_B, "B", false);
    out.push(ParamInfo::stepped(MONO, "Mono/Poly", &["Poly", "Mono"], 0));
    out.push(ParamInfo::toggle(LEGATO, "Legato", false));
    out.push(ParamInfo::stepped(
        VOICES,
        "Voices",
        &["2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12"],
        6,
    ));
    out.push(ParamInfo::continuous(
        SPREAD, "Spread", 0.0, 100.0, 50.0, "%",
    ));
    out.push(ParamInfo::stepped(
        STACKED,
        "Stacked Voices",
        &["Off", "2", "3", "4", "6", "8"],
        0,
    ));
    out.push(ParamInfo::continuous(
        DRIVE,
        "Mixer Drive",
        0.0,
        100.0,
        0.0,
        "%",
    ));
    out.push(ParamInfo::toggle(LIMIT, "Limit", false));
    out.push(ParamInfo::continuous(
        VOLUME, "Volume", -48.0, 12.0, 0.0, "dB",
    ));
    out.push(ParamInfo::toggle(USE_SCALE, "Use Current Scale", false));
    out.push(ParamInfo::toggle(LINK_ENV, "Link Envelopes", false));
    for (engine, e) in ["A", "B"].iter().enumerate() {
        for (s, source) in SOURCES.iter().enumerate() {
            for (t, target) in TARGETS.iter().enumerate() {
                out.push(ParamInfo::continuous(
                    cell(engine, s, t),
                    &format!("{e} {source} > {target}"),
                    -100.0,
                    100.0,
                    cell_default(s, t),
                    "%",
                ));
            }
        }
    }
    out
}

fn layout() -> Vec<Section> {
    let knob = |id| Item::Knob(ParamId(id));
    let menu = |id| Item::Menu(ParamId(id));
    let toggle = |id| Item::Toggle(ParamId(id));
    let engine = |b: u32, title: &'static str| Section {
        title,
        items: vec![
            Item::Row(vec![
                toggle(b + E_ON),
                menu(b + E_TYPE),
                knob(b + E_OCT),
                knob(b + E_SEMI),
                knob(b + E_CENTS),
            ]),
            Item::Row(vec![knob(b + E_M1), knob(b + E_M2)]),
            Item::Row(vec![
                toggle(b + F_ON),
                menu(b + F_TYPE),
                knob(b + F_FREQ),
                knob(b + F_M1),
                knob(b + F_M2),
            ]),
            Item::Row(vec![
                knob(b + MIX_VOLUME),
                knob(b + MIX_PAN),
                knob(b + MIX_TONE),
            ]),
            Item::Row(vec![
                knob(b + AE_ATTACK),
                knob(b + AE_DECAY),
                knob(b + AE_SUSTAIN),
                knob(b + AE_RELEASE),
            ]),
            Item::Row(vec![
                knob(b + AE_A_SLOPE),
                knob(b + AE_D_SLOPE),
                knob(b + AE_R_SLOPE),
                menu(b + AE_LOOP),
            ]),
            Item::Row(vec![
                knob(b + ME_ATTACK),
                knob(b + ME_DECAY),
                knob(b + ME_SUSTAIN),
                knob(b + ME_RELEASE),
            ]),
            Item::Row(vec![
                knob(b + ME_A_SLOPE),
                knob(b + ME_D_SLOPE),
                knob(b + ME_R_SLOPE),
                menu(b + ME_LOOP),
            ]),
            Item::Row(vec![
                knob(b + ME_INITIAL),
                knob(b + ME_PEAK),
                knob(b + ME_FINAL),
            ]),
            Item::Row(vec![
                menu(b + L1_TYPE),
                knob(b + L1_RATE),
                toggle(b + L1_SYNC),
                menu(b + L1_SYNC_RATE),
            ]),
            Item::Row(vec![
                knob(b + L1_M1),
                knob(b + L1_M2),
                knob(b + L1_PHASE),
                toggle(b + L1_RETRIG),
            ]),
            Item::Row(vec![
                menu(b + L1_FX1),
                knob(b + L1_FX1_AMT),
                menu(b + L1_FX2),
                knob(b + L1_FX2_AMT),
            ]),
            Item::Row(vec![
                menu(b + L2_TYPE),
                knob(b + L2_RATE),
                toggle(b + L2_SYNC),
                menu(b + L2_SYNC_RATE),
            ]),
            Item::Row(vec![knob(b + L2_PHASE), toggle(b + L2_RETRIG)]),
            Item::Row(vec![
                toggle(b + KEY_TRACK),
                toggle(b + PHASE_RESET),
                toggle(b + PHASE_SPREAD),
            ]),
            Item::Row(vec![
                toggle(b + OSC_SCALE),
                toggle(b + FILTER_SCALE),
                menu(b + GLIDE_MODE),
                knob(b + GLIDE_TIME),
            ]),
        ],
    };
    vec![
        Section {
            title: "Matrix",
            items: vec![Item::Panel("meld_matrix")],
        },
        engine(ENGINE_A, "Engine A"),
        engine(ENGINE_B, "Engine B"),
        Section {
            title: "Global",
            items: vec![
                Item::Row(vec![
                    menu(MONO),
                    toggle(LEGATO),
                    menu(VOICES),
                    menu(STACKED),
                    knob(SPREAD),
                ]),
                Item::Row(vec![
                    knob(DRIVE),
                    toggle(LIMIT),
                    knob(VOLUME),
                    toggle(USE_SCALE),
                    toggle(LINK_ENV),
                ]),
            ],
        },
    ]
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.meld",
    name: "Meld",
    role: DeviceRole::Instrument,
    category: "Synths",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::new(Meld::new()),
};

const MAX_VOICES: usize = 48;
const VOICE_GAIN: f32 = 0.6;
const PITCH_RANGE_ST: f32 = 48.0;
const QUANT_RANGE: f32 = 24.0;
const FREQ_RANGE_ST: f32 = 96.0;
const TIME_RANGE_OCT: f32 = 4.0;
const DRIVE_MAX_DB: f32 = 24.0;

#[derive(Clone, Copy, Default)]
struct LfoFx {
    value: f32,
    counter: u32,
    held: f32,
    delay: [f32; 8],
}

/// What LFO 1's FX `kind` does to `x`, with `a` (0..1) the macro. At 0 each leaves `x` alone.
fn apply_fx(kind: usize, x: f32, a: f32, s: &mut LfoFx, rng: &mut Rng) -> f32 {
    let y = match kind {
        0 => {
            s.value += (x - s.value) * (1.0 - a * 0.995).powi(2);
            s.value
        }
        1 => lerp(x, -x, a),
        2 => {
            let steps = lerp(64.0, 2.0, a);
            lerp(x, (x * steps).round() / steps, (a * 40.0).min(1.0))
        }
        3 => (x * (1.0 + a * 9.0)).clamp(-1.0, 1.0),
        4 => (x * (1.0 + a * 4.0) * std::f32::consts::FRAC_PI_2).sin(),
        5 => x.signum() * x.abs().powf(1.0 - a * 0.8),
        6 => {
            let rate = (1.0 - a).powi(2) + 0.002;
            s.value += if x > s.value {
                (x - s.value) * rate
            } else {
                x - s.value
            };
            s.value
        }
        7 => {
            let rate = (1.0 - a).powi(2) + 0.002;
            s.value += if x < s.value {
                (x - s.value) * rate
            } else {
                x - s.value
            };
            s.value
        }
        8 => (x + a).clamp(-1.0, 1.0),
        9 => x * (1.0 - a),
        10 => lerp(x, x.abs(), a),
        11 => {
            s.counter += 1;
            let period = 1 + (a * 40.0) as u32;
            if s.counter >= period {
                s.counter = 0;
                s.held = x;
            }
            s.held
        }
        12 => {
            let levels = lerp(128.0, 2.0, a).max(1.0);
            lerp(x, (x * levels).floor() / levels, (a * 40.0).min(1.0))
        }
        13 => lerp(x, x * (1.0 + x.signum() * 0.5), a).clamp(-1.0, 1.0),
        14 => {
            s.delay.rotate_left(1);
            s.delay[7] = x;
            lerp(x, s.delay[0], a)
        }
        15 => {
            s.counter += 1;
            if s.counter > (a * 8.0) as u32 {
                s.counter = 0;
                s.held = x;
            }
            lerp(x, s.held, (a * 40.0).min(1.0))
        }
        16 => x + rng.bipolar() * a * 0.5,
        _ => {
            s.counter += 1;
            if s.counter >= 16 {
                s.counter = 0;
                s.held = if rng.unipolar() < a { 0.0 } else { 1.0 };
            }
            x * lerp(1.0, s.held, (a * 40.0).min(1.0))
        }
    };
    sanitize(y).clamp(-2.0, 2.0)
}

fn euclid_step(step: u32, steps: u32, pulses: u32) -> bool {
    pulses > 0 && (step * pulses) % steps < pulses
}

#[derive(Clone)]
struct EngineVoice {
    osc: MeldOsc,
    control: OscControl,
    filter: MeldFilter,
    filter_on: bool,
    amp: Envelope,
    modenv: Envelope,
    lfo1: Lfo,
    lfo2: Lfo,
    last_phase: f32,
    held: f32,
    prev: f32,
    rng: Rng,
    fx: [LfoFx; 2],
    alt: f32,
    pitch: f32,
    pitch_step: f32,
    target: f32,
    bus: ModBus,
    tone_lp: f32,
    tone_hp: f32,
    lp_coef: f32,
    hp_coef: f32,
    tone_mix: f32,
    gain: f32,
    gain_target: f32,
    pan: (f32, f32),
    pan_target: (f32, f32),
}

impl EngineVoice {
    fn new(seed: u32, bus: ModBus, max_comb: usize) -> Self {
        Self {
            osc: MeldOsc::new(seed),
            control: OscControl {
                kind: 0,
                note: 60.0,
                hz: 261.6,
                sample_rate: 48_000.0,
                macro1: 0.0,
                macro2: 0.0,
                scale: None,
                song_scale: None,
                use_current_scale: false,
            },
            filter: MeldFilter::new(max_comb),
            filter_on: true,
            amp: Envelope::new(),
            modenv: Envelope::new(),
            lfo1: Lfo::new(seed.wrapping_add(1)),
            lfo2: Lfo::new(seed.wrapping_add(2)),
            last_phase: 0.0,
            held: 0.0,
            prev: 0.0,
            rng: Rng::new(seed.wrapping_add(3)),
            fx: [LfoFx::default(); 2],
            alt: 1.0,
            pitch: 60.0,
            pitch_step: 0.0,
            target: 60.0,
            bus,
            tone_lp: 0.0,
            tone_hp: 0.0,
            lp_coef: 0.0,
            hp_coef: 0.0,
            tone_mix: 0.0,
            gain: 0.0,
            gain_target: 0.0,
            pan: (1.0, 1.0),
            pan_target: (1.0, 1.0),
        }
    }
}

#[derive(Clone)]
struct Voice {
    active: bool,
    gated: bool,
    fresh: bool,
    key: f32,
    velocity: f32,
    random: f32,
    spread: f32,
    drive: f32,
    engines: [EngineVoice; 2],
}

impl Voice {
    fn silence(&mut self) {
        self.active = false;
        self.gated = false;
        self.fresh = false;
        for e in &mut self.engines {
            e.amp.kill();
            e.modenv.kill();
            e.filter.reset();
            e.osc.clear();
            e.gain = 0.0;
            e.tone_lp = 0.0;
            e.tone_hp = 0.0;
        }
    }

    fn release(&mut self) {
        self.gated = false;
        for e in &mut self.engines {
            e.amp.gate_off();
            e.modenv.gate_off();
        }
    }
}

/// Meld's engine.
pub struct Meld {
    v: Vec<f64>,
    alloc: VoiceAllocator,
    voices: Vec<Voice>,
    matrices: [ModMatrix; 2],
    free: [[Lfo; 2]; 2],
    sr: f32,
    ctrl_phase: usize,
    mod_wheel: f32,
    clock: BeatClock,
    scale: Option<ScaleInfo>,
    dirty: bool,
    rng: Rng,
    last_key: Option<f32>,
    note_count: usize,
    limit: bool,
}

impl Meld {
    /// How many voices are sounding right now.
    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active).count()
    }
}

impl Default for Meld {
    fn default() -> Self {
        Self::new()
    }
}

fn loop_settings(mode: usize) -> (LoopMode, LoopSpan) {
    match mode {
        1 => (LoopMode::Trigger, LoopSpan::AttackDecay),
        2 => (LoopMode::Loop, LoopSpan::AttackDecayRelease),
        3 => (LoopMode::Loop, LoopSpan::AttackDecay),
        _ => (LoopMode::Off, LoopSpan::AttackDecay),
    }
}

impl Meld {
    pub fn new() -> Self {
        // Worked out once, off the audio thread.
        super::meld_filter::membrane_ratios();
        let defaults = params();
        let mut v = vec![0.0; PARAM_COUNT];
        for p in &defaults {
            v[p.id as usize] = p.default;
        }
        let matrices = [
            ModMatrix::new(SOURCES.len(), TARGETS.len(), CELLS),
            ModMatrix::new(SOURCES.len(), TARGETS.len(), CELLS),
        ];
        let max_comb = 4_096;
        let voices = (0..MAX_VOICES)
            .map(|i| {
                let seed = 31 + i as u32 * 19;
                Voice {
                    active: false,
                    gated: false,
                    fresh: false,
                    key: 60.0,
                    velocity: 1.0,
                    random: 0.0,
                    spread: 0.0,
                    drive: 1.0,
                    engines: [
                        EngineVoice::new(seed, matrices[0].bus(), max_comb),
                        EngineVoice::new(seed.wrapping_add(7), matrices[1].bus(), max_comb),
                    ],
                }
            })
            .collect();
        let mut this = Self {
            v,
            alloc: VoiceAllocator::new(MAX_VOICES),
            voices,
            matrices,
            free: [
                [Lfo::new(201), Lfo::new(202)],
                [Lfo::new(203), Lfo::new(204)],
            ],
            sr: 48_000.0,
            ctrl_phase: 0,
            mod_wheel: 0.0,
            clock: BeatClock::default(),
            scale: None,
            dirty: true,
            rng: Rng::new(0x3E1D),
            last_key: None,
            note_count: 0,
            limit: false,
        };
        this.refresh();
        this
    }

    #[inline]
    fn f(&self, id: u32) -> f32 {
        self.v[id as usize] as f32
    }

    #[inline]
    fn on(&self, id: u32) -> bool {
        self.v[id as usize] >= 0.5
    }

    #[inline]
    fn index(&self, id: u32) -> usize {
        self.v[id as usize].round().max(0.0) as usize
    }

    fn lfo_settings(
        &self,
        base: u32,
        rate: u32,
        sync: u32,
        sync_rate: u32,
        phase: u32,
        time_mod: f32,
    ) -> LfoSettings {
        let lfo_rate = if self.on(base + sync) {
            LfoRate::Beats(
                SyncRate::from_index(self.v[(base + sync_rate) as usize]).beats()
                    * 2f64.powf(time_mod as f64),
            )
        } else {
            LfoRate::Hz(self.f(base + rate) * (-time_mod).exp2())
        };
        LfoSettings {
            rate: lfo_rate,
            offset: self.f(base + phase) / 360.0,
            ..Default::default()
        }
    }

    fn voice_settings(&self) -> VoiceSettings {
        let mono = self.on(MONO);
        let stack = STACK_COUNTS[self.index(STACKED).min(STACK_COUNTS.len() - 1)];
        VoiceSettings {
            mode: match (mono, self.on(LEGATO)) {
                (true, true) => VoiceMode::Legato,
                (true, false) => VoiceMode::Mono,
                _ => VoiceMode::Poly,
            },
            polyphony: (2 + self.index(VOICES).min(10)).min(12),
            unison: stack,
            glide: GlideMode::Off,
            bend_range: 0.0,
            ..VoiceSettings::default()
        }
    }

    fn refresh(&mut self) {
        let settings = self.voice_settings();
        self.alloc.set(&settings);
        for e in 0..2 {
            for s in 0..SOURCES.len() {
                for (t, multiplicative) in MULTIPLICATIVE.iter().enumerate() {
                    let slot = s * TARGETS.len() + t;
                    let amount = self.v[cell(e, s, t) as usize] as f32 * 0.01;
                    let route = (amount != 0.0).then_some(Route {
                        source: s,
                        dest: t,
                        amount,
                        kind: if *multiplicative {
                            RouteKind::Mul
                        } else {
                            RouteKind::Add
                        },
                        via: None,
                    });
                    self.matrices[e].set_route(slot, route);
                }
            }
        }
        self.limit = self.on(LIMIT);
    }

    fn engine_base(e: usize) -> u32 {
        if e == 0 {
            ENGINE_A
        } else {
            ENGINE_B
        }
    }

    /// Envelopes come from engine A when they're linked.
    fn env_base(&self, e: usize) -> u32 {
        if self.on(LINK_ENV) {
            ENGINE_A
        } else {
            Self::engine_base(e)
        }
    }

    fn envelope_settings(&self, e: usize, modenv: bool, time_scale: f32) -> EnvelopeSettings {
        let b = self.env_base(e);
        let (loop_mode, span) = loop_settings(
            self.index(b + if modenv { ME_LOOP } else { AE_LOOP })
                .min(3),
        );
        let (a, d, s, r, sa, sd, sr) = if modenv {
            (
                ME_ATTACK, ME_DECAY, ME_SUSTAIN, ME_RELEASE, ME_A_SLOPE, ME_D_SLOPE, ME_R_SLOPE,
            )
        } else {
            (
                AE_ATTACK, AE_DECAY, AE_SUSTAIN, AE_RELEASE, AE_A_SLOPE, AE_D_SLOPE, AE_R_SLOPE,
            )
        };
        let mut out = EnvelopeSettings {
            attack: self.f(b + a) * 0.001 * time_scale,
            decay: self.f(b + d) * 0.001 * time_scale,
            sustain: self.f(b + s).clamp(0.0, 1.0),
            release: self.f(b + r) * 0.001 * time_scale,
            attack_slope: self.f(b + sa) * 0.01,
            decay_slope: self.f(b + sd) * 0.01,
            release_slope: self.f(b + sr) * 0.01,
            loop_mode,
            loop_span: span,
            ..EnvelopeSettings::default()
        };
        if modenv {
            out.initial = self.f(b + ME_INITIAL);
            out.peak = self.f(b + ME_PEAK);
            out.end = self.f(b + ME_FINAL);
        }
        out
    }

    fn base_note(&self, e: usize, voice_key: f32) -> f32 {
        let b = Self::engine_base(e);
        if self.on(b + KEY_TRACK) {
            voice_key
        } else {
            match self.active_scale() {
                Some(s) => 60.0 + s.root as f32,
                None => 60.0,
            }
        }
    }

    fn active_scale(&self) -> Option<ScaleInfo> {
        if self.on(USE_SCALE) {
            self.scale
        } else {
            None
        }
    }

    fn start_voice(&mut self, index: usize, note: NoteInfo, overlapped: bool) {
        let velocity = note.velocity.clamp(0.0, 1.0);
        let stacked = note.unison_count > 1;
        let spread_amt = self.f(SPREAD) * 0.01;
        let spread_value = if stacked {
            note.spread
        } else {
            // A held chord fans out: -1, +1, -0.5, +0.5, then back around.
            const FAN: [f32; 6] = [-1.0, 1.0, -0.5, 0.5, -0.25, 0.25];
            let held = self.note_count;
            self.note_count = (self.note_count + 1) % FAN.len();
            FAN[held]
        } * spread_amt;
        let random = self.rng.bipolar();
        let last = self.last_key.unwrap_or(note.key as f32);
        for e in 0..2 {
            let b = Self::engine_base(e);
            let amp = self.envelope_settings(e, false, 1.0);
            let modenv = self.envelope_settings(e, true, 1.0);
            let (reset, spread_on, glide_time) = (
                self.on(b + PHASE_RESET),
                self.on(b + PHASE_SPREAD),
                self.f(b + GLIDE_TIME) * 0.001,
            );
            let lfo1 = self.lfo_settings(b, L1_RATE, L1_SYNC, L1_SYNC_RATE, L1_PHASE, 0.0);
            let lfo2 = self.lfo_settings(b, L2_RATE, L2_SYNC, L2_SYNC_RATE, L2_PHASE, 0.0);
            let (retrig1, retrig2) = (self.on(b + L1_RETRIG), self.on(b + L2_RETRIG));
            let target = self.base_note(e, note.key as f32);
            let start_note = self.base_note(e, last);
            let free = [self.free[e][0].clone(), self.free[e][1].clone()];
            let sr = self.sr;
            let ev = &mut self.voices[index].engines[e];
            ev.amp.set(&amp);
            ev.modenv.set(&modenv);
            ev.amp.gate_on();
            ev.modenv.gate_on();
            if reset {
                ev.osc
                    .reset_phase(if spread_on { spread_value * 0.5 } else { 0.0 });
            }
            ev.lfo1.set(&lfo1);
            ev.lfo2.set(&lfo2);
            if retrig1 {
                ev.lfo1.retrigger();
            } else {
                ev.lfo1.clone_from(&free[0]);
                ev.lfo1.set(&lfo1);
            }
            if retrig2 {
                ev.lfo2.retrigger();
            } else {
                ev.lfo2.clone_from(&free[1]);
                ev.lfo2.set(&lfo2);
            }
            ev.fx = [LfoFx::default(); 2];
            if overlapped && glide_time > 0.0 && start_note != target {
                ev.pitch = start_note;
                ev.target = target;
                ev.pitch_step = (target - start_note).abs() / (glide_time * sr);
            } else {
                ev.pitch = target;
                ev.target = target;
                ev.pitch_step = 0.0;
            }
        }
        let vo = &mut self.voices[index];
        vo.key = note.key as f32;
        vo.velocity = velocity;
        vo.random = random;
        vo.spread = spread_value;
        vo.active = true;
        vo.gated = true;
        vo.fresh = true;
        self.last_key = Some(note.key as f32);
    }

    fn legato(&mut self, index: usize, note: NoteInfo) {
        let key = note.key as f32;
        for e in 0..2 {
            let b = Self::engine_base(e);
            let glide_time = self.f(b + GLIDE_TIME) * 0.001;
            let target = self.base_note(e, key);
            let sr = self.sr;
            let ev = &mut self.voices[index].engines[e];
            ev.target = target;
            ev.pitch_step = if glide_time > 0.0 {
                (target - ev.pitch).abs() / (glide_time * sr)
            } else {
                ev.pitch = target;
                0.0
            };
        }
        self.voices[index].key = key;
        self.last_key = Some(key);
    }

    fn midi(&mut self, message: MidiMessage) {
        if let MidiMessage::ControlChange { cc: 1, value, .. } = message {
            self.mod_wheel = value as f32 / 127.0;
        }
        if matches!(message, MidiMessage::NoteOn { .. }) && self.dirty {
            self.refresh();
            self.dirty = false;
        }
        let overlapped = self
            .alloc
            .voices()
            .iter()
            .any(|v| v.state == crate::dsp::voice::VoiceState::Held);
        let mut events: [Option<VoiceEvent>; MAX_VOICES * 2] = [None; MAX_VOICES * 2];
        let mut count = 0;
        self.alloc.handle(message, |ev| {
            if count < events.len() {
                events[count] = Some(ev);
                count += 1;
            }
        });
        for ev in events.into_iter().take(count).flatten() {
            match ev {
                VoiceEvent::Start { voice, note, .. } => {
                    self.start_voice(voice, note, overlapped);
                    self.voice_control(voice);
                }
                VoiceEvent::Legato { voice, note } => self.legato(voice, note),
                VoiceEvent::Release { voice } => self.voices[voice].release(),
                VoiceEvent::Kill { voice } => self.voices[voice].silence(),
            }
        }
    }

    fn voice_control(&mut self, index: usize) {
        let info = *self.alloc.voice(index);
        let sr = self.sr;
        let frames = CONTROL_INTERVAL as f32;
        let channel_press = self.alloc.channel_pressure(index);
        let bend = self.alloc.channel_bend(index);
        let note_bend = self.alloc.note_bend(index);
        let scale = self.active_scale();
        let use_scale = self.on(USE_SCALE);
        let mw = self.mod_wheel;
        let clock = self.clock;
        let drive_amt = self.f(DRIVE) * 0.01;

        for e in 0..2 {
            let b = Self::engine_base(e);
            let (lfo1_kind, lfo2_kind) = (
                self.index(b + L1_TYPE).min(5),
                self.index(b + L2_TYPE).min(5),
            );
            // Time modulation last tick.
            let bus_prev = (
                self.voices[index].engines[e].bus.offset(T_L1_RATE),
                self.voices[index].engines[e].bus.offset(T_L2_RATE),
                self.voices[index].engines[e].bus.offset(T_AMP_TIME),
                self.voices[index].engines[e].bus.offset(T_MOD_TIME),
            );
            let lfo1_s = self.lfo_settings(
                b,
                L1_RATE,
                L1_SYNC,
                L1_SYNC_RATE,
                L1_PHASE,
                bus_prev.0 * TIME_RANGE_OCT,
            );
            let lfo2_s = self.lfo_settings(
                b,
                L2_RATE,
                L2_SYNC,
                L2_SYNC_RATE,
                L2_PHASE,
                bus_prev.1 * TIME_RANGE_OCT,
            );
            let (l1_m1_base, l1_m2_base) = (self.f(b + L1_M1) * 0.01, self.f(b + L1_M2) * 0.01);
            let l1_fx = [
                self.index(b + L1_FX1).min(17),
                self.index(b + L1_FX2).min(17),
            ];
            let l1_fx_amt = [self.f(b + L1_FX1_AMT) * 0.01, self.f(b + L1_FX2_AMT) * 0.01];
            let amp_scale = (bus_prev.2 * TIME_RANGE_OCT).exp2();
            let mod_scale = (bus_prev.3 * TIME_RANGE_OCT).exp2();
            let amp_env_s = self.envelope_settings(e, false, amp_scale);
            let mod_env_s = self.envelope_settings(e, true, mod_scale);

            let (values, matrices) = (&self.v, &self.matrices);
            let fv = |id: u32| values[id as usize] as f32;
            let iv = |id: u32| values[id as usize].round().max(0.0) as usize;
            let vo = &mut self.voices[index];
            let fresh = vo.fresh;
            let (key, velocity, random, spread) = (vo.key, vo.velocity, vo.random, vo.spread);
            let ev = &mut vo.engines[e];

            // LFO 1.
            ev.lfo1.set(&lfo1_s);
            ev.lfo1.set_clock(&clock);
            let [phase, _] = ev.lfo1.advance_phases(CONTROL_INTERVAL);
            let wrapped = phase < ev.last_phase;
            ev.last_phase = phase;
            if wrapped {
                ev.prev = ev.held;
                ev.held = ev.rng.bipolar();
                ev.alt = -ev.alt;
            }
            let m1 = (l1_m1_base + ev.bus.offset(T_L1_M1)).clamp(0.0, 1.0);
            let m2 = (l1_m2_base + ev.bus.offset(T_L1_M2)).clamp(0.0, 1.0);
            let raw = match lfo1_kind {
                0 => super::meld_osc::morph_shape(
                    phase,
                    0.001,
                    m1,
                    (0.5 - 0.45 * m2).clamp(0.02, 0.98),
                ),
                1 => {
                    let curve = m1 * 2.0 - 1.0;
                    let x = crate::dsp::envelope::slope_curve(phase, curve) * 2.0 - 1.0;
                    let steps = lerp(64.0, 3.0, m2);
                    (x * steps).round() / steps
                }
                2 => {
                    // A smooth random wander, Depth the size of the steps and Smooth the shape.
                    let t = if m2 > 0.0 {
                        (phase / m2.max(0.01)).min(1.0)
                    } else {
                        1.0
                    };
                    (ev.prev + (ev.held - ev.prev) * (t * t * (3.0 - 2.0 * t)))
                        * lerp(1.0, 0.25, 1.0 - m1)
                }
                3 => {
                    (TAU * phase * (1.0 + (m1 * 3.0).round())).sin()
                        * ev.alt.signum()
                        * lerp(1.0, 0.5, m2)
                }
                4 => {
                    let steps = 2.0 + (m1 * 14.0).round();
                    let pulses = (m2 * steps).round();
                    let step = (phase * steps) as u32;
                    if euclid_step(step, steps as u32, pulses as u32) {
                        1.0
                    } else {
                        -1.0
                    }
                }
                _ => {
                    let count = 1.0 + (m1 * 7.0).round();
                    let at = phase * count;
                    let tt = at - at.floor();
                    (-tt * lerp(2.0, 12.0, m2)).exp() * 2.0 - 1.0
                }
            };
            let fx1 = apply_fx(
                l1_fx[0],
                raw,
                (l1_fx_amt[0] + ev.bus.offset(T_L1_FX1)).clamp(0.0, 1.0),
                &mut ev.fx[0],
                &mut ev.rng,
            );
            let fx = apply_fx(
                l1_fx[1],
                fx1,
                (l1_fx_amt[1] + ev.bus.offset(T_L1_FX2)).clamp(0.0, 1.0),
                &mut ev.fx[1],
                &mut ev.rng,
            );

            // LFO 2.
            ev.lfo2.set(&lfo2_s);
            ev.lfo2.set_clock(&clock);
            let [p2, _] = ev.lfo2.advance_phases(CONTROL_INTERVAL);
            let lfo2 = match lfo2_kind {
                0 => (TAU * p2).sin(),
                1 => {
                    if p2 < 0.5 {
                        4.0 * p2 - 1.0
                    } else {
                        3.0 - 4.0 * p2
                    }
                }
                2 => 2.0 * p2 - 1.0,
                3 => 1.0 - 2.0 * p2,
                4 => {
                    if p2 < 0.5 {
                        1.0
                    } else {
                        -1.0
                    }
                }
                _ => ev.held,
            };

            ev.amp.set(&amp_env_s);
            ev.modenv.set(&mod_env_s);

            let mut src = [0.0f32; SOURCES.len()];
            src[S_AMP] = ev.amp.level();
            src[S_MOD] = ev.modenv.level();
            src[S_LFO1] = raw;
            src[S_LFO1FX] = fx;
            src[S_LFO2] = lfo2;
            src[S_VEL] = velocity;
            src[S_PITCH] = (key - 60.0) / 48.0;
            src[S_RANDOM] = random;
            src[S_SPREAD] = spread;
            src[S_PB] = bend;
            src[S_PRESS] = channel_press;
            src[S_MW] = mw;
            src[S_NOTEPB] = note_bend / 48.0;
            src[S_SLIDE] = info.slide;
            src[S_MPEPRESS] = info.pressure;
            matrices[e].evaluate(&src, &mut ev.bus);

            // Glide.
            if ev.pitch_step > 0.0 {
                let move_by = ev.pitch_step * frames;
                let d = ev.target - ev.pitch;
                if d.abs() <= move_by {
                    ev.pitch = ev.target;
                    ev.pitch_step = 0.0;
                } else {
                    ev.pitch += move_by.copysign(d);
                }
            }
            let gliss = values[(b + GLIDE_MODE) as usize] >= 0.5;
            let osc_aware = use_scale && values[(b + OSC_SCALE) as usize] >= 0.5;
            let mut glided = ev.pitch;
            if gliss && ev.pitch_step > 0.0 {
                glided = match scale.filter(|_| osc_aware) {
                    Some(s) => nearest_in_scale(glided, s),
                    None => glided.round(),
                };
            }
            // Transposition, in scale degrees under Use Current Scale.
            let semis = values[(b + E_SEMI) as usize].round() as i32;
            let octave = fv(b + E_OCT) * 12.0;
            let cents = fv(b + E_CENTS) * 0.01;
            let quant = ev.bus.offset(T_QUANT) * QUANT_RANGE;
            let note = match scale {
                Some(s) => {
                    let base = glided.round() as i32;
                    let fraction = glided - base as f32;
                    let degrees = semis + quant.round() as i32;
                    transpose_degrees(base, degrees, s) as f32 + fraction
                }
                None => glided + semis as f32 + quant.round(),
            };
            let pitch = note + octave + cents + ev.bus.offset(T_PITCH) * PITCH_RANGE_ST;
            let kind = iv(b + E_TYPE).min(OSC_TYPES.len() - 1);
            let aware = osc_aware && is_scale_aware(kind);
            let osc_macro =
                |id: u32, t: usize| (fv(b + id) * 0.01 + ev.bus.offset(t)).clamp(0.0, 1.0);
            let macro1 = osc_macro(E_M1, T_M1);
            let macro2 = osc_macro(E_M2, T_M2);
            ev.control = OscControl {
                kind,
                note: pitch,
                hz: note_hz(pitch).clamp(0.1, sr * 0.49),
                sample_rate: sr,
                macro1,
                macro2,
                scale: if aware { scale } else { None },
                song_scale: scale,
                use_current_scale: use_scale,
            };
            ev.osc.control(&ev.control, frames);

            // Filter.
            let ftype = iv(b + F_TYPE).min(FILTER_TYPES.len() - 1);
            let f_aware = use_scale
                && values[(b + FILTER_SCALE) as usize] >= 0.5
                && matches!(ftype, PLATE | MEMBRANE);
            let cutoff = fv(b + F_FREQ) * (ev.bus.offset(T_FREQ) * FREQ_RANGE_ST / 12.0).exp2();
            let f_macro =
                |id: u32, t: usize| (fv(b + id) * 0.01 + ev.bus.offset(t)).clamp(0.0, 1.0);
            let fm1 = f_macro(F_M1, T_FM1);
            let fm2 = f_macro(F_M2, T_FM2);
            ev.filter_on = values[(b + F_ON) as usize] >= 0.5;
            ev.filter.control(
                ftype,
                cutoff,
                fm1,
                fm2,
                sr,
                if f_aware { scale } else { None },
            );

            // Mix.
            let tone = (fv(b + MIX_TONE) + ev.bus.offset(T_TONE)).clamp(-1.0, 1.0);
            ev.tone_mix = tone;
            ev.lp_coef = if tone < 0.0 {
                1.0 - (-TAU * (200.0 * 100f32.powf(1.0 + tone)).min(sr * 0.45) / sr).exp()
            } else {
                1.0
            };
            ev.hp_coef = if tone > 0.0 {
                1.0 - (-TAU * (20.0 * 100f32.powf(tone)).min(sr * 0.45) / sr).exp()
            } else {
                0.0
            };
            let gain = ev
                .bus
                .apply(T_VOLUME, db_to_gain(fv(b + MIX_VOLUME)))
                .max(0.0);
            let on = values[(b + E_ON) as usize] >= 0.5;
            ev.gain_target = if on { gain } else { 0.0 };
            ev.pan_target = pan_gains(fv(b + MIX_PAN) + ev.bus.offset(T_PAN));
            if fresh {
                ev.gain = ev.gain_target;
                ev.pan = ev.pan_target;
            }
        }
        let vo = &mut self.voices[index];
        vo.drive = db_to_gain(drive_amt * DRIVE_MAX_DB);
        vo.fresh = false;
    }

    fn control_tick(&mut self) {
        if self.dirty {
            self.refresh();
            self.dirty = false;
        }
        self.alloc.advance(CONTROL_INTERVAL);
        for e in 0..2 {
            let b = Self::engine_base(e);
            let s1 = self.lfo_settings(b, L1_RATE, L1_SYNC, L1_SYNC_RATE, L1_PHASE, 0.0);
            let s2 = self.lfo_settings(b, L2_RATE, L2_SYNC, L2_SYNC_RATE, L2_PHASE, 0.0);
            let clock = self.clock;
            for (lfo, s) in self.free[e].iter_mut().zip([s1, s2]) {
                lfo.set(&s);
                lfo.set_clock(&clock);
                lfo.advance_phases(CONTROL_INTERVAL);
            }
        }
        self.clock.advance(CONTROL_INTERVAL);
        for i in 0..self.voices.len() {
            if self.voices[i].active {
                self.voice_control(i);
            }
        }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let frames = left.len().min(right.len());
        let on = [self.on(ENGINE_A + E_ON), self.on(ENGINE_B + E_ON)];
        let limit = self.limit;
        let mut pos = 0;
        while pos < frames {
            if self.ctrl_phase == 0 {
                self.control_tick();
            }
            let n = (CONTROL_INTERVAL - self.ctrl_phase).min(frames - pos);
            for v in 0..self.voices.len() {
                let vo = &mut self.voices[v];
                if !vo.active {
                    continue;
                }
                if render_voice(
                    vo,
                    on,
                    limit,
                    &mut left[pos..pos + n],
                    &mut right[pos..pos + n],
                ) {
                    vo.active = false;
                    self.alloc.finished(v);
                }
            }
            self.ctrl_phase = (self.ctrl_phase + n) % CONTROL_INTERVAL;
            pos += n;
        }
    }
}

fn render_voice(
    v: &mut Voice,
    on: [bool; 2],
    limit: bool,
    left: &mut [f32],
    right: &mut [f32],
) -> bool {
    let drive = v.drive;
    let driven = drive > 1.001;
    for (l, r) in left.iter_mut().zip(right.iter_mut()) {
        let (mut sum_l, mut sum_r) = (0.0f32, 0.0f32);
        for (i, e) in v.engines.iter_mut().enumerate() {
            let env = e.amp.next();
            e.modenv.next();
            if !on[i] {
                continue;
            }
            let x = e.osc.next(&e.control);
            let mut y = if e.filter_on { e.filter.process(x) } else { x };
            if e.tone_mix < 0.0 {
                e.tone_lp += e.lp_coef * (y - e.tone_lp);
                y = e.tone_lp;
            } else if e.tone_mix > 0.0 {
                e.tone_hp += e.hp_coef * (y - e.tone_hp);
                y -= e.tone_hp;
            }
            e.gain += (e.gain_target - e.gain) * 0.02;
            e.pan.0 += (e.pan_target.0 - e.pan.0) * 0.02;
            e.pan.1 += (e.pan_target.1 - e.pan.1) * 0.02;
            let out = y * env * e.gain * VOICE_GAIN;
            sum_l += out * e.pan.0;
            sum_r += out * e.pan.1;
        }
        if driven {
            sum_l = soft_clip(sum_l * drive);
            sum_r = soft_clip(sum_r * drive);
        }
        if limit {
            sum_l = sum_l.tanh();
            sum_r = sum_r.tanh();
        }
        *l += sanitize(sum_l);
        *r += sanitize(sum_r);
    }
    (!on[0] || !v.engines[0].amp.is_active()) && (!on[1] || !v.engines[1].amp.is_active())
}

impl Processor for Meld {
    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sr = sample_rate as f32;
        self.alloc.prepare(sample_rate);
        for v in &mut self.voices {
            for e in &mut v.engines {
                e.amp.prepare(sample_rate);
                e.modenv.prepare(sample_rate);
                e.filter.prepare(sample_rate);
                e.lfo1.prepare(sample_rate);
                e.lfo2.prepare(sample_rate);
            }
        }
        for row in &mut self.free {
            for l in row {
                l.prepare(sample_rate);
            }
        }
        self.reset();
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        self.clock = BeatClock::from_transport(ctx.transport);
        self.scale = ctx.transport.scale;
        let this = &mut *self;
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => {
                if let Some(v) = this.v.get_mut(id as usize) {
                    *v = value;
                    this.dirty = true;
                }
            }
            BlockStep::Midi(message) => this.midi(message),
            BlockStep::Audio(audio) => this.render(audio.left, audio.right),
        });
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
        }
        self.dirty = true;
    }

    fn reset(&mut self) {
        self.alloc.reset();
        self.voices.iter_mut().for_each(Voice::silence);
        self.ctrl_phase = 0;
        self.last_key = None;
        self.note_count = 0;
    }

    fn tail(&self) -> Tail {
        let release = self
            .f(ENGINE_A + AE_RELEASE)
            .max(self.f(ENGINE_B + AE_RELEASE))
            * 0.001;
        Tail::Frames((release * self.sr) as u32 + 1)
    }
}

/// The names of the macros currently on an engine, for the panel.
pub fn macro_labels(
    osc: usize,
    filter: usize,
) -> ((&'static str, &'static str), (&'static str, &'static str)) {
    (
        OSC_MACROS[osc.min(OSC_MACROS.len() - 1)],
        FILTER_MACROS[filter.min(FILTER_MACROS.len() - 1)],
    )
}
