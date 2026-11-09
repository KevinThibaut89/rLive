//! Wavetable (manual pp. 814-823): two wavetable oscillators with effects, a sub, two filters
//! with routing, three envelopes, two LFOs, a modulation matrix and six unison modes.
//!
//! The tables are original: made in code by `dsp::wavetable`, or loaded from your own WAV or AIFF
//! with the same clean-up or Raw mode the manual describes. The manual gives no ranges for times
//! or modulation depth, no list of unison voice counts and no frame size for imports, so those
//! are ours; so is the ordering of its filter circuits on the shared menu.

use super::common::{note_hz, pan_gains, saturated, CONTROL_INTERVAL};
use crate::dsp::envelope::{Envelope, EnvelopeSettings, LoopMode, LoopSpan};
use crate::dsp::filter::{Circuit, Filter, FilterKind, FilterSettings, FilterSlope};
use crate::dsp::lfo::{Lfo, LfoRate, LfoSettings};
use crate::dsp::modmatrix::{ModBus, ModMatrix, Route, RouteKind};
use crate::dsp::tempo::{BeatClock, SyncRate};
use crate::dsp::util::{db_to_gain, lerp, sanitize, Rng};
use crate::dsp::voice::{
    GlideCurve, GlideMode, NoteInfo, VoiceAllocator, VoiceEvent, VoiceMode, VoiceSettings,
};
use crate::dsp::wavetable::{generate_wavetable, TableKind, Wavetable, WavetableBuilder};
use crate::{BuiltinInfo, DeviceInit, DeviceRole};
use rlive_core::{
    BlockStep, DeviceDesc, Item, MidiMessage, ParamId, ParamInfo, ProcessContext, Processor,
    Resource, SampleBuffer, SampleSlot, Section, Status, Tail, ViewId,
};
use std::f32::consts::{PI, TAU};
use std::sync::{Arc, OnceLock};

// Oscillator block.
pub const O_ON: u32 = 0;
pub const O_GAIN: u32 = 1;
pub const O_PAN: u32 = 2;
pub const O_SEMI: u32 = 3;
pub const O_DETUNE: u32 = 4;
pub const O_CATEGORY: u32 = 5;
pub const O_TABLE: u32 = 6;
pub const O_RAW: u32 = 7;
pub const O_POSITION: u32 = 8;
pub const O_FX: u32 = 9;
pub const O_FX1: u32 = 10;
pub const O_FX2: u32 = 11;
const OSC_BLOCK: u32 = 12;
pub const OSC1: u32 = 0;
pub const OSC2: u32 = OSC_BLOCK;

pub const SUB_ON: u32 = OSC2 + OSC_BLOCK;
pub const SUB_GAIN: u32 = SUB_ON + 1;
pub const SUB_TONE: u32 = SUB_ON + 2;
pub const SUB_OCT: u32 = SUB_ON + 3;

// Filter block.
pub const F_ON: u32 = 0;
pub const F_TYPE: u32 = 1;
pub const F_SLOPE: u32 = 2;
pub const F_CIRCUIT: u32 = 3;
pub const F_FREQ: u32 = 4;
pub const F_RES: u32 = 5;
pub const F_DRIVE: u32 = 6;
pub const F_MORPH: u32 = 7;
const FILTER_BLOCK: u32 = 8;
pub const FILTER1: u32 = SUB_OCT + 1;
pub const FILTER2: u32 = FILTER1 + FILTER_BLOCK;
pub const ROUTING: u32 = FILTER2 + FILTER_BLOCK;

pub const MOD_TIME: u32 = ROUTING + 1;
pub const MOD_AMOUNT: u32 = ROUTING + 2;

// Envelope block.
pub const E_ATTACK: u32 = 0;
pub const E_DECAY: u32 = 1;
pub const E_SUSTAIN: u32 = 2;
pub const E_RELEASE: u32 = 3;
pub const E_A_SLOPE: u32 = 4;
pub const E_D_SLOPE: u32 = 5;
pub const E_R_SLOPE: u32 = 6;
pub const E_LOOP: u32 = 7;
pub const E_INITIAL: u32 = 8;
pub const E_PEAK: u32 = 9;
pub const E_FINAL: u32 = 10;
const ENV_BLOCK: u32 = 11;
pub const AMP_ENV: u32 = MOD_AMOUNT + 1;
pub const ENV2: u32 = AMP_ENV + ENV_BLOCK;
pub const ENV3: u32 = ENV2 + ENV_BLOCK;

// LFO block.
pub const L_WAVE: u32 = 0;
pub const L_SHAPE: u32 = 1;
pub const L_SYNC: u32 = 2;
pub const L_RATE: u32 = 3;
pub const L_SYNC_RATE: u32 = 4;
pub const L_AMOUNT: u32 = 5;
pub const L_OFFSET: u32 = 6;
pub const L_ATTACK: u32 = 7;
pub const L_RETRIG: u32 = 8;
const LFO_BLOCK: u32 = 9;
pub const LFO1: u32 = ENV3 + ENV_BLOCK;
pub const LFO2: u32 = LFO1 + LFO_BLOCK;

pub const TRANSPOSE: u32 = LFO2 + LFO_BLOCK;
pub const VOLUME: u32 = TRANSPOSE + 1;
pub const MONO: u32 = TRANSPOSE + 2;
pub const VOICES: u32 = TRANSPOSE + 3;
pub const GLIDE: u32 = TRANSPOSE + 4;
pub const UNISON: u32 = TRANSPOSE + 5;
pub const UNISON_VOICES: u32 = TRANSPOSE + 6;
pub const UNISON_AMOUNT: u32 = TRANSPOSE + 7;
pub const HI_QUALITY: u32 = TRANSPOSE + 8;
pub const MATRIX_BASE: u32 = TRANSPOSE + 9;

pub const SOURCES: [&str; 12] = [
    "Amp",
    "Env 2",
    "Env 3",
    "LFO 1",
    "LFO 2",
    "Velocity",
    "Note",
    "Pitch Bend",
    "Aftertouch",
    "Mod Wheel",
    "Random",
    "Slide",
];
pub const TARGETS: [&str; 28] = [
    "Osc 1 Pitch",
    "Osc 1 Position",
    "Osc 1 Effect 1",
    "Osc 1 Effect 2",
    "Osc 1 Gain",
    "Osc 1 Pan",
    "Osc 2 Pitch",
    "Osc 2 Position",
    "Osc 2 Effect 1",
    "Osc 2 Effect 2",
    "Osc 2 Gain",
    "Osc 2 Pan",
    "Sub Gain",
    "Sub Tone",
    "Filter 1 Freq",
    "Filter 1 Res",
    "Filter 2 Freq",
    "Filter 2 Res",
    "Volume",
    "Unison Amount",
    "Mod Amount",
    "Mod Time",
    "Env Sustain",
    "Env Initial",
    "LFO 1 Amount",
    "LFO 1 Rate",
    "LFO 2 Amount",
    "LFO 2 Rate",
];
const S_AMP: usize = 0;
const S_E2: usize = 1;
const S_E3: usize = 2;
const S_L1: usize = 3;
const S_L2: usize = 4;
const S_VEL: usize = 5;
const S_NOTE: usize = 6;
const S_PB: usize = 7;
const S_AT: usize = 8;
const S_MW: usize = 9;
const S_RANDOM: usize = 10;
const S_SLIDE: usize = 11;
const T_PITCH: [usize; 2] = [0, 6];
const T_POS: [usize; 2] = [1, 7];
const T_FX1: [usize; 2] = [2, 8];
const T_FX2: [usize; 2] = [3, 9];
const T_GAIN: [usize; 2] = [4, 10];
const T_PAN: [usize; 2] = [5, 11];
const T_SUB_GAIN: usize = 12;
const T_SUB_TONE: usize = 13;
const T_F_FREQ: [usize; 2] = [14, 16];
const T_F_RES: [usize; 2] = [15, 17];
const T_VOLUME: usize = 18;
const T_UNI: usize = 19;
const T_AMOUNT: usize = 20;
const T_TIME: usize = 21;
const T_SUSTAIN: usize = 22;
const T_INITIAL: usize = 23;
const T_LFO_AMT: [usize; 2] = [24, 26];
const T_LFO_RATE: [usize; 2] = [25, 27];

