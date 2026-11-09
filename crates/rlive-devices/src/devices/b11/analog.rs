//! Analog (manual pp. 704-713): two oscillators and noise, two multi-mode filters, two amps,
//! four envelopes, two LFOs, vibrato, unison and glide.
//!
//! The manual gives these numbers: Osc Detune ±300 ct, global Detune ±50 ct, Key Scaling 100%
//! default, two or four unison voices, sub an octave below, Env velocity and S.Time behaviour.
//! Everything else (time ranges, modulation depths, polyphony choices, stretch amount and the
//! list of MPE destinations) is ours: the manual gives no figures for those.

use super::common::{
    edge_distance, note_hz, pan_gains, poly_blep, pulse, saw, DcBlock, CONTROL_INTERVAL,
};
use crate::dsp::envelope::{EnvStage, Envelope, EnvelopeSettings, LoopMode, LoopSpan};
use crate::dsp::filter::{Circuit, Filter, FilterKind, FilterSettings, FilterSlope};
use crate::dsp::lfo::{Lfo, LfoRate, LfoSettings};
use crate::dsp::tempo::{BeatClock, SyncRate};
use crate::dsp::util::{db_to_gain, lerp, sanitize, Rng};
use crate::dsp::voice::{
    GlideCurve, GlideMode, NoteInfo, NotePriority, StealMode, VoiceAllocator, VoiceEvent,
    VoiceMode, VoiceSettings, VoiceState,
};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    BlockStep, DeviceDesc, Item, MidiMessage, ParamId, ParamInfo, ProcessContext, Processor,
    Section, Status, Tail, ViewId,
};
use std::f32::consts::TAU;

// Oscillator block offsets.
pub const O_ON: u32 = 0;
pub const O_LEVEL: u32 = 1;
pub const O_BAL: u32 = 2;
pub const O_SHAPE: u32 = 3;
pub const O_WIDTH: u32 = 4;
pub const O_WIDTH_LFO: u32 = 5;
pub const O_OCT: u32 = 6;
pub const O_SEMI: u32 = 7;
pub const O_DETUNE: u32 = 8;
pub const O_PITCH_LFO: u32 = 9;
pub const O_KEY: u32 = 10;
pub const O_PEG_INIT: u32 = 11;
pub const O_PEG_TIME: u32 = 12;
pub const O_MODE: u32 = 13;
pub const O_SUB: u32 = 14;
pub const O_RATIO: u32 = 15;
const OSC_BLOCK: u32 = 16;

pub const OSC1: u32 = 0;
pub const OSC2: u32 = OSC1 + OSC_BLOCK;
pub const NOISE_ON: u32 = OSC2 + OSC_BLOCK;
pub const NOISE_LEVEL: u32 = NOISE_ON + 1;
pub const NOISE_BAL: u32 = NOISE_ON + 2;
pub const NOISE_COLOR: u32 = NOISE_ON + 3;

// Envelope block offsets, in the tail of the filter and amp blocks.
const E_ATTACK: u32 = 0;
const E_ATT_VEL: u32 = 1;
const E_DECAY: u32 = 2;
const E_SUSTAIN: u32 = 3;
const E_STIME: u32 = 4;
const E_RELEASE: u32 = 5;
const E_ENV_VEL: u32 = 6;
const E_SLOPE_A: u32 = 7;
const E_SLOPE_D: u32 = 8;
const E_SLOPE_R: u32 = 9;
const E_LEGATO: u32 = 10;
const E_FREE: u32 = 11;
const E_LOOP: u32 = 12;
const ENV_BLOCK: u32 = 13;

// Filter block offsets.
pub const F_ON: u32 = 0;
pub const F_TYPE: u32 = 1;
pub const F_FREQ: u32 = 2;
pub const F_RES: u32 = 3;
/// Filter 1's To F2, Filter 2's Follow.
pub const F_ROUTE: u32 = 4;
pub const F_DRIVE: u32 = 5;
pub const F_FREQ_LFO: u32 = 6;
pub const F_FREQ_KEY: u32 = 7;
pub const F_FREQ_ENV: u32 = 8;
pub const F_RES_LFO: u32 = 9;
pub const F_RES_KEY: u32 = 10;
pub const F_RES_ENV: u32 = 11;
pub const F_ENV: u32 = 12;
const FILTER_BLOCK: u32 = F_ENV + ENV_BLOCK;

pub const FILTER1: u32 = NOISE_COLOR + 1;
pub const FILTER2: u32 = FILTER1 + FILTER_BLOCK;

// Amp block offsets.
pub const A_ON: u32 = 0;
pub const A_LEVEL: u32 = 1;
pub const A_PAN: u32 = 2;
pub const A_LEV_LFO: u32 = 3;
pub const A_LEV_KEY: u32 = 4;
pub const A_LEV_ENV: u32 = 5;
pub const A_PAN_LFO: u32 = 6;
pub const A_PAN_KEY: u32 = 7;
pub const A_PAN_ENV: u32 = 8;
pub const A_ENV: u32 = 9;
const AMP_BLOCK: u32 = A_ENV + ENV_BLOCK;

pub const AMP1: u32 = FILTER2 + FILTER_BLOCK;
pub const AMP2: u32 = AMP1 + AMP_BLOCK;

// LFO block offsets.
pub const L_ON: u32 = 0;
pub const L_RATE: u32 = 1;
pub const L_SYNC: u32 = 2;
pub const L_SYNC_RATE: u32 = 3;
pub const L_WAVE: u32 = 4;
pub const L_WIDTH: u32 = 5;
pub const L_DELAY: u32 = 6;
pub const L_ATTACK: u32 = 7;
pub const L_RETRIG: u32 = 8;
pub const L_OFFSET: u32 = 9;
const LFO_BLOCK: u32 = 10;

pub const LFO1: u32 = AMP2 + AMP_BLOCK;
pub const LFO2: u32 = LFO1 + LFO_BLOCK;

// Global.
pub const VOLUME: u32 = LFO2 + LFO_BLOCK;
pub const VIB_ON: u32 = VOLUME + 1;
pub const VIB_AMOUNT: u32 = VOLUME + 2;
pub const VIB_RATE: u32 = VOLUME + 3;
pub const VIB_DELAY: u32 = VOLUME + 4;
pub const VIB_ATTACK: u32 = VOLUME + 5;
pub const VIB_ERROR: u32 = VOLUME + 6;
pub const VIB_MW: u32 = VOLUME + 7;
pub const UNI_ON: u32 = VOLUME + 8;
pub const UNI_DETUNE: u32 = VOLUME + 9;
pub const UNI_VOICES: u32 = VOLUME + 10;
pub const UNI_DELAY: u32 = VOLUME + 11;
pub const GLIDE_ON: u32 = VOLUME + 12;
pub const GLIDE_TIME: u32 = VOLUME + 13;
pub const GLIDE_LEGATO: u32 = VOLUME + 14;
pub const GLIDE_MODE: u32 = VOLUME + 15;
pub const OCTAVE: u32 = VOLUME + 16;
pub const SEMI: u32 = VOLUME + 17;
pub const DETUNE: u32 = VOLUME + 18;
pub const PB_RANGE: u32 = VOLUME + 19;
pub const STRETCH: u32 = VOLUME + 20;
pub const ERROR: u32 = VOLUME + 21;
pub const VOICES: u32 = VOLUME + 22;
pub const PRIORITY: u32 = VOLUME + 23;
pub const MPE_ON: u32 = VOLUME + 24;
pub const PRESS_DEST_A: u32 = VOLUME + 25;
pub const PRESS_AMT_A: u32 = VOLUME + 26;
pub const PRESS_DEST_B: u32 = VOLUME + 27;
pub const PRESS_AMT_B: u32 = VOLUME + 28;
pub const SLIDE_DEST_A: u32 = VOLUME + 29;
pub const SLIDE_AMT_A: u32 = VOLUME + 30;
pub const SLIDE_DEST_B: u32 = VOLUME + 31;
pub const SLIDE_AMT_B: u32 = VOLUME + 32;
pub const NOTE_PB: u32 = VOLUME + 33;
pub const PARAM_COUNT: usize = (VOLUME + 34) as usize;

pub const SHAPES: [&str; 4] = ["Sine", "Saw", "Rect", "Noise"];
pub const O_MODES: [&str; 3] = ["Off", "Sub", "Sync"];
pub const FILTER_TYPES: [&str; 10] = [
    "LP 12",
    "LP 24",
    "BP 12",
    "BP 24",
    "Notch 12",
    "Notch 24",
    "HP 12",
    "HP 24",
    "Formant 12",
    "Formant 24",
];
pub const DRIVES: [&str; 7] = [
    "Off", "Sym 1", "Sym 2", "Sym 3", "Asym 1", "Asym 2", "Asym 3",
];
pub const ENV_LOOPS: [&str; 4] = ["Off", "AD-R", "ADR-R", "ADS-R"];
pub const LFO_WAVES: [&str; 5] = ["Sine", "Tri", "Rect", "Noise 1", "Noise 2"];
pub const VOICE_COUNTS: [usize; 8] = [1, 2, 4, 6, 8, 10, 12, 16];
pub const MPE_DESTS: [&str; 15] = [
    "Off",
    "Osc 1 Pitch",
    "Osc 2 Pitch",
    "Osc 1 Width",
    "Osc 2 Width",
    "F1 Freq",
    "F1 Reso",
    "F2 Freq",
    "F2 Reso",
    "Amp 1 Level",
    "Amp 2 Level",
    "Noise Level",
    "LFO 1 Rate",
    "LFO 2 Rate",
    "Vibrato",
];
const DEST_PITCH1: usize = 1;
const DEST_PITCH2: usize = 2;
const DEST_WIDTH1: usize = 3;
const DEST_WIDTH2: usize = 4;
const DEST_F1: usize = 5;
const DEST_R1: usize = 6;
const DEST_F2: usize = 7;
const DEST_R2: usize = 8;
const DEST_A1: usize = 9;
const DEST_A2: usize = 10;
const DEST_NOISE: usize = 11;
const DEST_LFO1: usize = 12;
const DEST_LFO2: usize = 13;
const DEST_VIB: usize = 14;

