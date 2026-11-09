//! Operator: four-oscillator FM with subtractive and additive sides (manual pages 755-772).
//!
//! Four oscillators in phase modulation read band-limited tables or a sine table. Antialias runs
//! them oversampled; the manual gives no figures for the scale of FM depth, the LFO's depth in
//! semitones, or the Tone curve, so those are ours. Its text names the eleven algorithms without
//! listing them (the icons are in the images), so the table in [`ALGORITHMS`] is ours too.

use super::common::{knob, limit, menu, note_hz, scale_note_bend, toggle, StealFade};
use super::operator_waves::{
    builtin_table, noise_loop, sine_table, MipTable, ParsedWaves, SINE_LEN, WAVE_NAMES,
    W_NOISE_LOOP, W_NOISE_WHITE, W_SAW_64, W_SINE, W_SQUARE_64, W_TRIANGLE, W_USER,
};
use crate::devices::b10::common::STATE_SLOT;
use crate::dsp::envelope::{Envelope, EnvelopeSettings, LoopMode, LoopSpan};
use crate::dsp::filter::{Circuit, Filter, FilterKind, FilterSettings, FilterSlope};
use crate::dsp::modmatrix::{ModBus, ModMatrix, Route, RouteKind};
use crate::dsp::oversample::{OversampleFactor, Oversampler};
use crate::dsp::tempo::{BeatClock, SyncRate};
use crate::dsp::util::{db_to_gain, sanitize, Rng};
use crate::dsp::voice::{
    GlideCurve, GlideMode, NoteInfo, StealMode, VoiceAllocator, VoiceEvent, VoiceMode,
    VoiceSettings, VoiceState,
};
use crate::dsp::{Biquad, BiquadKind};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    BlockStep, DeviceDesc, ExprKind, Item, MidiMessage, ParamInfo, ProcessContext, Processor,
    Resource, Section, Status, Tail, ViewId,
};
use std::f32::consts::{LN_2, PI, TAU};
use std::sync::Arc;

/// Oscillator parameters, relative to [`OSC_BASE`].
pub const OSC_BASE: [u32; 4] = [0, 40, 80, 120];
pub const O_ON: u32 = 0;
pub const O_COARSE: u32 = 1;
pub const O_FINE: u32 = 2;
pub const O_FIXED: u32 = 3;
pub const O_FREQ: u32 = 4;
pub const O_MULTI: u32 = 5;
pub const O_LEVEL: u32 = 6;
pub const O_WAVE: u32 = 7;
pub const O_FEEDBACK: u32 = 8;
pub const O_PHASE: u32 = 9;
pub const O_RETRIG: u32 = 10;
pub const O_FREQ_VEL: u32 = 11;
pub const O_FREQ_Q: u32 = 12;
pub const O_LEVEL_VEL: u32 = 13;
pub const O_LEVEL_KEY: u32 = 14;
pub const O_ENV: u32 = 15;

/// Envelope parameters, relative to an envelope's base.
pub const E_ATTACK: u32 = 0;
pub const E_DECAY: u32 = 1;
pub const E_RELEASE: u32 = 2;
pub const E_INITIAL: u32 = 3;
pub const E_PEAK: u32 = 4;
pub const E_SUSTAIN: u32 = 5;
pub const E_END: u32 = 6;
pub const E_LOOP: u32 = 7;
pub const E_REPEAT: u32 = 8;
pub const E_LOOP_TIME: u32 = 9;
pub const E_TIME_VEL: u32 = 10;
pub const E_A_SLOPE: u32 = 11;
pub const E_D_SLOPE: u32 = 12;
pub const E_R_SLOPE: u32 = 13;

pub const FILTER_BASE: u32 = 160;
pub const F_ON: u32 = FILTER_BASE;
pub const F_TYPE: u32 = FILTER_BASE + 1;
pub const F_CIRCUIT: u32 = FILTER_BASE + 2;
pub const F_SLOPE: u32 = FILTER_BASE + 3;
pub const F_FREQ: u32 = FILTER_BASE + 4;
pub const F_RES: u32 = FILTER_BASE + 5;
pub const F_DRIVE: u32 = FILTER_BASE + 6;
pub const F_MORPH: u32 = FILTER_BASE + 7;
pub const F_FREQ_VEL: u32 = FILTER_BASE + 8;
pub const F_FREQ_KEY: u32 = FILTER_BASE + 9;
pub const F_ENV_AMT: u32 = FILTER_BASE + 10;
pub const F_SHAPER: u32 = FILTER_BASE + 11;
pub const F_SHAPER_DRIVE: u32 = FILTER_BASE + 12;
pub const F_DRY_WET: u32 = FILTER_BASE + 13;
pub const F_ENV: u32 = FILTER_BASE + 14;

pub const LFO_BASE: u32 = 200;
pub const L_ON: u32 = LFO_BASE;
pub const L_WAVE: u32 = LFO_BASE + 1;
pub const L_RANGE: u32 = LFO_BASE + 2;
pub const L_RATE: u32 = LFO_BASE + 3;
pub const L_SYNC: u32 = LFO_BASE + 4;
pub const L_RETRIG: u32 = LFO_BASE + 5;
pub const L_AMOUNT: u32 = LFO_BASE + 6;
pub const L_DEST_A: u32 = LFO_BASE + 7;
pub const L_DEST_B_OSC: u32 = LFO_BASE + 8;
pub const L_DEST_C: u32 = LFO_BASE + 9;
pub const L_DEST_D: u32 = LFO_BASE + 10;
pub const L_DEST_FIL: u32 = LFO_BASE + 11;
pub const L_AMT_A: u32 = LFO_BASE + 12;
pub const L_DEST_B: u32 = LFO_BASE + 13;
pub const L_AMT_B: u32 = LFO_BASE + 14;
pub const L_AMT_VEL: u32 = LFO_BASE + 15;
pub const L_RATE_KEY: u32 = LFO_BASE + 16;
pub const L_ENV: u32 = LFO_BASE + 17;

pub const PITCH_BASE: u32 = 240;
pub const P_ON: u32 = PITCH_BASE;
pub const P_AMOUNT: u32 = PITCH_BASE + 1;
pub const P_SPREAD: u32 = PITCH_BASE + 2;
pub const P_TRANSPOSE: u32 = PITCH_BASE + 3;
pub const P_GLIDE: u32 = PITCH_BASE + 4;
pub const P_GLIDE_TIME: u32 = PITCH_BASE + 5;
pub const P_DEST_A: u32 = PITCH_BASE + 6;
pub const P_DEST_B_OSC: u32 = PITCH_BASE + 7;
pub const P_DEST_C: u32 = PITCH_BASE + 8;
pub const P_DEST_D: u32 = PITCH_BASE + 9;
pub const P_DEST_LFO: u32 = PITCH_BASE + 10;
pub const P_AMT_A: u32 = PITCH_BASE + 11;
pub const P_DEST_B: u32 = PITCH_BASE + 12;
pub const P_AMT_B: u32 = PITCH_BASE + 13;
pub const P_ENV: u32 = PITCH_BASE + 14;

pub const GLOBAL_BASE: u32 = 280;
pub const G_TIME: u32 = GLOBAL_BASE;
pub const G_TONE: u32 = GLOBAL_BASE + 1;
pub const G_VOLUME: u32 = GLOBAL_BASE + 2;
pub const G_ALGORITHM: u32 = GLOBAL_BASE + 3;
pub const G_VOICES: u32 = GLOBAL_BASE + 4;
pub const G_RETRIG: u32 = GLOBAL_BASE + 5;
pub const G_INTERP: u32 = GLOBAL_BASE + 6;
pub const G_ANTIALIAS: u32 = GLOBAL_BASE + 7;
pub const G_TIME_KEY: u32 = GLOBAL_BASE + 8;
pub const G_PB_RANGE: u32 = GLOBAL_BASE + 9;
pub const G_PAN: u32 = GLOBAL_BASE + 10;
pub const G_PAN_KEY: u32 = GLOBAL_BASE + 11;
pub const G_PAN_RND: u32 = GLOBAL_BASE + 12;
pub const G_PER_NOTE_PB: u32 = GLOBAL_BASE + 13;
/// `[dest A, amount A, dest B, amount B]` for Velocity, Key, Aftertouch, Pitch Bend and Mod Wheel.
pub const G_MIDI_BASE: [u32; 5] = [
    GLOBAL_BASE + 14,
    GLOBAL_BASE + 18,
    GLOBAL_BASE + 22,
    GLOBAL_BASE + 26,
    GLOBAL_BASE + 30,
];
const PARAMS: usize = (GLOBAL_BASE + 34) as usize;

/// Which oscillators modulate each one (bit 0 is A), and which sound. Oscillators are A to D in
/// order, and signals flow from D at the top towards A.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Algorithm {
    pub modulators: [u8; 4],
    pub carriers: u8,
}

const A: u8 = 1;
const B: u8 = 2;
const C: u8 = 4;
const D: u8 = 8;

const fn alg(modulators: [u8; 4], carriers: u8) -> Algorithm {
    Algorithm {
        modulators,
        carriers,
    }
}

