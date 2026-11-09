//! Drift (manual pp. 722-730): two oscillators and noise into a low-pass and high-pass, two
//! envelopes (the second can cycle), a nine-wave LFO, three free modulation slots and a voice
//! mode for each kind of stacking.
//!
//! The manual gives these numbers: Transpose ±48 st, every modulation amount -100%..100%, the
//! default oscillator gain of -6 dB, 12 and 24 dB low-pass slopes and the 32/16/8 note voice
//! counts. Time ranges, Drift's cent and cutoff spread, the modulation depths at 100% and the
//! high-pass slope are ours; the manual doesn't state them.

use super::common::{
    note_hz, pan_gains, pulse, rich_sine, saturated, saw, triangle, CONTROL_INTERVAL,
};
use crate::dsp::envelope::{Envelope, EnvelopeSettings};
use crate::dsp::filter::{Circuit, Filter, FilterKind, FilterSettings, FilterSlope};
use crate::dsp::modmatrix::{ModBus, ModMatrix, Route, RouteKind};
use crate::dsp::tempo::{BeatClock, SyncRate};
use crate::dsp::util::{db_to_gain, sanitize, Rng};
use crate::dsp::voice::{
    GlideCurve, GlideMode, NoteInfo, VoiceAllocator, VoiceEvent, VoiceMode, VoiceSettings,
};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    BlockStep, DeviceDesc, Item, MidiMessage, ParamId, ParamInfo, ProcessContext, Processor,
    Section, Status, Tail,
};
use std::f32::consts::TAU;

pub const OSC1_WAVE: u32 = 0;
pub const OSC1_OCT: u32 = 1;
pub const OSC1_SHAPE: u32 = 2;
pub const OSC1_SHAPE_SRC: u32 = 3;
pub const OSC1_SHAPE_AMT: u32 = 4;
pub const OSC2_WAVE: u32 = 5;
pub const OSC2_OCT: u32 = 6;
pub const OSC2_DETUNE: u32 = 7;
pub const PITCH_SRC_A: u32 = 8;
pub const PITCH_AMT_A: u32 = 9;
pub const PITCH_SRC_B: u32 = 10;
pub const PITCH_AMT_B: u32 = 11;
pub const OSC1_ON: u32 = 12;
pub const OSC1_GAIN: u32 = 13;
pub const OSC1_FILTER: u32 = 14;
pub const OSC2_ON: u32 = 15;
pub const OSC2_GAIN: u32 = 16;
pub const OSC2_FILTER: u32 = 17;
pub const NOISE_ON: u32 = 18;
pub const NOISE_GAIN: u32 = 19;
pub const NOISE_FILTER: u32 = 20;
pub const OSC_RETRIG: u32 = 21;
pub const LP_FREQ: u32 = 22;
pub const LP_TYPE: u32 = 23;
pub const LP_KEY: u32 = 24;
pub const LP_RES: u32 = 25;
pub const HP_FREQ: u32 = 26;
pub const LP_SRC_A: u32 = 27;
pub const LP_AMT_A: u32 = 28;
pub const LP_SRC_B: u32 = 29;
pub const LP_AMT_B: u32 = 30;
pub const E1_ATTACK: u32 = 31;
pub const E1_DECAY: u32 = 32;
pub const E1_SUSTAIN: u32 = 33;
pub const E1_RELEASE: u32 = 34;
pub const E2_ATTACK: u32 = 35;
pub const E2_DECAY: u32 = 36;
pub const E2_SUSTAIN: u32 = 37;
pub const E2_RELEASE: u32 = 38;
pub const CYC_ON: u32 = 39;
pub const CYC_MODE: u32 = 40;
pub const CYC_RATE: u32 = 41;
pub const CYC_RATIO: u32 = 42;
pub const CYC_TIME: u32 = 43;
pub const CYC_SYNC: u32 = 44;
pub const CYC_TILT: u32 = 45;
pub const CYC_HOLD: u32 = 46;
pub const LFO_WAVE: u32 = 47;
pub const LFO_MODE: u32 = 48;
pub const LFO_RATE: u32 = 49;
pub const LFO_RATIO: u32 = 50;
pub const LFO_TIME: u32 = 51;
pub const LFO_SYNC: u32 = 52;
pub const LFO_RETRIG: u32 = 53;
pub const LFO_AMOUNT: u32 = 54;
pub const LFO_MOD_SRC: u32 = 55;
pub const LFO_MOD_AMT: u32 = 56;
pub const MOD_BASE: u32 = 57;
const MOD_SLOT: u32 = 3;
pub const VOICE_MODE: u32 = MOD_BASE + 3 * MOD_SLOT;
pub const THICKNESS: u32 = VOICE_MODE + 1;
pub const SPREAD: u32 = VOICE_MODE + 2;
pub const STRENGTH: u32 = VOICE_MODE + 3;
pub const VOICES: u32 = VOICE_MODE + 4;
pub const DRIFT: u32 = VOICE_MODE + 5;
pub const LEGATO: u32 = VOICE_MODE + 6;
pub const GLIDE: u32 = VOICE_MODE + 7;
pub const VOLUME: u32 = VOICE_MODE + 8;
pub const VEL_VOL: u32 = VOICE_MODE + 9;
pub const TRANSPOSE: u32 = VOICE_MODE + 10;
pub const NOTE_PB: u32 = VOICE_MODE + 11;
pub const PB_RANGE: u32 = VOICE_MODE + 12;
pub const PARAM_COUNT: usize = (VOICE_MODE + 13) as usize;