/// The LEDs' view: `[left activity, right activity]`, one value per LED.
pub const VIEW: ViewId = ViewId(0);
pub const VIEW_LEN: usize = 2;

fn osc_params(out: &mut Vec<ParamInfo>, base: u32, n: usize) {
    let name = |s: &str| format!("Osc {n} {s}");
    let pct = |id, label: &str, min: f64, max: f64, def: f64| {
        ParamInfo::continuous(base + id, &name(label), min, max, def, "%")
    };
    out.push(ParamInfo::toggle(base + O_ON, &format!("Osc {n}"), n == 1));
    out.push(ParamInfo::continuous(
        base + O_LEVEL,
        &name("Level"),
        0.0,
        1.0,
        0.7,
        "",
    ));
    out.push(ParamInfo::continuous(
        base + O_BAL,
        &name("F1/F2"),
        -1.0,
        1.0,
        -1.0,
        "",
    ));
    out.push(ParamInfo::stepped(
        base + O_SHAPE,
        &name("Shape"),
        &SHAPES,
        1,
    ));
    out.push(pct(O_WIDTH, "Width", 1.0, 100.0, 100.0));
    out.push(pct(O_WIDTH_LFO, "Width < LFO", -100.0, 100.0, 0.0));
    out.push(ParamInfo::continuous(
        base + O_OCT,
        &name("Octave"),
        -3.0,
        3.0,
        0.0,
        "",
    ));
    out.push(ParamInfo::continuous(
        base + O_SEMI,
        &name("Semi"),
        -12.0,
        12.0,
        0.0,
        "st",
    ));
    out.push(ParamInfo::continuous(
        base + O_DETUNE,
        &name("Detune"),
        -300.0,
        300.0,
        0.0,
        "ct",
    ));
    out.push(pct(O_PITCH_LFO, "Pitch < LFO", -100.0, 100.0, 0.0));
    out.push(pct(O_KEY, "Key", -100.0, 200.0, 100.0));
    out.push(pct(O_PEG_INIT, "Pitch Env Initial", -100.0, 100.0, 0.0));
    out.push(ParamInfo::skewed(
        base + O_PEG_TIME,
        &name("Pitch Env Time"),
        0.0,
        5_000.0,
        50.0,
        "ms",
        3.0,
    ));
    out.push(ParamInfo::stepped(
        base + O_MODE,
        &name("Sub/Sync"),
        &O_MODES,
        0,
    ));
    out.push(pct(O_SUB, "Sub Level", 0.0, 100.0, 50.0));
    out.push(pct(O_RATIO, "Sync Ratio", 0.0, 100.0, 0.0));
}

fn env_params(out: &mut Vec<ParamInfo>, base: u32, label: &str) {
    let name = |s: &str| format!("{label} {s}");
    let time = |id, s: &str, max: f64, def: f64| {
        ParamInfo::skewed(base + id, &name(s), 0.5, max, def, "ms", 4.0)
    };
    let pct = |id, s: &str, min: f64, def: f64| {
        ParamInfo::continuous(base + id, &name(s), min, 100.0, def, "%")
    };
    out.push(time(E_ATTACK, "Attack", 20_000.0, 2.0));
    out.push(pct(E_ATT_VEL, "Att < Vel", 0.0, 0.0));
    out.push(time(E_DECAY, "Decay", 60_000.0, 600.0));
    out.push(ParamInfo::continuous(
        base + E_SUSTAIN,
        &name("Sustain"),
        0.0,
        1.0,
        0.7,
        "",
    ));
    out.push(ParamInfo::skewed(
        base + E_STIME,
        &name("S.Time"),
        10.0,
        60_000.0,
        60_000.0,
        "ms",
        4.0,
    ));
    out.push(time(E_RELEASE, "Release", 60_000.0, 50.0));
    out.push(pct(E_ENV_VEL, "Env < Vel", 0.0, 0.0));
    out.push(ParamInfo::toggle(
        base + E_SLOPE_A,
        &name("Attack Exp"),
        false,
    ));
    out.push(ParamInfo::toggle(
        base + E_SLOPE_D,
        &name("Decay Exp"),
        true,
    ));
    out.push(ParamInfo::toggle(
        base + E_SLOPE_R,
        &name("Release Exp"),
        true,
    ));
    out.push(ParamInfo::toggle(base + E_LEGATO, &name("Legato"), false));
    out.push(ParamInfo::toggle(base + E_FREE, &name("Free"), false));
    out.push(ParamInfo::stepped(
        base + E_LOOP,
        &name("Loop"),
        &ENV_LOOPS,
        0,
    ));
}

fn filter_params(out: &mut Vec<ParamInfo>, base: u32, n: usize) {
    let name = |s: &str| format!("Fil {n} {s}");
    let pct = |id, s: &str| ParamInfo::continuous(base + id, &name(s), -100.0, 100.0, 0.0, "%");
    out.push(ParamInfo::toggle(base + F_ON, &format!("Fil {n}"), n == 1));
    out.push(ParamInfo::stepped(
        base + F_TYPE,
        &name("Type"),
        &FILTER_TYPES,
        1,
    ));
    out.push(ParamInfo::skewed(
        base + F_FREQ,
        &name("Freq"),
        20.0,
        20_000.0,
        5_000.0,
        "Hz",
        2.5,
    ));
    out.push(ParamInfo::continuous(
        base + F_RES,
        &name("Reso"),
        0.0,
        1.0,
        0.2,
        "",
    ));
    if n == 1 {
        out.push(ParamInfo::continuous(
            base + F_ROUTE,
            &name("To F2"),
            0.0,
            100.0,
            0.0,
            "%",
        ));
    } else {
        out.push(ParamInfo::toggle(base + F_ROUTE, &name("Follow"), false));
    }
    out.push(ParamInfo::stepped(
        base + F_DRIVE,
        &name("Drive"),
        &DRIVES,
        0,
    ));
    out.push(pct(F_FREQ_LFO, "Freq < LFO"));
    out.push(pct(F_FREQ_KEY, "Freq < Key"));
    out.push(pct(F_FREQ_ENV, "Freq < Env"));
    out.push(pct(F_RES_LFO, "Res < LFO"));
    out.push(pct(F_RES_KEY, "Res < Key"));
    out.push(pct(F_RES_ENV, "Res < Env"));
    env_params(out, base + F_ENV, &format!("FEG{n}"));
}

fn amp_params(out: &mut Vec<ParamInfo>, base: u32, n: usize) {
    let name = |s: &str| format!("Amp {n} {s}");
    let pct =
        |id, s: &str, def: f64| ParamInfo::continuous(base + id, &name(s), -100.0, 100.0, def, "%");
    out.push(ParamInfo::toggle(base + A_ON, &format!("Amp {n}"), n == 1));
    out.push(ParamInfo::continuous(
        base + A_LEVEL,
        &name("Level"),
        0.0,
        1.0,
        0.8,
        "",
    ));
    out.push(ParamInfo::continuous(
        base + A_PAN,
        &name("Pan"),
        -1.0,
        1.0,
        0.0,
        "",
    ));
    out.push(pct(A_LEV_LFO, "Level < LFO", 0.0));
    out.push(pct(A_LEV_KEY, "Level < Key", 0.0));
    out.push(pct(A_LEV_ENV, "Level < Env", 100.0));
    out.push(pct(A_PAN_LFO, "Pan < LFO", 0.0));
    out.push(pct(A_PAN_KEY, "Pan < Key", 0.0));
    out.push(pct(A_PAN_ENV, "Pan < Env", 0.0));
    env_params(out, base + A_ENV, &format!("AEG{n}"));
}