/// Multiplicative targets, as the manual marks them.
const MULTIPLICATIVE: [usize; 7] = [
    T_VOLUME,
    T_UNI,
    T_AMOUNT,
    T_SUSTAIN,
    T_INITIAL,
    T_LFO_AMT[0],
    T_LFO_AMT[1],
];

const CELLS: usize = SOURCES.len() * TARGETS.len();
pub const PARAM_COUNT: usize = MATRIX_BASE as usize + CELLS;

/// The id of one matrix cell.
pub const fn cell(source: usize, target: usize) -> u32 {
    MATRIX_BASE + (source * TARGETS.len() + target) as u32
}

pub const FX_TYPES: [&str; 4] = ["None", "FM", "Classic", "Modern"];
pub const FX_LABELS: [(&str, &str); 4] = [
    ("-", "-"),
    ("Amt", "Tune"),
    ("PW", "Sync"),
    ("Warp", "Fold"),
];
pub const FILTER_TYPES: [&str; 5] = ["Low-pass", "High-pass", "Band-pass", "Notch", "Morph"];
pub const CIRCUITS: [&str; 5] = ["Clean", "OSR", "MS2", "SMP", "PRD"];
pub const ROUTINGS: [&str; 3] = ["Serial", "Parallel", "Split"];
pub const ENV_LOOPS: [&str; 3] = ["None", "Trigger", "Loop"];
pub const LFO_WAVES: [&str; 5] = ["Sine", "Saw", "Triangle", "Square", "Random"];
pub const UNISON_MODES: [&str; 7] = [
    "None",
    "Classic",
    "Shimmer",
    "Noise",
    "Phase Sync",
    "Position Spread",
    "Random Note",
];

/// `(category, table names)`. The last category is the oscillator's loaded file.
pub const CATEGORIES: [(&str, &[&str]); 4] = [
    ("Basics", &["Basic Shapes", "Pulse Width", "Harmonics"]),
    (
        "Digital",
        &[
            "FM",
            "Spectral 1",
            "Spectral 2",
            "Spectral 3",
            "Spectral 4",
            "Spectral 5",
            "Spectral 6",
        ],
    ),
    ("Vocal", &["Formants"]),
    ("User", &["Loaded File"]),
];
const USER_CATEGORY: usize = 3;

const FRAME_LEN: usize = 2048;
/// The most frames an imported file can have; longer files are sampled across their length.
pub const MAX_USER_FRAMES: usize = 128;

/// The view: where each oscillator is, then their waveforms.
pub const VIEW: ViewId = ViewId(0);
pub const VIEW_FRAMES: usize = 12;
pub const VIEW_POINTS: usize = 48;
pub const VIEW_LEN: usize = 2 + 2 * VIEW_FRAMES * VIEW_POINTS;
const VIEW_EVERY: u32 = 6;

fn osc_params(out: &mut Vec<ParamInfo>, base: u32, n: usize) {
    let name = |s: &str| format!("Osc {n} {s}");
    out.push(ParamInfo::toggle(base + O_ON, &format!("Osc {n}"), n == 1));
    out.push(ParamInfo::continuous(
        base + O_GAIN,
        &name("Gain"),
        0.0,
        1.0,
        0.7,
        "",
    ));
    out.push(ParamInfo::continuous(
        base + O_PAN,
        &name("Pan"),
        -1.0,
        1.0,
        0.0,
        "",
    ));
    out.push(ParamInfo::continuous(
        base + O_SEMI,
        &name("Semi"),
        -24.0,
        24.0,
        0.0,
        "st",
    ));
    out.push(ParamInfo::continuous(
        base + O_DETUNE,
        &name("Detune"),
        -50.0,
        50.0,
        0.0,
        "ct",
    ));
    out.push(ParamInfo::stepped(
        base + O_CATEGORY,
        &name("Category"),
        &["Basics", "Digital", "Vocal", "User"],
        0,
    ));
    out.push(ParamInfo::stepped(
        base + O_TABLE,
        &name("Table"),
        &["1", "2", "3", "4", "5", "6", "7", "8", "9", "10"],
        0,
    ));
    out.push(ParamInfo::toggle(base + O_RAW, &name("Raw"), false));
    out.push(ParamInfo::continuous(
        base + O_POSITION,
        &name("Position"),
        0.0,
        1.0,
        0.0,
        "",
    ));
    out.push(ParamInfo::stepped(
        base + O_FX,
        &name("Effect"),
        &FX_TYPES,
        0,
    ));
    out.push(ParamInfo::continuous(
        base + O_FX1,
        &name("Effect 1"),
        -100.0,
        100.0,
        0.0,
        "%",
    ));
    out.push(ParamInfo::continuous(
        base + O_FX2,
        &name("Effect 2"),
        -100.0,
        100.0,
        0.0,
        "%",
    ));
}

fn filter_params(out: &mut Vec<ParamInfo>, base: u32, n: usize) {
    let name = |s: &str| format!("Filter {n} {s}");
    out.push(ParamInfo::toggle(
        base + F_ON,
        &format!("Filter {n}"),
        n == 1,
    ));
    out.push(ParamInfo::stepped(
        base + F_TYPE,
        &name("Type"),
        &FILTER_TYPES,
        0,
    ));
    out.push(ParamInfo::stepped(
        base + F_SLOPE,
        &name("Slope"),
        &["12 dB", "24 dB"],
        0,
    ));
    out.push(ParamInfo::stepped(
        base + F_CIRCUIT,
        &name("Circuit"),
        &CIRCUITS,
        0,
    ));
    out.push(ParamInfo::skewed(
        base + F_FREQ,
        &name("Freq"),
        20.0,
        20_000.0,
        8_000.0,
        "Hz",
        2.5,
    ));
    out.push(ParamInfo::continuous(
        base + F_RES,
        &name("Res"),
        0.0,
        1.0,
        0.2,
        "",
    ));
    out.push(ParamInfo::continuous(
        base + F_DRIVE,
        &name("Drive"),
        0.0,
        24.0,
        0.0,
        "dB",
    ));
    out.push(ParamInfo::continuous(
        base + F_MORPH,
        &name("Morph"),
        0.0,
        1.0,
        0.0,
        "",
    ));
}

fn env_params(out: &mut Vec<ParamInfo>, base: u32, label: &str, levels: bool) {
    let name = |s: &str| format!("{label} {s}");
    let time =
        |id, s: &str, def| ParamInfo::skewed(base + id, &name(s), 0.5, 20_000.0, def, "ms", 4.0);
    let slope = |id, s: &str| ParamInfo::continuous(base + id, &name(s), -100.0, 100.0, 0.0, "%");
    out.push(time(E_ATTACK, "Attack", 2.0));
    out.push(time(E_DECAY, "Decay", 300.0));
    out.push(ParamInfo::continuous(
        base + E_SUSTAIN,
        &name("Sustain"),
        0.0,
        1.0,
        0.8,
        "",
    ));
    out.push(time(E_RELEASE, "Release", 150.0));
    out.push(slope(E_A_SLOPE, "Attack Slope"));
    out.push(slope(E_D_SLOPE, "Decay Slope"));
    out.push(slope(E_R_SLOPE, "Release Slope"));
    out.push(ParamInfo::stepped(
        base + E_LOOP,
        &name("Loop"),
        &ENV_LOOPS,
        0,
    ));
    // The amp envelope has no levels but keeps the ids, so every block has the same shape.
    if levels {
        out.push(ParamInfo::continuous(
            base + E_INITIAL,
            &name("Initial"),
            0.0,
            1.0,
            0.0,
            "",
        ));
        out.push(ParamInfo::continuous(
            base + E_PEAK,
            &name("Peak"),
            0.0,
            1.0,
            1.0,
            "",
        ));
        out.push(ParamInfo::continuous(
            base + E_FINAL,
            &name("Final"),
            0.0,
            1.0,
            0.0,
            "",
        ));
    }
}