pub const OSC1_WAVES: [&str; 7] = [
    "Sine",
    "Triangle",
    "Shark Tooth",
    "Saturated",
    "Saw",
    "Pulse",
    "Rectangle",
];
pub const OSC2_WAVES: [&str; 5] = ["Sine", "Triangle", "Saturated", "Saw", "Rectangle"];
/// What each entry of the second oscillator's list is on the first's.
const OSC2_TO_OSC1: [u8; 5] = [0, 1, 3, 4, 6];
pub const TIME_MODES: [&str; 4] = ["Rate", "Ratio", "Time", "Sync"];
pub const SOURCES: [&str; 8] = [
    "Env 1",
    "Env 2/Cyc",
    "LFO",
    "Key",
    "Velocity",
    "Modwheel",
    "Pressure",
    "Slide",
];
pub const LFO_WAVES: [&str; 9] = [
    "Sine",
    "Triangle",
    "Saw Up",
    "Saw Down",
    "Square",
    "Sample & Hold",
    "Wander",
    "Linear Env",
    "Exp Env",
];
pub const DESTINATIONS: [&str; 11] = [
    "Osc 1 Gain",
    "Osc 1 Shape",
    "Osc 2 Gain",
    "Osc 2 Detune",
    "Noise Gain",
    "LP Frequency",
    "LP Resonance",
    "HP Frequency",
    "LFO Rate",
    "Cyc Env Rate",
    "Main Volume",
];
pub const MODES: [&str; 4] = ["Poly", "Mono", "Stereo", "Unison"];
pub const VOICE_COUNTS: [usize; 7] = [4, 8, 12, 16, 20, 24, 32];

const SRC_ENV1: usize = 0;
const SRC_ENV2: usize = 1;
const SRC_LFO: usize = 2;
const SRC_KEY: usize = 3;
const SRC_VEL: usize = 4;
const SRC_MW: usize = 5;
const SRC_PRESS: usize = 6;
const SRC_SLIDE: usize = 7;
const SOURCE_COUNT: usize = 8;

// Destination indices in the matrix: the eleven the Mod section offers, then the fixed slots.
const D_OSC1_GAIN: usize = 0;
const D_SHAPE: usize = 1;
const D_OSC2_GAIN: usize = 2;
const D_DETUNE: usize = 3;
const D_NOISE: usize = 4;
const D_LP: usize = 5;
const D_RES: usize = 6;
const D_HP: usize = 7;
const D_LFO_RATE: usize = 8;
const D_CYC_RATE: usize = 9;
const D_VOLUME: usize = 10;
const D_PITCH: usize = 11;
const D_LFO_AMT: usize = 12;
const DEST_COUNT: usize = 13;

/// Matrix slots: three in the Mod section, two for pitch, one for shape, two for the low-pass,
/// one for the LFO's amount.
const ROUTE_COUNT: usize = 9;

const MAX_VOICES: usize = 32;
const VOICE_GAIN: f32 = 0.5;
const PITCH_MOD_ST: f32 = 24.0;
const CUTOFF_MOD_ST: f32 = 72.0;
const VOLUME_MOD_DB: f32 = 12.0;
const RATE_MOD_OCT: f32 = 4.0;
const DRIFT_PITCH_ST: f32 = 0.3;
const DRIFT_CUTOFF_ST: f32 = 1.0;
const UNISON_DETUNE_ST: f32 = 0.5;
const MONO_DETUNE_ST: f32 = 0.12;