fn lfo_params(out: &mut Vec<ParamInfo>, base: u32, n: usize) {
    let name = |s: &str| format!("LFO {n} {s}");
    out.push(ParamInfo::toggle(base + L_ON, &format!("LFO {n}"), false));
    out.push(ParamInfo::skewed(
        base + L_RATE,
        &name("Rate"),
        0.01,
        30.0,
        2.0,
        "Hz",
        3.0,
    ));
    out.push(ParamInfo::toggle(base + L_SYNC, &name("Sync"), false));
    out.push(ParamInfo::stepped(
        base + L_SYNC_RATE,
        &name("Beat Rate"),
        &SyncRate::LABELS,
        12,
    ));
    out.push(ParamInfo::stepped(
        base + L_WAVE,
        &name("Wave"),
        &LFO_WAVES,
        0,
    ));
    out.push(ParamInfo::continuous(
        base + L_WIDTH,
        &name("Width"),
        0.0,
        100.0,
        50.0,
        "%",
    ));
    out.push(ParamInfo::skewed(
        base + L_DELAY,
        &name("Delay"),
        0.0,
        5_000.0,
        0.0,
        "ms",
        3.0,
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
    out.push(ParamInfo::toggle(base + L_RETRIG, &name("Retrig"), false));
    out.push(ParamInfo::continuous(
        base + L_OFFSET,
        &name("Offset"),
        0.0,
        360.0,
        0.0,
        "°",
    ));
}

pub fn params() -> Vec<ParamInfo> {
    let mut out = Vec::with_capacity(PARAM_COUNT);
    osc_params(&mut out, OSC1, 1);
    osc_params(&mut out, OSC2, 2);
    out.push(ParamInfo::toggle(NOISE_ON, "Noise", false));
    out.push(ParamInfo::continuous(
        NOISE_LEVEL,
        "Noise Level",
        0.0,
        1.0,
        0.5,
        "",
    ));
    out.push(ParamInfo::continuous(
        NOISE_BAL,
        "Noise F1/F2",
        -1.0,
        1.0,
        -1.0,
        "",
    ));
    out.push(ParamInfo::skewed(
        NOISE_COLOR,
        "Noise Color",
        20.0,
        20_000.0,
        12_000.0,
        "Hz",
        3.0,
    ));
    filter_params(&mut out, FILTER1, 1);
    filter_params(&mut out, FILTER2, 2);
    amp_params(&mut out, AMP1, 1);
    amp_params(&mut out, AMP2, 2);
    lfo_params(&mut out, LFO1, 1);
    lfo_params(&mut out, LFO2, 2);
    let pct = |id, name: &str, min, max, def| ParamInfo::continuous(id, name, min, max, def, "%");
    out.extend([
        ParamInfo::continuous(VOLUME, "Volume", -36.0, 12.0, 0.0, "dB"),
        ParamInfo::toggle(VIB_ON, "Vib", false),
        pct(VIB_AMOUNT, "Vib Amount", 0.0, 100.0, 20.0),
        ParamInfo::skewed(VIB_RATE, "Vib Rate", 0.1, 20.0, 5.0, "Hz", 2.0),
        ParamInfo::skewed(VIB_DELAY, "Vib Delay", 0.0, 5_000.0, 0.0, "ms", 3.0),
        ParamInfo::skewed(VIB_ATTACK, "Vib Attack", 0.0, 5_000.0, 0.0, "ms", 3.0),
        pct(VIB_ERROR, "Vib Error", 0.0, 100.0, 0.0),
        pct(VIB_MW, "Vib Amt < MW", -100.0, 100.0, 0.0),
        ParamInfo::toggle(UNI_ON, "Uni", false),
        pct(UNI_DETUNE, "Uni Detune", 0.0, 100.0, 15.0),
        ParamInfo::stepped(UNI_VOICES, "Uni Voices", &["2", "4"], 0),
        ParamInfo::skewed(UNI_DELAY, "Uni Delay", 0.0, 500.0, 0.0, "ms", 2.5),
        ParamInfo::toggle(GLIDE_ON, "Gli", false),
        ParamInfo::skewed(GLIDE_TIME, "Glide Time", 0.0, 5_000.0, 100.0, "ms", 3.0),
        ParamInfo::toggle(GLIDE_LEGATO, "Glide Legato", false),
        ParamInfo::stepped(GLIDE_MODE, "Glide Mode", &["Const", "Prop"], 0),
        ParamInfo::continuous(OCTAVE, "Octave", -3.0, 3.0, 0.0, ""),
        ParamInfo::continuous(SEMI, "Semi", -12.0, 12.0, 0.0, "st"),
        ParamInfo::continuous(DETUNE, "Detune", -50.0, 50.0, 0.0, "ct"),
        ParamInfo::continuous(PB_RANGE, "PB Range", 0.0, 48.0, 2.0, "st"),
        pct(STRETCH, "Stretch", -100.0, 100.0, 0.0),
        pct(ERROR, "Error", 0.0, 100.0, 0.0),
        ParamInfo::stepped(
            VOICES,
            "Voices",
            &["1", "2", "4", "6", "8", "10", "12", "16"],
            6,
        ),
        ParamInfo::stepped(PRIORITY, "Priority", &["High", "Low", "Last"], 2),
        ParamInfo::toggle(MPE_ON, "MPE", true),
        ParamInfo::stepped(PRESS_DEST_A, "Pressure Dest A", &MPE_DESTS, 0),
        pct(PRESS_AMT_A, "Pressure Amount A", -100.0, 100.0, 0.0),
        ParamInfo::stepped(PRESS_DEST_B, "Pressure Dest B", &MPE_DESTS, 0),
        pct(PRESS_AMT_B, "Pressure Amount B", -100.0, 100.0, 0.0),
        ParamInfo::stepped(SLIDE_DEST_A, "Slide Dest A", &MPE_DESTS, 0),
        pct(SLIDE_AMT_A, "Slide Amount A", -100.0, 100.0, 0.0),
        ParamInfo::stepped(SLIDE_DEST_B, "Slide Dest B", &MPE_DESTS, 0),
        pct(SLIDE_AMT_B, "Slide Amount B", -100.0, 100.0, 0.0),
        ParamInfo::continuous(NOTE_PB, "Note PB", 0.0, 48.0, 48.0, "st"),
    ]);
    out
}

/// What each of the four Quick Routing buttons sets. The oscillators' level, tuning and shape
/// are left alone.
pub fn quick_routing(index: usize) -> Vec<(u32, f64)> {
    let (bal1, bal2, fil1, fil2, to_f2, amp1, amp2) = match index {
        // Parallel: each oscillator its own filter and amp.
        0 => (-1.0, 1.0, 1.0, 1.0, 0.0, 1.0, 1.0),
        // Both oscillators shared between the filters.
        1 => (0.0, 0.0, 1.0, 1.0, 0.0, 1.0, 1.0),
        // Everything through Filter 1 and Amp 1.
        2 => (-1.0, -1.0, 1.0, 0.0, 0.0, 1.0, 0.0),
        // Serial: Filter 1 into Filter 2 and Amp 2.
        _ => (-1.0, -1.0, 1.0, 1.0, 100.0, 0.0, 1.0),
    };
    vec![
        (OSC1 + O_BAL, bal1),
        (OSC2 + O_BAL, bal2),
        (FILTER1 + F_ON, fil1),
        (FILTER2 + F_ON, fil2),
        (FILTER1 + F_ROUTE, to_f2),
        (AMP1 + A_ON, amp1),
        (AMP2 + A_ON, amp2),
    ]
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
                knob(b + O_LEVEL),
                knob(b + O_BAL),
                menu(b + O_SHAPE),
            ]),
            Item::Row(vec![
                knob(b + O_OCT),
                knob(b + O_SEMI),
                knob(b + O_DETUNE),
                knob(b + O_KEY),
            ]),
            Item::Row(vec![
                knob(b + O_WIDTH),
                knob(b + O_WIDTH_LFO),
                knob(b + O_PITCH_LFO),
            ]),
            Item::Row(vec![knob(b + O_PEG_INIT), knob(b + O_PEG_TIME)]),
            Item::Row(vec![menu(b + O_MODE), knob(b + O_SUB), knob(b + O_RATIO)]),
        ],
    };
    let env = |b: u32, label: &'static str| Section {
        title: label,
        items: vec![
            Item::Row(vec![
                knob(b + E_ATTACK),
                knob(b + E_ATT_VEL),
                knob(b + E_DECAY),
                knob(b + E_SUSTAIN),
            ]),
            Item::Row(vec![
                knob(b + E_STIME),
                knob(b + E_RELEASE),
                knob(b + E_ENV_VEL),
            ]),
            Item::Row(vec![
                toggle(b + E_SLOPE_A),
                toggle(b + E_SLOPE_D),
                toggle(b + E_SLOPE_R),
            ]),
            Item::Row(vec![
                toggle(b + E_LEGATO),
                toggle(b + E_FREE),
                menu(b + E_LOOP),
            ]),
        ],
    };
    let fil = |b: u32, title: &'static str| Section {
        title,
        items: vec![
            Item::Row(vec![toggle(b + F_ON), menu(b + F_TYPE), menu(b + F_DRIVE)]),
            Item::Row(vec![knob(b + F_FREQ), knob(b + F_RES), knob(b + F_ROUTE)]),
            Item::Row(vec![
                knob(b + F_FREQ_LFO),
                knob(b + F_FREQ_KEY),
                knob(b + F_FREQ_ENV),
            ]),
            Item::Row(vec![
                knob(b + F_RES_LFO),
                knob(b + F_RES_KEY),
                knob(b + F_RES_ENV),
            ]),
        ],
    };
    let amp = |b: u32, title: &'static str| Section {
        title,
        items: vec![
            Item::Row(vec![toggle(b + A_ON), knob(b + A_LEVEL), knob(b + A_PAN)]),
            Item::Row(vec![
                knob(b + A_LEV_LFO),
                knob(b + A_LEV_KEY),
                knob(b + A_LEV_ENV),
            ]),
            Item::Row(vec![
                knob(b + A_PAN_LFO),
                knob(b + A_PAN_KEY),
                knob(b + A_PAN_ENV),
            ]),
        ],
    };
    let lfo = |b: u32, title: &'static str| Section {
        title,
        items: vec![
            Item::Row(vec![
                toggle(b + L_ON),
                knob(b + L_RATE),
                toggle(b + L_SYNC),
                menu(b + L_SYNC_RATE),
            ]),
            Item::Row(vec![
                menu(b + L_WAVE),
                knob(b + L_WIDTH),
                knob(b + L_OFFSET),
            ]),
            Item::Row(vec![
                knob(b + L_DELAY),
                knob(b + L_ATTACK),
                toggle(b + L_RETRIG),
            ]),
        ],
    };
    vec![
        Section {
            title: "Display",
            items: vec![Item::Panel("analog_display")],
        },
        osc(OSC1, "Osc 1"),
        osc(OSC2, "Osc 2"),
        Section {
            title: "Noise",
            items: vec![Item::Row(vec![
                toggle(NOISE_ON),
                knob(NOISE_LEVEL),
                knob(NOISE_BAL),
                knob(NOISE_COLOR),
            ])],
        },
        fil(FILTER1, "Fil 1"),
        env(FILTER1 + F_ENV, "FEG 1"),
        fil(FILTER2, "Fil 2"),
        env(FILTER2 + F_ENV, "FEG 2"),
        amp(AMP1, "Amp 1"),
        env(AMP1 + A_ENV, "AEG 1"),
        amp(AMP2, "Amp 2"),
        env(AMP2 + A_ENV, "AEG 2"),
        lfo(LFO1, "LFO 1"),
        lfo(LFO2, "LFO 2"),
        Section {
            title: "Global",
            items: vec![
                Item::Row(vec![
                    knob(VOLUME),
                    toggle(VIB_ON),
                    knob(VIB_AMOUNT),
                    knob(VIB_RATE),
                ]),
                Item::Row(vec![
                    knob(VIB_DELAY),
                    knob(VIB_ATTACK),
                    knob(VIB_ERROR),
                    knob(VIB_MW),
                ]),
                Item::Row(vec![
                    toggle(UNI_ON),
                    knob(UNI_DETUNE),
                    menu(UNI_VOICES),
                    knob(UNI_DELAY),
                ]),
                Item::Row(vec![
                    toggle(GLIDE_ON),
                    knob(GLIDE_TIME),
                    toggle(GLIDE_LEGATO),
                    menu(GLIDE_MODE),
                ]),
                Item::Row(vec![knob(OCTAVE), knob(SEMI), knob(DETUNE), knob(PB_RANGE)]),
                Item::Row(vec![
                    knob(STRETCH),
                    knob(ERROR),
                    menu(VOICES),
                    menu(PRIORITY),
                ]),
            ],
        },
        Section {
            title: "MPE",
            items: vec![
                Item::Row(vec![toggle(MPE_ON), knob(NOTE_PB)]),
                Item::Row(vec![
                    menu(PRESS_DEST_A),
                    knob(PRESS_AMT_A),
                    menu(PRESS_DEST_B),
                    knob(PRESS_AMT_B),
                ]),
                Item::Row(vec![
                    menu(SLIDE_DEST_A),
                    knob(SLIDE_AMT_A),
                    menu(SLIDE_DEST_B),
                    knob(SLIDE_AMT_B),
                ]),
            ],
        },
    ]
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.analog",
    name: "Analog",
    role: DeviceRole::Instrument,
    category: "Synths",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::new(Analog::new()),
};