fn lfo_params(out: &mut Vec<ParamInfo>, base: u32, n: usize) {
    let name = |s: &str| format!("LFO {n} {s}");
    out.push(ParamInfo::stepped(
        base + L_WAVE,
        &name("Wave"),
        &LFO_WAVES,
        0,
    ));
    out.push(ParamInfo::continuous(
        base + L_SHAPE,
        &name("Shape"),
        -1.0,
        1.0,
        0.0,
        "",
    ));
    out.push(ParamInfo::toggle(base + L_SYNC, &name("Sync"), false));
    out.push(ParamInfo::skewed(
        base + L_RATE,
        &name("Rate"),
        0.01,
        40.0,
        2.0,
        "Hz",
        3.0,
    ));
    out.push(ParamInfo::stepped(
        base + L_SYNC_RATE,
        &name("Beat Rate"),
        &SyncRate::LABELS,
        12,
    ));
    out.push(ParamInfo::continuous(
        base + L_AMOUNT,
        &name("Amount"),
        0.0,
        1.0,
        1.0,
        "",
    ));
    out.push(ParamInfo::continuous(
        base + L_OFFSET,
        &name("Offset"),
        0.0,
        360.0,
        0.0,
        "°",
    ));
    out.push(ParamInfo::skewed(
        base + L_ATTACK,
        &name("Attack"),
        0.0,
        5_000.0,
        0.0,
        "ms",
        3.0,
    ));
    out.push(ParamInfo::toggle(base + L_RETRIG, &name("Retrigger"), true));
}

fn cell_default(source: usize, target: usize) -> f64 {
    if source == S_PB && (target == T_PITCH[0] || target == T_PITCH[1]) {
        2.0 / 48.0 * 100.0
    } else {
        0.0
    }
}

pub fn params() -> Vec<ParamInfo> {
    let mut out = Vec::with_capacity(PARAM_COUNT);
    osc_params(&mut out, OSC1, 1);
    osc_params(&mut out, OSC2, 2);
    out.push(ParamInfo::toggle(SUB_ON, "Sub", false));
    out.push(ParamInfo::continuous(
        SUB_GAIN, "Sub Gain", 0.0, 1.0, 0.5, "",
    ));
    out.push(ParamInfo::continuous(
        SUB_TONE, "Sub Tone", 0.0, 100.0, 0.0, "%",
    ));
    out.push(ParamInfo::stepped(
        SUB_OCT,
        "Sub Octave",
        &["0", "-1", "-2"],
        1,
    ));
    filter_params(&mut out, FILTER1, 1);
    filter_params(&mut out, FILTER2, 2);
    out.push(ParamInfo::stepped(ROUTING, "Routing", &ROUTINGS, 0));
    out.push(ParamInfo::continuous(MOD_TIME, "Time", -1.0, 1.0, 0.0, ""));
    out.push(ParamInfo::continuous(
        MOD_AMOUNT, "Amount", 0.0, 1.0, 1.0, "",
    ));
    env_params(&mut out, AMP_ENV, "Amp", false);
    env_params(&mut out, ENV2, "Env 2", true);
    env_params(&mut out, ENV3, "Env 3", true);
    lfo_params(&mut out, LFO1, 1);
    lfo_params(&mut out, LFO2, 2);
    out.extend([
        ParamInfo::continuous(TRANSPOSE, "Transpose", -48.0, 48.0, 0.0, "st"),
        ParamInfo::continuous(VOLUME, "Volume", -48.0, 12.0, 0.0, "dB"),
        ParamInfo::toggle(MONO, "Mono", false),
        ParamInfo::stepped(
            VOICES,
            "Poly Voices",
            &["2", "3", "4", "5", "6", "7", "8"],
            6,
        ),
        ParamInfo::skewed(GLIDE, "Glide", 0.0, 5_000.0, 0.0, "ms", 3.0),
        ParamInfo::stepped(UNISON, "Unison", &UNISON_MODES, 0),
        ParamInfo::stepped(
            UNISON_VOICES,
            "Unison Voices",
            &["2", "3", "4", "5", "6", "7", "8"],
            2,
        ),
        ParamInfo::continuous(UNISON_AMOUNT, "Unison Amount", 0.0, 1.0, 0.3, ""),
        ParamInfo::toggle(HI_QUALITY, "Hi-Quality", false),
    ]);
    for (s, source) in SOURCES.iter().enumerate() {
        for (t, target) in TARGETS.iter().enumerate() {
            out.push(ParamInfo::continuous(
                cell(s, t),
                &format!("{source} > {target}"),
                -100.0,
                100.0,
                cell_default(s, t),
                "%",
            ));
        }
    }
    out
}

fn layout() -> Vec<Section> {
    let knob = |id| Item::Knob(ParamId(id));
    let menu = |id| Item::Menu(ParamId(id));
    let toggle = |id| Item::Toggle(ParamId(id));
    let osc = |b: u32, title: &'static str| Section {
        title,
        items: vec![
            Item::Row(vec![
                toggle(b + O_ON),
                menu(b + O_CATEGORY),
                menu(b + O_TABLE),
                toggle(b + O_RAW),
            ]),
            Item::Row(vec![
                knob(b + O_POSITION),
                knob(b + O_GAIN),
                knob(b + O_PAN),
            ]),
            Item::Row(vec![knob(b + O_SEMI), knob(b + O_DETUNE)]),
            Item::Row(vec![menu(b + O_FX), knob(b + O_FX1), knob(b + O_FX2)]),
        ],
    };
    let fil = |b: u32, title: &'static str| Section {
        title,
        items: vec![
            Item::Row(vec![
                toggle(b + F_ON),
                menu(b + F_TYPE),
                menu(b + F_SLOPE),
                menu(b + F_CIRCUIT),
            ]),
            Item::Row(vec![
                knob(b + F_FREQ),
                knob(b + F_RES),
                knob(b + F_DRIVE),
                knob(b + F_MORPH),
            ]),
        ],
    };
    let env = |b: u32, title: &'static str, levels: bool| {
        let mut items = vec![
            Item::Row(vec![
                knob(b + E_ATTACK),
                knob(b + E_DECAY),
                knob(b + E_SUSTAIN),
                knob(b + E_RELEASE),
            ]),
            Item::Row(vec![
                knob(b + E_A_SLOPE),
                knob(b + E_D_SLOPE),
                knob(b + E_R_SLOPE),
                menu(b + E_LOOP),
            ]),
        ];
        if levels {
            items.push(Item::Row(vec![
                knob(b + E_INITIAL),
                knob(b + E_PEAK),
                knob(b + E_FINAL),
            ]));
        }
        Section { title, items }
    };
    let lfo = |b: u32, title: &'static str| Section {
        title,
        items: vec![
            Item::Row(vec![
                menu(b + L_WAVE),
                knob(b + L_SHAPE),
                toggle(b + L_SYNC),
                knob(b + L_RATE),
                menu(b + L_SYNC_RATE),
            ]),
            Item::Row(vec![
                knob(b + L_AMOUNT),
                knob(b + L_OFFSET),
                knob(b + L_ATTACK),
                toggle(b + L_RETRIG),
            ]),
        ],
    };
    vec![
        Section {
            title: "Tables",
            items: vec![Item::Panel("wavetable_view")],
        },
        osc(OSC1, "Osc 1"),
        osc(OSC2, "Osc 2"),
        Section {
            title: "Sub",
            items: vec![Item::Row(vec![
                toggle(SUB_ON),
                knob(SUB_GAIN),
                knob(SUB_TONE),
                menu(SUB_OCT),
            ])],
        },
        fil(FILTER1, "Filter 1"),
        fil(FILTER2, "Filter 2"),
        Section {
            title: "Matrix",
            items: vec![
                Item::Panel("wavetable_matrix"),
                Item::Row(vec![menu(ROUTING), knob(MOD_TIME), knob(MOD_AMOUNT)]),
            ],
        },
        env(AMP_ENV, "Amp", false),
        env(ENV2, "Env 2", true),
        env(ENV3, "Env 3", true),
        lfo(LFO1, "LFO 1"),
        lfo(LFO2, "LFO 2"),
        Section {
            title: "Global",
            items: vec![
                Item::Row(vec![
                    knob(TRANSPOSE),
                    knob(VOLUME),
                    toggle(MONO),
                    menu(VOICES),
                    knob(GLIDE),
                ]),
                Item::Row(vec![
                    menu(UNISON),
                    menu(UNISON_VOICES),
                    knob(UNISON_AMOUNT),
                    toggle(HI_QUALITY),
                ]),
            ],
        },
    ]
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.wavetable",
    name: "Wavetable",
    role: DeviceRole::Instrument,
    category: "Synths",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        samples: vec![
            SampleSlot {
                name: "Osc 1 Table",
            },
            SampleSlot {
                name: "Osc 2 Table",
            },
        ],
        ..Default::default()
    },
    create: |init: &DeviceInit| Box::new(WavetableSynth::new(init)),
};

// ------------------------------------------------------------------- generated tables