pub fn params() -> Vec<ParamInfo> {
    let pct = |id, name: &str, min, max, def| ParamInfo::continuous(id, name, min, max, def, "%");
    let time = |id, name: &str, def| ParamInfo::skewed(id, name, 1.0, 20_000.0, def, "ms", 4.0);
    let source = |id, name: &str, def| ParamInfo::stepped(id, name, &SOURCES, def);
    let sync_rate = |id, name: &str| ParamInfo::stepped(id, name, &SyncRate::LABELS, 12);
    let mut out = vec![
        ParamInfo::stepped(OSC1_WAVE, "Osc 1", &OSC1_WAVES, 4),
        ParamInfo::continuous(OSC1_OCT, "Osc 1 Oct", -3.0, 3.0, 0.0, ""),
        pct(OSC1_SHAPE, "Osc 1 Shape", 0.0, 100.0, 0.0),
        source(OSC1_SHAPE_SRC, "Osc 1 Shape Mod Source", SRC_ENV2),
        pct(OSC1_SHAPE_AMT, "Osc 1 Shape Mod Amount", -100.0, 100.0, 0.0),
        ParamInfo::stepped(OSC2_WAVE, "Osc 2", &OSC2_WAVES, 3),
        ParamInfo::continuous(OSC2_OCT, "Osc 2 Oct", -3.0, 3.0, 0.0, ""),
        ParamInfo::continuous(OSC2_DETUNE, "Osc 2 Detune", -12.0, 12.0, 0.0, "st"),
        source(PITCH_SRC_A, "Pitch Mod Source A", SRC_ENV2),
        pct(PITCH_AMT_A, "Pitch Mod Amount A", -100.0, 100.0, 0.0),
        source(PITCH_SRC_B, "Pitch Mod Source B", SRC_LFO),
        pct(PITCH_AMT_B, "Pitch Mod Amount B", -100.0, 100.0, 0.0),
        ParamInfo::toggle(OSC1_ON, "Osc 1 On", true),
        ParamInfo::continuous(OSC1_GAIN, "Osc 1 Gain", -60.0, 6.0, -6.0, "dB"),
        ParamInfo::toggle(OSC1_FILTER, "Osc 1 Filter", true),
        ParamInfo::toggle(OSC2_ON, "Osc 2 On", false),
        ParamInfo::continuous(OSC2_GAIN, "Osc 2 Gain", -60.0, 6.0, -6.0, "dB"),
        ParamInfo::toggle(OSC2_FILTER, "Osc 2 Filter", true),
        ParamInfo::toggle(NOISE_ON, "Noise On", false),
        ParamInfo::continuous(NOISE_GAIN, "Noise Gain", -60.0, 6.0, -12.0, "dB"),
        ParamInfo::toggle(NOISE_FILTER, "Noise Filter", true),
        ParamInfo::toggle(OSC_RETRIG, "R", false),
        ParamInfo::skewed(LP_FREQ, "LP Freq", 20.0, 20_000.0, 8_000.0, "Hz", 2.5),
        ParamInfo::stepped(LP_TYPE, "LP Type", &["I (12 dB)", "II (24 dB)"], 0),
        ParamInfo::continuous(LP_KEY, "LP Key", 0.0, 1.0, 0.0, ""),
        ParamInfo::continuous(LP_RES, "LP Res", 0.0, 1.0, 0.0, ""),
        ParamInfo::skewed(HP_FREQ, "HP", 10.0, 2_000.0, 10.0, "Hz", 2.5),
        source(LP_SRC_A, "LP Mod Source A", SRC_ENV2),
        pct(LP_AMT_A, "LP Mod Amount A", -100.0, 100.0, 0.0),
        source(LP_SRC_B, "LP Mod Source B", SRC_LFO),
        pct(LP_AMT_B, "LP Mod Amount B", -100.0, 100.0, 0.0),
        time(E1_ATTACK, "Env 1 Attack", 2.0),
        time(E1_DECAY, "Env 1 Decay", 300.0),
        ParamInfo::continuous(E1_SUSTAIN, "Env 1 Sustain", 0.0, 1.0, 0.8, ""),
        time(E1_RELEASE, "Env 1 Release", 100.0),
        time(E2_ATTACK, "Env 2 Attack", 2.0),
        time(E2_DECAY, "Env 2 Decay", 400.0),
        ParamInfo::continuous(E2_SUSTAIN, "Env 2 Sustain", 0.0, 1.0, 0.2, ""),
        time(E2_RELEASE, "Env 2 Release", 200.0),
        ParamInfo::toggle(CYC_ON, "Cycling Env", false),
        ParamInfo::stepped(CYC_MODE, "Cyc Time Mode", &TIME_MODES, 0),
        ParamInfo::skewed(CYC_RATE, "Cyc Rate", 0.05, 100.0, 2.0, "Hz", 3.0),
        ParamInfo::skewed(CYC_RATIO, "Cyc Ratio", 0.1, 16.0, 1.0, "", 2.5),
        ParamInfo::skewed(CYC_TIME, "Cyc Time", 1.0, 10_000.0, 250.0, "ms", 3.0),
        sync_rate(CYC_SYNC, "Cyc Sync"),
        pct(CYC_TILT, "Tilt", 0.0, 100.0, 50.0),
        pct(CYC_HOLD, "Hold", 0.0, 100.0, 0.0),
        ParamInfo::stepped(LFO_WAVE, "LFO Wave", &LFO_WAVES, 0),
        ParamInfo::stepped(LFO_MODE, "LFO Time Mode", &TIME_MODES, 0),
        ParamInfo::skewed(LFO_RATE, "LFO Rate", 0.01, 100.0, 3.0, "Hz", 3.0),
        ParamInfo::skewed(LFO_RATIO, "LFO Ratio", 0.1, 16.0, 1.0, "", 2.5),
        ParamInfo::skewed(LFO_TIME, "LFO Time", 1.0, 10_000.0, 250.0, "ms", 3.0),
        sync_rate(LFO_SYNC, "LFO Sync"),
        ParamInfo::toggle(LFO_RETRIG, "LFO R", false),
        pct(LFO_AMOUNT, "LFO Amount", 0.0, 100.0, 100.0),
        source(LFO_MOD_SRC, "LFO Mod Source", SRC_MW),
        pct(LFO_MOD_AMT, "LFO Mod Amount", -100.0, 100.0, 0.0),
    ];
    for slot in 0..3u32 {
        let n = slot + 1;
        let b = MOD_BASE + slot * MOD_SLOT;
        out.push(source(b, &format!("Mod {n} Source"), SRC_MW));
        out.push(ParamInfo::stepped(
            b + 1,
            &format!("Mod {n} Destination"),
            &DESTINATIONS,
            D_LP,
        ));
        out.push(pct(b + 2, &format!("Mod {n} Amount"), -100.0, 100.0, 0.0));
    }
    out.extend([
        ParamInfo::stepped(VOICE_MODE, "Mode", &MODES, 0),
        pct(THICKNESS, "Mono Thickness", 0.0, 100.0, 50.0),
        pct(SPREAD, "Stereo Spread", 0.0, 100.0, 80.0),
        pct(STRENGTH, "Unison Strength", 0.0, 100.0, 50.0),
        ParamInfo::stepped(
            VOICES,
            "Voices",
            &["4", "8", "12", "16", "20", "24", "32"],
            6,
        ),
        pct(DRIFT, "Drift", 0.0, 100.0, 0.0),
        ParamInfo::toggle(LEGATO, "Legato", false),
        ParamInfo::skewed(GLIDE, "Glide", 0.0, 5_000.0, 0.0, "ms", 3.0),
        ParamInfo::continuous(VOLUME, "Volume", -48.0, 12.0, 0.0, "dB"),
        pct(VEL_VOL, "Vel > Vol", 0.0, 100.0, 50.0),
        ParamInfo::continuous(TRANSPOSE, "Transpose", -48.0, 48.0, 0.0, "st"),
        ParamInfo::toggle(NOTE_PB, "Note PB", true),
        ParamInfo::continuous(PB_RANGE, "PB Range", 0.0, 48.0, 2.0, "st"),
    ]);
    out
}