const MAX_VOICES: usize = 16;
const VOICE_GAIN: f32 = 0.35;
const STRETCH_ST_PER_OCTAVE: f32 = 0.15;
const ERROR_RANGE_ST: f32 = 0.3;
const VIB_RANGE_ST: f32 = 1.0;
const LFO_PITCH_ST: f32 = 12.0;
const PEG_RANGE_ST: f32 = 24.0;
const FILTER_LFO_ST: f32 = 48.0;
const FILTER_ENV_ST: f32 = 96.0;
const LEVEL_KEY_DB_PER_OCT: f32 = 3.0;
const UNI_DETUNE_ST: f32 = 0.5;
const EXP_SLOPE: f32 = 0.75;
const SYNC_MAX_RATIO: f32 = 8.0;
const FORMANT_FREQS: [[f32; 3]; 5] = [
    [730.0, 1_090.0, 2_440.0],
    [530.0, 1_840.0, 2_480.0],
    [270.0, 2_290.0, 3_010.0],
    [570.0, 840.0, 2_410.0],
    [300.0, 870.0, 2_240.0],
];

#[derive(Clone, Copy)]
struct Osc {
    phase: f32,
    master: f32,
    sub: f32,
    dt: f32,
    dt_step: f32,
    target_dt: f32,
    sync_from: f32,
    width: f32,
    level: f32,
    level_step: f32,
    target_level: f32,
    to_f1: f32,
    shape: u8,
    mode: u8,
    sub_level: f32,
    master_ratio: f32,
    noise: Rng,
}

impl Default for Osc {
    fn default() -> Self {
        Self {
            phase: 0.0,
            master: 0.0,
            sub: 0.0,
            dt: 0.0,
            dt_step: 0.0,
            target_dt: 0.0,
            sync_from: 0.0,
            width: 0.5,
            level: 0.0,
            level_step: 0.0,
            target_level: 0.0,
            to_f1: 1.0,
            shape: 1,
            mode: 0,
            sub_level: 0.0,
            master_ratio: 1.0,
            noise: Rng::new(17),
        }
    }
}

impl Osc {
    #[inline]
    fn naive(&self, p: f32) -> f32 {
        match self.shape {
            0 => (TAU * p).sin(),
            1 => 2.0 * p - 1.0,
            _ => {
                if p < self.width {
                    1.0
                } else {
                    -1.0
                }
            }
        }
    }

    #[inline]
    fn next(&mut self) -> f32 {
        self.dt = (self.dt + self.dt_step).max(1e-6);
        self.level += self.level_step;
        let dt = self.dt;
        let p = self.phase;
        if self.shape == 3 {
            self.phase = (self.phase + dt).fract();
            return self.noise.bipolar() * self.level;
        }
        let dm = dt * self.master_ratio;
        let sync = self.mode == 2 && self.master_ratio > 1.001;
        let mut y = match self.shape {
            0 => (TAU * p).sin(),
            1 => saw(p, dt),
            _ => pulse(p, dt, self.width),
        };
        if sync {
            let d = edge_distance(self.master, 0.0, dm);
            if d.abs() < 2.0 {
                let from = if d < 0.0 {
                    (p - d * dt).fract()
                } else {
                    self.sync_from
                };
                y += (self.naive(0.0) - self.naive(from)) * poly_blep(d);
            }
        }
        if self.mode == 1 {
            let sub_dt = dt * 0.5;
            let s = if self.shape == 0 {
                (TAU * self.sub).sin()
            } else {
                pulse(self.sub, sub_dt, 0.5)
            };
            y += s * self.sub_level;
            self.sub += sub_dt;
            self.sub = self.sub.fract();
        }
        self.phase += dt;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }
        if sync {
            self.master += dm;
            if self.master >= 1.0 {
                self.master -= 1.0;
                self.sync_from = self.phase;
                self.phase = self.master / dm.max(1e-6) * dt;
            }
        }
        y * self.level
    }
}

#[derive(Clone, Copy)]
struct AdsrEnv {
    env: Envelope,
    settings: EnvelopeSettings,
    sustain: f32,
    sustain_step: f32,
    loop_mode: u8,
    free: bool,
    legato: bool,
    gain: f32,
}

impl Default for AdsrEnv {
    fn default() -> Self {
        Self {
            env: Envelope::new(),
            settings: EnvelopeSettings::default(),
            sustain: 0.7,
            sustain_step: 0.0,
            loop_mode: 0,
            free: false,
            legato: false,
            gain: 1.0,
        }
    }
}

impl AdsrEnv {
    fn configure(&mut self, v: &[f64], base: u32, sr: f32, velocity: f32) {
        let g = |id: u32| v[(base + id) as usize] as f32;
        let att_vel = g(E_ATT_VEL) * 0.01;
        let attack_ms = g(E_ATTACK) * 2f32.powf(-att_vel * velocity * 6.0);
        let sustain = g(E_SUSTAIN).clamp(0.0, 1.0);
        let slope = |id: u32| if g(id) >= 0.5 { EXP_SLOPE } else { 0.0 };
        let loop_mode = v[(base + E_LOOP) as usize].round() as u8;
        let free = g(E_FREE) >= 0.5;
        let (loop_kind, span) = match (loop_mode, free) {
            (1, _) => (LoopMode::Loop, LoopSpan::AttackDecay),
            (2, _) => (LoopMode::Loop, LoopSpan::AttackDecayRelease),
            (_, true) => (LoopMode::Trigger, LoopSpan::AttackDecay),
            _ => (LoopMode::Off, LoopSpan::AttackDecay),
        };
        self.settings = EnvelopeSettings {
            attack: attack_ms * 0.001,
            decay: g(E_DECAY) * 0.001,
            release: g(E_RELEASE) * 0.001,
            sustain,
            attack_slope: slope(E_SLOPE_A),
            decay_slope: slope(E_SLOPE_D),
            release_slope: slope(E_SLOPE_R),
            loop_mode: loop_kind,
            loop_span: span,
            ..EnvelopeSettings::default()
        };
        self.loop_mode = loop_mode;
        self.free = free;
        self.legato = g(E_LEGATO) >= 0.5;
        let env_vel = g(E_ENV_VEL) * 0.01;
        self.gain = 1.0 - env_vel + env_vel * velocity;
        self.sustain = sustain;
        let s_time = g(E_STIME) * 0.001;
        self.sustain_step = if s_time >= 59.9 {
            0.0
        } else {
            sustain * CONTROL_INTERVAL as f32 / (s_time * sr).max(1.0)
        };
        self.env.set(&self.settings);
    }