/// One generated table for each `(category, table)` entry in [`CATEGORIES`].
fn generated(category: usize, table: usize) -> &'static Wavetable {
    static TABLES: OnceLock<Vec<Vec<Wavetable>>> = OnceLock::new();
    let all = TABLES.get_or_init(|| {
        let make = |kind, frames, seed| generate_wavetable(kind, frames, seed);
        let mut digital = vec![make(TableKind::Fm, 32, 0)];
        digital.extend((0..6).map(|i| make(TableKind::Spectral, 16, 100 + i * 37)));
        vec![
            vec![
                make(TableKind::BasicShapes, 32, 0),
                make(TableKind::PulseWidth, 32, 0),
                make(TableKind::Harmonics, 16, 0),
            ],
            digital,
            vec![make(TableKind::Formants, 16, 0)],
        ]
    });
    let cat = all.get(category).unwrap_or(&all[0]);
    cat.get(table).unwrap_or(&cat[0])
}

/// Builds every generated table now, so nothing is first built on the audio thread.
pub fn prebuild_tables() {
    generated(0, 0);
}

// ------------------------------------------------------------------------ import

/// How a sample is cut into frames: `(samples per frame, frame count)`.
pub fn frame_layout(len: usize) -> (usize, usize) {
    if len == 0 {
        return (FRAME_LEN, 0);
    }
    for size in [FRAME_LEN, 1024, 512, 256] {
        if len >= size && len.is_multiple_of(size) {
            return (size, (len / size).min(MAX_USER_FRAMES));
        }
    }
    (FRAME_LEN, (len / FRAME_LEN).clamp(1, MAX_USER_FRAMES))
}

/// Writes frame `index` of a file `frames` frames long into `out`, a [`FRAME_LEN`]-sample cycle,
/// mono. With `raw` the samples are only resampled; otherwise DC and the seam at the loop point
/// are removed and the frame is levelled, so cuts between arbitrary points don't buzz.
pub fn read_frame(
    sample: &SampleBuffer,
    frames: usize,
    size: usize,
    index: usize,
    raw: bool,
    out: &mut [f32],
) {
    let len = sample.len();
    let spaced = (index * (len / size).max(1))
        .checked_div(frames)
        .unwrap_or(0);
    let start = spaced * size;
    for (i, o) in out.iter_mut().enumerate() {
        let at = start as f64 + i as f64 * size as f64 / FRAME_LEN as f64;
        let i0 = at as usize;
        let t = (at - i0 as f64) as f32;
        let get = |n: usize, buf: &[f32]| buf.get(n.min(start + size - 1)).copied().unwrap_or(0.0);
        let mono = |n: usize| 0.5 * (get(n, &sample.left) + get(n, &sample.right));
        *o = lerp(mono(i0), mono(i0 + 1), t);
    }
    if raw {
        return;
    }
    let mean = out.iter().sum::<f32>() / out.len() as f32;
    for s in out.iter_mut() {
        *s -= mean;
    }
    // A short crossfade across the loop point makes the cycle join up.
    let fade = (out.len() / 32).max(1);
    for i in 0..fade {
        let t = (i + 1) as f32 / (fade + 1) as f32;
        let (head, tail) = (out[i], out[out.len() - 1 - i]);
        let mid = 0.5 * (head + tail);
        out[i] = lerp(mid, head, t);
        let n = out.len() - 1 - i;
        out[n] = lerp(mid, tail, t);
    }
    let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if peak > 1e-6 {
        for s in out.iter_mut() {
            *s /= peak;
        }
    }
}

/// Importing a file a frame at a time, within what a block may cost.
#[derive(Clone, Copy)]
struct Build {
    slot: usize,
    next: usize,
    frames: usize,
    size: usize,
    raw: bool,
    /// Tell the host the category is now User.
    announce: bool,
}

const BUILD_FRAMES_PER_BLOCK: usize = 2;

// ------------------------------------------------------------------------- voices

const MAX_UNISON: usize = 8;

#[derive(Clone, Copy, Default)]
struct UniVoice {
    phase: f32,
    mod_phase: f32,
    ratio: f32,
    pos: f32,
    pan_l: f32,
    pan_r: f32,
    jitter: f32,
    jitter_target: f32,
    timer: f32,
}

#[derive(Clone, Copy, Default)]
struct OscCtl {
    on: bool,
    fx: u8,
    fx1: f32,
    fx2: f32,
    position: f32,
    dt: f32,
    gain: f32,
    pan: (f32, f32),
    voices: usize,
    /// The table is in a family that has real band limits; loads through this index.
    table: (u8, u8),
}

#[derive(Clone)]
struct LfoUnit {
    lfo: Lfo,
    rng: Rng,
    held: f32,
    last: f32,
    fade: f32,
    fade_step: f32,
    value: f32,
}

impl LfoUnit {
    fn new(seed: u32) -> Self {
        Self {
            lfo: Lfo::new(seed),
            rng: Rng::new(seed.wrapping_mul(3)),
            held: 0.0,
            last: 0.0,
            fade: 1.0,
            fade_step: 0.0,
            value: 0.0,
        }
    }
}

/// An LFO's value at `p`. `shape` is -1..1. Sine and Saw bend, Triangle moves its peak, Square
/// its width and Random how far it reaches.
fn lfo_wave(wave: u8, p: f32, shape: f32, held: f32) -> f32 {
    let bend = |x: f32| {
        let k = shape * 3.0;
        if k.abs() < 1e-3 {
            x
        } else {
            x.signum() * x.abs().powf(k.exp())
        }
    };
    match wave {
        0 => bend((TAU * p).sin()),
        1 => bend(2.0 * p - 1.0),
        2 => {
            let rise = (0.5 + 0.5 * shape).clamp(0.01, 0.99);
            if p < rise {
                -1.0 + 2.0 * p / rise
            } else {
                1.0 - 2.0 * (p - rise) / (1.0 - rise)
            }
        }
        3 => {
            if p < (0.5 + 0.45 * shape).clamp(0.02, 0.98) {
                1.0
            } else {
                -1.0
            }
        }
        _ => bend(held),
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
    bus: ModBus,
    amp: Envelope,
    env2: Envelope,
    env3: Envelope,
    env2_looped: bool,
    env3_looped: bool,
    lfo: [LfoUnit; 2],
    unison: [[UniVoice; MAX_UNISON]; 2],
    osc: [OscCtl; 2],
    sub_on: bool,
    sub_phase: f32,
    sub_dt: f32,
    sub_gain: f32,
    sub_tone: f32,
    f: [[Filter; 2]; 2],
    f_on: [bool; 2],
    routing: u8,
    gain: f32,
    gain_target: f32,
    rng: Rng,
}

impl Voice {
    fn silence(&mut self) {
        self.active = false;
        self.gated = false;
        self.fresh = false;
        self.amp.kill();
        self.env2.kill();
        self.env3.kill();
        for pair in self.f.iter_mut() {
            pair.iter_mut().for_each(Filter::reset);
        }
        self.gain = 0.0;
    }

    fn release(&mut self) {
        self.gated = false;
        self.amp.gate_off();
        if !self.env2_looped {
            self.env2.gate_off();
        }
        if !self.env3_looped {
            self.env3.gate_off();
        }
    }
}

const MAX_VOICES: usize = 8;
const PITCH_RANGE_ST: f32 = 48.0;
const FREQ_RANGE_ST: f32 = 48.0;
const FM_MAX_INDEX: f32 = 3.0;
const SYNC_MAX: f32 = 8.0;
const TIME_RANGE_OCT: f32 = 4.0;
const UNI_DETUNE_ST: f32 = 0.5;

/// The Wavetable device.
pub struct WavetableSynth {
    v: Vec<f64>,
    alloc: VoiceAllocator,
    voices: Vec<Voice>,
    matrix: ModMatrix,
    sr: f32,
    ctrl_phase: usize,
    mod_wheel: f32,
    clock: BeatClock,
    dirty: bool,
    rng: Rng,
    user: [Box<Wavetable>; 2],
    user_ready: [bool; 2],
    spare: Box<Wavetable>,
    samples: [Option<Arc<SampleBuffer>>; 2],
    builder: WavetableBuilder,
    build: Option<Build>,
    queued: [bool; 2],
    announce: [bool; 2],
    frame: Vec<f32>,
    view_counter: u32,
    view_out: Vec<f32>,
    raw_seen: [bool; 2],
}

impl WavetableSynth {
    /// How many voices are sounding right now.
    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active).count()
    }

    /// Whether oscillator `slot`'s loaded file has been turned into its table.
    pub fn user_table_ready(&self, slot: usize) -> bool {
        self.user_ready[slot]
    }
}