fn layout() -> Vec<Section> {
    let knob = |id| Item::Knob(ParamId(id));
    let menu = |id| Item::Menu(ParamId(id));
    let toggle = |id| Item::Toggle(ParamId(id));
    vec![
        Section {
            title: "Display",
            items: vec![Item::Panel("drift_display")],
        },
        Section {
            title: "Oscillators",
            items: vec![
                Item::Row(vec![menu(OSC1_WAVE), knob(OSC1_OCT), knob(OSC1_SHAPE)]),
                Item::Row(vec![menu(OSC1_SHAPE_SRC), knob(OSC1_SHAPE_AMT)]),
                Item::Row(vec![menu(OSC2_WAVE), knob(OSC2_OCT), knob(OSC2_DETUNE)]),
                Item::Row(vec![
                    menu(PITCH_SRC_A),
                    knob(PITCH_AMT_A),
                    menu(PITCH_SRC_B),
                    knob(PITCH_AMT_B),
                ]),
            ],
        },
        Section {
            title: "Mixer",
            items: vec![
                Item::Row(vec![toggle(OSC1_ON), knob(OSC1_GAIN), toggle(OSC1_FILTER)]),
                Item::Row(vec![toggle(OSC2_ON), knob(OSC2_GAIN), toggle(OSC2_FILTER)]),
                Item::Row(vec![
                    toggle(NOISE_ON),
                    knob(NOISE_GAIN),
                    toggle(NOISE_FILTER),
                    toggle(OSC_RETRIG),
                ]),
            ],
        },
        Section {
            title: "Filter",
            items: vec![
                Item::Row(vec![
                    knob(LP_FREQ),
                    menu(LP_TYPE),
                    knob(LP_KEY),
                    knob(LP_RES),
                    knob(HP_FREQ),
                ]),
                Item::Row(vec![
                    menu(LP_SRC_A),
                    knob(LP_AMT_A),
                    menu(LP_SRC_B),
                    knob(LP_AMT_B),
                ]),
            ],
        },
        Section {
            title: "Env 1",
            items: vec![Item::Row(vec![
                knob(E1_ATTACK),
                knob(E1_DECAY),
                knob(E1_SUSTAIN),
                knob(E1_RELEASE),
            ])],
        },
        Section {
            title: "Env 2",
            items: vec![
                Item::Row(vec![
                    toggle(CYC_ON),
                    knob(E2_ATTACK),
                    knob(E2_DECAY),
                    knob(E2_SUSTAIN),
                    knob(E2_RELEASE),
                ]),
                Item::Row(vec![
                    menu(CYC_MODE),
                    knob(CYC_RATE),
                    knob(CYC_RATIO),
                    knob(CYC_TIME),
                    menu(CYC_SYNC),
                ]),
                Item::Row(vec![knob(CYC_TILT), knob(CYC_HOLD)]),
            ],
        },
        Section {
            title: "LFO",
            items: vec![
                Item::Row(vec![
                    menu(LFO_WAVE),
                    menu(LFO_MODE),
                    knob(LFO_RATE),
                    knob(LFO_RATIO),
                ]),
                Item::Row(vec![
                    knob(LFO_TIME),
                    menu(LFO_SYNC),
                    toggle(LFO_RETRIG),
                    knob(LFO_AMOUNT),
                ]),
                Item::Row(vec![menu(LFO_MOD_SRC), knob(LFO_MOD_AMT)]),
            ],
        },
        Section {
            title: "Mod",
            items: (0..3u32)
                .map(|slot| {
                    let b = MOD_BASE + slot * MOD_SLOT;
                    Item::Row(vec![menu(b), menu(b + 1), knob(b + 2)])
                })
                .collect(),
        },
        Section {
            title: "Global",
            items: vec![
                Item::Row(vec![
                    menu(VOICE_MODE),
                    menu(VOICES),
                    knob(THICKNESS),
                    knob(SPREAD),
                    knob(STRENGTH),
                ]),
                Item::Row(vec![knob(DRIFT), toggle(LEGATO), knob(GLIDE)]),
                Item::Row(vec![
                    knob(VOLUME),
                    knob(VEL_VOL),
                    knob(TRANSPOSE),
                    toggle(NOTE_PB),
                    knob(PB_RANGE),
                ]),
            ],
        },
    ]
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.drift",
    name: "Drift",
    role: DeviceRole::Instrument,
    category: "Synths",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::new(Drift::new()),
};

/// One sample of an oscillator waveform, band-limited. `wave` indexes [`OSC1_WAVES`]. The display
/// calls this too, with a small `dt`.
#[inline]
pub fn osc_sample(wave: u8, phase: f32, dt: f32, shape: f32) -> f32 {
    match wave {
        0 => rich_sine(phase, dt, shape),
        1 => triangle(phase, dt, 0.5 - 0.45 * shape),
        2 => triangle(phase, dt, 0.8 - 0.3 * shape),
        3 => saturated(phase, dt, shape),
        4 => (saw(phase, dt) - shape * 0.8 * saw((phase + 0.5).fract(), dt)) / (1.0 + shape * 0.5),
        5 => pulse(phase, dt, 0.25 * (1.0 - 0.9 * shape)),
        _ => pulse(phase, dt, 0.5 * (1.0 - 0.9 * shape)),
    }
}

/// The second oscillator's waveform, indexing [`OSC2_WAVES`], has no Shape.
#[inline]
pub fn osc2_sample(wave: u8, phase: f32, dt: f32) -> f32 {
    osc_sample(OSC2_TO_OSC1[wave.min(4) as usize], phase, dt, 0.0)
}

/// The first saturation point, before the filter: linear to half scale, then rounding off.
#[inline]
pub fn pre_saturate(x: f32) -> f32 {
    let a = x.abs();
    if a <= 0.5 {
        x
    } else {
        (0.5 + ((a - 0.5) * 2.0).tanh() * 0.5).copysign(x)
    }
}

/// The second saturation point, after the filter: linear to full scale.
#[inline]
pub fn post_saturate(x: f32) -> f32 {
    let a = x.abs();
    if a <= 1.0 {
        x
    } else {
        (1.0 + (a - 1.0).tanh() * 0.5).copysign(x)
    }
}

/// The cycling envelope's shape at `phase`: up to 1 by the midpoint `tilt`, held for `hold` of
/// the cycle, then back to 0.
#[inline]
pub fn cycling_shape(phase: f32, tilt: f32, hold: f32) -> f32 {
    let hold = hold.clamp(0.0, 0.95);
    let span = 1.0 - hold;
    let rise = (tilt * span).clamp(0.002, span - 0.002);
    let t = phase.fract();
    let fall_start = rise + hold;
    if t < rise {
        t / rise
    } else if t < fall_start {
        1.0
    } else {
        1.0 - (t - fall_start) / (1.0 - fall_start)
    }
}