    fn gate_on(&mut self) {
        self.env.gate_on();
    }

    fn gate_off(&mut self) {
        if self.free && matches!(self.loop_mode, 1 | 2) {
            // Free and a loop: as if the key were held for good.
            return;
        }
        if self.loop_mode == 3 {
            // ADS-R: the attack once more, then the release from the peak.
            let mut s = self.settings;
            s.loop_mode = LoopMode::Trigger;
            s.decay = 0.0;
            s.sustain = s.peak;
            self.env.set(&s);
            self.env.gate_on();
            return;
        }
        self.env.gate_off();
    }

    /// At control rate: the sustain level falling away while the key stays down.
    fn tick(&mut self) {
        if self.sustain_step > 0.0 && self.env.stage() == EnvStage::Sustain {
            self.sustain = (self.sustain - self.sustain_step).max(0.0);
            self.settings.sustain = self.sustain;
            self.env.set(&self.settings);
        }
    }

    #[inline]
    fn next(&mut self) -> f32 {
        self.env.next() * self.gain
    }
}

#[derive(Clone)]
struct LfoUnit {
    lfo: Lfo,
    last: f32,
    held: f32,
    prev: f32,
    rng: Rng,
    value: f32,
    delay_left: f32,
    attack_step: f32,
    fade: f32,
}

impl LfoUnit {
    fn new(seed: u32) -> Self {
        Self {
            lfo: Lfo::new(seed),
            last: 0.0,
            held: 0.0,
            prev: 0.0,
            rng: Rng::new(seed),
            value: 0.0,
            delay_left: 0.0,
            attack_step: 0.0,
            fade: 1.0,
        }
    }

    fn start(&mut self, delay: f32, attack: f32, sr: f32, retrig: bool) {
        if retrig {
            self.lfo.retrigger();
        }
        self.delay_left = delay * sr;
        self.fade = if delay > 0.0 || attack > 0.0 {
            0.0
        } else {
            1.0
        };
        self.attack_step = if attack > 0.0 {
            1.0 / (attack * sr)
        } else {
            1.0
        };
    }

    fn tick(&mut self, wave: u8, width: f32) -> f32 {
        let frames = CONTROL_INTERVAL as f32;
        let p = self.lfo.advance_phases(CONTROL_INTERVAL)[0];
        if p < self.last {
            self.prev = self.held;
            self.held = self.rng.bipolar();
        }
        self.last = p;
        let raw = match wave {
            0 => (TAU * p).sin(),
            1 => {
                // Low width rises for the whole cycle: an upwards saw.
                let rise = (1.0 - width).clamp(0.001, 0.999);
                if p < rise {
                    -1.0 + 2.0 * p / rise
                } else {
                    1.0 - 2.0 * (p - rise) / (1.0 - rise)
                }
            }
            2 => {
                if p < 1.0 - width {
                    1.0
                } else {
                    -1.0
                }
            }
            3 => self.held,
            _ => lerp(self.prev, self.held, p),
        };
        if self.delay_left > 0.0 {
            self.delay_left -= frames;
            self.value = 0.0;
        } else {
            self.fade = (self.fade + self.attack_step * frames).min(1.0);
            self.value = raw * self.fade;
        }
        self.value
    }
}

#[derive(Clone, Copy)]
struct FilterUnit {
    filter: Filter,
    formant: [Filter; 3],
    formant_gain: [f32; 3],
    kind: u8,
    drive: u8,
    on: bool,
}

impl Default for FilterUnit {
    fn default() -> Self {
        Self {
            filter: Filter::new(),
            formant: [Filter::new(); 3],
            formant_gain: [1.0, 0.7, 0.4],
            kind: 1,
            drive: 0,
            on: true,
        }
    }
}

impl FilterUnit {
    fn set(&mut self, cutoff: f32, res: f32) {
        let res = res.clamp(0.0, 1.0);
        if self.kind >= 8 {
            let order = if self.kind == 9 {
                FilterSlope::Db24
            } else {
                FilterSlope::Db12
            };
            let v = (res * (FORMANT_FREQS.len() as f32 - 1.0)).max(0.0);
            let i = (v as usize).min(FORMANT_FREQS.len() - 2);
            let t = v - i as f32;
            let scale = (cutoff / 1_000.0).clamp(0.25, 4.0);
            for (f, filter) in self.formant.iter_mut().enumerate() {
                let hz = lerp(FORMANT_FREQS[i][f], FORMANT_FREQS[i + 1][f], t) * scale;
                filter.set(&FilterSettings {
                    kind: FilterKind::BandPass,
                    slope: order,
                    cutoff: hz,
                    resonance: 0.82,
                    ..Default::default()
                });
            }
        } else {
            let slope = if self.kind % 2 == 1 {
                FilterSlope::Db24
            } else {
                FilterSlope::Db12
            };
            let kind = match self.kind / 2 {
                0 => FilterKind::LowPass,
                1 => FilterKind::BandPass,
                2 => FilterKind::Notch,
                _ => FilterKind::HighPass,
            };
            self.filter.set(&FilterSettings {
                circuit: Circuit::Svf,
                kind,
                slope,
                cutoff,
                resonance: res,
                ..Default::default()
            });
        }
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        if !self.on {
            return x;
        }
        let mut y = if self.kind >= 8 {
            let mut s = 0.0;
            for (i, f) in self.formant.iter_mut().enumerate() {
                s += f.process(x) * self.formant_gain[i];
            }
            s
        } else {
            self.filter.process(x)
        };
        if self.drive > 0 {
            y = drive(y, self.drive);
        }
        y
    }

    fn reset(&mut self) {
        self.filter.reset();
        self.formant.iter_mut().for_each(Filter::reset);
    }
}

/// The saturator on a filter's output.
#[inline]
pub fn drive(x: f32, mode: u8) -> f32 {
    let (gain, asym) = match mode {
        1 => (1.5, false),
        2 => (3.0, false),
        3 => (6.0, false),
        4 => (1.5, true),
        5 => (3.0, true),
        6 => (6.0, true),
        _ => return x,
    };
    let driven = x * gain;
    if asym {
        if driven >= 0.0 {
            driven.tanh()
        } else {
            (driven * 0.5).tanh() * 2.0
        }
    } else {
        driven.tanh()
    }
}

#[derive(Clone)]
struct Voice {
    active: bool,
    /// Started since the last control tick: pitch and levels jump straight to their targets.
    fresh: bool,
    gated: bool,
    pending_release: bool,
    delay_left: f32,
    key: f32,
    velocity: f32,
    error: f32,
    vib_rate: f32,
    vib_amount_err: f32,
    vib_delay: f32,
    vib_attack: f32,
    vib_phase: f32,
    vib_fade: f32,
    peg_value: [f32; 2],
    peg_step: [f32; 2],
    osc: [Osc; 2],
    noise: f32,
    noise_coef: f32,
    noise_level: f32,
    noise_to_f1: f32,
    noise_rng: Rng,
    f_env: [AdsrEnv; 2],
    a_env: [AdsrEnv; 2],
    filters: [FilterUnit; 2],
    to_f2: f32,
    amp_on: [bool; 2],
    amp_level: [f32; 2],
    amp_step: [f32; 2],
    amp_target: [f32; 2],
    env_amt: [f32; 2],
    pan: [(f32, f32); 2],
    pan_target: [(f32, f32); 2],
    lfo: [LfoUnit; 2],
    dc: [DcBlock; 2],
}

impl Voice {
    fn new(seed: u32) -> Self {
        Self {
            active: false,
            fresh: false,
            gated: false,
            pending_release: false,
            delay_left: 0.0,
            key: 60.0,
            velocity: 1.0,
            error: 0.0,
            vib_rate: 5.0,
            vib_amount_err: 1.0,
            vib_delay: 0.0,
            vib_attack: 0.0,
            vib_phase: 0.0,
            vib_fade: 1.0,
            peg_value: [0.0; 2],
            peg_step: [0.0; 2],
            osc: [
                Osc {
                    noise: Rng::new(seed.wrapping_add(21)),
                    ..Osc::default()
                },
                Osc {
                    noise: Rng::new(seed.wrapping_add(22)),
                    ..Osc::default()
                },
            ],
            noise: 0.0,
            noise_coef: 0.0,
            noise_level: 0.0,
            noise_to_f1: 1.0,
            noise_rng: Rng::new(seed.wrapping_add(11)),
            f_env: [AdsrEnv::default(); 2],
            a_env: [AdsrEnv::default(); 2],
            filters: [FilterUnit::default(); 2],
            to_f2: 0.0,
            amp_on: [true; 2],
            amp_level: [0.0; 2],
            amp_step: [0.0; 2],
            amp_target: [0.0; 2],
            env_amt: [1.0, 1.0],
            pan: [(1.0, 1.0); 2],
            pan_target: [(1.0, 1.0); 2],
            lfo: [
                LfoUnit::new(seed.wrapping_add(2)),
                LfoUnit::new(seed.wrapping_add(3)),
            ],
            dc: [DcBlock::default(); 2],
        }
    }