impl Default for WavetableSynth {
    fn default() -> Self {
        Self::new(&DeviceInit::default())
    }
}

impl WavetableSynth {
    pub fn new(init: &DeviceInit) -> Self {
        prebuild_tables();
        let defaults = params();
        let mut v = vec![0.0; PARAM_COUNT];
        for p in &defaults {
            v[p.id as usize] = p.default;
        }
        let matrix = ModMatrix::new(SOURCES.len(), TARGETS.len(), CELLS);
        let voices = (0..MAX_VOICES)
            .map(|i| {
                let seed = 41 + i as u32 * 23;
                Voice {
                    active: false,
                    gated: false,
                    fresh: false,
                    key: 60.0,
                    velocity: 1.0,
                    random: 0.0,
                    bus: matrix.bus(),
                    amp: Envelope::new(),
                    env2: Envelope::new(),
                    env3: Envelope::new(),
                    env2_looped: false,
                    env3_looped: false,
                    lfo: [LfoUnit::new(seed), LfoUnit::new(seed.wrapping_add(1))],
                    unison: [[UniVoice::default(); MAX_UNISON]; 2],
                    osc: [OscCtl::default(); 2],
                    sub_on: false,
                    sub_phase: 0.0,
                    sub_dt: 0.0,
                    sub_gain: 0.0,
                    sub_tone: 0.0,
                    f: [[Filter::new(); 2]; 2],
                    f_on: [true; 2],
                    routing: 0,
                    gain: 0.0,
                    gain_target: 0.0,
                    rng: Rng::new(seed.wrapping_add(5)),
                }
            })
            .collect();
        let mut this = Self {
            v,
            alloc: VoiceAllocator::new(MAX_VOICES),
            voices,
            matrix,
            sr: 48_000.0,
            ctrl_phase: 0,
            mod_wheel: 0.0,
            clock: BeatClock::default(),
            dirty: true,
            rng: Rng::new(0xB17D),
            user: [
                Box::new(Wavetable::with_capacity(MAX_USER_FRAMES, FRAME_LEN)),
                Box::new(Wavetable::with_capacity(MAX_USER_FRAMES, FRAME_LEN)),
            ],
            user_ready: [false; 2],
            spare: Box::new(Wavetable::with_capacity(MAX_USER_FRAMES, FRAME_LEN)),
            samples: [None, None],
            builder: WavetableBuilder::new(FRAME_LEN),
            build: None,
            queued: [false; 2],
            announce: [false; 2],
            frame: vec![0.0; FRAME_LEN],
            view_counter: 0,
            view_out: vec![0.0; VIEW_LEN],
            raw_seen: [false; 2],
        };
        this.refresh();
        for slot in 0..2 {
            this.samples[slot] = init.samples.get(slot).cloned().flatten();
            if this.samples[slot].is_some() {
                this.build_now(slot);
            }
        }
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

    fn osc_base(slot: usize) -> u32 {
        if slot == 0 {
            OSC1
        } else {
            OSC2
        }
    }

    /// Build a whole table at once, while no audio is running.
    fn build_now(&mut self, slot: usize) {
        let Some(sample) = self.samples[slot].clone() else {
            return;
        };
        let (size, frames) = frame_layout(sample.len());
        let raw = self.on(Self::osc_base(slot) + O_RAW);
        for i in 0..frames {
            read_frame(&sample, frames, size, i, raw, &mut self.frame);
            self.builder.write_frame(&mut self.spare, i, &self.frame);
        }
        self.builder.finish(&mut self.spare, frames.max(1));
        std::mem::swap(&mut self.spare, &mut self.user[slot]);
        self.user_ready[slot] = true;
    }

    /// A file that's waiting and the current build advanced by a block.
    fn advance_import(&mut self, ctx: &mut ProcessContext) {
        if self.build.is_none() {
            for slot in 0..2 {
                if self.queued[slot] {
                    self.queued[slot] = false;
                    if let Some(sample) = &self.samples[slot] {
                        let (size, frames) = frame_layout(sample.len());
                        self.build = Some(Build {
                            slot,
                            next: 0,
                            frames,
                            size,
                            raw: self.on(Self::osc_base(slot) + O_RAW),
                            announce: self.announce[slot],
                        });
                        self.announce[slot] = false;
                        break;
                    }
                }
            }
        }
        let Some(mut b) = self.build else {
            return;
        };
        let Some(sample) = self.samples[b.slot].clone() else {
            self.build = None;
            return;
        };
        for _ in 0..BUILD_FRAMES_PER_BLOCK {
            if b.next >= b.frames {
                break;
            }
            read_frame(&sample, b.frames, b.size, b.next, b.raw, &mut self.frame);
            self.builder
                .write_frame(&mut self.spare, b.next, &self.frame);
            b.next += 1;
        }
        self.build = if b.next >= b.frames {
            self.builder.finish(&mut self.spare, b.frames.max(1));
            std::mem::swap(&mut self.spare, &mut self.user[b.slot]);
            self.user_ready[b.slot] = true;
            if b.announce {
                ctx.param_out
                    .push(Self::osc_base(b.slot) + O_CATEGORY, USER_CATEGORY as f64);
            }
            None
        } else {
            Some(b)
        };
    }

    fn set_routes(&mut self) {
        for s in 0..SOURCES.len() {
            for t in 0..TARGETS.len() {
                let amount = self.v[cell(s, t) as usize] as f32 * 0.01;
                let route = (amount != 0.0).then_some(Route {
                    source: s,
                    dest: t,
                    amount,
                    kind: if MULTIPLICATIVE.contains(&t) {
                        RouteKind::Mul
                    } else {
                        RouteKind::Add
                    },
                    via: None,
                });
                self.matrix.set_route(s * TARGETS.len() + t, route);
            }
        }
    }

    fn refresh(&mut self) {
        let settings = VoiceSettings {
            mode: if self.on(MONO) {
                VoiceMode::Legato
            } else {
                VoiceMode::Poly
            },
            polyphony: (2 + self.index(VOICES).min(6)).min(8),
            unison: 1,
            glide: if self.on(MONO) {
                GlideMode::LegatoOnly
            } else {
                GlideMode::Off
            },
            glide_curve: GlideCurve::ConstantTime,
            glide_time: self.f(GLIDE) * 0.001,
            bend_range: 0.0,
            ..VoiceSettings::default()
        };
        self.alloc.set(&settings);
        self.set_routes();
        for slot in 0..2 {
            let raw = self.on(Self::osc_base(slot) + O_RAW);
            if raw != self.raw_seen[slot] {
                self.raw_seen[slot] = raw;
                if self.samples[slot].is_some() {
                    self.queued[slot] = true;
                }
            }
        }
    }

    fn envelope(&self, base: u32, time: f32) -> EnvelopeSettings {
        let (mode, span) = match self.index(base + E_LOOP).min(2) {
            1 => (LoopMode::Trigger, LoopSpan::AttackDecay),
            2 => (LoopMode::Loop, LoopSpan::AttackDecayRelease),
            _ => (LoopMode::Off, LoopSpan::AttackDecay),
        };
        let levels = base != AMP_ENV;
        EnvelopeSettings {
            attack: self.f(base + E_ATTACK) * 0.001 * time,
            decay: self.f(base + E_DECAY) * 0.001 * time,
            sustain: self.f(base + E_SUSTAIN).clamp(0.0, 1.0),
            release: self.f(base + E_RELEASE) * 0.001 * time,
            attack_slope: self.f(base + E_A_SLOPE) * 0.01,
            decay_slope: self.f(base + E_D_SLOPE) * 0.01,
            release_slope: self.f(base + E_R_SLOPE) * 0.01,
            loop_mode: mode,
            loop_span: span,
            initial: if levels {
                self.f(base + E_INITIAL)
            } else {
                0.0
            },
            peak: if levels { self.f(base + E_PEAK) } else { 1.0 },
            end: if levels { self.f(base + E_FINAL) } else { 0.0 },
            ..EnvelopeSettings::default()
        }
    }

    fn lfo_settings(&self, base: u32, rate_mult: f32) -> LfoSettings {
        LfoSettings {
            rate: if self.on(base + L_SYNC) {
                LfoRate::Beats(
                    SyncRate::from_index(self.v[(base + L_SYNC_RATE) as usize]).beats()
                        / rate_mult.max(1e-3) as f64,
                )
            } else {
                LfoRate::Hz(self.f(base + L_RATE) * rate_mult)
            },
            offset: self.f(base + L_OFFSET) / 360.0,
            ..Default::default()
        }
    }

    fn start_voice(&mut self, index: usize, note: NoteInfo) {
        let time = (self.f(MOD_TIME) * TIME_RANGE_OCT).exp2();
        let (amp, env2, env3) = (
            self.envelope(AMP_ENV, time),
            self.envelope(ENV2, time),
            self.envelope(ENV3, time),
        );
        let random = self.rng.bipolar();
        let mode = self.index(UNISON).min(6);
        let positions = [self.f(OSC1 + O_POSITION), self.f(OSC2 + O_POSITION)];
        let settings = [self.lfo_settings(LFO1, 1.0), self.lfo_settings(LFO2, 1.0)];
        let retrig = [self.on(LFO1 + L_RETRIG), self.on(LFO2 + L_RETRIG)];
        let attacks = [
            self.f(LFO1 + L_ATTACK) * 0.001,
            self.f(LFO2 + L_ATTACK) * 0.001,
        ];
        let sr = self.sr;
        let vo = &mut self.voices[index];
        vo.key = note.key as f32;
        vo.velocity = note.velocity.clamp(0.0, 1.0);
        vo.random = random;
        vo.amp.set(&amp);
        vo.env2.set(&env2);
        vo.env3.set(&env3);
        vo.env2_looped = env2.loop_mode == LoopMode::Loop;
        vo.env3_looped = env3.loop_mode == LoopMode::Loop;
        vo.amp.gate_on();
        vo.env2.gate_on();
        vo.env3.gate_on();
        for (i, unit) in vo.lfo.iter_mut().enumerate() {
            unit.lfo.set(&settings[i]);
            if retrig[i] {
                unit.lfo.retrigger();
            }
            unit.fade = if attacks[i] > 0.0 { 0.0 } else { 1.0 };
            unit.fade_step = if attacks[i] > 0.0 {
                CONTROL_INTERVAL as f32 / (attacks[i] * sr)
            } else {
                1.0
            };
        }
        let phase_sync = mode == 4;
        for (osc, position) in vo.unison.iter_mut().zip(positions) {
            for uni in osc.iter_mut() {
                if phase_sync {
                    uni.phase = 0.0;
                    uni.mod_phase = 0.0;
                } else {
                    uni.phase = vo.rng.unipolar();
                    uni.mod_phase = vo.rng.unipolar();
                }
                uni.pos = if mode == 6 {
                    (vo.rng.unipolar() - position) * 0.5
                } else {
                    0.0
                };
                uni.jitter = if mode == 6 { vo.rng.bipolar() } else { 0.0 };
                uni.jitter_target = uni.jitter;
                uni.timer = 0.0;
            }
        }
        vo.sub_phase = 0.0;
        vo.active = true;
        vo.gated = true;
        vo.fresh = true;
    }

    fn midi(&mut self, message: MidiMessage) {
        if let MidiMessage::ControlChange { cc: 1, value, .. } = message {
            self.mod_wheel = value as f32 / 127.0;
        }
        if matches!(message, MidiMessage::NoteOn { .. }) && self.dirty {
            self.refresh();
            self.dirty = false;
        }
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
                    self.start_voice(voice, note);
                    self.voice_control(voice, CONTROL_INTERVAL as f32);
                }
                VoiceEvent::Legato { voice, note } => {
                    self.voices[voice].key = note.key as f32;
                    self.voices[voice].velocity = note.velocity.clamp(0.0, 1.0);
                }
                VoiceEvent::Release { voice } => self.voices[voice].release(),
                VoiceEvent::Kill { voice } => self.voices[voice].silence(),
            }
        }
    }

    fn interval(&self) -> usize {
        if self.on(HI_QUALITY) {
            4
        } else {
            32
        }
    }

    fn voice_control(&mut self, index: usize, frames: f32) {
        let info = *self.alloc.voice(index);
        let pitch = self.alloc.pitch(index);
        let sr = self.sr;
        let (bend, channel_press) = (
            self.alloc.channel_bend(index),
            self.alloc.channel_pressure(index),
        );
        let mw = self.mod_wheel;
        let clock = self.clock;
        let time_param = self.f(MOD_TIME) * TIME_RANGE_OCT;
        let (unison_mode, uni_voices, routing, volume, amount_param) = (
            self.index(UNISON).min(6),
            2 + self.index(UNISON_VOICES).min(6),
            self.index(ROUTING).min(2) as u8,
            db_to_gain(self.f(VOLUME)),
            self.f(MOD_AMOUNT),
        );
        let uni_amount = self.f(UNISON_AMOUNT);
        let transpose = self.f(TRANSPOSE);
        let lfo_cfg = [
            (
                self.index(LFO1 + L_WAVE).min(4) as u8,
                self.f(LFO1 + L_SHAPE),
                self.f(LFO1 + L_AMOUNT),
            ),
            (
                self.index(LFO2 + L_WAVE).min(4) as u8,
                self.f(LFO2 + L_SHAPE),
                self.f(LFO2 + L_AMOUNT),
            ),
        ];
        // Each modulator's time is every assignment to Time except its own.
        let cell_amount = |s: usize| self.v[cell(s, T_TIME) as usize] as f32 * 0.01;
        let self_time = [
            cell_amount(S_AMP),
            cell_amount(S_E2),
            cell_amount(S_E3),
            cell_amount(S_L1),
            cell_amount(S_L2),
        ];
        let env_times = {
            let vo = &self.voices[index];
            let t = vo.bus.offset(T_TIME);
            [
                t - self_time[0] * vo.amp.level(),
                t - self_time[1] * vo.env2.level(),
                t - self_time[2] * vo.env3.level(),
            ]
        };
        let amp_env = self.envelope(AMP_ENV, (env_times[0] * TIME_RANGE_OCT + time_param).exp2());
        let mut env2 = self.envelope(ENV2, (env_times[1] * TIME_RANGE_OCT + time_param).exp2());
        let mut env3 = self.envelope(ENV3, (env_times[2] * TIME_RANGE_OCT + time_param).exp2());
        // Multiplicative modulation of the envelope levels.
        {
            let vo = &self.voices[index];
            let (a, sustain, initial) = (
                vo.bus.gain(T_AMOUNT),
                vo.bus.gain(T_SUSTAIN),
                vo.bus.gain(T_INITIAL),
            );
            let scale = |g: f32| lerp(1.0, g, (amount_param * a).clamp(0.0, 1.0));
            env2.sustain = (env2.sustain * scale(sustain)).clamp(0.0, 1.0);
            env3.sustain = (env3.sustain * scale(sustain)).clamp(0.0, 1.0);
            env2.initial *= scale(initial);
            env3.initial *= scale(initial);
        }
        // An LFO's speed is every assignment to Time except its own, and what's routed to its Rate.
        let lfo_settings = {
            let vo = &self.voices[index];
            let t = vo.bus.offset(T_TIME);
            let last = [vo.lfo[0].value, vo.lfo[1].value];
            let rate = |i: usize, s_time: f32, base: u32, voice: &Voice| {
                let own = (t - s_time * last[i]) * TIME_RANGE_OCT + time_param;
                let rate_mod = voice.bus.offset(T_LFO_RATE[i]) * TIME_RANGE_OCT;
                self.lfo_settings(base, (rate_mod - own).exp2())
            };
            [
                rate(0, self_time[3], LFO1, vo),
                rate(1, self_time[4], LFO2, vo),
            ]
        };
        let (values, matrix) = (&self.v, &self.matrix);
        let g = |id: u32| values[id as usize] as f32;
        let on = |id: u32| values[id as usize] >= 0.5;
        let vo = &mut self.voices[index];
        let fresh = vo.fresh;
        vo.amp.set(&amp_env);
        vo.env2.set(&env2);
        vo.env3.set(&env3);

        // Mod sources.
        let mut lfo_val = [0.0f32; 2];
        let frames_u = frames as usize;
        for (i, unit) in vo.lfo.iter_mut().enumerate() {
            unit.lfo.set(&lfo_settings[i]);
            unit.lfo.set_clock(&clock);
            let [p, _] = unit.lfo.advance_phases(frames_u);
            if p < unit.last {
                let r = unit.rng.bipolar();
                unit.held = r;
            }
            unit.last = p;
            unit.fade = (unit.fade + unit.fade_step).min(1.0);
            let (wave, shape, _) = lfo_cfg[i];
            lfo_val[i] = lfo_wave(wave, p, shape, unit.held) * unit.fade;
            unit.value = lfo_val[i];
        }
        let a = (amount_param * vo.bus.gain(T_AMOUNT)).clamp(0.0, 1.0);
        for i in 0..2 {
            lfo_val[i] *= (lfo_cfg[i].2 * lerp(1.0, vo.bus.gain(T_LFO_AMT[i]), a)).clamp(0.0, 1.0);
        }
        let mut src = [0.0f32; SOURCES.len()];
        src[S_AMP] = vo.amp.level();
        src[S_E2] = vo.env2.level();
        src[S_E3] = vo.env3.level();
        src[S_L1] = lfo_val[0];
        src[S_L2] = lfo_val[1];
        src[S_VEL] = vo.velocity;
        src[S_NOTE] = (vo.key - 60.0) / 48.0;
        src[S_PB] = bend;
        src[S_AT] = info.pressure.max(channel_press);
        src[S_MW] = mw;
        src[S_RANDOM] = vo.random;
        src[S_SLIDE] = info.slide;
        matrix.evaluate(&src, &mut vo.bus);
        let bus = &vo.bus;
        let offset = |t: usize| bus.offset(t) * a;
        let mul = |t: usize| lerp(1.0, bus.gain(t), a);
        let semis_base = pitch + transpose;
        let pitch_for = |i: usize| semis_base + offset(T_PITCH[i]) * PITCH_RANGE_ST;

        let uni_amount_m = uni_amount * mul(T_UNI);
        let unison_count = if unison_mode == 0 {
            1
        } else {
            uni_voices.min(MAX_UNISON)
        };

        for i in 0..2 {
            let base = if i == 0 { OSC1 } else { OSC2 };
            let enabled = on(base + O_ON);
            let semi = g(base + O_SEMI) + g(base + O_DETUNE) * 0.01;
            let semis = pitch_for(i) + semi;
            let hz = note_hz(semis).clamp(0.1, sr * 0.49);
            let dt = hz / sr;
            let fx = values[(base + O_FX) as usize].round() as u8;
            let fx1 = g(base + O_FX1) * 0.01 + offset(T_FX1[i]);
            let fx2 = g(base + O_FX2) * 0.01 + offset(T_FX2[i]);
            let pos = (g(base + O_POSITION) + offset(T_POS[i])).clamp(0.0, 1.0);
            let gain = (g(base + O_GAIN) + offset(T_GAIN[i])).clamp(0.0, 2.0);
            let pan = (g(base + O_PAN) + offset(T_PAN[i])).clamp(-1.0, 1.0);
            let ctl = &mut vo.osc[i];
            ctl.on = enabled;
            ctl.fx = fx.min(3);
            ctl.fx1 = fx1.clamp(-1.0, 1.0);
            ctl.fx2 = fx2.clamp(-1.0, 1.0);
            ctl.position = pos;
            ctl.dt = dt;
            ctl.gain = gain;
            ctl.pan = pan_gains(pan);
            ctl.voices = unison_count;
            ctl.table = (
                values[(base + O_CATEGORY) as usize].round() as u8,
                values[(base + O_TABLE) as usize].round() as u8,
            );
            let unis = &mut vo.unison[i];
            let width = unison_count.max(2) as f32 - 1.0;
            for (u, uni) in unis.iter_mut().take(unison_count).enumerate() {
                let spread = if unison_count > 1 {
                    2.0 * u as f32 / width - 1.0
                } else {
                    0.0
                };
                let (detune, position, pan_u) = match unison_mode {
                    1 | 4 => (
                        spread * uni_amount_m * UNI_DETUNE_ST,
                        0.0,
                        if u % 2 == 0 { -1.0 } else { 1.0 },
                    ),
                    2 | 3 => {
                        let rate = if unison_mode == 2 { 6.0 } else { 800.0 };
                        uni.timer += frames * rate / sr;
                        if uni.timer >= 1.0 {
                            uni.timer -= uni.timer.floor();
                            uni.jitter_target = vo.rng.bipolar();
                        }
                        uni.jitter += (uni.jitter_target - uni.jitter) * 0.2;
                        (
                            uni.jitter * uni_amount_m * UNI_DETUNE_ST,
                            spread * 0.03,
                            if u % 2 == 0 { -0.6 } else { 0.6 },
                        )
                    }
                    5 => (spread * 0.05, spread * uni_amount_m * 0.5, spread * 0.7),
                    6 => (
                        uni.jitter * uni_amount_m * UNI_DETUNE_ST,
                        uni.pos,
                        spread * 0.7,
                    ),
                    _ => (0.0, 0.0, 0.0),
                };
                uni.ratio = (detune / 12.0).exp2();
                uni.pos = if unison_mode == 6 { uni.pos } else { position };
                let (l, r) = pan_gains(pan_u);
                uni.pan_l = l;
                uni.pan_r = r;
            }
        }
        vo.sub_on = on(SUB_ON);
        vo.sub_gain = (g(SUB_GAIN) + offset(T_SUB_GAIN)).max(0.0);
        vo.sub_tone = (g(SUB_TONE) * 0.01 + offset(T_SUB_TONE)).clamp(0.0, 1.0);
        let sub_oct = values[SUB_OCT as usize].round() as i32;
        vo.sub_dt = (note_hz(semis_base - 12.0 * sub_oct as f32) / sr).clamp(1e-6, 0.45);
        for i in 0..2 {
            let base = if i == 0 { FILTER1 } else { FILTER2 };
            let ty = (values[(base + F_TYPE) as usize].round() as usize).min(4);
            let (kind, morph_type) = match ty {
                0 => (FilterKind::LowPass, false),
                1 => (FilterKind::HighPass, false),
                2 => (FilterKind::BandPass, false),
                3 => (FilterKind::Notch, false),
                _ => (FilterKind::Morph, true),
            };
            let lp_or_hp = ty <= 1;
            let circuit_index = values[(base + F_CIRCUIT) as usize].round() as usize;
            // MS2, SMP and PRD only exist for low-pass and high-pass.
            let circuit = match circuit_index {
                1 => Circuit::Osr,
                2 if lp_or_hp => Circuit::Ms2,
                3 if lp_or_hp => Circuit::Smp,
                4 if lp_or_hp => Circuit::Prd,
                _ => Circuit::Svf,
            };
            let slope = if values[(base + F_SLOPE) as usize] >= 0.5 {
                FilterSlope::Db24
            } else {
                FilterSlope::Db12
            };
            let cutoff = g(base + F_FREQ) * (offset(T_F_FREQ[i]) * FREQ_RANGE_ST / 12.0).exp2();
            let settings = FilterSettings {
                circuit,
                kind,
                slope,
                cutoff: cutoff.clamp(10.0, sr * 0.48),
                resonance: (g(base + F_RES) + offset(T_F_RES[i])).clamp(0.0, 1.0),
                drive: if circuit != Circuit::Svf && (ty <= 2) {
                    g(base + F_DRIVE)
                } else {
                    0.0
                },
                morph: if morph_type { g(base + F_MORPH) } else { 0.0 },
            };
            vo.f[i][0].set(&settings);
            vo.f[i][1].set(&settings);
            vo.f_on[i] = on(base + F_ON);
        }
        vo.routing = routing;
        vo.gain_target = volume * mul(T_VOLUME);
        if fresh {
            vo.gain = vo.gain_target;
        }
        vo.fresh = false;
    }

    fn control_tick(&mut self, interval: usize) {
        if self.dirty {
            self.refresh();
            self.dirty = false;
        }
        self.alloc.advance(interval);
        self.clock.advance(interval);
        for i in 0..self.voices.len() {
            if self.voices[i].active {
                self.voice_control(i, interval as f32);
            }
        }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let frames = left.len().min(right.len());
        let interval = self.interval();
        let mut pos = 0;
        while pos < frames {
            if self.ctrl_phase == 0 {
                self.control_tick(interval);
            }
            let n = (interval - self.ctrl_phase).min(frames - pos);
            let Self {
                voices,
                user,
                user_ready,
                alloc,
                ..
            } = self;
            for (i, vo) in voices.iter_mut().enumerate() {
                if !vo.active {
                    continue;
                }
                let t1 = table_for(user, user_ready, vo.osc[0].table, 0);
                let t2 = table_for(user, user_ready, vo.osc[1].table, 1);
                if render_voice(
                    vo,
                    [t1, t2],
                    &mut left[pos..pos + n],
                    &mut right[pos..pos + n],
                ) {
                    vo.active = false;
                    alloc.finished(i);
                }
            }
            self.ctrl_phase = (self.ctrl_phase + n) % interval;
            pos += n;
        }
    }

    fn publish(&mut self, ctx: &ProcessContext) {
        if self.view_counter > 0 {
            self.view_counter -= 1;
            return;
        }
        self.view_counter = VIEW_EVERY;
        let mut at = 2;
        for slot in 0..2 {
            let base = Self::osc_base(slot);
            let (cat, tab) = (self.index(base + O_CATEGORY), self.index(base + O_TABLE));
            let table = table_for(&self.user, &self.user_ready, (cat as u8, tab as u8), slot);
            self.view_out[slot] = self.f(base + O_POSITION);
            for f in 0..VIEW_FRAMES {
                let pos = f as f32 / (VIEW_FRAMES - 1) as f32;
                for p in 0..VIEW_POINTS {
                    let phase = p as f32 / VIEW_POINTS as f32;
                    self.view_out[at] = table.read(phase, pos, 1.0 / FRAME_LEN as f32);
                    at += 1;
                }
            }
        }
        ctx.view.publish(VIEW, &self.view_out);
    }
}