/// The LFO's value at `phase`. `previous`/`current` are the random values Sample & Hold and
/// Wander read. `elapsed` is how many whole cycles have run, which ends the one-shot envelopes.
#[inline]
fn lfo_shape(wave: u8, phase: f32, previous: f32, current: f32, elapsed: u32) -> f32 {
    let p = phase;
    match wave {
        0 => (TAU * p).sin(),
        1 => {
            if p < 0.25 {
                4.0 * p
            } else if p < 0.75 {
                2.0 - 4.0 * p
            } else {
                4.0 * p - 4.0
            }
        }
        2 => 2.0 * p - 1.0,
        3 => 1.0 - 2.0 * p,
        4 => {
            if p < 0.5 {
                1.0
            } else {
                -1.0
            }
        }
        5 => current,
        6 => {
            let t = 0.5 - 0.5 * (p * std::f32::consts::PI).cos();
            previous + (current - previous) * t
        }
        7 => {
            if elapsed == 0 {
                1.0 - p
            } else {
                0.0
            }
        }
        _ => {
            if elapsed == 0 {
                (-5.0 * p).exp()
            } else {
                0.0
            }
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
    unison_index: u8,
    spread: f32,
    drift_pitch: [f32; 2],
    drift_cut: f32,
    wander: f32,
    phase: [f32; 2],
    dt: [f32; 2],
    dt_step: [f32; 2],
    fm_depth: f32,
    fm_cyc_depth: f32,
    env1: Envelope,
    env2: Envelope,
    cyc_phase: f32,
    cyc_hz: f32,
    lfo_phase: f32,
    lfo_dt: f32,
    lfo_prev: f32,
    lfo_cur: f32,
    lfo_cycles: u32,
    lfo_value: f32,
    lfo_rng: Rng,
    rng: Rng,
    bus: ModBus,
    lp: Filter,
    hp: Filter,
    gain: [f32; 3],
    gain_target: [f32; 3],
    amp: f32,
    amp_target: f32,
    pan: (f32, f32),
    pressure: f32,
    slide: f32,
    shape: f32,
}

impl Voice {
    fn new(seed: u32, bus: ModBus) -> Self {
        Self {
            active: false,
            gated: false,
            fresh: false,
            key: 60.0,
            velocity: 1.0,
            unison_index: 0,
            spread: 0.0,
            drift_pitch: [0.0; 2],
            drift_cut: 0.0,
            wander: 0.0,
            phase: [0.0; 2],
            dt: [0.0; 2],
            dt_step: [0.0; 2],
            fm_depth: 0.0,
            fm_cyc_depth: 0.0,
            env1: Envelope::new(),
            env2: Envelope::new(),
            cyc_phase: 0.0,
            cyc_hz: 1.0,
            lfo_phase: 0.0,
            lfo_dt: 0.0,
            lfo_prev: 0.0,
            lfo_cur: 0.0,
            lfo_cycles: 0,
            lfo_value: 0.0,
            lfo_rng: Rng::new(seed),
            rng: Rng::new(seed.wrapping_add(1)),
            bus,
            lp: Filter::new(),
            hp: Filter::new(),
            gain: [0.0; 3],
            gain_target: [0.0; 3],
            amp: 0.0,
            amp_target: 0.0,
            pan: (1.0, 1.0),
            pressure: 0.0,
            slide: 0.0,
            shape: 0.0,
        }
    }

    fn silence(&mut self) {
        self.active = false;
        self.gated = false;
        self.fresh = false;
        self.env1.kill();
        self.env2.kill();
        self.lp.reset();
        self.hp.reset();
        self.gain = [0.0; 3];
        self.amp = 0.0;
    }

    fn release(&mut self) {
        self.gated = false;
        self.env1.gate_off();
        self.env2.gate_off();
    }
}

#[derive(Clone, Copy, Default)]
struct Render {
    osc1_wave: u8,
    osc2_wave: u8,
    osc1_on: bool,
    osc2_on: bool,
    noise_on: bool,
    filters: [bool; 3],
    lfo_wave: u8,
    lfo_amount: f32,
    cycling: bool,
    tilt: f32,
    hold: f32,
    sample_rate: f32,
}

/// Drift's engine.
pub struct Drift {
    v: Vec<f64>,
    alloc: VoiceAllocator,
    voices: Vec<Voice>,
    matrix: ModMatrix,
    sr: f32,
    ctrl_phase: usize,
    mod_wheel: f32,
    free_lfo: f32,
    clock: BeatClock,
    tempo: f64,
    dirty: bool,
    render_state: Render,
    wander_rng: Rng,
}

impl Drift {
    /// How many voices are sounding right now.
    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active).count()
    }
}

impl Default for Drift {
    fn default() -> Self {
        Self::new()
    }
}