    fn prepare(&mut self, sr: f64) {
        for e in self.f_env.iter_mut().chain(self.a_env.iter_mut()) {
            e.env.prepare(sr);
        }
        for f in self.filters.iter_mut() {
            f.filter.prepare(sr);
            f.formant.iter_mut().for_each(|x| x.prepare(sr));
        }
        for l in self.lfo.iter_mut() {
            l.lfo.prepare(sr);
        }
    }

    fn envs_mut(&mut self) -> impl Iterator<Item = &mut AdsrEnv> {
        self.f_env.iter_mut().chain(self.a_env.iter_mut())
    }

    fn silence(&mut self) {
        self.fresh = false;
        self.active = false;
        self.gated = false;
        self.pending_release = false;
        self.delay_left = 0.0;
        for e in self.envs_mut() {
            e.env.kill();
        }
        self.filters.iter_mut().for_each(FilterUnit::reset);
        self.dc.iter_mut().for_each(DcBlock::reset);
        self.amp_level = [0.0; 2];
        self.amp_step = [0.0; 2];
        for o in &mut self.osc {
            o.level = 0.0;
            o.level_step = 0.0;
        }
    }

    fn release(&mut self) {
        self.gated = false;
        if self.delay_left > 0.0 {
            self.pending_release = true;
            return;
        }
        for e in self.envs_mut() {
            e.gate_off();
        }
    }

    fn gate_envs(&mut self) {
        for e in self.envs_mut() {
            e.gate_on();
        }
    }
}

/// Analog's engine.
pub struct Analog {
    v: Vec<f64>,
    alloc: VoiceAllocator,
    voices: Vec<Voice>,
    sr: f32,
    dc_coeff: f32,
    ctrl_phase: usize,
    mod_wheel: f32,
    clock: BeatClock,
    tempo: f64,
    rng: Rng,
    free_lfos: [Lfo; 2],
    dirty: bool,
    pressure_seen: f32,
    slide_seen: f32,
}

impl Analog {
    /// How many voices are sounding right now.
    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active).count()
    }
}

impl Default for Analog {
    fn default() -> Self {
        Self::new()
    }
}