/// The table an oscillator reads: a generated one, or its loaded file once that's ready.
#[inline]
fn table_for<'a>(
    user: &'a [Box<Wavetable>; 2],
    ready: &[bool; 2],
    (category, table): (u8, u8),
    slot: usize,
) -> &'a Wavetable {
    if category as usize == USER_CATEGORY {
        if ready[slot] {
            &user[slot]
        } else {
            generated(0, 0)
        }
    } else {
        generated(category as usize, table as usize)
    }
}

#[inline]
fn warp_pw(p: f32, w: f32) -> f32 {
    if p < w {
        0.5 * p / w.max(1e-3)
    } else {
        0.5 + 0.5 * (p - w) / (1.0 - w).max(1e-3)
    }
}

#[inline]
fn fold(x: f32) -> f32 {
    (x * PI * 0.5).sin()
}

/// One oscillator's stereo output for one sample. `dt` is how far the voice has moved.
#[inline]
fn read_unison(table: &Wavetable, ctl: &OscCtl, unis: &mut [UniVoice; MAX_UNISON]) -> [f32; 2] {
    let (mut l, mut r) = (0.0f32, 0.0f32);
    let (fx1, fx2) = (ctl.fx1, ctl.fx2);
    let dt0 = ctl.dt;
    for uni in unis.iter_mut().take(ctl.voices) {
        let dt = dt0 * uni.ratio;
        let p = uni.phase;
        uni.phase += dt;
        if uni.phase >= 1.0 {
            uni.phase -= 1.0;
        }
        let pos = (ctl.position + uni.pos).clamp(0.0, 1.0);
        let y = match ctl.fx {
            1 => {
                let amt = fx1.abs() * FM_MAX_INDEX;
                let ratio = (fx2 * 2.0).exp2();
                uni.mod_phase = (uni.mod_phase + dt * ratio).fract();
                let m = (TAU * uni.mod_phase).sin();
                let eff = dt * (1.0 + amt * ratio);
                table.read(p + amt * m / TAU, pos, eff)
            }
            2 => {
                let w = (0.5 - 0.49 * fx1.max(0.0)).clamp(0.01, 0.5);
                let sync = 1.0 + fx2.abs() * (SYNC_MAX - 1.0);
                let p1 = (p * sync).fract();
                let p2 = warp_pw(p1, w);
                table.read(p2, pos, dt * sync / w.max(0.05))
            }
            3 => {
                let warp = (fx1 * 2.0).exp2();
                let warped = p.powf(warp);
                let y = table.read(warped, pos, dt * warp.max(1.0));
                let f = fx2.max(0.0);
                if f > 0.0 {
                    fold(y * (1.0 + f * 4.0))
                } else {
                    y
                }
            }
            _ => table.read(p, pos, dt),
        };
        l += y * uni.pan_l;
        r += y * uni.pan_r;
    }
    let norm = 1.0 / (ctl.voices.max(1) as f32).sqrt();
    [l * norm, r * norm]
}