impl Drift {
    pub fn new() -> Self {
        let defaults = params();
        let mut v = vec![0.0; PARAM_COUNT];
        for p in &defaults {
            v[p.id as usize] = p.default;
        }
        let matrix = ModMatrix::new(SOURCE_COUNT, DEST_COUNT, ROUTE_COUNT);
        let voices = (0..MAX_VOICES)
            .map(|i| Voice::new(11 + i as u32 * 17, matrix.bus()))
            .collect();
        let mut this = Self {
            v,
            alloc: VoiceAllocator::new(MAX_VOICES),
            voices,
            matrix,
            sr: 48_000.0,
            ctrl_phase: 0,
            mod_wheel: 0.0,
            free_lfo: 0.0,
            clock: BeatClock::default(),
            tempo: 120.0,
            dirty: true,
            render_state: Render::default(),
            wander_rng: Rng::new(0xD21F),
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

    fn voice_mode(&self) -> usize {
        self.index(VOICE_MODE).min(3)
    }

    fn voice_settings(&self) -> VoiceSettings {
        let count = VOICE_COUNTS[self.index(VOICES).min(VOICE_COUNTS.len() - 1)];
        let glide = self.f(GLIDE) * 0.001;
        let (mode, polyphony, unison) = match self.voice_mode() {
            1 => {
                let m = if self.on(LEGATO) {
                    VoiceMode::Legato
                } else {
                    VoiceMode::Mono
                };
                (m, 1, 4)
            }
            2 => (VoiceMode::Poly, count / 2, 2),
            3 => (VoiceMode::Poly, count / 4, 4),
            _ => (VoiceMode::Poly, count, 1),
        };
        VoiceSettings {
            mode,
            polyphony: polyphony.max(1),
            unison,
            glide: if glide > 0.0 {
                GlideMode::LegatoOnly
            } else {
                GlideMode::Off
            },
            glide_curve: GlideCurve::ConstantTime,
            glide_time: glide,
            bend_range: self.f(PB_RANGE),
            ..VoiceSettings::default()
        }
    }

    fn set_route(&mut self, slot: usize, source: usize, dest: usize, amount: f32) {
        self.matrix.set_route(
            slot,
            (amount != 0.0).then_some(Route {
                source,
                dest,
                amount,
                kind: RouteKind::Add,
                via: None,
            }),
        );
    }

    fn refresh(&mut self) {
        let settings = self.voice_settings();
        self.alloc.set(&settings);
        for slot in 0..3 {
            let b = MOD_BASE + slot as u32 * MOD_SLOT;
            let (src, dest, amt) = (self.index(b), self.index(b + 1), self.f(b + 2) * 0.01);
            self.set_route(slot, src.min(7), dest.min(10), amt);
        }
        let slots = [
            (PITCH_SRC_A, D_PITCH, PITCH_AMT_A),
            (PITCH_SRC_B, D_PITCH, PITCH_AMT_B),
            (OSC1_SHAPE_SRC, D_SHAPE, OSC1_SHAPE_AMT),
            (LP_SRC_A, D_LP, LP_AMT_A),
            (LP_SRC_B, D_LP, LP_AMT_B),
            (LFO_MOD_SRC, D_LFO_AMT, LFO_MOD_AMT),
        ];
        for (i, (src, dest, amt)) in slots.into_iter().enumerate() {
            let (src, amount) = (self.index(src).min(7), self.f(amt) * 0.01);
            self.set_route(3 + i, src, dest, amount);
        }
        self.render_state = Render {
            osc1_wave: self.index(OSC1_WAVE).min(6) as u8,
            osc2_wave: self.index(OSC2_WAVE).min(4) as u8,
            osc1_on: self.on(OSC1_ON),
            osc2_on: self.on(OSC2_ON),
            noise_on: self.on(NOISE_ON),
            filters: [
                self.on(OSC1_FILTER),
                self.on(OSC2_FILTER),
                self.on(NOISE_FILTER),
            ],
            lfo_wave: self.index(LFO_WAVE).min(8) as u8,
            lfo_amount: self.f(LFO_AMOUNT) * 0.01,
            cycling: self.on(CYC_ON),
            tilt: self.f(CYC_TILT) * 0.01,
            hold: self.f(CYC_HOLD) * 0.01,
            sample_rate: self.sr,
        };
    }

    fn envelope(&self, attack: u32, decay: u32, sustain: u32, release: u32) -> EnvelopeSettings {
        EnvelopeSettings {
            attack: self.f(attack) * 0.001,
            decay: self.f(decay) * 0.001,
            sustain: self.f(sustain).clamp(0.0, 1.0),
            release: self.f(release) * 0.001,
            decay_slope: 0.6,
            release_slope: 0.6,
            ..EnvelopeSettings::default()
        }
    }

    fn start_voice(&mut self, index: usize, note: NoteInfo, stolen: bool) {
        let env1 = self.envelope(E1_ATTACK, E1_DECAY, E1_SUSTAIN, E1_RELEASE);
        let env2 = self.envelope(E2_ATTACK, E2_DECAY, E2_SUSTAIN, E2_RELEASE);
        let retrig_osc = self.on(OSC_RETRIG);
        let retrig_lfo = self.on(LFO_RETRIG);
        let free = self.free_lfo;
        let vo = &mut self.voices[index];
        vo.key = note.key as f32;
        vo.velocity = note.velocity.clamp(0.0, 1.0);
        vo.unison_index = note.unison_index;
        vo.spread = note.spread;
        vo.env1.set(&env1);
        vo.env2.set(&env2);
        if retrig_osc {
            vo.phase = [0.0; 2];
        } else if !stolen {
            vo.phase = [vo.rng.unipolar(), vo.rng.unipolar()];
        }
        vo.cyc_phase = 0.0;
        vo.lfo_phase = if retrig_lfo { 0.0 } else { free };
        vo.lfo_cycles = 0;
        vo.lfo_prev = vo.lfo_rng.bipolar();
        vo.lfo_cur = vo.lfo_rng.bipolar();
        vo.drift_pitch = [vo.rng.bipolar(), vo.rng.bipolar()];
        vo.drift_cut = vo.rng.bipolar();
        vo.wander = 0.0;
        vo.env1.gate_on();
        vo.env2.gate_on();
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
                VoiceEvent::Start {
                    voice,
                    note,
                    stolen,
                } => {
                    self.start_voice(voice, note, stolen);
                    self.voice_control(voice);
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

    fn time_hz(
        &self,
        mode: usize,
        rate: u32,
        ratio: u32,
        time: u32,
        sync: u32,
        pitch_hz: f32,
    ) -> f32 {
        match mode {
            1 => pitch_hz * self.f(ratio),
            2 => 1_000.0 / self.f(time).max(0.1),
            3 => {
                let beats = SyncRate::from_index(self.v[sync as usize])
                    .beats()
                    .max(1e-3);
                (self.tempo / 60.0 / beats) as f32
            }
            _ => self.f(rate),
        }
    }

    fn voice_control(&mut self, index: usize) {
        let info = self.alloc.voice(index);
        let (pressure, slide) = (
            info.pressure.max(self.alloc.channel_pressure(index)),
            info.slide,
        );
        let unison_index = info.note.unison_index as usize;
        let note_bend = self.alloc.note_bend(index);
        let base_pitch =
            self.alloc.pitch(index) - note_bend + if self.on(NOTE_PB) { note_bend } else { 0.0 };
        let mode = self.voice_mode();
        let sr = self.sr;
        let frames = CONTROL_INTERVAL as f32;
        let drift = self.f(DRIFT) * 0.01;
        let cycling = self.on(CYC_ON);
        let lfo_mode = self.index(LFO_MODE).min(3);
        let cyc_mode = self.index(CYC_MODE).min(3);
        let lfo_rate = self.time_hz(
            lfo_mode,
            LFO_RATE,
            LFO_RATIO,
            LFO_TIME,
            LFO_SYNC,
            note_hz(base_pitch),
        );
        let cyc_rate = self.time_hz(
            cyc_mode,
            CYC_RATE,
            CYC_RATIO,
            CYC_TIME,
            CYC_SYNC,
            note_hz(base_pitch),
        );
        let (transpose, wave1, wave_detune) =
            (self.f(TRANSPOSE), self.f(OSC1_OCT), self.f(OSC2_DETUNE));
        let oct2 = self.f(OSC2_OCT);
        let (gain_db, lp_key, lp_base, lp_res, hp_base, lp_type, volume_db, vel_vol) = (
            [self.f(OSC1_GAIN), self.f(OSC2_GAIN), self.f(NOISE_GAIN)],
            self.f(LP_KEY),
            self.f(LP_FREQ),
            self.f(LP_RES),
            self.f(HP_FREQ),
            self.index(LP_TYPE).min(1),
            self.f(VOLUME),
            self.f(VEL_VOL) * 0.01,
        );
        let (thickness, stereo_spread, strength) = (
            self.f(THICKNESS) * 0.01,
            self.f(SPREAD) * 0.01,
            self.f(STRENGTH) * 0.01,
        );
        let shape_base = self.f(OSC1_SHAPE) * 0.01;
        let pitch_fm: [(bool, f32); 2] = [
            (
                self.index(PITCH_SRC_A) == SRC_LFO && lfo_mode == 1,
                self.f(PITCH_AMT_A) * 0.01,
            ),
            (
                self.index(PITCH_SRC_B) == SRC_LFO && lfo_mode == 1,
                self.f(PITCH_AMT_B) * 0.01,
            ),
        ];
        let cyc_fm: [(bool, f32); 2] = [
            (
                self.index(PITCH_SRC_A) == SRC_ENV2 && cycling && cyc_mode == 1,
                self.f(PITCH_AMT_A) * 0.01,
            ),
            (
                self.index(PITCH_SRC_B) == SRC_ENV2 && cycling && cyc_mode == 1,
                self.f(PITCH_AMT_B) * 0.01,
            ),
        ];
        let wander_noise = self.wander_rng.bipolar();
        let mw = self.mod_wheel;
        let vo = &mut self.voices[index];
        vo.wander += (wander_noise - vo.wander) * 0.002;
        // Sources.
        let env2_value = if cycling {
            cycling_shape(vo.cyc_phase, self.render_state.tilt, self.render_state.hold)
        } else {
            vo.env2.level()
        };
        let mut sources = [0.0f32; SOURCE_COUNT];
        sources[SRC_ENV1] = vo.env1.level();
        sources[SRC_ENV2] = env2_value;
        sources[SRC_LFO] = vo.lfo_value;
        sources[SRC_KEY] = (vo.key - 60.0) / 60.0;
        sources[SRC_VEL] = vo.velocity;
        sources[SRC_MW] = mw;
        sources[SRC_PRESS] = pressure;
        sources[SRC_SLIDE] = slide;
        vo.pressure = pressure;
        vo.slide = slide;
        self.matrix.evaluate(&sources, &mut vo.bus);
        let bus = &vo.bus;
        // Pitch.
        let unison_detune = match mode {
            1 => vo.spread * MONO_DETUNE_ST,
            3 => vo.spread * strength * UNISON_DETUNE_ST,
            _ => 0.0,
        };
        let drift_st =
            |i: usize| vo.drift_pitch[i] * drift * DRIFT_PITCH_ST + vo.wander * drift * 0.15;
        let mod_st = bus.offset(D_PITCH) * PITCH_MOD_ST;
        // A ratio-mode LFO on pitch runs at audio rate, so its part leaves the control-rate sum.
        let fm_pitch = pitch_fm.iter().filter(|p| p.0).map(|p| p.1).sum::<f32>();
        let cyc_fm_pitch = cyc_fm.iter().filter(|p| p.0).map(|p| p.1).sum::<f32>();
        let control_st = mod_st
            - fm_pitch * sources[SRC_LFO] * PITCH_MOD_ST
            - cyc_fm_pitch * sources[SRC_ENV2] * PITCH_MOD_ST;
        vo.fm_depth = fm_pitch * PITCH_MOD_ST / 12.0;
        vo.fm_cyc_depth = cyc_fm_pitch * PITCH_MOD_ST / 12.0;
        let semis = [
            base_pitch + transpose + wave1 * 12.0 + unison_detune + drift_st(0) + control_st,
            base_pitch
                + transpose
                + oct2 * 12.0
                + (wave_detune + bus.offset(D_DETUNE) * 12.0)
                + unison_detune
                + drift_st(1)
                + control_st,
        ];
        for (i, s) in semis.iter().enumerate() {
            let dt = (note_hz(*s) / sr).clamp(1e-6, 0.49);
            if vo.fresh {
                vo.dt[i] = dt;
            }
            vo.dt_step[i] = (dt - vo.dt[i]) / frames;
        }
        // Shape, gains, filter.
        vo.shape = (shape_base + bus.offset(D_SHAPE)).clamp(0.0, 1.0);
        let thick = if mode == 1 && unison_index > 0 {
            thickness
        } else {
            1.0
        };
        let mod_gain = |d: usize, db: f32| (db_to_gain(db) + bus.offset(d)).clamp(0.0, 4.0) * thick;
        vo.gain_target = [
            mod_gain(D_OSC1_GAIN, gain_db[0]),
            mod_gain(D_OSC2_GAIN, gain_db[1]),
            mod_gain(D_NOISE, gain_db[2]),
        ];
        if vo.fresh {
            vo.gain = vo.gain_target;
        }
        let tracked = (vo.key - 60.0) * lp_key;
        let cutoff = lp_base
            * 2f32.powf(
                (tracked
                    + bus.offset(D_LP) * CUTOFF_MOD_ST
                    + vo.drift_cut * drift * DRIFT_CUTOFF_ST)
                    / 12.0,
            );
        let res = (lp_res + bus.offset(D_RES)).clamp(0.0, 1.0);
        let (circuit, slope) = if lp_type == 0 {
            (Circuit::Dfm, FilterSlope::Db12)
        } else {
            (Circuit::Ms2, FilterSlope::Db24)
        };
        vo.lp.set(&FilterSettings {
            circuit,
            slope,
            cutoff: cutoff.clamp(10.0, sr * 0.48),
            resonance: res,
            ..Default::default()
        });
        vo.hp.set(&FilterSettings {
            kind: FilterKind::HighPass,
            cutoff: (hp_base * 2f32.powf(bus.offset(D_HP) * CUTOFF_MOD_ST / 12.0))
                .clamp(5.0, sr * 0.48),
            ..Default::default()
        });
        // Amp, pan and rates.
        vo.amp_target = (db_to_gain(volume_db + bus.offset(D_VOLUME) * VOLUME_MOD_DB)
            * (1.0 - vel_vol + vel_vol * vo.velocity))
            .min(8.0);
        if vo.fresh {
            vo.amp = vo.amp_target;
        }
        vo.pan = if mode == 2 {
            pan_gains(vo.spread * stereo_spread)
        } else {
            (1.0, 1.0)
        };
        let lfo_gain = (bus.offset(D_LFO_RATE) * RATE_MOD_OCT).exp2();
        let cyc_gain = (bus.offset(D_CYC_RATE) * RATE_MOD_OCT).exp2();
        vo.lfo_dt = (lfo_rate * lfo_gain / sr).clamp(0.0, 0.5);
        vo.cyc_hz = cyc_rate * cyc_gain;
        vo.fresh = false;
    }

    fn control_tick(&mut self) {
        if self.dirty {
            self.refresh();
            self.dirty = false;
        }
        self.alloc.advance(CONTROL_INTERVAL);
        let lfo_dt = self.f(LFO_RATE).max(0.0) * CONTROL_INTERVAL as f32 / self.sr;
        self.free_lfo = (self.free_lfo + lfo_dt).fract();
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
            let state = self.render_state;
            let lfo_amt_base = state.lfo_amount;
            for v in 0..self.voices.len() {
                let vo = &mut self.voices[v];
                if !vo.active {
                    continue;
                }
                let lfo_scale = (lfo_amt_base + vo.bus.offset(D_LFO_AMT)).clamp(0.0, 1.0);
                if render_voice(
                    vo,
                    &state,
                    lfo_scale,
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
    s: &Render,
    lfo_scale: f32,
    left: &mut [f32],
    right: &mut [f32],
) -> bool {
    let sr = s.sample_rate;
    let fm = v.fm_depth != 0.0 || v.fm_cyc_depth != 0.0;
    for (l, r) in left.iter_mut().zip(right.iter_mut()) {
        let [mut dt1, mut dt2] = v.dt;
        v.dt[0] += v.dt_step[0];
        v.dt[1] += v.dt_step[1];
        v.lfo_phase += v.lfo_dt;
        if v.lfo_phase >= 1.0 {
            v.lfo_phase -= 1.0;
            v.lfo_cycles = v.lfo_cycles.saturating_add(1);
            v.lfo_prev = v.lfo_cur;
            v.lfo_cur = v.lfo_rng.bipolar();
        }
        let lfo =
            lfo_shape(s.lfo_wave, v.lfo_phase, v.lfo_prev, v.lfo_cur, v.lfo_cycles) * lfo_scale;
        v.lfo_value = lfo;
        v.cyc_phase = (v.cyc_phase + v.cyc_hz / sr).fract();
        if fm {
            let cyc = if s.cycling {
                cycling_shape(v.cyc_phase, s.tilt, s.hold)
            } else {
                0.0
            };
            let factor = (v.fm_depth * lfo + v.fm_cyc_depth * cyc).exp2();
            dt1 = (dt1 * factor).clamp(1e-6, 0.49);
            dt2 = (dt2 * factor).clamp(1e-6, 0.49);
        }
        let o1 = if s.osc1_on {
            v.phase[0] = (v.phase[0] + dt1).fract();
            osc_sample(s.osc1_wave, v.phase[0], dt1, v.shape)
        } else {
            0.0
        };
        let o2 = if s.osc2_on {
            v.phase[1] = (v.phase[1] + dt2).fract();
            osc2_sample(s.osc2_wave, v.phase[1], dt2)
        } else {
            0.0
        };
        let n = if s.noise_on { v.rng.bipolar() } else { 0.0 };
        let (g1, g2, g3) = (v.gain[0], v.gain[1], v.gain[2]);
        let mut filtered = 0.0;
        let mut direct = 0.0;
        for (src, gain, flag) in [
            (o1, g1, s.filters[0]),
            (o2, g2, s.filters[1]),
            (n, g3, s.filters[2]),
        ] {
            if flag {
                filtered += src * gain;
            } else {
                direct += src * gain;
            }
        }
        let wet = v.hp.process(pre_saturate(filtered));
        let wet = post_saturate(v.lp.process(wet));
        let env = v.env1.next();
        v.env2.next();
        v.gain[0] += (v.gain_target[0] - v.gain[0]) * 0.02;
        v.gain[1] += (v.gain_target[1] - v.gain[1]) * 0.02;
        v.gain[2] += (v.gain_target[2] - v.gain[2]) * 0.02;
        v.amp += (v.amp_target - v.amp) * 0.02;
        let out = sanitize((wet + direct) * env * v.amp * VOICE_GAIN);
        *l += out * v.pan.0;
        *r += out * v.pan.1;
    }
    !v.env1.is_active()
}

impl Processor for Drift {
    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sr = sample_rate as f32;
        self.alloc.prepare(sample_rate);
        for v in &mut self.voices {
            v.env1.prepare(sample_rate);
            v.env2.prepare(sample_rate);
            v.lp.prepare(sample_rate);
            v.hp.prepare(sample_rate);
        }
        self.refresh();
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
    }

    fn tail(&self) -> Tail {
        Tail::Frames((self.f(E1_RELEASE) * 0.001 * self.sr) as u32 + 1)
    }
}