/// The manual shows these as icons only. In order: a single chain, then trees that fan in, then
/// pairs, then branching stacks, down to four parallel carriers.
pub const ALGORITHMS: [Algorithm; 11] = [
    alg([B, C, D, 0], A),
    alg([B, C | D, 0, 0], A),
    alg([B | C, 0, D, 0], A),
    alg([B | C | D, 0, 0, 0], A),
    alg([B | C, D, D, 0], A),
    alg([C, C, D, 0], A | B),
    alg([C | D, C | D, 0, 0], A | B),
    alg([B, 0, D, 0], A | C),
    alg([D, D, D, 0], A | B | C),
    alg([0, 0, D, 0], A | B | C),
    alg([0, 0, 0, 0], A | B | C | D),
];

pub const FM_INDEX_AT_FULL: f32 = 4.0;
/// Radians of phase modulation a full-level modulator gives. The manual doesn't say.
const FEEDBACK_SCALE: f32 = 3.0;

pub const VOICES_MAX: usize = 32;
/// The internal block for control-rate work.
const CHUNK: usize = 16;
const OVERSAMPLE: OversampleFactor = OversampleFactor::X2;
const OS_MULTIPLY: usize = 2;

pub const ENV_LOOP_NONE: usize = 0;
pub const ENV_LOOP_LOOP: usize = 1;
pub const ENV_LOOP_BEAT: usize = 2;
pub const ENV_LOOP_SYNC: usize = 3;
pub const ENV_LOOP_TRIGGER: usize = 4;

const LFO_WAVES: [&str; 7] = [
    "Sine", "Square", "Triangle", "Saw Up", "Saw Down", "S&H", "Noise",
];
const LFO_SINE: usize = 0;
const LFO_SQUARE: usize = 1;
const LFO_TRIANGLE: usize = 2;
const LFO_SAW_UP: usize = 3;
const LFO_SAW_DOWN: usize = 4;
const LFO_SH: usize = 5;
const LFO_NOISE: usize = 6;

const DEST_LABELS: [&str; 24] = [
    "Off",
    "OSC Volume A",
    "OSC Volume B",
    "OSC Volume C",
    "OSC Volume D",
    "OSC Crossfade A/C",
    "OSC Crossfade B/D",
    "OSC Feedback",
    "OSC Fixed Frequency",
    "FM Drive",
    "Filter Frequency",
    "Filter Q (Legacy)",
    "Filter Res",
    "Filter Morph",
    "Filter Drive",
    "Filter Envelope Amount",
    "Shaper Drive",
    "LFO Rate",
    "LFO Amount",
    "Pitch Envelope Amount",
    "Volume",
    "Panorama",
    "Tone",
    "Time",
];
const D_VOL: [usize; 4] = [1, 2, 3, 4];
const D_XFADE_AC: usize = 5;
const D_XFADE_BD: usize = 6;
const D_FEEDBACK: usize = 7;
const D_FIXED: usize = 8;
const D_FM_DRIVE: usize = 9;
const D_FILTER_FREQ: usize = 10;
const D_FILTER_Q: usize = 11;
const D_FILTER_RES: usize = 12;
const D_FILTER_MORPH: usize = 13;
const D_FILTER_DRIVE: usize = 14;
const D_FILTER_ENV: usize = 15;
const D_SHAPER_DRIVE: usize = 16;
const D_LFO_RATE: usize = 17;
const D_LFO_AMOUNT: usize = 18;
const D_PE_AMOUNT: usize = 19;
const D_VOLUME: usize = 20;
const D_PAN: usize = 21;
const D_TONE: usize = 22;
const D_TIME: usize = 23;

const SRC_VEL: usize = 0;
const SRC_KEY: usize = 1;
const SRC_AT: usize = 2;
const SRC_PB: usize = 3;
const SRC_MW: usize = 4;
const SRC_LFO: usize = 5;
const SRC_PE: usize = 6;
const SOURCES: usize = 7;
const ROUTES: usize = 12;

pub const SHAPERS: [&str; 5] = ["Off", "Soft", "Hard", "Sine", "4 Bit"];

pub const VIEW: ViewId = ViewId(0);
/// `[level of A, B, C, D, filter, pitch and LFO envelopes]`.
pub const VIEW_LEN: usize = 7;

/// A silent level for the decibel envelopes.
const DB_FLOOR: f32 = -70.0;

fn level_gain(db: f32) -> f32 {
    if db <= DB_FLOOR {
        0.0
    } else {
        db_to_gain(db)
    }
}