impl Analog {
    pub fn new() -> Self {
        let defaults = params();
        let mut v = vec![0.0; PARAM_COUNT];
        for p in &defaults {
            v[p.id as usize] = p.default;
        }
        let mut this = Self {
            v,
            alloc: VoiceAllocator::new(MAX_VOICES),
            voices: (0..MAX_VOICES)
                .map(|i| Voice::new(7 + i as u32 * 13))
                .collect(),
            sr: 48_000.0,
            dc_coeff: 0.999,
            ctrl_phase: 0,
            mod_wheel: 0.0,
            clock: BeatClock::default(),
            tempo: 120.0,
            rng: Rng::new(0xA7A1),
            free_lfos: [Lfo::new(101), Lfo::new(102)],
            dirty: true,
            pressure_seen: 0.0,
            slide_seen: 0.0,
        };
        this.apply_voice_settings();
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

    fn voice_settings(&self) -> VoiceSettings {
        let count = VOICE_COUNTS[self.v[VOICES as usize].round().clamp(0.0, 7.0) as usize];
        let unison = if self.on(UNI_ON) {
            if self.v[UNI_VOICES as usize] >= 0.5 {
                4
            } else {
                2
            }
        } else {
            1
        };
        let priority = self.v[PRIORITY as usize].round() as i32;
        VoiceSettings {
            mode: if count == 1 {
                VoiceMode::Mono
            } else {
                VoiceMode::Poly
            },
            polyphony: count,
            unison,
            steal: match priority {
                0 => StealMode::Lowest,
                1 => StealMode::Highest,
                _ => StealMode::Oldest,
            },
            priority: match priority {
                0 => NotePriority::High,
                1 => NotePriority::Low,
                _ => NotePriority::Last,
            },
            glide: if !self.on(GLIDE_ON) {
                GlideMode::Off
            } else if self.on(GLIDE_LEGATO) {
                GlideMode::LegatoOnly
            } else {
                GlideMode::Always
            },
            glide_curve: if self.v[GLIDE_MODE as usize] >= 0.5 {
                GlideCurve::ConstantRate
            } else {
                GlideCurve::ConstantTime
            },
            glide_time: self.f(GLIDE_TIME) * 0.001,
            bend_range: self.f(PB_RANGE),
            ..VoiceSettings::default()
        }
    }

    fn apply_voice_settings(&mut self) {
        let settings = self.voice_settings();
        self.alloc.set(&settings);
    }

    fn lfo_settings(&self, base: u32) -> LfoSettings {
        let rate = if self.on(base + L_SYNC) {
            LfoRate::Beats(SyncRate::from_index(self.v[(base + L_SYNC_RATE) as usize]).beats())
        } else {
            LfoRate::Hz(self.f(base + L_RATE))
        };
        LfoSettings {
            rate,
            offset: self.f(base + L_OFFSET) / 360.0,
            ..Default::default()
        }
    }

    fn start_voice(&mut self, index: usize, note: NoteInfo, overlapped: bool) {
        let velocity = note.velocity.clamp(0.0, 1.0);
        let sr = self.sr;
        let mut rng = self.rng;
        let error = rng.bipolar() * self.f(ERROR) * 0.01 * ERROR_RANGE_ST;
        let vib_error = self.f(VIB_ERROR) * 0.01;
        let vib_rate = self.f(VIB_RATE) * (1.0 + rng.bipolar() * vib_error * 0.5);
        let vib_amount_err = 1.0 + rng.bipolar() * vib_error * 0.5;
        let vib_delay = self.f(VIB_DELAY) * 0.001 * (1.0 + rng.bipolar() * vib_error * 0.5);
        let vib_attack = self.f(VIB_ATTACK) * 0.001 * (1.0 + rng.bipolar() * vib_error * 0.5);
        self.rng = rng;

        let lfo_settings = [self.lfo_settings(LFO1), self.lfo_settings(LFO2)];
        let first_held = (0..self.voices.len())
            .find(|&i| i != index && self.voices[i].active && self.voices[i].gated);
        let uni_delay = self.f(UNI_DELAY) * 0.001 * note.unison_index as f32;
        let v = &self.v;
        let voice_settings = (0..2).map(|e| {
            (
                FILTER1 + e * FILTER_BLOCK + F_ENV,
                AMP1 + e * AMP_BLOCK + A_ENV,
            )
        });
        let keep = overlapped;
        // Envelope legato: a new note joins the first held note's envelope where it is.
        let snapshot = first_held.map(|f| {
            (
                self.voices[f].f_env.map(|e| e.env),
                self.voices[f].a_env.map(|e| e.env),
            )
        });
        let vo = &mut self.voices[index];
        vo.key = note.key as f32;
        vo.velocity = velocity;
        vo.error = error;
        vo.vib_rate = vib_rate;
        vo.vib_amount_err = vib_amount_err;
        vo.vib_delay = vib_delay.max(0.0);
        vo.vib_attack = vib_attack.max(0.0);
        vo.vib_fade = if vib_delay > 0.0 || vib_attack > 0.0 {
            0.0
        } else {
            1.0
        };
        vo.vib_phase = 0.0;
        for (e, (f_base, a_base)) in voice_settings.enumerate() {
            let (f_env, a_env) = (&mut vo.f_env[e], &mut vo.a_env[e]);
            f_env.configure(v, f_base, sr, velocity);
            a_env.configure(v, a_base, sr, velocity);
        }
        for (i, base) in [OSC1, OSC2].into_iter().enumerate() {
            let g = |id: u32| v[(base + id) as usize] as f32;
            let init = g(O_PEG_INIT) * 0.01 * PEG_RANGE_ST;
            let time = g(O_PEG_TIME) * 0.001;
            vo.peg_value[i] = init;
            vo.peg_step[i] = if time > 0.0 {
                -init * CONTROL_INTERVAL as f32 / (time * sr)
            } else {
                -init
            };
        }
        vo.osc.iter_mut().for_each(|o| {
            o.level = 0.0;
            o.level_step = 0.0;
        });
        let retrig = [
            v[(LFO1 + L_RETRIG) as usize] >= 0.5,
            v[(LFO2 + L_RETRIG) as usize] >= 0.5,
        ];
        for (i, base) in [LFO1, LFO2].into_iter().enumerate() {
            let delay = v[(base + L_DELAY) as usize] as f32 * 0.001;
            let attack = v[(base + L_ATTACK) as usize] as f32 * 0.001;
            let unit = &mut vo.lfo[i];
            unit.lfo.set(&lfo_settings[i]);
            unit.start(delay, attack, sr, retrig[i]);
            if !retrig[i] {
                unit.lfo.clone_from(&self.free_lfos[i]);
                unit.lfo.set(&lfo_settings[i]);
            }
        }
        vo.pending_release = false;
        vo.gated = true;
        vo.active = true;
        vo.fresh = true;
        vo.delay_left = uni_delay * sr;
        if vo.delay_left <= 0.0 {
            let legato_copy =
                |e: &mut AdsrEnv, from: Option<Envelope>| match (e.legato && keep, from) {
                    (true, Some(state)) => {
                        let settings = e.settings;
                        e.env = state;
                        e.env.set(&settings);
                    }
                    (true, None) => {}
                    _ => e.gate_on(),
                };
            for i in 0..2 {
                legato_copy(&mut vo.f_env[i], snapshot.map(|s| s.0[i]));
                legato_copy(&mut vo.a_env[i], snapshot.map(|s| s.1[i]));
            }
        }
    }

    fn midi(&mut self, message: MidiMessage) {
        if let MidiMessage::ControlChange { cc: 1, value, .. } = message {
            self.mod_wheel = value as f32 / 127.0;
        }
        if matches!(message, MidiMessage::NoteOn { .. }) {
            self.apply_voice_settings();
        }
        let overlapped = self
            .alloc
            .voices()
            .iter()
            .any(|v| v.state == VoiceState::Held);
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
                VoiceEvent::Legato { voice, note } => self.voices[voice].key = note.key as f32,
                VoiceEvent::Release { voice } => self.voices[voice].release(),
                VoiceEvent::Kill { voice } => self.voices[voice].silence(),
            }
        }
    }

    fn mpe_mods(&self, index: usize) -> [f32; MPE_DESTS.len()] {
        let mut out = [0.0; MPE_DESTS.len()];
        if !self.on(MPE_ON) {
            return out;
        }
        let (press, slide) = (
            self.alloc.voice(index).pressure,
            self.alloc.voice(index).slide,
        );
        let press = press.max(self.alloc.channel_pressure(index));
        for (d, a, src) in [
            (PRESS_DEST_A, PRESS_AMT_A, press),
            (PRESS_DEST_B, PRESS_AMT_B, press),
            (SLIDE_DEST_A, SLIDE_AMT_A, slide),
            (SLIDE_DEST_B, SLIDE_AMT_B, slide),
        ] {
            let dest = self.v[d as usize].round() as usize;
            if dest > 0 && dest < out.len() {
                out[dest] += self.f(a) * 0.01 * src;
            }
        }
        out
    }

    fn voice_control(&mut self, index: usize) {
        let sr = self.sr;
        let mpe = self.mpe_mods(index);
        let bend_scale = self.f(NOTE_PB) / 48.0;
        let note_bend = self.alloc.note_bend(index);
        let glided = self.alloc.pitch(index) - note_bend + note_bend * bend_scale;
        let info = self.alloc.voice(index).note;
        let vib_on = self.on(VIB_ON);
        let vib_amt = (self.f(VIB_AMOUNT) * 0.01 * (1.0 + self.f(VIB_MW) * 0.01 * self.mod_wheel)
            + mpe[DEST_VIB])
            .max(0.0);
        let transpose = self.f(OCTAVE) * 12.0 + self.f(SEMI) + self.f(DETUNE) * 0.01;
        let stretch = self.f(STRETCH) * 0.01 * STRETCH_ST_PER_OCTAVE * (glided - 60.0) / 12.0;
        let uni_detune = info.spread * self.f(UNI_DETUNE) * 0.01 * UNI_DETUNE_ST;
        let noise_on = self.on(NOISE_ON);
        let noise_gain = (self.f(NOISE_LEVEL) + mpe[DEST_NOISE]).clamp(0.0, 1.0);
        let noise_color = self.f(NOISE_COLOR);
        let noise_bal = self.f(NOISE_BAL);

        let lfo_on = [self.on(LFO1 + L_ON), self.on(LFO2 + L_ON)];
        let waves = [
            self.v[(LFO1 + L_WAVE) as usize].round() as u8,
            self.v[(LFO2 + L_WAVE) as usize].round() as u8,
        ];
        let widths = [self.f(LFO1 + L_WIDTH) * 0.01, self.f(LFO2 + L_WIDTH) * 0.01];
        let lfo_settings = [self.lfo_settings(LFO1), self.lfo_settings(LFO2)];
        let lfo_boost = [mpe[DEST_LFO1], mpe[DEST_LFO2]];
        let clock = self.clock;

        let values = &self.v;
        let g = |id: u32| values[id as usize] as f32;
        let on = |id: u32| values[id as usize] >= 0.5;
        let vo = &mut self.voices[index];
        let fresh = vo.fresh;
        if vo.delay_left > 0.0 {
            vo.delay_left -= CONTROL_INTERVAL as f32;
            if vo.delay_left <= 0.0 {
                vo.gate_envs();
                if vo.pending_release {
                    vo.pending_release = false;
                    vo.release();
                }
            }
        }
        let mut lfo = [0.0f32; 2];
        for i in 0..2 {
            if lfo_on[i] {
                let unit = &mut vo.lfo[i];
                let mut s = lfo_settings[i];
                if let LfoRate::Hz(hz) = s.rate {
                    s.rate = LfoRate::Hz(hz * 2f32.powf(lfo_boost[i] * 2.0));
                }
                unit.lfo.set(&s);
                unit.lfo.set_clock(&clock);
                lfo[i] = unit.tick(waves[i], widths[i]);
            }
        }
        let frames = CONTROL_INTERVAL as f32;
        // Vibrato.
        let mut vib = 0.0;
        if vib_on {
            if vo.vib_delay > 0.0 {
                vo.vib_delay -= frames / sr;
            } else {
                vo.vib_fade = if vo.vib_attack > 0.0 {
                    (vo.vib_fade + frames / (sr * vo.vib_attack)).min(1.0)
                } else {
                    1.0
                };
                vo.vib_phase = (vo.vib_phase + vo.vib_rate * frames / sr).fract();
                vib = (TAU * vo.vib_phase).sin()
                    * vib_amt
                    * vo.vib_amount_err
                    * VIB_RANGE_ST
                    * vo.vib_fade;
            }
        }
        let key = glided;
        for (i, base) in [OSC1, OSC2].into_iter().enumerate() {
            let on_o = on(base + O_ON);
            let f = |id: u32| g(base + id);
            let shape = values[(base + O_SHAPE) as usize].round() as u8;
            let key_track = f(O_KEY) * 0.01;
            let mpe_pitch = mpe[if i == 0 { DEST_PITCH1 } else { DEST_PITCH2 }] * 24.0;
            let semis = 60.0
                + (key - 60.0) * key_track
                + f(O_OCT) * 12.0
                + f(O_SEMI)
                + f(O_DETUNE) * 0.01
                + lfo[i] * f(O_PITCH_LFO) * 0.01 * LFO_PITCH_ST
                + vo.peg_value[i]
                + vib
                + transpose
                + stretch
                + vo.error
                + uni_detune
                + mpe_pitch;
            let hz = note_hz(semis).clamp(0.1, sr * 0.49);
            let osc = &mut vo.osc[i];
            let dt = hz / sr;
            osc.target_dt = dt;
            if fresh {
                osc.dt = dt;
            }
            osc.dt_step = (dt - osc.dt) / frames;
            osc.shape = shape;
            let mode = values[(base + O_MODE) as usize].round() as u8;
            osc.mode = if shape == 3 { 0 } else { mode };
            osc.sub_level = f(O_SUB) * 0.01;
            let ratio_amt = f(O_RATIO) * 0.01;
            osc.master_ratio = if mode == 2 {
                1.0 + ratio_amt * (SYNC_MAX_RATIO - 1.0)
            } else {
                1.0
            };
            let width = (f(O_WIDTH) * 0.01
                + lfo[i] * f(O_WIDTH_LFO) * 0.01
                + mpe[if i == 0 { DEST_WIDTH1 } else { DEST_WIDTH2 }])
            .clamp(0.01, 1.0);
            osc.width = width * 0.5;
            let bal = (f(O_BAL) + 1.0) * 0.5;
            osc.to_f1 = 1.0 - bal;
            let level = if on_o { f(O_LEVEL) } else { 0.0 };
            osc.target_level = level;
            if fresh {
                osc.level = level;
            }
            osc.level_step = (level - osc.level) / frames;
            // Pitch envelope.
            let init = g(base + O_PEG_INIT) * 0.01 * PEG_RANGE_ST;
            let step = vo.peg_step[i];
            let value = &mut vo.peg_value[i];
            if (init >= 0.0 && *value > 0.0) || (init < 0.0 && *value < 0.0) {
                *value += step;
                if (init >= 0.0 && *value < 0.0) || (init < 0.0 && *value > 0.0) {
                    *value = 0.0;
                }
            } else {
                *value = 0.0;
            }
        }
        vo.noise_coef = 1.0 - (-TAU * noise_color / sr).exp();
        vo.noise_level = if noise_on { noise_gain } else { 0.0 };
        vo.noise_to_f1 = 1.0 - (noise_bal + 1.0) * 0.5;
        vo.to_f2 = g(FILTER1 + F_ROUTE) * 0.01;

        // Filters.
        let mut cutoff1 = 0.0;
        for (i, base) in [FILTER1, FILTER2].into_iter().enumerate() {
            let f = |id: u32| g(base + id);
            let fe = vo.f_env[i].env.level() * vo.f_env[i].gain;
            let key_amt = f(F_FREQ_KEY) * 0.01;
            let semis = key_amt * (key - 60.0)
                + lfo[i] * f(F_FREQ_LFO) * 0.01 * FILTER_LFO_ST
                + fe * f(F_FREQ_ENV) * 0.01 * FILTER_ENV_ST
                + mpe[if i == 0 { DEST_F1 } else { DEST_F2 }] * 48.0;
            let hz = if i == 1 && on(FILTER2 + F_ROUTE) {
                // Follow: Filter 2's knob is an offset from Filter 1, at its centre none.
                cutoff1 * (f(F_FREQ) / 632.46).max(0.001) * 2f32.powf(semis / 12.0)
            } else {
                f(F_FREQ) * 2f32.powf(semis / 12.0)
            };
            let res = f(F_RES)
                + lfo[i] * f(F_RES_LFO) * 0.01
                + (key - 60.0) / 60.0 * f(F_RES_KEY) * 0.01
                + fe * f(F_RES_ENV) * 0.01
                + mpe[if i == 0 { DEST_R1 } else { DEST_R2 }];
            let hz = hz.clamp(10.0, sr * 0.48);
            if i == 0 {
                cutoff1 = hz;
            }
            let unit = &mut vo.filters[i];
            unit.on = on(base + F_ON);
            unit.kind = values[(base + F_TYPE) as usize].round() as u8;
            unit.drive = values[(base + F_DRIVE) as usize].round() as u8;
            unit.set(hz, res);
        }

        // Amps.
        for (i, base) in [AMP1, AMP2].into_iter().enumerate() {
            let f = |id: u32| g(base + id);
            let semis = key - 60.0;
            let db = semis / 12.0 * f(A_LEV_KEY) * 0.01 * LEVEL_KEY_DB_PER_OCT;
            let level = f(A_LEVEL)
                * (1.0 + lfo[i] * f(A_LEV_LFO) * 0.01).clamp(0.0, 2.0)
                * db_to_gain(db)
                * (1.0 + mpe[if i == 0 { DEST_A1 } else { DEST_A2 }]).clamp(0.0, 2.0);
            vo.amp_on[i] = on(base + A_ON);
            vo.amp_target[i] = if vo.amp_on[i] { level } else { 0.0 };
            if fresh {
                vo.amp_level[i] = vo.amp_target[i];
            }
            vo.amp_step[i] = (vo.amp_target[i] - vo.amp_level[i]) / frames;
            vo.env_amt[i] = f(A_LEV_ENV) * 0.01;
            let pan_env = f(A_PAN_ENV) * 0.01;
            let pan = (f(A_PAN)
                + lfo[i] * f(A_PAN_LFO) * 0.01
                + semis / 24.0 * f(A_PAN_KEY) * 0.01
                + vo.a_env[i].env.level() * pan_env)
                .clamp(-1.0, 1.0);
            vo.pan_target[i] = pan_gains(pan);
            if fresh {
                vo.pan[i] = vo.pan_target[i];
            }
        }
        vo.fresh = false;
        vo.f_env.iter_mut().for_each(AdsrEnv::tick);
        vo.a_env.iter_mut().for_each(AdsrEnv::tick);
    }

    fn control_tick(&mut self) {
        if self.dirty {
            self.apply_voice_settings();
            let settings = [self.lfo_settings(LFO1), self.lfo_settings(LFO2)];
            for (lfo, s) in self.free_lfos.iter_mut().zip(settings) {
                lfo.set(&s);
            }
            self.dirty = false;
        }
        self.alloc.advance(CONTROL_INTERVAL);
        for lfo in &mut self.free_lfos {
            lfo.set_clock(&self.clock);
            lfo.advance_phases(CONTROL_INTERVAL);
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
        let mut pos = 0;
        while pos < frames {
            if self.ctrl_phase == 0 {
                self.control_tick();
            }
            let n = (CONTROL_INTERVAL - self.ctrl_phase).min(frames - pos);
            let l = &mut left[pos..pos + n];
            let r = &mut right[pos..pos + n];
            let dc = self.dc_coeff;
            for v in 0..self.voices.len() {
                let vo = &mut self.voices[v];
                if !vo.active {
                    continue;
                }
                let finished = render_voice(vo, l, r, dc);
                if finished {
                    vo.active = false;
                    self.alloc.finished(v);
                }
            }
            self.ctrl_phase = (self.ctrl_phase + n) % CONTROL_INTERVAL;
            pos += n;
        }
    }
}