fn render_voice(
    v: &mut Voice,
    tables: [&Wavetable; 2],
    left: &mut [f32],
    right: &mut [f32],
) -> bool {
    let routing = v.routing;
    for (l, r) in left.iter_mut().zip(right.iter_mut()) {
        let env = v.amp.next();
        v.env2.next();
        v.env3.next();
        let mut o = [[0.0f32; 2]; 2];
        for i in 0..2 {
            if v.osc[i].on {
                let [a, b] = read_unison(tables[i], &v.osc[i], &mut v.unison[i]);
                o[i] = [
                    a * v.osc[i].gain * v.osc[i].pan.0,
                    b * v.osc[i].gain * v.osc[i].pan.1,
                ];
            }
        }
        let mut sub = 0.0;
        if v.sub_on {
            let p = v.sub_phase;
            v.sub_phase = (v.sub_phase + v.sub_dt).fract();
            sub = lerp((TAU * p).sin(), saturated(p, v.sub_dt, 1.0), v.sub_tone) * v.sub_gain;
        }
        let mut output = [0.0f32; 2];
        for ch in 0..2 {
            let (a, b, s) = (o[0][ch], o[1][ch], sub);
            let (f1_on, f2_on) = (v.f_on[0], v.f_on[1]);
            output[ch] = match routing {
                0 => {
                    let x = a + b + s;
                    let y1 = if f1_on { v.f[0][ch].process(x) } else { x };
                    if f2_on {
                        v.f[1][ch].process(y1)
                    } else {
                        y1
                    }
                }
                1 => {
                    let x = a + b + s;
                    let y1 = if f1_on { v.f[0][ch].process(x) } else { x };
                    let y2 = if f2_on { v.f[1][ch].process(x) } else { x };
                    if f1_on == f2_on {
                        0.5 * (y1 + y2)
                    } else if f1_on {
                        y1
                    } else {
                        y2
                    }
                }
                _ => {
                    let y1 = a + s * 0.5;
                    let y2 = b + s * 0.5;
                    let y1 = if f1_on { v.f[0][ch].process(y1) } else { y1 };
                    let y2 = if f2_on { v.f[1][ch].process(y2) } else { y2 };
                    y1 + y2
                }
            };
        }
        v.gain += (v.gain_target - v.gain) * 0.02;
        let g = v.gain * env;
        *l += sanitize(output[0] * g);
        *r += sanitize(output[1] * g);
    }
    !v.amp.is_active()
}