fn env_params(
    base: u32,
    name: &str,
    unit: &str,
    min: f64,
    max: f64,
    defaults: [f64; 3],
) -> Vec<ParamInfo> {
    let n = |s: &str| format!("{name} {s}");
    vec![
        ParamInfo::skewed(base + E_ATTACK, &n("Attack"), 0.0, 20_000.0, 0.0, "ms", 3.0),
        ParamInfo::skewed(base + E_DECAY, &n("Decay"), 0.0, 60_000.0, 600.0, "ms", 3.0),
        ParamInfo::skewed(
            base + E_RELEASE,
            &n("Release"),
            0.0,
            60_000.0,
            50.0,
            "ms",
            3.0,
        ),
        ParamInfo::continuous(base + E_INITIAL, &n("Initial"), min, max, defaults[0], unit),
        ParamInfo::continuous(base + E_PEAK, &n("Peak"), min, max, defaults[1], unit),
        ParamInfo::continuous(base + E_SUSTAIN, &n("Sustain"), min, max, defaults[2], unit),
        ParamInfo::continuous(base + E_END, &n("End"), min, max, defaults[0], unit),
        ParamInfo::stepped(
            base + E_LOOP,
            &n("Loop"),
            &["None", "Loop", "Beat", "Sync", "Trigger"],
            ENV_LOOP_NONE,
        ),
        ParamInfo::stepped(base + E_REPEAT, &n("Repeat"), &SyncRate::LABELS, 12),
        ParamInfo::skewed(
            base + E_LOOP_TIME,
            &n("Time"),
            0.0,
            20_000.0,
            0.0,
            "ms",
            3.0,
        ),
        ParamInfo::continuous(base + E_TIME_VEL, &n("Time < Vel"), -100.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(base + E_A_SLOPE, &n("A.Slope"), -100.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(base + E_D_SLOPE, &n("D.Slope"), -100.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(base + E_R_SLOPE, &n("R.Slope"), -100.0, 100.0, 0.0, "%"),
    ]
}

fn params() -> Vec<ParamInfo> {
    let pct = |id, name: &str, d| ParamInfo::continuous(id, name, 0.0, 100.0, d, "%");
    let bi = |id, name: &str, d| ParamInfo::continuous(id, name, -100.0, 100.0, d, "%");
    let toggle_p = |id, name: &str, d: bool| ParamInfo::toggle(id, name, d);
    let dest = |id, name: &str| ParamInfo::stepped(id, name, &DEST_LABELS, 0);
    let mut v: Vec<ParamInfo> = Vec::new();
    for (i, (base, letter)) in OSC_BASE.into_iter().zip(["A", "B", "C", "D"]).enumerate() {
        let n = |s: &str| format!("{letter} {s}");
        v.extend([
            toggle_p(base + O_ON, &n("On"), i == 0),
            ParamInfo::continuous(base + O_COARSE, &n("Coarse"), 0.0, 48.0, 1.0, ""),
            ParamInfo::continuous(base + O_FINE, &n("Fine"), 0.0, 1000.0, 0.0, ""),
            toggle_p(base + O_FIXED, &n("Fixed"), false),
            ParamInfo::skewed(base + O_FREQ, &n("Freq"), 1.0, 1_000.0, 100.0, "Hz", 3.0),
            ParamInfo::stepped(
                base + O_MULTI,
                &n("Multi"),
                &["0.1", "1", "10", "100", "1000"],
                1,
            ),
            ParamInfo::continuous(
                base + O_LEVEL,
                &n("Level"),
                -70.0,
                0.0,
                if i == 0 { 0.0 } else { -70.0 },
                "dB",
            ),
            ParamInfo::stepped(base + O_WAVE, &n("Wave"), &WAVE_NAMES, W_SINE),
            pct(base + O_FEEDBACK, &n("Feedback"), 0.0),
            ParamInfo::continuous(base + O_PHASE, &n("Phase"), 0.0, 360.0, 0.0, "deg"),
            toggle_p(base + O_RETRIG, &n("Retrigger"), false),
            bi(base + O_FREQ_VEL, &n("Osc < Vel"), 0.0),
            toggle_p(base + O_FREQ_Q, &n("Q"), false),
            pct(base + O_LEVEL_VEL, &n("Vel"), 0.0),
            bi(base + O_LEVEL_KEY, &n("Key"), 0.0),
        ]);
        v.extend(env_params(
            base + O_ENV,
            letter,
            "dB",
            DB_FLOOR as f64,
            0.0,
            [DB_FLOOR as f64, 0.0, 0.0],
        ));
    }
    v.extend([
        toggle_p(F_ON, "Filter On", false),
        ParamInfo::stepped(
            F_TYPE,
            "Filter Type",
            &["Low-pass", "High-pass", "Band-pass", "Notch", "Morph"],
            0,
        ),
        ParamInfo::stepped(
            F_CIRCUIT,
            "Circuit Type",
            &["Clean", "OSR", "MS2", "SMP", "PRD"],
            0,
        ),
        ParamInfo::stepped(F_SLOPE, "Filter Slope", &["12 dB", "24 dB"], 0),
        ParamInfo::skewed(F_FREQ, "Filter Freq", 20.0, 20_000.0, 2_000.0, "Hz", 3.0),
        pct(F_RES, "Filter Res", 20.0),
        ParamInfo::continuous(F_DRIVE, "Flt. Drive", 0.0, 24.0, 0.0, "dB"),
        pct(F_MORPH, "Morph", 0.0),
        bi(F_FREQ_VEL, "Freq < Vel", 0.0),
        pct(F_FREQ_KEY, "Freq < Key", 0.0),
        bi(F_ENV_AMT, "Envelope", 0.0),
        ParamInfo::stepped(F_SHAPER, "Shaper", &SHAPERS, 0),
        ParamInfo::continuous(F_SHAPER_DRIVE, "Shp. Drive", -24.0, 24.0, 0.0, "dB"),
        pct(F_DRY_WET, "Dry/Wet", 100.0),
    ]);
    v.extend(env_params(F_ENV, "Fe", "%", 0.0, 100.0, [0.0, 100.0, 0.0]));
    v.extend([
        toggle_p(L_ON, "LFO On", false),
        ParamInfo::stepped(L_WAVE, "LFO Waveform", &LFO_WAVES, LFO_SINE),
        ParamInfo::stepped(L_RANGE, "LFO Range", &["Low", "Hi", "Sync"], 0),
        pct(L_RATE, "LFO Rate", 40.0),
        ParamInfo::stepped(L_SYNC, "LFO Beat Rate", &SyncRate::LABELS, 12),
        toggle_p(L_RETRIG, "LFO Retrigger", false),
        pct(L_AMOUNT, "LFO Amount", 100.0),
        toggle_p(L_DEST_A, "LFO to A", true),
        toggle_p(L_DEST_B_OSC, "LFO to B", false),
        toggle_p(L_DEST_C, "LFO to C", false),
        toggle_p(L_DEST_D, "LFO to D", false),
        toggle_p(L_DEST_FIL, "LFO to FIL", false),
        pct(L_AMT_A, "LFO Amount A", 50.0),
        dest(L_DEST_B, "LFO Destination B"),
        bi(L_AMT_B, "LFO Amount B", 0.0),
        pct(L_AMT_VEL, "LFO Amt < Vel", 0.0),
        pct(L_RATE_KEY, "LFO Rate < Key", 0.0),
    ]);
    v.extend(env_params(
        L_ENV,
        "Le",
        "%",
        0.0,
        100.0,
        [100.0, 100.0, 100.0],
    ));
    v.extend([
        toggle_p(P_ON, "Pitch Envelope On", false),
        bi(P_AMOUNT, "Pitch Env", 100.0),
        pct(P_SPREAD, "Spread", 0.0),
        ParamInfo::continuous(P_TRANSPOSE, "Transpose", -48.0, 48.0, 0.0, "st"),
        toggle_p(P_GLIDE, "Glide", false),
        ParamInfo::skewed(P_GLIDE_TIME, "Glide Time", 0.0, 5_000.0, 80.0, "ms", 3.0),
        toggle_p(P_DEST_A, "Pitch to A", true),
        toggle_p(P_DEST_B_OSC, "Pitch to B", true),
        toggle_p(P_DEST_C, "Pitch to C", true),
        toggle_p(P_DEST_D, "Pitch to D", true),
        toggle_p(P_DEST_LFO, "Pitch to LFO", false),
        pct(P_AMT_A, "Pitch Amount A", 100.0),
        dest(P_DEST_B, "Pitch Destination B"),
        bi(P_AMT_B, "Pitch Amount B", 0.0),
    ]);
    v.extend(env_params(P_ENV, "Pe", "st", -48.0, 48.0, [0.0, 0.0, 0.0]));
    v.extend([
        bi(G_TIME, "Time", 0.0),
        pct(G_TONE, "Tone", 70.0),
        ParamInfo::continuous(G_VOLUME, "Volume", -48.0, 12.0, -6.0, "dB"),
        ParamInfo::stepped(
            G_ALGORITHM,
            "Algorithm",
            &["1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11"],
            0,
        ),
        ParamInfo::continuous(G_VOICES, "Voices", 1.0, VOICES_MAX as f64, 8.0, ""),
        toggle_p(G_RETRIG, "Retrigger", false),
        toggle_p(G_INTERP, "Interpolation", true),
        toggle_p(G_ANTIALIAS, "Antialias", true),
        pct(G_TIME_KEY, "Time < Key", 0.0),
        ParamInfo::continuous(G_PB_RANGE, "PB Range", 0.0, 48.0, 2.0, "st"),
        bi(G_PAN, "Pan", 0.0),
        pct(G_PAN_KEY, "Pan < Key", 0.0),
        pct(G_PAN_RND, "Pan < Rnd", 0.0),
        toggle_p(G_PER_NOTE_PB, "Enable Per-Note Pitch Bend", true),
    ]);
    for (base, name) in
        G_MIDI_BASE
            .into_iter()
            .zip(["Velocity", "Key", "Aftertouch", "Pitch Bend", "Mod Wheel"])
    {
        v.push(dest(base, &format!("{name} Destination A")));
        v.push(bi(base + 1, &format!("{name} Amount A"), 0.0));
        v.push(dest(base + 2, &format!("{name} Destination B")));
        v.push(bi(base + 3, &format!("{name} Amount B"), 0.0));
    }
    v
}

fn env_items(base: u32, end: bool, slopes: bool) -> Vec<Item> {
    let mut v = vec![
        knob(base + E_ATTACK),
        knob(base + E_DECAY),
        knob(base + E_RELEASE),
        knob(base + E_INITIAL),
        knob(base + E_PEAK),
        knob(base + E_SUSTAIN),
    ];
    if end {
        v.push(knob(base + E_END));
    }
    v.extend([
        menu(base + E_LOOP),
        menu(base + E_REPEAT),
        knob(base + E_LOOP_TIME),
        knob(base + E_TIME_VEL),
    ]);
    if slopes {
        v.extend([
            knob(base + E_A_SLOPE),
            knob(base + E_D_SLOPE),
            knob(base + E_R_SLOPE),
        ]);
    }
    v
}

fn layout() -> Vec<Section> {
    let mut sections = vec![Section {
        title: "Operator",
        items: vec![Item::Panel("operator_display")],
    }];
    for (base, title) in OSC_BASE.into_iter().zip([
        "Oscillator A",
        "Oscillator B",
        "Oscillator C",
        "Oscillator D",
    ]) {
        let mut items = vec![
            toggle(base + O_ON),
            knob(base + O_COARSE),
            knob(base + O_FINE),
            toggle(base + O_FIXED),
            knob(base + O_FREQ),
            menu(base + O_MULTI),
            knob(base + O_LEVEL),
            menu(base + O_WAVE),
            knob(base + O_FEEDBACK),
            knob(base + O_PHASE),
            toggle(base + O_RETRIG),
            knob(base + O_FREQ_VEL),
            toggle(base + O_FREQ_Q),
            knob(base + O_LEVEL_VEL),
            knob(base + O_LEVEL_KEY),
        ];
        items.extend(env_items(base + O_ENV, false, false));
        sections.push(Section { title, items });
    }
    let mut filter = vec![
        toggle(F_ON),
        menu(F_TYPE),
        menu(F_CIRCUIT),
        menu(F_SLOPE),
        knob(F_FREQ),
        knob(F_RES),
        knob(F_DRIVE),
        knob(F_MORPH),
        knob(F_FREQ_VEL),
        knob(F_FREQ_KEY),
        knob(F_ENV_AMT),
        menu(F_SHAPER),
        knob(F_SHAPER_DRIVE),
        knob(F_DRY_WET),
    ];
    filter.extend(env_items(F_ENV, true, true));
    sections.push(Section {
        title: "Filter",
        items: filter,
    });
    let mut lfo = vec![
        toggle(L_ON),
        menu(L_WAVE),
        menu(L_RANGE),
        knob(L_RATE),
        menu(L_SYNC),
        toggle(L_RETRIG),
        knob(L_AMOUNT),
        toggle(L_DEST_A),
        toggle(L_DEST_B_OSC),
        toggle(L_DEST_C),
        toggle(L_DEST_D),
        toggle(L_DEST_FIL),
        knob(L_AMT_A),
        menu(L_DEST_B),
        knob(L_AMT_B),
        knob(L_AMT_VEL),
        knob(L_RATE_KEY),
    ];
    lfo.extend(env_items(L_ENV, true, false));
    sections.push(Section {
        title: "LFO",
        items: lfo,
    });
    let mut pitch = vec![
        toggle(P_ON),
        knob(P_AMOUNT),
        knob(P_SPREAD),
        knob(P_TRANSPOSE),
        toggle(P_GLIDE),
        knob(P_GLIDE_TIME),
        toggle(P_DEST_A),
        toggle(P_DEST_B_OSC),
        toggle(P_DEST_C),
        toggle(P_DEST_D),
        toggle(P_DEST_LFO),
        knob(P_AMT_A),
        menu(P_DEST_B),
        knob(P_AMT_B),
    ];
    pitch.extend(env_items(P_ENV, true, true));
    sections.push(Section {
        title: "Pitch",
        items: pitch,
    });
    let mut global = vec![
        knob(G_TIME),
        knob(G_TONE),
        knob(G_VOLUME),
        menu(G_ALGORITHM),
        knob(G_VOICES),
        toggle(G_RETRIG),
        toggle(G_INTERP),
        toggle(G_ANTIALIAS),
        knob(G_TIME_KEY),
        knob(G_PB_RANGE),
        knob(G_PAN),
        knob(G_PAN_KEY),
        knob(G_PAN_RND),
        toggle(G_PER_NOTE_PB),
    ];
    for base in G_MIDI_BASE {
        global.extend([menu(base), knob(base + 1), menu(base + 2), knob(base + 3)]);
    }
    sections.push(Section {
        title: "Global",
        items: global,
    });
    sections
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.operator",
    name: "Operator",
    role: DeviceRole::Instrument,
    category: "Physical Models",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ..Default::default()
    },
    create: |init| Box::new(Operator::new(init.state.as_deref())),
};

// -------------------------------------------------------------------------------- envelopes

#[derive(Clone, Copy, Default)]
struct EnvSpec {
    attack: f32,
    decay: f32,
    release: f32,
    initial: f32,
    peak: f32,
    sustain: f32,
    end: f32,
    attack_slope: f32,
    decay_slope: f32,
    release_slope: f32,
    mode: usize,
    repeat_beats: f64,
    loop_time: f32,
}

/// An envelope with Operator's Loop and Sync behaviour on top of the shared one.
#[derive(Clone, Copy)]
pub struct OpEnv {
    env: Envelope,
    gate: bool,
    settled: EnvelopeSettings,
    sync_next: f64,
    sync_repeat: f64,
    sync_active: bool,
    beat: f64,
    bps: f64,
}

impl OpEnv {
    fn new() -> Self {
        Self {
            env: Envelope::new(),
            gate: false,
            settled: EnvelopeSettings::default(),
            sync_next: f64::MAX,
            sync_repeat: 1.0,
            sync_active: false,
            beat: 0.0,
            bps: 0.0,
        }
    }

    fn prepare(&mut self, sample_rate: f64) {
        self.env.prepare(sample_rate);
    }

    fn apply(&mut self, spec: &EnvSpec, time_scale: f32, clock: &BeatClock) {
        let mut s = EnvelopeSettings {
            initial: spec.initial,
            peak: spec.peak,
            sustain: spec.sustain,
            end: spec.end,
            attack: spec.attack * time_scale,
            decay: spec.decay * time_scale,
            release: spec.release * time_scale,
            attack_slope: spec.attack_slope,
            decay_slope: spec.decay_slope,
            release_slope: spec.release_slope,
            loop_mode: LoopMode::Off,
            loop_span: LoopSpan::AttackDecay,
            loop_time: 0.0,
            repeat_beats: spec.repeat_beats,
        };
        self.settled = s;
        if spec.mode == ENV_LOOP_TRIGGER {
            self.settled.loop_mode = LoopMode::Trigger;
        }
        self.sync_active = false;
        match spec.mode {
            ENV_LOOP_LOOP if self.gate => {
                // Past the sustain level it falls back to Initial in Loop Time, then attacks from
                // wherever that left it. At zero Loop Time it attacks straight from Sustain.
                s.loop_mode = LoopMode::Loop;
                if spec.loop_time > 0.0 {
                    s.loop_span = LoopSpan::AttackDecayRelease;
                    s.release = spec.loop_time * time_scale;
                    s.end = spec.initial;
                }
            }
            ENV_LOOP_BEAT => s.loop_mode = LoopMode::Beat,
            ENV_LOOP_SYNC if self.gate && clock.playing => {
                self.sync_active = true;
                self.sync_repeat = spec.repeat_beats;
            }
            ENV_LOOP_SYNC => s.loop_mode = LoopMode::Beat,
            ENV_LOOP_TRIGGER => s.loop_mode = LoopMode::Trigger,
            _ => {}
        }
        self.beat = clock.beat;
        self.bps = clock.beats_per_sample;
        self.env.set(&s);
        self.env.set_clock(clock);
    }

    fn gate_on(&mut self, spec: &EnvSpec, clock: &BeatClock) {
        self.gate = true;
        self.apply(spec, 1.0, clock);
        self.env.gate_on();
        // Sync's first repeat lands on the nearest 16th note; the rest follow from it.
        self.sync_next = ((clock.beat + spec.repeat_beats) * 4.0).round() / 4.0;
        self.sync_repeat = spec.repeat_beats;
    }

    fn gate_off(&mut self) {
        self.gate = false;
        self.sync_active = false;
        self.env.set(&self.settled);
        self.env.gate_off();
    }

    fn kill(&mut self) {
        self.gate = false;
        self.sync_active = false;
        self.env.kill();
    }

    #[inline]
    fn next(&mut self) -> f32 {
        if self.sync_active {
            if self.beat >= self.sync_next {
                self.env.gate_on();
                self.sync_next += self.sync_repeat.max(1e-6);
            }
            self.beat += self.bps;
        }
        self.env.next()
    }

    fn advance(&mut self, frames: usize) -> f32 {
        let mut last = self.env.level();
        for _ in 0..frames {
            last = self.next();
        }
        last
    }

    fn is_active(&self) -> bool {
        self.env.is_active()
    }

    fn level(&self) -> f32 {
        self.env.level()
    }
}

// ----------------------------------------------------------------------------------- voices

#[derive(Clone, Copy)]
enum WaveReader {
    Sine,
    Table(usize),
    User(usize),
    NoiseLoop,
    NoiseWhite,
}

#[derive(Clone, Copy, Default)]
struct OpPlan {
    on: bool,
    gain: f32,
    increment: f32,
    feedback: f32,
    level: usize,
    lfo_on: bool,
}

struct Voice {
    active: bool,
    note: NoteInfo,
    key_center: f32,
    velocity: f32,
    pan_random: f32,
    env: [OpEnv; 4],
    filter_env: OpEnv,
    pitch_env: OpEnv,
    lfo_env: OpEnv,
    phase: [f32; 4],
    feedback: [[f32; 2]; 4],
    lfo_phase: f64,
    lfo_held: f32,
    lfo_band: Biquad,
    lfo_value: f32,
    filter: Filter,
    bus: ModBus,
    rng: Rng,
    fade: StealFade,
    pan_gains: [f32; 2],
    plan: [OpPlan; 4],
    lfo_dt: f32,
    lfo_level: usize,
    lfo_depth: f32,
    lfo_linear: bool,
    lfo_linear_depth: f32,
    lfo_wave: usize,
    lfo_on: bool,
    lfo_sync_beats: Option<f64>,
    shaper: usize,
    shaper_drive: f32,
    shaper_mix: f32,
    filter_on: bool,
    fm_index_scale: [f32; 4],
    level_trim: f32,
}

impl Voice {
    fn new(seed: u32, matrix: &ModMatrix) -> Self {
        let mut rng = Rng::new(seed);
        let phases = [
            rng.unipolar(),
            rng.unipolar(),
            rng.unipolar(),
            rng.unipolar(),
        ];
        Self {
            active: false,
            note: NoteInfo {
                key: 0,
                channel: 0,
                note_id: 0,
                velocity: 0.0,
                unison_index: 0,
                unison_count: 1,
                spread: 0.0,
            },
            key_center: 0.0,
            velocity: 0.0,
            pan_random: 0.0,
            env: [OpEnv::new(); 4],
            filter_env: OpEnv::new(),
            pitch_env: OpEnv::new(),
            lfo_env: OpEnv::new(),
            phase: phases,
            feedback: [[0.0; 2]; 4],
            lfo_phase: 0.0,
            lfo_held: 0.0,
            lfo_band: Biquad::default(),
            lfo_value: 0.0,
            filter: Filter::new(),
            bus: matrix.bus(),
            rng,
            fade: StealFade::default(),
            pan_gains: [0.0; 2],
            plan: [OpPlan::default(); 4],
            lfo_dt: 0.0,
            lfo_level: 0,
            lfo_depth: 0.0,
            lfo_linear: false,
            lfo_linear_depth: 0.0,
            lfo_wave: 0,
            lfo_on: false,
            lfo_sync_beats: None,
            shaper: 0,
            shaper_drive: 1.0,
            shaper_mix: 0.0,
            filter_on: false,
            fm_index_scale: [1.0; 4],
            level_trim: 1.0,
        }
    }

    fn silence(&mut self) {
        self.active = false;
        for e in &mut self.env {
            e.kill();
        }
        self.filter_env.kill();
        self.pitch_env.kill();
        self.lfo_env.kill();
        self.filter.reset();
        self.feedback = [[0.0; 2]; 4];
        self.fade.clear();
    }
}

/// 2^x for the modest exponents in pitch modulation, without a log call.
#[inline]
fn exp2_fast(x: f32) -> f32 {
    let floor = x.floor();
    let f = x - floor;
    let p = 1.0 + f * (LN_2 + f * (0.240_226 + f * (0.055_504 + f * 0.009_618)));
    p * 2f32.powi(floor as i32)
}

/// Operator. See the [module docs](self).
pub struct Operator {
    values: [f64; PARAMS],
    sample_rate: f32,
    alloc: VoiceAllocator,
    voices: Vec<Voice>,
    oversamplers: Vec<Oversampler>,
    matrix: ModMatrix,
    routes_dirty: bool,
    waves: Arc<ParsedWaves>,
    pb: f32,
    wheel: f32,
    clock: BeatClock,
    readers: [WaveReader; 4],
    interp: bool,
    antialias: bool,
    algorithm: Algorithm,
}

impl Operator {
    pub fn new(state: Option<&str>) -> Self {
        let mut values = [0.0; PARAMS];
        for p in params() {
            values[p.id as usize] = p.default;
        }
        Self {
            values,
            sample_rate: 48_000.0,
            alloc: VoiceAllocator::new(VOICES_MAX * 2),
            voices: Vec::new(),
            oversamplers: Vec::new(),
            matrix: ModMatrix::new(SOURCES, DEST_LABELS.len(), ROUTES),
            routes_dirty: true,
            waves: Arc::new(ParsedWaves::parse(state)),
            pb: 0.0,
            wheel: 0.0,
            clock: BeatClock::default(),
            readers: [WaveReader::Sine; 4],
            interp: true,
            antialias: true,
            algorithm: ALGORITHMS[0],
        }
    }

    fn v(&self, id: u32) -> f32 {
        self.values[id as usize] as f32
    }

    fn on(&self, id: u32) -> bool {
        self.values[id as usize] >= 0.5
    }

    fn sync_settings(&mut self) {
        let voices = (self.v(G_VOICES).round() as usize).clamp(1, VOICES_MAX);
        let spread = self.v(P_SPREAD) > 0.0;
        self.alloc.set(&VoiceSettings {
            mode: if voices == 1 {
                VoiceMode::Legato
            } else {
                VoiceMode::Poly
            },
            polyphony: voices,
            unison: if spread { 2 } else { 1 },
            steal: StealMode::Oldest,
            retrigger: self.on(G_RETRIG),
            glide: if self.on(P_GLIDE) {
                GlideMode::Always
            } else {
                GlideMode::Off
            },
            glide_curve: GlideCurve::ConstantTime,
            glide_time: self.v(P_GLIDE_TIME) * 0.001,
            bend_range: self.v(G_PB_RANGE),
            ..Default::default()
        });
    }

    fn sync_routes(&mut self) {
        let route = |source, dest: f32, amount: f32| {
            let dest = dest.round() as usize;
            (dest != 0).then_some(Route {
                source,
                dest,
                amount: amount * 0.01,
                kind: RouteKind::Add,
                via: None,
            })
        };
        let mut slot = 0;
        for (i, source) in [SRC_VEL, SRC_KEY, SRC_AT, SRC_PB, SRC_MW]
            .into_iter()
            .enumerate()
        {
            let base = G_MIDI_BASE[i];
            self.matrix
                .set_route(slot, route(source, self.v(base), self.v(base + 1)));
            self.matrix
                .set_route(slot + 1, route(source, self.v(base + 2), self.v(base + 3)));
            slot += 2;
        }
        self.matrix
            .set_route(slot, route(SRC_LFO, self.v(L_DEST_B), self.v(L_AMT_B)));
        self.matrix
            .set_route(slot + 1, route(SRC_PE, self.v(P_DEST_B), self.v(P_AMT_B)));
    }

    fn sync_waves(&mut self) {
        for (i, base) in OSC_BASE.into_iter().enumerate() {
            let wave = self.v(base + O_WAVE).round() as usize;
            self.readers[i] = match wave {
                W_SINE => WaveReader::Sine,
                W_NOISE_LOOP => WaveReader::NoiseLoop,
                W_NOISE_WHITE => WaveReader::NoiseWhite,
                W_USER => WaveReader::User(i),
                other => WaveReader::Table(other),
            };
        }
        self.interp = self.on(G_INTERP);
        self.antialias = self.on(G_ANTIALIAS);
        self.algorithm =
            ALGORITHMS[(self.v(G_ALGORITHM).round() as usize).min(ALGORITHMS.len() - 1)];
    }

    fn env_spec(&self, base: u32, dbs: bool, fixed_shape: bool) -> EnvSpec {
        let g = |id: u32| self.v(base + id);
        let level = |x: f32| {
            if dbs {
                level_gain(x)
            } else {
                x
            }
        };
        let slope = |id: u32| g(id) * 0.01;
        EnvSpec {
            attack: g(E_ATTACK) * 0.001,
            decay: g(E_DECAY) * 0.001,
            release: g(E_RELEASE) * 0.001,
            initial: level(g(E_INITIAL)),
            peak: level(g(E_PEAK)),
            sustain: level(g(E_SUSTAIN)),
            end: if dbs { 0.0 } else { level(g(E_END)) },
            // The oscillators' envelopes: linear attack, exponential decay and release.
            attack_slope: if fixed_shape { 0.0 } else { slope(E_A_SLOPE) },
            decay_slope: if fixed_shape { 0.8 } else { slope(E_D_SLOPE) },
            release_slope: if fixed_shape { 0.8 } else { slope(E_R_SLOPE) },
            mode: g(E_LOOP).round() as usize,
            repeat_beats: SyncRate::from_index(self.values[(base + E_REPEAT) as usize]).beats(),
            loop_time: g(E_LOOP_TIME) * 0.001,
        }
    }

    fn start(&mut self, i: usize, note: NoteInfo) {
        let clock = self.clock;
        let specs = [
            self.env_spec(OSC_BASE[0] + O_ENV, true, true),
            self.env_spec(OSC_BASE[1] + O_ENV, true, true),
            self.env_spec(OSC_BASE[2] + O_ENV, true, true),
            self.env_spec(OSC_BASE[3] + O_ENV, true, true),
        ];
        let filter = self.env_spec(F_ENV, false, false);
        let pitch = self.env_spec(P_ENV, false, false);
        let lfo = self.env_spec(L_ENV, false, true);
        let retrig_lfo = self.on(L_RETRIG);
        let values = self.values;
        let phases: [(bool, f32); 4] = OSC_BASE.map(|base| {
            (
                values[(base + O_RETRIG) as usize] >= 0.5,
                values[(base + O_PHASE) as usize] as f32 / 360.0,
            )
        });
        let voice = &mut self.voices[i];
        voice.silence();
        voice.active = true;
        voice.note = note;
        voice.key_center = note.key as f32 - 60.0;
        voice.velocity = note.velocity;
        voice.pan_random = voice.rng.bipolar();
        for (e, s) in voice.env.iter_mut().zip(&specs) {
            e.gate_on(s, &clock);
        }
        voice.filter_env.gate_on(&filter, &clock);
        voice.pitch_env.gate_on(&pitch, &clock);
        voice.lfo_env.gate_on(&lfo, &clock);
        for (op, (retrig, phase)) in phases.into_iter().enumerate() {
            if retrig {
                voice.phase[op] = phase;
            }
        }
        if retrig_lfo {
            voice.lfo_phase = 0.0;
            voice.lfo_band.reset();
        }
    }

    fn release(&mut self, i: usize) {
        let voice = &mut self.voices[i];
        for e in &mut voice.env {
            e.gate_off();
        }
        voice.filter_env.gate_off();
        voice.pitch_env.gate_off();
        voice.lfo_env.gate_off();
    }

    fn time_scale(&self, time_vel_id: u32, velocity: f32, key_center: f32, mod_time: f32) -> f32 {
        let global = (self.v(G_TIME) * 0.01 + mod_time).clamp(-1.0, 1.0);
        let key = self.v(G_TIME_KEY) * 0.01 * key_center / 12.0;
        let vel = self.v(time_vel_id) * 0.01 * velocity;
        2f32.powf(global * 3.0 - key - vel)
    }

    fn control(&mut self, i: usize) {
        let sr = self.sample_rate;
        let os = if self.antialias { OS_MULTIPLY } else { 1 };
        let clock = self.clock;
        let values = self.values;
        let g = |id: u32| values[id as usize] as f32;
        let pc = |id: u32| values[id as usize] as f32 * 0.01;
        let flag = |id: u32| values[id as usize] >= 0.5;

        let alloc = *self.alloc.voice(i);
        let pressure = alloc.pressure.max(self.alloc.channel_pressure(i));
        let (velocity, key_center) = (self.voices[i].velocity, self.voices[i].key_center);
        let key_norm = key_center / 60.0;
        let lfo_out = {
            let v = &self.voices[i];
            v.lfo_value
                * pc(L_AMOUNT)
                * (1.0 - pc(L_AMT_VEL) * (1.0 - velocity))
                * v.lfo_env.level()
        };
        let pe_on = flag(P_ON);
        let pe_norm = if pe_on {
            self.voices[i].pitch_env.level() / 48.0 * pc(P_AMOUNT)
        } else {
            0.0
        };
        let mut sources = [0.0f32; SOURCES];
        sources[SRC_VEL] = velocity;
        sources[SRC_KEY] = key_norm;
        sources[SRC_AT] = pressure;
        sources[SRC_PB] = self.pb;
        sources[SRC_MW] = self.wheel;
        sources[SRC_LFO] = lfo_out;
        sources[SRC_PE] = pe_norm;
        {
            let (matrix, voices) = (&self.matrix, &mut self.voices);
            matrix.evaluate(&sources, &mut voices[i].bus);
        }

        let bus_time = self.voices[i].bus.offset(D_TIME);
        // Time, Time < Key and each envelope's own Time < Vel, then advance the slow ones.
        let oscillator_scale = [
            self.time_scale(
                OSC_BASE[0] + O_ENV + E_TIME_VEL,
                velocity,
                key_center,
                bus_time,
            ),
            self.time_scale(
                OSC_BASE[1] + O_ENV + E_TIME_VEL,
                velocity,
                key_center,
                bus_time,
            ),
            self.time_scale(
                OSC_BASE[2] + O_ENV + E_TIME_VEL,
                velocity,
                key_center,
                bus_time,
            ),
            self.time_scale(
                OSC_BASE[3] + O_ENV + E_TIME_VEL,
                velocity,
                key_center,
                bus_time,
            ),
        ];
        let filter_scale = self.time_scale(F_ENV + E_TIME_VEL, velocity, key_center, bus_time);
        let pitch_scale = self.time_scale(P_ENV + E_TIME_VEL, velocity, key_center, bus_time);
        let lfo_scale = self.time_scale(L_ENV + E_TIME_VEL, velocity, key_center, bus_time);

        let oscillator_specs = [
            self.env_spec(OSC_BASE[0] + O_ENV, true, true),
            self.env_spec(OSC_BASE[1] + O_ENV, true, true),
            self.env_spec(OSC_BASE[2] + O_ENV, true, true),
            self.env_spec(OSC_BASE[3] + O_ENV, true, true),
        ];
        let filter_spec = self.env_spec(F_ENV, false, false);
        let pitch_spec = self.env_spec(P_ENV, false, false);
        let lfo_spec = self.env_spec(L_ENV, false, true);
        let tone = (pc(G_TONE) + self.voices[i].bus.offset(D_TONE)).clamp(0.0, 1.0);
        let antialias = self.antialias;
        let bent = self.alloc.pitch(i);
        let transposed = bent + g(P_TRANSPOSE);
        let filter_on = flag(F_ON);

        let table_for_level = builtin_table(W_SINE);
        let voice = &mut self.voices[i];
        for ((env, spec), scale) in voice
            .env
            .iter_mut()
            .zip(&oscillator_specs)
            .zip(oscillator_scale)
        {
            env.apply(spec, scale, &clock);
        }
        voice.filter_env.apply(&filter_spec, filter_scale, &clock);
        voice.pitch_env.apply(&pitch_spec, pitch_scale, &clock);
        voice.lfo_env.apply(&lfo_spec, lfo_scale, &clock);
        let filter_level = voice.filter_env.advance(CHUNK) * 0.01;
        let pitch_level = voice.pitch_env.advance(CHUNK);
        let lfo_level = voice.lfo_env.advance(CHUNK) * 0.01;

        // Pitch envelope: at 100% the levels are the pitch change in semitones.
        let bus = &voice.bus;
        let pe_amount = (pc(P_AMOUNT) + bus.offset(D_PE_AMOUNT)).clamp(-2.0, 2.0);
        let pe_semis = if pe_on {
            pitch_level * pe_amount * pc(P_AMT_A)
        } else {
            0.0
        };

        // Spread: the note's two voices, one left and one right, detuned against each other.
        let spread = voice.note.spread * pc(P_SPREAD) * 0.5;
        let lfo_semitones = if flag(L_ON) {
            lfo_level
                * (pc(L_AMOUNT) + bus.offset(D_LFO_AMOUNT)).max(0.0)
                * (1.0 - pc(L_AMT_VEL) * (1.0 - velocity))
                * pc(L_AMT_A)
                * 12.0
        } else {
            0.0
        };

        let carrier_mod = |op: usize| 1.0 + bus.offset(D_VOL[op]);
        let xfade_ac = bus.offset(D_XFADE_AC).clamp(-1.0, 1.0);
        let xfade_bd = bus.offset(D_XFADE_BD).clamp(-1.0, 1.0);
        let xfade = [
            1.0 - xfade_ac.max(0.0),
            1.0 - xfade_bd.max(0.0),
            1.0 + xfade_ac.min(0.0),
            1.0 + xfade_bd.min(0.0),
        ];
        let fm_drive = 1.0 + bus.offset(D_FM_DRIVE);
        let feedback_mod = bus.offset(D_FEEDBACK);
        let algorithm = self.algorithm;
        let is_modulator = [
            algorithm.modulators.iter().any(|m| m & A != 0),
            algorithm.modulators.iter().any(|m| m & B != 0),
            algorithm.modulators.iter().any(|m| m & C != 0),
            algorithm.modulators.iter().any(|m| m & D != 0),
        ];
        let fixed_mod = 48.0 * bus.offset(D_FIXED);
        let pitch_flags = [
            flag(P_DEST_A),
            flag(P_DEST_B_OSC),
            flag(P_DEST_C),
            flag(P_DEST_D),
        ];
        let lfo_flags = [
            flag(L_DEST_A),
            flag(L_DEST_B_OSC),
            flag(L_DEST_C),
            flag(L_DEST_D),
        ];

        for (op, base) in OSC_BASE.into_iter().enumerate() {
            let plan = &mut voice.plan[op];
            plan.on = flag(base + O_ON);
            let level = level_gain(g(base + O_LEVEL));
            let vel_scale = 1.0 - pc(base + O_LEVEL_VEL) * (1.0 - velocity);
            let key_scale = 1.0 + pc(base + O_LEVEL_KEY) * key_norm;
            let mut gain =
                (level * vel_scale * key_scale.max(0.0) * carrier_mod(op).max(0.0) * xfade[op])
                    .max(0.0);
            if is_modulator[op] {
                gain *= fm_drive.max(0.0);
            }
            plan.gain = gain;
            plan.feedback = if algorithm.modulators[op] == 0 {
                (pc(base + O_FEEDBACK) + feedback_mod).clamp(0.0, 1.0) * FEEDBACK_SCALE
            } else {
                0.0
            };
            let pe = if pitch_flags[op] { pe_semis } else { 0.0 };
            let hz = if flag(base + O_FIXED) {
                let multi =
                    [0.1, 1.0, 10.0, 100.0, 1000.0][(g(base + O_MULTI).round() as usize).min(4)];
                g(base + O_FREQ) * multi * 2f32.powf((fixed_mod + pe) / 12.0)
            } else {
                let mut coarse = g(base + O_COARSE).round();
                let shift = pc(base + O_FREQ_VEL) * velocity * coarse.max(1.0);
                if flag(base + O_FREQ_Q) {
                    coarse = (coarse + shift).round().max(0.0);
                } else {
                    coarse = (coarse + shift).max(0.0);
                }
                let ratio = if coarse <= 0.0 { 0.5 } else { coarse };
                // The tuning seam: every oscillator's frequency comes from one note-to-Hz call.
                note_hz(transposed + spread + pe) * ratio * (1.0 + g(base + O_FINE) * 0.001)
            };
            let increment = (hz.clamp(0.1, sr * 0.49) / (sr * os as f32)).min(0.49);
            plan.increment = increment;
            plan.lfo_on = lfo_flags[op] && lfo_semitones != 0.0;
            // Tone reads duller tables and thins out the modulators of higher notes.
            let reach = 1.0 + (1.0 - tone) * 31.0;
            // With Antialias, oscillators stop short of the band the decimator can't keep out.
            let band_limit = hz / (0.84 * sr);
            let dt = if antialias || tone < 1.0 {
                increment.max(band_limit) * reach
            } else {
                1e-6
            };
            plan.level = table_for_level.level_for(dt);
            voice.fm_index_scale[op] =
                1.0 / (1.0 + (hz / (1_500.0 * 2f32.powf(tone * 6.0))).powi(2));
        }
        voice.lfo_on = flag(L_ON);
        voice.lfo_depth = lfo_semitones / 12.0;
        voice.lfo_linear_depth = 2f32.powf(voice.lfo_depth) - 1.0;
        voice.lfo_wave = (g(L_WAVE).round() as usize).min(LFO_WAVES.len() - 1);

        let mut lfo_rate_octaves =
            bus.offset(D_LFO_RATE) * 5.0 + pc(L_RATE_KEY) * key_center / 12.0;
        if pe_on && flag(P_DEST_LFO) {
            lfo_rate_octaves += pe_semis / 12.0;
        }
        let range = g(L_RANGE).round() as usize;
        voice.lfo_linear = range == 1;
        let lfo_hz = match range {
            0 => 0.02 * (30.0f32 / 0.02).powf(pc(L_RATE)),
            1 => 8.0 * (12_000.0f32 / 8.0).powf(pc(L_RATE)),
            _ => {
                let beats = SyncRate::from_index(values[L_SYNC as usize]).beats();
                1.0 / (beats as f32 * clock.beats_per_sample as f32 * sr).max(1e-6)
            }
        };
        let lfo_hz = (lfo_hz * 2f32.powf(lfo_rate_octaves)).clamp(0.001, sr * 0.49);
        voice.lfo_dt = lfo_hz / (sr * os as f32);
        voice.lfo_level = table_for_level.level_for(voice.lfo_dt);
        voice.lfo_sync_beats = (range == 2 && !flag(L_RETRIG))
            .then(|| SyncRate::from_index(values[L_SYNC as usize]).beats());
        if voice.lfo_wave == LFO_NOISE {
            voice.lfo_band.set(
                BiquadKind::BandPass,
                lfo_hz.min(sr * 0.4),
                2.0,
                0.0,
                sr * os as f32,
            );
        }

        // Filter and shaper.
        voice.filter_on = filter_on;
        if filter_on {
            let bus = &voice.bus;
            let kind = g(F_TYPE).round() as usize;
            let circuit_choice = g(F_CIRCUIT).round() as usize;
            let (kind, is_morph) = match kind {
                1 => (FilterKind::HighPass, false),
                2 => (FilterKind::BandPass, false),
                3 => (FilterKind::Notch, false),
                4 => (FilterKind::Morph, true),
                _ => (FilterKind::LowPass, false),
            };
            let lp_hp = matches!(kind, FilterKind::LowPass | FilterKind::HighPass);
            let circuit = match circuit_choice {
                1 => Circuit::Osr,
                2 if lp_hp => Circuit::Ms2,
                3 if lp_hp => Circuit::Smp,
                4 if lp_hp => Circuit::Prd,
                _ => Circuit::Svf,
            };
            let env_amount = (pc(F_ENV_AMT) + bus.offset(D_FILTER_ENV)).clamp(-2.0, 2.0);
            let mut octaves = env_amount * filter_level * 9.0
                + pc(F_FREQ_VEL) * velocity * 5.0
                + pc(F_FREQ_KEY) * key_center / 12.0
                + bus.offset(D_FILTER_FREQ) * 5.0;
            if flag(L_DEST_FIL) && voice.lfo_on {
                octaves += voice.lfo_value * lfo_level * pc(L_AMOUNT) * pc(L_AMT_A) * 4.0;
            }
            let cutoff = (g(F_FREQ) * 2f32.powf(octaves)).clamp(20.0, sr * 0.45);
            let resonance =
                (pc(F_RES) + bus.offset(D_FILTER_RES) + bus.offset(D_FILTER_Q)).clamp(0.0, 1.0);
            // Drive exists for the low-pass, high-pass and band-pass with any circuit but Clean.
            let drive_allowed = circuit != Circuit::Svf && !is_morph && kind != FilterKind::Notch;
            let drive = if drive_allowed {
                (g(F_DRIVE) + bus.offset(D_FILTER_DRIVE) * 24.0).clamp(0.0, 36.0)
            } else {
                0.0
            };
            voice.filter.set(&FilterSettings {
                circuit,
                kind,
                slope: if g(F_SLOPE).round() as usize == 1 {
                    FilterSlope::Db24
                } else {
                    FilterSlope::Db12
                },
                cutoff,
                resonance: resonance * 0.98,
                drive,
                morph: (pc(F_MORPH) + bus.offset(D_FILTER_MORPH)).rem_euclid(1.0),
            });
            voice.shaper = g(F_SHAPER).round() as usize;
            voice.shaper_drive = db_to_gain(g(F_SHAPER_DRIVE) + bus.offset(D_SHAPER_DRIVE) * 24.0);
            voice.shaper_mix = if voice.shaper == 0 {
                0.0
            } else {
                pc(F_DRY_WET)
            };
        }

        // The output.
        let bus = &voice.bus;
        let pan = (pc(G_PAN)
            + pc(G_PAN_KEY) * key_norm
            + pc(G_PAN_RND) * voice.pan_random
            + bus.offset(D_PAN))
        .clamp(-1.0, 1.0);
        let (spread_left, spread_right) = if alloc.note.unison_count == 2 {
            if alloc.note.unison_index == 0 {
                (1.0, 0.0)
            } else {
                (0.0, 1.0)
            }
        } else {
            (1.0, 1.0)
        };
        let angle = (pan + 1.0) * PI * 0.25;
        voice.pan_gains = [
            angle.cos() * 1.414 * spread_left,
            angle.sin() * 1.414 * spread_right,
        ];
        voice.level_trim = 1.0 + bus.offset(D_VOLUME);

        let lfo_on = voice.lfo_on;
        if lfo_on {
            if let Some(beats) = voice.lfo_sync_beats {
                if clock.playing {
                    voice.lfo_phase = (clock.beat / beats).rem_euclid(1.0);
                }
            }
        }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        if self.routes_dirty {
            self.routes_dirty = false;
            self.sync_routes();
            self.sync_waves();
        }
        let frames = left.len().min(right.len());
        let mut at = 0;
        while at < frames {
            let n = (frames - at).min(CHUNK);
            self.alloc.advance(n);
            let (mut mix_l, mut mix_r) = ([0.0f32; CHUNK], [0.0f32; CHUNK]);
            for i in 0..self.voices.len() {
                if !self.voices[i].active {
                    continue;
                }
                if let Some(next) = self.voices[i].fade.poll() {
                    match next {
                        Some(note) => self.start(i, note),
                        None => {
                            self.voices[i].silence();
                            continue;
                        }
                    }
                }
                self.control(i);
                self.render_voice(i, &mut mix_l[..n], &mut mix_r[..n]);
                let done = !self.voices[i].env.iter().any(|e| e.is_active());
                let held = self.alloc.voice(i).state == VoiceState::Held;
                if done && !held && !self.voices[i].fade.is_fading() {
                    self.voices[i].silence();
                    self.alloc.finished(i);
                }
            }
            for (out, mix) in left[at..at + n].iter_mut().zip(&mix_l[..n]) {
                *out += limit(*mix);
            }
            for (out, mix) in right[at..at + n].iter_mut().zip(&mix_r[..n]) {
                *out += limit(*mix);
            }
            self.clock.advance(n);
            at += n;
        }
    }

    fn render_voice(&mut self, i: usize, left: &mut [f32], right: &mut [f32]) {
        let antialias = self.antialias;
        let n = left.len().min(right.len());
        let volume = db_to_gain(self.v(G_VOLUME));
        let zeros = [0.0f32; CHUNK];
        let mut mono = [0.0f32; CHUNK];
        let waves = self.waves.clone();
        let readers = self.readers;
        let interp = self.interp;
        let algorithm = self.algorithm;
        let voice = &mut self.voices[i];
        if antialias {
            self.oversamplers[i].process(&zeros[..n], &mut mono[..n], |hi| {
                render_fm(voice, hi, OS_MULTIPLY, &algorithm, &readers, &waves, interp);
            });
        } else {
            render_fm(
                voice,
                &mut mono[..n],
                1,
                &algorithm,
                &readers,
                &waves,
                interp,
            );
        }
        for (s, (l, r)) in mono[..n].iter().zip(left.iter_mut().zip(right.iter_mut())) {
            let mut y = *s;
            if voice.filter_on {
                y = voice.filter.process(y);
                if voice.shaper_mix > 0.0 {
                    let shaped = shape(voice.shaper, y * voice.shaper_drive);
                    y += (shaped - y) * voice.shaper_mix;
                }
            }
            let y = sanitize(y * voice.level_trim.max(0.0) * volume) * voice.fade.gain();
            *l += y * voice.pan_gains[0];
            *r += y * voice.pan_gains[1];
        }
    }

    fn midi(&mut self, message: MidiMessage) {
        match message {
            MidiMessage::PitchBend { value, .. } => self.pb = value,
            MidiMessage::ControlChange { cc: 1, value, .. } => self.wheel = value as f32 / 127.0,
            MidiMessage::NoteExpr {
                kind: ExprKind::Pitch,
                ..
            } if !self.on(G_PER_NOTE_PB) => return,
            _ => {}
        }
        let message = scale_note_bend(message, rlive_core::MPE_BEND_RANGE);
        let mut starts: [Option<(usize, NoteInfo, bool)>; 16] = [None; 16];
        let mut releases: [Option<usize>; 64] = [None; 64];
        let (mut started, mut released) = (0, 0);
        let (alloc, voices) = (&mut self.alloc, &mut self.voices);
        alloc.handle(message, |event| match event {
            VoiceEvent::Start {
                voice,
                note,
                stolen,
            } => {
                if started < starts.len() {
                    starts[started] = Some((voice, note, stolen));
                    started += 1;
                }
            }
            VoiceEvent::Release { voice } => {
                if released < releases.len() {
                    releases[released] = Some(voice);
                    released += 1;
                }
            }
            VoiceEvent::Kill { voice } => voices[voice].fade.start(None),
            VoiceEvent::Legato { voice, note } => {
                voices[voice].note = note;
                voices[voice].key_center = note.key as f32 - 60.0;
                voices[voice].velocity = note.velocity;
            }
        });
        for voice in releases.into_iter().flatten() {
            self.release(voice);
        }
        for (voice, note, stolen) in starts.into_iter().flatten() {
            if stolen && self.voices[voice].active {
                self.voices[voice].fade.start(Some(note));
            } else {
                self.start(voice, note);
            }
        }
    }

    fn publish(&self, ctx: &ProcessContext) {
        let mut view = [0.0f32; VIEW_LEN];
        if let Some(v) = self.voices.iter().find(|v| v.active) {
            for (slot, env) in v.env.iter().enumerate() {
                view[slot] = env.level();
            }
            view[4] = v.filter_env.level();
            view[5] = v.pitch_env.level();
            view[6] = v.lfo_env.level();
        }
        ctx.view.publish(VIEW, &view);
    }
}

/// The waveshaper after the filter. Names and curves are ours; the manual only says it has a chooser.
#[inline]
fn shape(kind: usize, x: f32) -> f32 {
    match kind {
        1 => x.tanh(),
        2 => x.clamp(-1.0, 1.0),
        3 => (x * std::f32::consts::FRAC_PI_2).sin(),
        4 => (x.clamp(-1.0, 1.0) * 7.5).round() / 7.5,
        _ => x,
    }
}

/// Fills `out`, at `oversampling` times the base rate, with the voice's four oscillators.
#[allow(clippy::too_many_arguments)]
fn render_fm(
    voice: &mut Voice,
    out: &mut [f32],
    oversampling: usize,
    algorithm: &Algorithm,
    readers: &[WaveReader; 4],
    waves: &ParsedWaves,
    interp: bool,
) {
    let sine = sine_table();
    let noise = noise_loop();
    let sine_len = SINE_LEN as f32;
    let plan = voice.plan;
    let index_scale = voice.fm_index_scale;
    for frame in out.chunks_mut(oversampling) {
        let amp = [
            voice.env[0].next() * plan[0].gain,
            voice.env[1].next() * plan[1].gain,
            voice.env[2].next() * plan[2].gain,
            voice.env[3].next() * plan[3].gain,
        ];
        for sample in frame.iter_mut() {
            let lfo_mult = if voice.lfo_on {
                let v = lfo_tick(voice);
                voice.lfo_value = v;
                if voice.lfo_linear {
                    // At audio rates it's an operator: linear FM, so the note keeps its pitch.
                    1.0 + v * voice.lfo_linear_depth
                } else {
                    exp2_fast(v * voice.lfo_depth)
                }
            } else {
                1.0
            };
            let mut outs = [0.0f32; 4];
            for op in (0..4).rev() {
                let p = &plan[op];
                if !p.on {
                    // Oscillators that are off don't sound, but still cost, as the manual says.
                    voice.phase[op] = (voice.phase[op] + p.increment).fract();
                    continue;
                }
                let mut pm = 0.0;
                let mods = algorithm.modulators[op];
                if mods != 0 {
                    for (m, o) in outs.iter().enumerate() {
                        if mods & (1 << m) != 0 {
                            pm += *o * index_scale[m];
                        }
                    }
                    pm *= FM_INDEX_AT_FULL / TAU;
                }
                if p.feedback != 0.0 {
                    let fb = 0.5 * (voice.feedback[op][0] + voice.feedback[op][1]);
                    pm += fb * p.feedback / TAU;
                }
                let phase = voice.phase[op] + pm;
                let phase = phase - phase.floor();
                let y = match readers[op] {
                    WaveReader::Sine => {
                        let at = phase * sine_len;
                        let j = (at as usize).min(SINE_LEN - 1);
                        if interp {
                            let t = at - j as f32;
                            sine[j] + (sine[j + 1] - sine[j]) * t
                        } else {
                            sine[j]
                        }
                    }
                    WaveReader::Table(w) => read_table(builtin_table(w), p.level, phase, interp),
                    WaveReader::User(w) => {
                        read_table(&waves.tables[w], p.level, phase, interp) * waves.gains[w]
                    }
                    WaveReader::NoiseLoop => {
                        let at = phase * noise.len() as f32;
                        let j = (at as usize).min(noise.len() - 1);
                        if interp {
                            let t = at - j as f32;
                            let next = noise[(j + 1) % noise.len()];
                            noise[j] + (next - noise[j]) * t
                        } else {
                            noise[j]
                        }
                    }
                    WaveReader::NoiseWhite => voice.rng.bipolar(),
                };
                let value = y * amp[op];
                outs[op] = value;
                voice.feedback[op][1] = voice.feedback[op][0];
                voice.feedback[op][0] = value;
                let increment = if p.lfo_on {
                    (p.increment * lfo_mult).min(0.49)
                } else {
                    p.increment
                };
                voice.phase[op] = (voice.phase[op] + increment).fract();
            }
            let mut mix = 0.0;
            for (op, o) in outs.iter().enumerate() {
                if algorithm.carriers & (1 << op) != 0 {
                    mix += *o;
                }
            }
            *sample = sanitize(mix);
        }
    }
}

#[inline]
fn read_table(table: &MipTable, level: usize, phase: f32, interpolate: bool) -> f32 {
    table.read(level, phase, interpolate)
}

#[inline]
fn lfo_tick(voice: &mut Voice) -> f32 {
    let dt = voice.lfo_dt;
    let wrapped = voice.lfo_phase + dt as f64 >= 1.0;
    voice.lfo_phase = (voice.lfo_phase + dt as f64).fract();
    let p = voice.lfo_phase as f32;
    match voice.lfo_wave {
        LFO_SQUARE => read_table(builtin_table(W_SQUARE_64), voice.lfo_level, p, true),
        LFO_TRIANGLE => read_table(builtin_table(W_TRIANGLE), voice.lfo_level, p, true),
        LFO_SAW_UP => -read_table(builtin_table(W_SAW_64), voice.lfo_level, p, true),
        LFO_SAW_DOWN => read_table(builtin_table(W_SAW_64), voice.lfo_level, p, true),
        LFO_SH => {
            if wrapped {
                voice.lfo_held = voice.rng.bipolar();
            }
            voice.lfo_held
        }
        LFO_NOISE => voice.lfo_band.process(voice.rng.bipolar()) * 2.0,
        _ => {
            let t = sine_table();
            let at = p * SINE_LEN as f32;
            let j = (at as usize).min(SINE_LEN - 1);
            t[j] + (t[j + 1] - t[j]) * (at - j as f32)
        }
    }
}

impl Processor for Operator {
    fn sleeps_when_silent(&self) -> bool {
        true
    }

    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sample_rate = sample_rate as f32;
        self.alloc.prepare(sample_rate);
        // Build the shared tables now, so the audio thread only ever reads them.
        sine_table();
        noise_loop();
        builtin_table(W_SINE);
        let matrix = &self.matrix;
        self.voices = (0..VOICES_MAX * 2)
            .map(|i| Voice::new(i as u32 + 41, matrix))
            .collect();
        self.oversamplers = (0..VOICES_MAX * 2)
            .map(|_| Oversampler::new(OVERSAMPLE, CHUNK))
            .collect();
        for v in &mut self.voices {
            for e in &mut v.env {
                e.prepare(sample_rate);
            }
            v.filter_env.prepare(sample_rate);
            v.pitch_env.prepare(sample_rate);
            v.lfo_env.prepare(sample_rate);
            v.filter.prepare(sample_rate);
        }
        self.sync_settings();
        self.routes_dirty = true;
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        self.clock = BeatClock::from_transport(ctx.transport);
        let this = &mut *self;
        ctx.for_each_step(|step| match step {
            BlockStep::Audio(a) => this.render(a.left, a.right),
            BlockStep::Param { id, value } => this.set_param(id, value),
            BlockStep::Midi(m) => this.midi(m),
        });
        if ctx.view.is_active() {
            self.publish(ctx);
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.values.get_mut(id as usize) {
            *v = value;
            self.routes_dirty = true;
            if matches!(
                id,
                G_VOICES | G_RETRIG | G_PB_RANGE | P_GLIDE | P_GLIDE_TIME | P_SPREAD
            ) {
                self.sync_settings();
            }
        }
    }

    fn reset(&mut self) {
        self.alloc.reset();
        self.pb = 0.0;
        self.wheel = 0.0;
        for v in &mut self.voices {
            v.silence();
        }
        for os in &mut self.oversamplers {
            os.reset();
        }
    }

    fn latency(&self) -> u32 {
        if self.values[G_ANTIALIAS as usize] >= 0.5 {
            self.oversamplers.first().map_or(0, |os| os.latency())
        } else {
            0
        }
    }

    fn tail(&self) -> Tail {
        let longest = OSC_BASE
            .into_iter()
            .map(|base| self.v(base + O_ENV + E_RELEASE) * 0.001)
            .fold(0.0f32, f32::max);
        // The slowest a note can run: the lowest key against Time < Key, and every Time < Vel.
        let slowest_vel = OSC_BASE
            .into_iter()
            .map(|base| -self.v(base + O_ENV + E_TIME_VEL).min(0.0) * 0.01)
            .fold(0.0f32, f32::max);
        let scale = 2f32.powf(self.v(G_TIME) * 0.03 + self.v(G_TIME_KEY) * 0.05 + slowest_vel);
        Tail::Frames(((longest * scale + 0.05) * self.sample_rate) as u32)
    }

    fn set_resource(&mut self, slot: u32, resource: Resource) -> Option<Resource> {
        if slot == STATE_SLOT {
            return match resource.downcast::<ParsedWaves>() {
                Ok(state) => Some(std::mem::replace(&mut self.waves, state) as Resource),
                Err(refused) => Some(refused),
            };
        }
        Some(resource)
    }
}