fn render_voice(v: &mut Voice, left: &mut [f32], right: &mut [f32], dc: f32) -> bool {
    if v.delay_left > 0.0 {
        return false;
    }
    let amp_on = v.amp_on;
    for (l, r) in left.iter_mut().zip(right.iter_mut()) {
        let o1 = v.osc[0].next();
        let o2 = v.osc[1].next();
        v.noise += v.noise_coef * (v.noise_rng.bipolar() - v.noise);
        let n = v.noise * v.noise_level;
        let (t1, t2) = (v.osc[0].to_f1, v.osc[1].to_f1);
        let f1_in = o1 * t1 + o2 * t2 + n * v.noise_to_f1;
        let f2_in = o1 * (1.0 - t1) + o2 * (1.0 - t2) + n * (1.0 - v.noise_to_f1);
        let f1_out = v.filters[0].process(f1_in);
        let f2_out = v.filters[1].process(f2_in + f1_out * v.to_f2);
        let (e1, e2) = (v.a_env[0].next(), v.a_env[1].next());
        v.f_env[0].next();
        v.f_env[1].next();
        v.amp_level[0] += v.amp_step[0];
        v.amp_level[1] += v.amp_step[1];
        let a1 =
            v.dc[0].process(f1_out * VOICE_GAIN, dc) * v.amp_level[0] * env_gain(e1, v.env_amt[0]);
        let a2 =
            v.dc[1].process(f2_out * VOICE_GAIN, dc) * v.amp_level[1] * env_gain(e2, v.env_amt[1]);
        for ch in 0..2 {
            v.pan[ch].0 += (v.pan_target[ch].0 - v.pan[ch].0) * 0.02;
            v.pan[ch].1 += (v.pan_target[ch].1 - v.pan[ch].1) * 0.02;
        }
        *l += sanitize(a1 * v.pan[0].0 + a2 * v.pan[1].0);
        *r += sanitize(a1 * v.pan[0].1 + a2 * v.pan[1].1);
    }
    let a1_done = !amp_on[0] || !v.a_env[0].env.is_active();
    let a2_done = !amp_on[1] || !v.a_env[1].env.is_active();
    a1_done && a2_done
}

#[inline]
fn env_gain(env: f32, amount: f32) -> f32 {
    if amount >= 0.0 {
        lerp(1.0, env, amount)
    } else {
        lerp(1.0, 1.0 - env, -amount)
    }
}

impl Processor for Analog {
    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sr = sample_rate as f32;
        self.dc_coeff = super::common::dc_coeff(self.sr);
        self.alloc.prepare(sample_rate);
        for v in &mut self.voices {
            v.prepare(sample_rate);
        }
        for l in &mut self.free_lfos {
            l.prepare(sample_rate);
        }
        self.reset();
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        self.clock = BeatClock::from_transport(ctx.transport);
        self.tempo = ctx.transport.tempo;
        let this = &mut *self;
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => {
                if let Some(v) = this.v.get_mut(id as usize) {
                    *v = value;
                    this.dirty = true;
                }
            }
            BlockStep::Midi(message) => {
                if let MidiMessage::NoteExpr { kind, value, .. } = message {
                    match kind {
                        rlive_core::ExprKind::Pressure => this.pressure_seen = value,
                        rlive_core::ExprKind::Slide => this.slide_seen = value,
                        rlive_core::ExprKind::Pitch => {}
                    }
                }
                this.midi(message);
            }
            BlockStep::Audio(audio) => this.render(audio.left, audio.right),
        });
        if ctx.view.is_active() {
            ctx.view
                .publish(VIEW, &[self.pressure_seen, self.slide_seen]);
        }
        self.pressure_seen *= 0.9;
        self.slide_seen *= 0.9;
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
        }
        self.dirty = true;
        self.apply_voice_settings();
    }

    fn reset(&mut self) {
        self.alloc.reset();
        self.voices.iter_mut().for_each(Voice::silence);
        self.ctrl_phase = 0;
    }

    fn tail(&self) -> Tail {
        let looped = |b: u32| {
            self.on(b + E_FREE) && matches!(self.v[(b + E_LOOP) as usize].round() as u8, 1 | 2)
        };
        if looped(AMP1 + A_ENV) || looped(AMP2 + A_ENV) {
            return Tail::Infinite;
        }
        let release = self
            .f(AMP1 + A_ENV + E_RELEASE)
            .max(self.f(AMP2 + A_ENV + E_RELEASE))
            * 0.001;
        Tail::Frames(((release + self.f(UNI_DELAY) * 0.004) * self.sr) as u32 + 1)
    }
}