impl Processor for WavetableSynth {
    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sr = sample_rate as f32;
        self.alloc.prepare(sample_rate);
        for v in &mut self.voices {
            v.amp.prepare(sample_rate);
            v.env2.prepare(sample_rate);
            v.env3.prepare(sample_rate);
            for u in &mut v.lfo {
                u.lfo.prepare(sample_rate);
            }
            for pair in v.f.iter_mut() {
                for f in pair.iter_mut() {
                    f.prepare(sample_rate);
                }
            }
        }
        self.refresh();
        self.reset();
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        self.clock = BeatClock::from_transport(ctx.transport);
        self.advance_import(ctx);
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
        if ctx.view.is_active() {
            self.publish(ctx);
        }
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
    }

    fn tail(&self) -> Tail {
        let time = (self.f(MOD_TIME) * TIME_RANGE_OCT).exp2();
        Tail::Frames((self.f(AMP_ENV + E_RELEASE) * 0.001 * time * self.sr) as u32 + 1)
    }

    fn set_resource(&mut self, slot: u32, resource: Resource) -> Option<Resource> {
        let slot = slot as usize;
        if slot >= 2 {
            return Some(resource);
        }
        let sample: Arc<SampleBuffer> = match resource.downcast() {
            Ok(s) => s,
            Err(r) => return Some(r),
        };
        if self.build.is_some_and(|b| b.slot == slot) {
            self.build = None;
        }
        let previous = self.samples[slot].replace(sample);
        self.queued[slot] = true;
        self.announce[slot] = true;
        previous.map(|p| p as Resource)
    }
}
