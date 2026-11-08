//! Roar: up to three gain stages in seven routings, with a modulation matrix, a feedback path
//! and a compressor in it.
//!
//! The manual gives every section's controls: Drive, Tone Amount and Frequency, Color
//! Compensation, the routings, each stage's Amount, Bias, Level, twelve shapers and nine filters,
//! the matrix with LFO 1, LFO 2, Env and Noise, Feedback Amount, Invert, Gate, its band-pass and
//! its five modes, Compression, its sidechain high-pass, Output into a hard clip, Dry/Wet, and
//! the sidechain controls. It gives almost no numbers, so ranges, curves and most behaviour
//! details are ours. In particular: how Mid Side and Delay use Blend, the compressor's threshold,
//! attack and release, and what happens at the extremes of each shaper.
//!
//! The whole wet path runs at twice the sample rate, so feedback and the folding shapers alias
//! less, which is the 33 sample latency. External Source and Tapping Point are the routing
//! batch's job. The audio sidechain comes in through `ctx.sidechain`; MIDI > FB Note reads
//! `ctx.events`, which is where a routed MIDI sidechain has to arrive.

use super::common::{decode_ms, encode_ms, DcBlock, DryLine, CONTROL_FRAMES};
use crate::dsp::delay::{InterpDelay, Interpolation};
use crate::dsp::filter::{Circuit, Filter, FilterKind, FilterSettings, FilterSlope};
use crate::dsp::follower::{DetectMode, EnvelopeFollower};
use crate::dsp::lfo::{Lfo, LfoRate, LfoSettings};
use crate::dsp::modmatrix::{ModBus, ModMatrix, Route, RouteKind};
use crate::dsp::oversample::{OversampleFactor, Oversampler};
use crate::dsp::sidechain::{SidechainDetector, SidechainEq, SidechainSettings};
use crate::dsp::tempo::BeatClock;
use crate::dsp::util::{midi_to_hz, sanitize, Rng};
use crate::dsp::{soft_clip, Biquad, BiquadKind};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    db_to_gain, AudioBuffer, AudioInput, BlockStep, DeviceDesc, Item, MidiMessage, ParamId,
    ParamInfo, Ports, ProcessContext, Processor, Section, Status, Tail, ViewId,
};
use std::f32::consts::{FRAC_PI_2, PI, TAU};

pub const DRIVE: u32 = 0;
pub const TONE_AMOUNT: u32 = 1;
pub const TONE_FREQ: u32 = 2;
pub const COLOR_COMP: u32 = 3;
pub const ROUTING: u32 = 4;
pub const BLEND: u32 = 5;
pub const XOVER_LO: u32 = 6;
pub const XOVER_HI: u32 = 7;

pub const STAGE_BASE: u32 = 8;
pub const STAGE_STEP: u32 = 13;
pub const STAGE_ON: u32 = 0;
pub const STAGE_AMOUNT: u32 = 1;
pub const STAGE_BIAS: u32 = 2;
pub const STAGE_SHAPER: u32 = 3;
pub const STAGE_SHAPER_ON: u32 = 4;
pub const STAGE_LEVEL: u32 = 5;
pub const STAGE_FILTER_ON: u32 = 6;
pub const STAGE_FILTER: u32 = 7;
pub const STAGE_FREQ: u32 = 8;
pub const STAGE_RES: u32 = 9;
pub const STAGE_PRE: u32 = 10;
pub const STAGE_PEAK: u32 = 11;
pub const STAGE_MORPH: u32 = 12;

/// The id of parameter `offset` of gain stage `stage` (0 to 2).
pub const fn stage_param(stage: usize, offset: u32) -> u32 {
    STAGE_BASE + stage as u32 * STAGE_STEP + offset
}

pub const LFO1_BASE: u32 = 47;
pub const LFO2_BASE: u32 = 53;
pub const LFO_WAVE: u32 = 0;
pub const LFO_MODE: u32 = 1;
pub const LFO_RATE: u32 = 2;
pub const LFO_SYNC: u32 = 3;
pub const LFO_MORPH: u32 = 4;
pub const LFO_SMOOTH: u32 = 5;

pub const ENV_ATTACK: u32 = 59;
pub const ENV_RELEASE: u32 = 60;
pub const ENV_HOLD: u32 = 61;
pub const ENV_THRESHOLD: u32 = 62;
pub const ENV_GAIN: u32 = 63;
pub const ENV_FREQ: u32 = 64;
pub const ENV_WIDTH: u32 = 65;
pub const ENV_LISTEN: u32 = 66;

pub const NOISE_TYPE: u32 = 67;
pub const NOISE_MODE: u32 = 68;
pub const NOISE_RATE: u32 = 69;
pub const NOISE_SYNC: u32 = 70;
pub const NOISE_SMOOTH: u32 = 71;

pub const GLOBAL_MOD: u32 = 72;

pub const FB_MODE: u32 = 73;
pub const FB_AMOUNT: u32 = 74;
pub const FB_INVERT: u32 = 75;
pub const FB_GATE: u32 = 76;
pub const FB_FREQ: u32 = 77;
pub const FB_WIDTH: u32 = 78;
pub const FB_TIME: u32 = 79;
pub const FB_SYNC: u32 = 80;
pub const FB_NOTE: u32 = 81;

pub const COMP_AMOUNT: u32 = 82;
pub const COMP_HP: u32 = 83;
pub const OUTPUT: u32 = 84;
pub const DRY_WET: u32 = 85;

pub const EXT_SC: u32 = 86;
pub const SC_MIX: u32 = 87;
pub const SC_GAIN: u32 = 88;
pub const MIDI_FB: u32 = 89;
pub const SC_LISTEN: u32 = 90;

pub const MATRIX_BASE: u32 = 91;

pub const ROUTINGS: [&str; 7] = [
    "Single",
    "Serial",
    "Parallel",
    "Multi Band",
    "Mid Side",
    "Feedback",
    "Delay",
];

pub const SHAPERS: [&str; 12] = [
    "Soft Sine",
    "Digital Clip",
    "Bit Crusher",
    "Diode Clipper",
    "Tube Preamp",
    "Half Wave Rectifier",
    "Full Wave Rectifier",
    "Polynomial",
    "Fractal",
    "Tri Fold",
    "Noise Injection",
    "Shards",
];

pub const FILTERS: [&str; 9] = [
    "LP",
    "BP",
    "HP",
    "Notch",
    "Peak",
    "Morph",
    "Comb",
    "Resampling",
    "Dispersion",
];

pub const LFO_WAVES: [&str; 5] = ["Sine", "Triangle", "Square", "Up", "Down"];
pub const RATE_MODES: [&str; 5] = ["Free", "Synced", "Triplet", "Dotted", "Sixteenth"];
pub const NOISE_TYPES: [&str; 4] = ["Simplex", "Wander", "S & H", "Brown"];
pub const FB_MODES: [&str; 5] = ["Time", "Synced", "Triplet", "Dotted", "Note"];
pub const DIVISIONS: [&str; 10] = [
    "1/64", "1/32", "1/16", "1/8", "1/4", "1/2", "1", "2", "4", "8",
];
const DIVISION_BEATS: [f64; 10] = [0.0625, 0.125, 0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0];

/// The beats of a rate mode's division, or `None` for the free-running rate.
fn mode_beats(mode: usize, division: usize) -> Option<f64> {
    let base = DIVISION_BEATS[division.min(DIVISION_BEATS.len() - 1)];
    match mode {
        1 => Some(base),
        2 => Some(base * 2.0 / 3.0),
        3 => Some(base * 1.5),
        4 => Some((division + 1) as f64 * 0.25),
        _ => None,
    }
}

pub const SOURCES: [&str; 4] = ["LFO 1", "LFO 2", "Env", "Noise"];
pub const SOURCE_COUNT: usize = SOURCES.len();

pub const DEST_DRIVE: usize = 0;
pub const DEST_TONE_AMOUNT: usize = 1;
pub const DEST_TONE_FREQ: usize = 2;
pub const DEST_BLEND: usize = 3;
pub const DEST_FB_AMOUNT: usize = 4;
pub const DEST_FB_FREQ: usize = 5;
pub const DEST_FB_WIDTH: usize = 6;
pub const DEST_FB_TIME: usize = 7;
pub const DEST_COMP: usize = 8;
pub const DEST_OUTPUT: usize = 9;
pub const DEST_DRY_WET: usize = 10;
pub const DEST_STAGE_BASE: usize = 11;
pub const DEST_STAGE_STEP: usize = 6;
pub const DEST_AMOUNT: usize = 0;
pub const DEST_BIAS: usize = 1;
pub const DEST_LEVEL: usize = 2;
pub const DEST_FREQ: usize = 3;
pub const DEST_RES: usize = 4;
pub const DEST_MORPH: usize = 5;

pub const DESTS: [&str; 29] = [
    "Drive",
    "Tone Amount",
    "Tone Freq",
    "Blend",
    "Feedback Amount",
    "Feedback Freq",
    "Feedback Width",
    "Feedback Time",
    "Compression",
    "Output",
    "Dry/Wet",
    "Stage 1 Amount",
    "Stage 1 Bias",
    "Stage 1 Level",
    "Stage 1 Freq",
    "Stage 1 Reso",
    "Stage 1 Morph",
    "Stage 2 Amount",
    "Stage 2 Bias",
    "Stage 2 Level",
    "Stage 2 Freq",
    "Stage 2 Reso",
    "Stage 2 Morph",
    "Stage 3 Amount",
    "Stage 3 Bias",
    "Stage 3 Level",
    "Stage 3 Freq",
    "Stage 3 Reso",
    "Stage 3 Morph",
];
pub const DEST_COUNT: usize = DESTS.len();

/// The parameter of a matrix cell: `source` modulating `dest`.
pub const fn cell_param(source: usize, dest: usize) -> u32 {
    MATRIX_BASE + (source * DEST_COUNT + dest) as u32
}

const CELLS: usize = SOURCE_COUNT * DEST_COUNT;
const PARAMS: usize = MATRIX_BASE as usize + CELLS;

/// The published mod source levels, for the matrix LEDs.
pub const MOD_LEVELS: ViewId = ViewId(0);
pub const VIEW_LEN: usize = SOURCE_COUNT;

/// Ours; the manual doesn't say.
pub const TONE_RANGE_DB: f32 = 12.0;
const COMP_THRESHOLD: f32 = 0.25;
/// Oversampling of the wet path, as a multiple.
const OVERSAMPLE: OversampleFactor = OversampleFactor::X2;
/// The longest feedback delay, in seconds. Longer sync divisions are capped to it.
const MAX_DELAY_SECONDS: f32 = 4.0;

fn params() -> Vec<ParamInfo> {
    let mut p = vec![
        ParamInfo::continuous(DRIVE, "Drive", -24.0, 36.0, 0.0, "dB"),
        ParamInfo::continuous(TONE_AMOUNT, "Tone Amount", -1.0, 1.0, 0.0, ""),
        ParamInfo::skewed(TONE_FREQ, "Tone Freq", 20.0, 20_000.0, 500.0, "Hz", 2.5),
        ParamInfo::toggle(COLOR_COMP, "Color Compensation", false),
        ParamInfo::stepped(ROUTING, "Routing", &ROUTINGS, 0),
        ParamInfo::continuous(BLEND, "Blend", 0.0, 1.0, 0.5, ""),
        ParamInfo::skewed(XOVER_LO, "Crossover Low", 30.0, 5_000.0, 250.0, "Hz", 2.5),
        ParamInfo::skewed(
            XOVER_HI,
            "Crossover High",
            200.0,
            18_000.0,
            2_500.0,
            "Hz",
            2.5,
        ),
    ];
    for s in 0..3 {
        let name = |what: &str| format!("Stage {} {what}", s + 1);
        p.extend([
            ParamInfo::toggle(stage_param(s, STAGE_ON), &name("On"), s == 0),
            ParamInfo::continuous(
                stage_param(s, STAGE_AMOUNT),
                &name("Amount"),
                0.0,
                1.0,
                0.0,
                "",
            ),
            ParamInfo::continuous(
                stage_param(s, STAGE_BIAS),
                &name("Bias"),
                -1.0,
                1.0,
                0.0,
                "",
            ),
            ParamInfo::stepped(stage_param(s, STAGE_SHAPER), &name("Shaper"), &SHAPERS, 0),
            ParamInfo::toggle(stage_param(s, STAGE_SHAPER_ON), &name("Shaper On"), true),
            ParamInfo::continuous(
                stage_param(s, STAGE_LEVEL),
                &name("Level"),
                -24.0,
                24.0,
                0.0,
                "dB",
            ),
            ParamInfo::toggle(stage_param(s, STAGE_FILTER_ON), &name("Filter On"), false),
            ParamInfo::stepped(stage_param(s, STAGE_FILTER), &name("Filter"), &FILTERS, 0),
            ParamInfo::skewed(
                stage_param(s, STAGE_FREQ),
                &name("Freq"),
                20.0,
                20_000.0,
                2_000.0,
                "Hz",
                2.5,
            ),
            ParamInfo::continuous(
                stage_param(s, STAGE_RES),
                &name("Resonance"),
                0.0,
                1.0,
                0.2,
                "",
            ),
            ParamInfo::toggle(stage_param(s, STAGE_PRE), &name("Filter Pre"), false),
            ParamInfo::continuous(
                stage_param(s, STAGE_PEAK),
                &name("Peak Gain"),
                -24.0,
                24.0,
                0.0,
                "dB",
            ),
            ParamInfo::continuous(
                stage_param(s, STAGE_MORPH),
                &name("Morph"),
                0.0,
                1.0,
                0.0,
                "",
            ),
        ]);
    }
    for (base, name) in [(LFO1_BASE, "LFO 1"), (LFO2_BASE, "LFO 2")] {
        let n = |what: &str| format!("{name} {what}");
        p.extend([
            ParamInfo::stepped(base + LFO_WAVE, &n("Wave"), &LFO_WAVES, 0),
            ParamInfo::stepped(base + LFO_MODE, &n("Mode"), &RATE_MODES, 0),
            ParamInfo::skewed(base + LFO_RATE, &n("Rate"), 0.01, 50.0, 1.0, "Hz", 3.0),
            ParamInfo::stepped(base + LFO_SYNC, &n("Sync"), &DIVISIONS, 4),
            ParamInfo::continuous(base + LFO_MORPH, &n("Morph"), 0.0, 1.0, 0.5, ""),
            ParamInfo::continuous(base + LFO_SMOOTH, &n("Smooth"), 0.0, 1.0, 0.0, ""),
        ]);
    }
    p.extend([
        ParamInfo::skewed(ENV_ATTACK, "Env Attack", 0.1, 200.0, 5.0, "ms", 2.5),
        ParamInfo::skewed(ENV_RELEASE, "Env Release", 1.0, 2_000.0, 150.0, "ms", 2.5),
        ParamInfo::toggle(ENV_HOLD, "Env Hold", false),
        ParamInfo::continuous(ENV_THRESHOLD, "Env Threshold", -60.0, 0.0, -60.0, "dB"),
        ParamInfo::continuous(ENV_GAIN, "Env Gain", -24.0, 24.0, 0.0, "dB"),
        ParamInfo::skewed(ENV_FREQ, "Env Freq", 30.0, 15_000.0, 1_000.0, "Hz", 2.5),
        ParamInfo::continuous(ENV_WIDTH, "Env Width", 0.0, 1.0, 1.0, ""),
        ParamInfo::toggle(ENV_LISTEN, "Env Listen", false),
        ParamInfo::stepped(NOISE_TYPE, "Noise Type", &NOISE_TYPES, 0),
        ParamInfo::stepped(NOISE_MODE, "Noise Mode", &RATE_MODES, 0),
        ParamInfo::skewed(NOISE_RATE, "Noise Rate", 0.01, 50.0, 2.0, "Hz", 3.0),
        ParamInfo::stepped(NOISE_SYNC, "Noise Sync", &DIVISIONS, 4),
        ParamInfo::continuous(NOISE_SMOOTH, "Noise Smooth", 0.0, 1.0, 0.0, ""),
        ParamInfo::continuous(GLOBAL_MOD, "Global Mod Amount", 0.0, 1.0, 1.0, ""),
        ParamInfo::stepped(FB_MODE, "Feedback Mode", &FB_MODES, 0),
        ParamInfo::continuous(FB_AMOUNT, "Feedback Amount", 0.0, 1.0, 0.0, ""),
        ParamInfo::toggle(FB_INVERT, "Feedback Invert", false),
        ParamInfo::toggle(FB_GATE, "Feedback Gate", false),
        ParamInfo::skewed(FB_FREQ, "Feedback Freq", 30.0, 18_000.0, 1_500.0, "Hz", 2.5),
        ParamInfo::continuous(FB_WIDTH, "Feedback Width", 0.1, 8.0, 8.0, "oct"),
        ParamInfo::skewed(FB_TIME, "Feedback Time", 1.0, 1_000.0, 100.0, "ms", 2.5),
        ParamInfo::stepped(FB_SYNC, "Feedback Sync", &DIVISIONS, 3),
        ParamInfo::continuous(FB_NOTE, "Feedback Note", 24.0, 96.0, 60.0, "st"),
        ParamInfo::continuous(COMP_AMOUNT, "Compression", 0.0, 1.0, 0.0, ""),
        ParamInfo::toggle(COMP_HP, "Comp SC HP", false),
        ParamInfo::continuous(OUTPUT, "Output", -36.0, 36.0, 0.0, "dB"),
        ParamInfo::continuous(DRY_WET, "Dry/Wet", 0.0, 1.0, 1.0, ""),
        ParamInfo::toggle(EXT_SC, "External SC", false),
        ParamInfo::continuous(SC_MIX, "SC Mix", 0.0, 1.0, 1.0, ""),
        ParamInfo::continuous(SC_GAIN, "SC Gain", -24.0, 24.0, 0.0, "dB"),
        ParamInfo::toggle(MIDI_FB, "MIDI > FB Note", false),
        ParamInfo::toggle(SC_LISTEN, "Sidechain Listen", false),
    ]);
    for (s, source) in SOURCES.iter().enumerate() {
        for (d, dest) in DESTS.iter().enumerate() {
            p.push(ParamInfo::continuous(
                cell_param(s, d),
                &format!("{source} > {dest}"),
                -1.0,
                1.0,
                0.0,
                "",
            ));
        }
    }
    p
}

fn layout() -> Vec<Section> {
    let knob = |id| Item::Knob(ParamId(id));
    let toggle = |id| Item::Toggle(ParamId(id));
    let menu = |id| Item::Menu(ParamId(id));
    let mut sections = vec![Section {
        title: "Input",
        items: vec![
            knob(DRIVE),
            knob(TONE_AMOUNT),
            knob(TONE_FREQ),
            toggle(COLOR_COMP),
            menu(ROUTING),
            knob(BLEND),
            knob(XOVER_LO),
            knob(XOVER_HI),
        ],
    }];
    for (s, title) in ["Stage 1", "Stage 2", "Stage 3"].into_iter().enumerate() {
        let id = |offset| stage_param(s, offset);
        sections.push(Section {
            title,
            items: vec![
                toggle(id(STAGE_ON)),
                knob(id(STAGE_AMOUNT)),
                knob(id(STAGE_BIAS)),
                menu(id(STAGE_SHAPER)),
                toggle(id(STAGE_SHAPER_ON)),
                knob(id(STAGE_LEVEL)),
                toggle(id(STAGE_FILTER_ON)),
                menu(id(STAGE_FILTER)),
                knob(id(STAGE_FREQ)),
                knob(id(STAGE_RES)),
                toggle(id(STAGE_PRE)),
                knob(id(STAGE_PEAK)),
                knob(id(STAGE_MORPH)),
            ],
        });
    }
    for (base, title) in [(LFO1_BASE, "LFO 1"), (LFO2_BASE, "LFO 2")] {
        sections.push(Section {
            title,
            items: vec![
                menu(base + LFO_WAVE),
                menu(base + LFO_MODE),
                knob(base + LFO_RATE),
                menu(base + LFO_SYNC),
                knob(base + LFO_MORPH),
                knob(base + LFO_SMOOTH),
            ],
        });
    }
    sections.extend([
        Section {
            title: "Env",
            items: vec![
                knob(ENV_ATTACK),
                knob(ENV_RELEASE),
                toggle(ENV_HOLD),
                knob(ENV_THRESHOLD),
                knob(ENV_GAIN),
                knob(ENV_FREQ),
                knob(ENV_WIDTH),
                toggle(ENV_LISTEN),
            ],
        },
        Section {
            title: "Noise",
            items: vec![
                menu(NOISE_TYPE),
                menu(NOISE_MODE),
                knob(NOISE_RATE),
                menu(NOISE_SYNC),
                knob(NOISE_SMOOTH),
            ],
        },
        Section {
            title: "Matrix",
            items: vec![Item::Panel("roar_matrix"), knob(GLOBAL_MOD)],
        },
        Section {
            title: "Feedback",
            items: vec![
                menu(FB_MODE),
                knob(FB_AMOUNT),
                toggle(FB_INVERT),
                toggle(FB_GATE),
                knob(FB_FREQ),
                knob(FB_WIDTH),
                knob(FB_TIME),
                menu(FB_SYNC),
                knob(FB_NOTE),
            ],
        },
        Section {
            title: "Global",
            items: vec![
                knob(COMP_AMOUNT),
                toggle(COMP_HP),
                knob(OUTPUT),
                knob(DRY_WET),
            ],
        },
        Section {
            title: "Sidechain",
            items: vec![
                toggle(EXT_SC),
                knob(SC_MIX),
                knob(SC_GAIN),
                toggle(MIDI_FB),
                toggle(SC_LISTEN),
            ],
        },
    ]);
    sections
}

// ------------------------------------------------------------------------------ shapers

/// The curve of shaper `kind` at `v`, for the stateless ones. `amount` is the stage's 0..1.
/// Diode Clipper, Noise Injection and Shards also need [`Stage`] state and go through it.
#[inline]
pub fn curve(kind: usize, v: f32, amount: f32) -> f32 {
    match kind {
        0 => (v.clamp(-1.0, 1.0) * FRAC_PI_2).sin(),
        1 => v.clamp(-1.0, 1.0),
        2 => crush(v, amount),
        4 => tube_preamp(v),
        5 => v.clamp(0.0, 1.5),
        6 => v.abs().min(1.5),
        7 => polynomial(v, amount),
        8 => fractal(v),
        9 => tri_fold(v),
        _ => v.tanh(),
    }
}

#[inline]
fn crush(v: f32, amount: f32) -> f32 {
    let levels = 2f32.powf(1.0 + 13.0 * (1.0 - amount));
    let mu = 1.0 + amount * 60.0;
    let x = v.clamp(-1.0, 1.0);
    let c = x.signum() * (1.0 + mu * x.abs()).ln() / (1.0 + mu).ln();
    let q = (c * levels).round() / levels;
    q.signum() * ((1.0 + mu).powf(q.abs()) - 1.0) / mu
}

#[inline]
fn tube_preamp(v: f32) -> f32 {
    const O: f32 = 0.2;
    ((v + O).tanh() - O.tanh()) * 1.1
}

#[inline]
fn polynomial(v: f32, amount: f32) -> f32 {
    let u = v.clamp(-1.0, 1.0);
    let sine = (u * FRAC_PI_2).sin();
    let t3 = 4.0 * u * u * u - 3.0 * u;
    sine + (t3 - sine) * amount
}

#[inline]
fn fractal(v: f32) -> f32 {
    let u = v.clamp(-2.0, 2.0);
    let mut sum = 0.0;
    let mut scale = 1.0;
    let mut gain = 0.5;
    for _ in 0..4 {
        sum += gain * (u * scale * PI).sin();
        scale *= 2.0;
        gain *= 0.6;
    }
    sum
}

#[inline]
fn tri_fold(v: f32) -> f32 {
    let p = (v * 0.5 + 0.5).rem_euclid(2.0);
    let t = if p < 1.0 { p } else { 2.0 - p };
    t * 2.0 - 1.0
}

/// Shards' segment count across the curve: where the slopes break.
const SHARD_SEGMENTS: f32 = 4.0;

/// The curve's piecewise slopes at a phase: each segment's slope wanders, rhythmically.
#[inline]
pub fn shards(v: f32, amount: f32, phase: f32) -> f32 {
    let u = v.clamp(-1.5, 1.5);
    let seg = (u * SHARD_SEGMENTS).floor();
    let slope = 1.0 + amount * 1.5 * (TAU * phase + seg * 2.1).sin();
    u * slope
}

/// One stage's controls, resolved for a control block.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct StageCtl {
    on: bool,
    amount: f32,
    bias: f32,
    gain: f32,
    kind: usize,
    shaper_on: bool,
    level: f32,
    filter_on: bool,
    filter: usize,
    pre: bool,
    freq: f32,
    res: f32,
    peak_db: f32,
    morph: f32,
}

const DISPERSION_SECTIONS: usize = 12;

/// A gain stage's sound for one channel.
struct Stage {
    /// Selects what `tick` uses for the filter.
    filter: Filter,
    peak: Biquad,
    comb: InterpDelay,
    comb_read: f32,
    comb_fb: f32,
    resample_phase: f32,
    resample_hold: f32,
    resample_step: f32,
    ap_coeff: [f32; DISPERSION_SECTIONS],
    ap_x1: [f32; DISPERSION_SECTIONS],
    ap_y1: [f32; DISPERSION_SECTIONS],
    dc: DcBlock,
    diode_lp: f32,
    noise: Rng,
    shard_phase: f32,
    shard_step: f32,
    tuned: (usize, f32, f32, f32, f32),
}

impl Stage {
    fn new(seed: u32, max_comb: usize) -> Self {
        Self {
            filter: Filter::new(),
            peak: Biquad::default(),
            comb: InterpDelay::new(max_comb),
            comb_read: 1.0,
            comb_fb: 0.0,
            resample_phase: 1.0,
            resample_hold: 0.0,
            resample_step: 1.0,
            ap_coeff: [0.0; DISPERSION_SECTIONS],
            ap_x1: [0.0; DISPERSION_SECTIONS],
            ap_y1: [0.0; DISPERSION_SECTIONS],
            dc: DcBlock::default(),
            diode_lp: 0.0,
            noise: Rng::new(seed),
            shard_phase: seed as f32 * 0.37,
            shard_step: 0.0,
            tuned: (usize::MAX, 0.0, 0.0, 0.0, 0.0),
        }
    }

    fn prepare(&mut self, rate: f64) {
        self.filter.prepare(rate);
        self.dc.set(10.0, rate as f32);
        self.shard_step = 5.0 / rate as f32;
        self.tuned = (usize::MAX, 0.0, 0.0, 0.0, 0.0);
        self.reset();
    }

    fn reset(&mut self) {
        self.filter.reset();
        self.peak.reset();
        self.comb.clear();
        self.resample_phase = 1.0;
        self.resample_hold = 0.0;
        self.ap_x1 = [0.0; DISPERSION_SECTIONS];
        self.ap_y1 = [0.0; DISPERSION_SECTIONS];
        self.dc.reset();
        self.diode_lp = 0.0;
    }

    fn tune(&mut self, c: &StageCtl, rate: f32) {
        let tuned = (c.filter, c.freq, c.res, c.peak_db, c.morph);
        if tuned == self.tuned {
            return;
        }
        self.tuned = tuned;
        let freq = c.freq.clamp(10.0, rate * 0.45);
        match c.filter {
            4 => self
                .peak
                .set(BiquadKind::Peak, freq, 0.5 + c.res * 9.0, c.peak_db, rate),
            6 => {
                self.comb_read = (rate / freq - 1.0).max(1.0);
                self.comb_fb = c.res * 0.9;
            }
            7 => self.resample_step = freq / rate,
            8 => {
                let spread = 1.0 + c.res * 4.0;
                let n = DISPERSION_SECTIONS as f32;
                for (i, coeff) in self.ap_coeff.iter_mut().enumerate() {
                    let at = (i as f32 + 0.5) / n - 0.5;
                    let f = (freq * 2f32.powf(at * spread)).clamp(10.0, rate * 0.45);
                    let t = (PI * f / rate).tan();
                    *coeff = (t - 1.0) / (t + 1.0);
                }
            }
            _ => {
                let kind = match c.filter {
                    1 => FilterKind::BandPass,
                    2 => FilterKind::HighPass,
                    3 => FilterKind::Notch,
                    5 => FilterKind::Morph,
                    _ => FilterKind::LowPass,
                };
                self.filter.set(&FilterSettings {
                    circuit: Circuit::Svf,
                    kind,
                    slope: FilterSlope::Db12,
                    cutoff: freq,
                    resonance: c.res * 0.95,
                    drive: 0.0,
                    // LP at 0, BP in the middle, HP at 1.
                    morph: c.morph * 0.5,
                });
            }
        }
    }

    #[inline]
    fn filter_tick(&mut self, c: &StageCtl, x: f32) -> f32 {
        match c.filter {
            4 => self.peak.process(x),
            6 => {
                let d = self.comb.read(self.comb_read, Interpolation::Cubic);
                self.comb.write(x + d * self.comb_fb);
                0.5 * (x + d)
            }
            7 => {
                self.resample_phase += self.resample_step;
                if self.resample_phase >= 1.0 {
                    self.resample_phase = (self.resample_phase - 1.0).fract();
                    self.resample_hold = x;
                }
                self.resample_hold
            }
            8 => {
                let mut y = x;
                for i in 0..DISPERSION_SECTIONS {
                    let c = self.ap_coeff[i];
                    let out = sanitize(c * y + self.ap_x1[i] - c * self.ap_y1[i]);
                    self.ap_x1[i] = y;
                    self.ap_y1[i] = out;
                    y = out;
                }
                y
            }
            _ => self.filter.process(x),
        }
    }

    #[inline]
    fn shape(&mut self, c: &StageCtl, x: f32) -> f32 {
        if c.amount <= 0.0 {
            return x;
        }
        let v = x * c.gain + c.bias * 2.0;
        let shaped = match c.kind {
            3 => {
                self.diode_lp += (v.tanh() - self.diode_lp) * 0.35;
                self.diode_lp
            }
            10 => (v + self.noise.bipolar() * c.amount * 0.5).tanh(),
            11 => {
                self.shard_phase = (self.shard_phase + self.shard_step).fract();
                shards(v, c.amount, self.shard_phase)
            }
            kind => curve(kind, v, c.amount),
        };
        // Rises from a straight line at 0% so a low Amount can't jump.
        let blend = (c.amount * 8.0).min(1.0);
        let y = x + (shaped - x) * blend;
        self.dc.process(y) * c.level
    }

    #[inline]
    fn tick(&mut self, c: &StageCtl, x: f32) -> f32 {
        if !c.on {
            return x;
        }
        let mut v = x;
        if c.filter_on && c.pre {
            v = self.filter_tick(c, v);
        }
        if c.shaper_on {
            v = self.shape(c, v);
        }
        if c.filter_on && !c.pre {
            v = self.filter_tick(c, v);
        }
        v
    }
}

/// The controls of a whole channel, resolved for a control block.
#[derive(Clone, Copy, Debug, Default)]
struct Ctl {
    routing: usize,
    blend: f32,
    tone_db: f32,
    tone_freq: f32,
    color: bool,
    xover_lo: f32,
    xover_hi: f32,
    stages: [StageCtl; 3],
    fb_amount: f32,
    fb_invert: bool,
    fb_freq: f32,
    fb_width: f32,
    /// Feedback delay in samples at the oversampled rate.
    fb_delay: f32,
    comp: f32,
    comp_hp: bool,
}

impl Ctl {
    /// Whether the wet path is doing nothing but delaying: Single routing with its stage
    /// passing the signal straight through, and no tone, feedback or compression.
    fn is_transparent(&self) -> bool {
        let s = &self.stages[0];
        self.routing == 0
            && (!s.on || (!s.filter_on && (!s.shaper_on || s.amount <= 0.0)))
            && self.tone_db == 0.0
            && self.fb_amount <= 0.0
            && self.comp <= 0.0
    }
}

/// One channel of the oversampled wet path.
struct Channel {
    stages: [Stage; 3],
    tone_pre: Biquad,
    tone_post: Biquad,
    tuned_tone: (f32, f32),
    xover: [Biquad; 8],
    tuned_xover: (f32, f32),
    fb_line: InterpDelay,
    fb_hp: Biquad,
    fb_lp: Biquad,
    tuned_fb: (f32, f32),
    delay_now: f32,
    delay_k: f32,
    /// The next tune jumps straight to the delay time, so a fresh instance doesn't glide in.
    snap_delay: bool,
    comp_follow: EnvelopeFollower,
    comp_hp_filter: Biquad,
    rate: f32,
}

impl Channel {
    fn new(seed: u32, max_comb: usize, delay_capacity: usize) -> Self {
        Self {
            stages: [
                Stage::new(seed + 1, max_comb),
                Stage::new(seed + 2, max_comb),
                Stage::new(seed + 3, max_comb),
            ],
            tone_pre: Biquad::default(),
            tone_post: Biquad::default(),
            tuned_tone: (f32::NAN, 0.0),
            xover: [Biquad::default(); 8],
            tuned_xover: (f32::NAN, 0.0),
            fb_line: InterpDelay::new(delay_capacity),
            fb_hp: Biquad::default(),
            fb_lp: Biquad::default(),
            tuned_fb: (f32::NAN, 0.0),
            delay_now: 1.0,
            delay_k: 0.01,
            snap_delay: true,
            comp_follow: EnvelopeFollower::new(),
            comp_hp_filter: Biquad::default(),
            rate: 96_000.0,
        }
    }

    fn prepare(&mut self, rate: f64) {
        self.rate = rate as f32;
        for s in &mut self.stages {
            s.prepare(rate);
        }
        self.comp_follow.prepare(rate);
        self.comp_follow.set_mode(DetectMode::Peak);
        self.comp_follow.set_times(0.001, 0.1);
        self.comp_hp_filter
            .set(BiquadKind::HighPass, 200.0, 0.707, 0.0, self.rate);
        self.delay_k = 1.0 - (-1.0 / (0.02 * self.rate)).exp();
        self.reset();
    }

    fn reset(&mut self) {
        for s in &mut self.stages {
            s.reset();
        }
        for b in [
            &mut self.tone_pre,
            &mut self.tone_post,
            &mut self.fb_hp,
            &mut self.fb_lp,
            &mut self.comp_hp_filter,
        ] {
            b.reset();
        }
        self.xover.iter_mut().for_each(Biquad::reset);
        self.fb_line.clear();
        self.comp_follow.reset();
        self.snap_delay = true;
    }

    fn tune(&mut self, c: &Ctl) {
        let rate = self.rate;
        if self.snap_delay {
            self.snap_delay = false;
            self.delay_now = c.fb_delay;
        }
        if (c.tone_db, c.tone_freq) != self.tuned_tone {
            self.tuned_tone = (c.tone_db, c.tone_freq);
            self.tone_pre
                .set(BiquadKind::LowShelf, c.tone_freq, 0.707, -c.tone_db, rate);
            self.tone_post
                .set(BiquadKind::LowShelf, c.tone_freq, 0.707, c.tone_db, rate);
        }
        if (c.xover_lo, c.xover_hi) != self.tuned_xover {
            self.tuned_xover = (c.xover_lo, c.xover_hi);
            let (lo, hi) = (c.xover_lo, c.xover_hi);
            for (b, kind, f) in [
                (0, BiquadKind::LowPass, lo),
                (1, BiquadKind::LowPass, lo),
                (2, BiquadKind::HighPass, lo),
                (3, BiquadKind::HighPass, lo),
                (4, BiquadKind::LowPass, hi),
                (5, BiquadKind::LowPass, hi),
                (6, BiquadKind::HighPass, hi),
                (7, BiquadKind::HighPass, hi),
            ] {
                self.xover[b].set(kind, f, 0.707, 0.0, rate);
            }
        }
        if (c.fb_freq, c.fb_width) != self.tuned_fb {
            self.tuned_fb = (c.fb_freq, c.fb_width);
            let half = 2f32.powf(c.fb_width * 0.5);
            let low = (c.fb_freq / half).clamp(10.0, rate * 0.45);
            let high = (c.fb_freq * half).clamp(10.0, rate * 0.45);
            self.fb_hp.set(BiquadKind::HighPass, low, 0.707, 0.0, rate);
            self.fb_lp.set(BiquadKind::LowPass, high, 0.707, 0.0, rate);
        }
        for (stage, ctl) in self.stages.iter_mut().zip(&c.stages) {
            if ctl.filter_on {
                stage.tune(ctl, rate);
            }
        }
    }

    /// The filtered, optionally inverted delayed signal, before any feedback gain.
    #[inline]
    fn delayed(&mut self, c: &Ctl) -> f32 {
        self.delay_now += (c.fb_delay - self.delay_now) * self.delay_k;
        let raw = self
            .fb_line
            .read(self.delay_now.max(0.0), Interpolation::Cubic);
        let band = self.fb_lp.process(self.fb_hp.process(raw));
        if c.fb_invert {
            -band
        } else {
            band
        }
    }

    #[inline]
    fn compress(&mut self, y: f32, c: &Ctl) -> f32 {
        if c.comp <= 0.0 {
            return 1.0;
        }
        let key = if c.comp_hp {
            self.comp_hp_filter.process(y)
        } else {
            y
        };
        let level = self.comp_follow.process(key);
        if level <= COMP_THRESHOLD {
            return 1.0;
        }
        let ratio = 1.0 + 19.0 * c.comp;
        (COMP_THRESHOLD / level).powf(1.0 - 1.0 / ratio)
    }

    /// One sample of the wet path, at the oversampled rate.
    #[inline]
    fn tick(&mut self, x: f32, c: &Ctl, ch: usize) -> f32 {
        let toned = self.tone_pre.process(x) * db_to_gain(c.tone_db * 0.5);
        let delayed = self.delayed(c);
        let fb = delayed * c.fb_amount;
        let [s0, s1, s2] = &mut self.stages;
        let (y, write_extra) = match c.routing {
            1 => {
                let a = s0.tick(&c.stages[0], toned + fb);
                let b = s1.tick(&c.stages[1], a);
                (a + (b - a) * c.blend, None)
            }
            2 => {
                let a = s0.tick(&c.stages[0], toned + fb);
                let b = s1.tick(&c.stages[1], toned + fb);
                (a + (b - a) * c.blend, None)
            }
            3 => {
                let [lo1, lo2, hi1, hi2, mid1, mid2, top1, top2] = &mut self.xover;
                let input = toned + fb;
                let low = lo2.process(lo1.process(input));
                let rest = hi2.process(hi1.process(input));
                let mid = mid2.process(mid1.process(rest));
                let high = top2.process(top1.process(rest));
                (
                    s0.tick(&c.stages[0], low)
                        + s1.tick(&c.stages[1], mid)
                        + s2.tick(&c.stages[2], high),
                    None,
                )
            }
            4 => {
                let (stage, gain) = if ch == 0 {
                    (s0, c.blend.mul_add(-2.0, 2.0).min(1.0))
                } else {
                    (s1, (c.blend * 2.0).min(1.0))
                };
                let ctl = if ch == 0 { &c.stages[0] } else { &c.stages[1] };
                (stage.tick(ctl, toned + fb) * gain, None)
            }
            5 => {
                // The input and its feedback go through separate stages.
                let a = s0.tick(&c.stages[0], toned);
                let b = s1.tick(&c.stages[1], fb);
                (a + (b - a) * c.blend, Some(a + b))
            }
            6 => {
                // The delayed signal reaches stage 2 without the feedback gain. Stage 2's own
                // output is what gets scaled and fed back.
                let a = s0.tick(&c.stages[0], toned);
                let b = s1.tick(&c.stages[1], delayed);
                (a + (b - a) * c.blend, Some(a + b * c.fb_amount))
            }
            _ => (s0.tick(&c.stages[0], toned + fb), None),
        };
        let mut y = y;
        if c.color {
            y = self.tone_post.process(y) * db_to_gain(-c.tone_db * 0.5);
        }
        let g = self.compress(y, c);
        let out = y * g;
        let write = write_extra.map_or(out, |w| w * g);
        self.fb_line.write(bound(write));
        out
    }
}

/// Keeps what goes round the feedback loop finite and within ±2.
#[inline]
fn bound(x: f32) -> f32 {
    let a = x.abs();
    if a <= 1.0 {
        sanitize(x)
    } else {
        sanitize(x.signum() * (1.0 + soft_clip(a - 1.0)))
    }
}

// ------------------------------------------------------------------------------ mod sources

#[derive(Clone, Copy, Debug)]
struct Smooth {
    value: f32,
}

impl Smooth {
    /// Moves towards `target`, with 0 as no smoothing and 1 as about half a second.
    #[inline]
    fn step(&mut self, target: f32, amount: f32, frames: usize, sr: f32) -> f32 {
        if amount <= 0.0 {
            self.value = target;
        } else {
            let k = 1.0 - (-(frames as f32) / (amount * 0.5 * sr)).exp();
            self.value += (target - self.value) * k;
        }
        self.value
    }
}

struct LfoSource {
    lfo: Lfo,
    smooth: Smooth,
    value: f32,
}

impl LfoSource {
    fn new(seed: u32) -> Self {
        Self {
            lfo: Lfo::new(seed),
            smooth: Smooth { value: 0.0 },
            value: 0.0,
        }
    }
}

#[inline]
fn warp(p: f32, morph: f32) -> f32 {
    let c = morph.clamp(0.02, 0.98);
    if p < c {
        p / c * 0.5
    } else {
        0.5 + (p - c) / (1.0 - c) * 0.5
    }
}

#[inline]
fn wave(kind: usize, p: f32) -> f32 {
    match kind {
        1 => {
            if p < 0.25 {
                4.0 * p
            } else if p < 0.75 {
                2.0 - 4.0 * p
            } else {
                4.0 * p - 4.0
            }
        }
        2 => {
            if p < 0.5 {
                1.0
            } else {
                -1.0
            }
        }
        3 => 2.0 * p - 1.0,
        4 => 1.0 - 2.0 * p,
        _ => (p * TAU).sin(),
    }
}

/// The noise modulator: stepped values with four ways of moving between them.
struct NoiseSource {
    rng: Rng,
    /// Where the current segment is, 0..1.
    phase: f32,
    from: f32,
    to: f32,
    length: f32,
    brown: f32,
    smooth: Smooth,
    value: f32,
}

impl NoiseSource {
    fn new(seed: u32) -> Self {
        Self {
            rng: Rng::new(seed),
            phase: 1.0,
            from: 0.0,
            to: 0.0,
            length: 1.0,
            brown: 0.0,
            smooth: Smooth { value: 0.0 },
            value: 0.0,
        }
    }

    fn reset(&mut self) {
        self.phase = 1.0;
        self.from = 0.0;
        self.to = 0.0;
        self.brown = 0.0;
        self.value = 0.0;
        self.smooth.value = 0.0;
    }

    fn step(&mut self, kind: usize, cycles: f32) -> f32 {
        self.phase += cycles / self.length.max(0.1);
        if self.phase >= 1.0 {
            self.phase = self.phase.fract();
            self.from = self.to;
            self.to = self.rng.bipolar();
            // Simplex wanders irregularly: each segment gets its own length.
            self.length = if kind == 0 {
                0.4 + 1.6 * self.rng.unipolar()
            } else {
                1.0
            };
        }
        let t = self.phase;
        match kind {
            0 | 1 => {
                let s = t * t * (3.0 - 2.0 * t);
                self.from + (self.to - self.from) * s
            }
            2 => self.to,
            _ => {
                // Rate is the low-pass's corner. Dividing by the expected spread keeps the output
                // the same size at any rate.
                let k = cycles.clamp(1e-6, 1.0);
                self.brown += (self.rng.bipolar() - self.brown) * k;
                (self.brown * (2.0 / k - 1.0).sqrt()).clamp(-1.0, 1.0)
            }
        }
    }
}

// ------------------------------------------------------------------------------ device

pub struct Roar {
    v: Box<[f64; PARAMS]>,
    sr: f32,
    channels: [Channel; 2],
    oversamplers: [Oversampler; 2],
    dry_line: DryLine,
    latency: u32,
    max_block: usize,

    detector: SidechainDetector,
    env_state: f32,
    env_held: u32,
    env_peak: f32,

    lfos: [LfoSource; 2],
    noise: NoiseSource,
    matrix: ModMatrix,
    bus: ModBus,
    sources: [f32; SOURCE_COUNT],

    clock: BeatClock,
    held_note: Option<u8>,
    gate_level: f32,
    /// How far into the block `process` is, for the sidechain.
    sidechain_pos: usize,
    detector_stale: bool,
}

impl Default for Roar {
    fn default() -> Self {
        let mut v = Box::new([0.0; PARAMS]);
        for p in params() {
            v[p.id as usize] = p.default;
        }
        let matrix = ModMatrix::new(SOURCE_COUNT, DEST_COUNT, CELLS);
        let bus = matrix.bus();
        let (comb, delay) = (2_048, 1 << 16);
        let latency = Oversampler::new(OVERSAMPLE, 16).latency();
        Self {
            v,
            sr: 48_000.0,
            channels: [Channel::new(10, comb, delay), Channel::new(20, comb, delay)],
            oversamplers: [
                Oversampler::new(OVERSAMPLE, 512),
                Oversampler::new(OVERSAMPLE, 512),
            ],
            dry_line: DryLine::new(latency as usize),
            latency,
            max_block: 512,
            detector: SidechainDetector::new(),
            env_state: 0.0,
            env_held: 0,
            env_peak: 0.0,
            lfos: [LfoSource::new(101), LfoSource::new(202)],
            noise: NoiseSource::new(303),
            matrix,
            bus,
            sources: [0.0; SOURCE_COUNT],
            clock: BeatClock::default(),
            held_note: None,
            gate_level: 0.0,
            sidechain_pos: 0,
            detector_stale: true,
        }
    }
}

impl Roar {
    #[inline]
    fn val(&self, id: u32) -> f32 {
        self.v[id as usize] as f32
    }

    #[inline]
    fn on(&self, id: u32) -> bool {
        self.v[id as usize] >= 0.5
    }

    fn set_cell(&mut self, source: usize, dest: usize, amount: f32) {
        let route = (amount != 0.0).then_some(Route {
            source,
            dest,
            amount,
            kind: RouteKind::Add,
            via: None,
        });
        self.matrix.set_route(source * DEST_COUNT + dest, route);
    }

    fn update_detector(&mut self) {
        let width = self.val(ENV_WIDTH);
        let ext = self.on(EXT_SC);
        self.detector.set(&SidechainSettings {
            gain_db: self.val(SC_GAIN),
            mix: if ext { self.val(SC_MIX) } else { 0.0 },
            eq: SidechainEq::BandPass,
            eq_freq: self.val(ENV_FREQ),
            eq_q: 0.3 + (1.0 - width) * 5.0,
            eq_gain_db: 0.0,
            attack: 0.001,
            release: 0.05,
            detect: DetectMode::Peak,
            listen: self.on(ENV_LISTEN) || self.on(SC_LISTEN),
        });
    }

    /// Runs the envelope for a block of key peaks. Returns its level after this block, 0..1.
    fn run_envelope(&mut self, peaks: impl Iterator<Item = f32>) -> f32 {
        let sr = self.sr;
        let attack = (self.val(ENV_ATTACK) / 1_000.0 * sr).max(1.0);
        let release = (self.val(ENV_RELEASE) / 1_000.0 * sr).max(1.0);
        let (k_up, k_down) = ((-1.0 / attack).exp(), (-1.0 / release).exp());
        let hold = self.on(ENV_HOLD);
        let (thr, gain) = (
            db_to_gain(self.val(ENV_THRESHOLD)),
            db_to_gain(self.val(ENV_GAIN)),
        );
        for level in peaks {
            let rising = level > self.env_state;
            if rising && hold && self.env_held == 0 {
                self.env_held = attack as u32;
                self.env_peak = level;
            }
            let k = if self.env_held > 0 || rising {
                k_up
            } else {
                k_down
            };
            let target = if self.env_held > 0 {
                // Keeps climbing to the peak of what started this attack, even if the level
                // has already dropped away.
                self.env_peak = self.env_peak.max(level);
                self.env_peak
            } else {
                level
            };
            self.env_state = sanitize(target + (self.env_state - target) * k);
            self.env_held = self.env_held.saturating_sub(1);
        }
        (((self.env_state - thr) / (1.0 - thr).max(1e-3)).max(0.0) * gain).clamp(0.0, 1.0)
    }

    fn update_sources(&mut self, frames: usize, env: f32) {
        let sr = self.sr;
        let spb = 1.0 / (self.clock.beats_per_sample * sr as f64).max(1e-6);
        for i in 0..2 {
            let base = if i == 0 { LFO1_BASE } else { LFO2_BASE };
            let mode = self.val(base + LFO_MODE).round() as usize;
            let division = self.val(base + LFO_SYNC).round() as usize;
            let rate = match mode_beats(mode, division) {
                Some(beats) => LfoRate::Beats(beats),
                None => LfoRate::Hz(self.val(base + LFO_RATE)),
            };
            let kind = self.val(base + LFO_WAVE).round() as usize;
            let morph = self.val(base + LFO_MORPH);
            let smooth = self.val(base + LFO_SMOOTH);
            let lfo = &mut self.lfos[i];
            lfo.lfo.set(&LfoSettings {
                rate,
                ..Default::default()
            });
            let phase = lfo.lfo.advance_phases(frames)[0];
            let value = wave(kind, warp(phase, morph));
            lfo.value = lfo.smooth.step(value, smooth, frames, sr);
        }
        let mode = self.val(NOISE_MODE).round() as usize;
        let division = self.val(NOISE_SYNC).round() as usize;
        let hz = match mode_beats(mode, division) {
            Some(beats) => (1.0 / (beats * spb).max(1e-6)) as f32,
            None => self.val(NOISE_RATE),
        };
        let kind = self.val(NOISE_TYPE).round() as usize;
        let smooth = self.val(NOISE_SMOOTH);
        let cycles = (hz * frames as f32 / sr).min(1.0);
        let value = self.noise.step(kind, cycles);
        self.noise.value = self.noise.smooth.step(value, smooth, frames, sr);
        self.sources = [
            self.lfos[0].value,
            self.lfos[1].value,
            env,
            self.noise.value,
        ];
    }

    fn resolve(&mut self, frames: usize, active: bool) -> (Ctl, f32, f32, f32) {
        self.matrix.evaluate(&self.sources, &mut self.bus);
        let global = self.val(GLOBAL_MOD);
        let bus = &self.bus;
        let m = |d: usize| bus.offset(d) * global;
        let v = |id: u32| self.v[id as usize] as f32;
        let on = |id: u32| self.v[id as usize] >= 0.5;

        let routing = v(ROUTING).round() as usize;
        let drive = db_to_gain(v(DRIVE) + m(DEST_DRIVE) * 36.0);
        let tone = (v(TONE_AMOUNT) + m(DEST_TONE_AMOUNT) * 2.0).clamp(-1.0, 1.0);
        let tone_freq = v(TONE_FREQ) * 2f32.powf(m(DEST_TONE_FREQ) * 4.0);
        let blend = (v(BLEND) + m(DEST_BLEND)).clamp(0.0, 1.0);
        let fb_freq = v(FB_FREQ) * 2f32.powf(m(DEST_FB_FREQ) * 4.0);
        let fb_width = (v(FB_WIDTH) + m(DEST_FB_WIDTH) * 8.0).clamp(0.1, 8.0);
        let comp = (v(COMP_AMOUNT) + m(DEST_COMP)).clamp(0.0, 1.0);
        let out_db = v(OUTPUT) + m(DEST_OUTPUT) * 24.0;
        let mix = (v(DRY_WET) + m(DEST_DRY_WET)).clamp(0.0, 1.0);

        let midi_on = on(MIDI_FB);
        let mode = if midi_on {
            4
        } else {
            v(FB_MODE).round() as usize
        };
        let time_mod = 2f32.powf(m(DEST_FB_TIME) * 3.0);
        let rate_hi = self.channels[0].rate;
        let spb = 1.0 / (self.clock.beats_per_sample * self.sr as f64).max(1e-6);
        let seconds = match mode {
            0 => v(FB_TIME) / 1_000.0,
            1..=3 => {
                let division = v(FB_SYNC).round() as usize;
                let beats = mode_beats(mode, division).unwrap_or(1.0);
                (beats * spb) as f32
            }
            _ => {
                let note = if midi_on {
                    self.held_note.map_or(v(FB_NOTE), |n| n as f32)
                } else {
                    v(FB_NOTE)
                };
                1.0 / midi_to_hz(note).max(1.0)
            }
        };
        let cap = self.channels[0].fb_line.capacity() as f32 - 2.0;
        let fb_delay = (seconds * time_mod * rate_hi - 1.0).clamp(0.0, cap);

        let mut fb_amount = (v(FB_AMOUNT) + m(DEST_FB_AMOUNT)).clamp(0.0, 1.0);
        if midi_on {
            fb_amount = if self.held_note.is_some() { 0.85 } else { 0.0 };
        }
        let gate_target = if !on(FB_GATE) || active { 1.0 } else { 0.0 };
        let dt = frames as f32 / (0.03 * self.sr);
        self.gate_level += (gate_target - self.gate_level) * dt.min(1.0);

        let mut stages = [StageCtl::default(); 3];
        for (s, c) in stages.iter_mut().enumerate() {
            let id = |offset: u32| stage_param(s, offset);
            let dest = |offset: usize| m(DEST_STAGE_BASE + s * DEST_STAGE_STEP + offset);
            let amount = (v(id(STAGE_AMOUNT)) + dest(DEST_AMOUNT)).clamp(0.0, 1.0);
            let filter = v(id(STAGE_FILTER)).round() as usize;
            *c = StageCtl {
                on: on(id(STAGE_ON)),
                amount,
                bias: (v(id(STAGE_BIAS)) + dest(DEST_BIAS) * 2.0).clamp(-2.0, 2.0),
                gain: db_to_gain(amount * 36.0),
                kind: v(id(STAGE_SHAPER)).round() as usize,
                shaper_on: on(id(STAGE_SHAPER_ON)),
                level: db_to_gain(v(id(STAGE_LEVEL)) + dest(DEST_LEVEL) * 24.0),
                filter_on: on(id(STAGE_FILTER_ON)),
                filter,
                pre: on(id(STAGE_PRE)),
                freq: v(id(STAGE_FREQ)) * 2f32.powf(dest(DEST_FREQ) * 5.0),
                res: (v(id(STAGE_RES)) + dest(DEST_RES)).clamp(0.0, 1.0),
                peak_db: v(id(STAGE_PEAK))
                    + if filter == 4 {
                        dest(DEST_MORPH) * 24.0
                    } else {
                        0.0
                    },
                morph: (v(id(STAGE_MORPH)) + dest(DEST_MORPH)).clamp(0.0, 1.0),
            };
        }

        let ctl = Ctl {
            routing,
            blend,
            tone_db: tone * TONE_RANGE_DB,
            tone_freq,
            color: on(COLOR_COMP),
            xover_lo: v(XOVER_LO),
            xover_hi: v(XOVER_HI).max(v(XOVER_LO) * 1.05),
            stages,
            fb_amount: fb_amount * self.gate_level,
            fb_invert: on(FB_INVERT),
            fb_freq,
            fb_width,
            fb_delay,
            comp,
            comp_hp: on(COMP_HP),
        };
        (ctl, drive, db_to_gain(out_db), mix)
    }

    fn render(&mut self, audio: AudioBuffer, sidechain: Option<AudioInput<'_>>) {
        let frames = audio.frames();
        let (left, right) = (audio.left, audio.right);
        let sidechain = if self.on(EXT_SC) { sidechain } else { None };
        let mut pos = 0;
        while pos < frames {
            let n = (frames - pos).min(CONTROL_FRAMES).min(self.max_block);
            let (l, r) = (&mut left[pos..pos + n], &mut right[pos..pos + n]);
            let at = self.sidechain_pos + pos;
            let ext = sidechain
                .filter(|s| s.frames() >= at + n)
                .map(|s| AudioInput {
                    left: &s.left[at..at + n],
                    right: &s.right[at..at + n],
                });
            if self.detector_stale {
                self.detector_stale = false;
                self.update_detector();
            }
            let key = self.detector.process(l, r, ext);
            let mut peaks = [0.0f32; CONTROL_FRAMES];
            for (peak, (l, r)) in peaks.iter_mut().zip(key.left.iter().zip(key.right)) {
                *peak = l.abs().max(r.abs());
            }
            let active = peaks[..n].iter().any(|p| *p > 1e-4);
            let env = self.run_envelope(peaks[..n].iter().copied());
            self.update_sources(n, env);
            let (ctl, drive, output, mix) = self.resolve(n, active);

            let mut dry_l = [0.0f32; CONTROL_FRAMES];
            let mut dry_r = [0.0f32; CONTROL_FRAMES];
            for i in 0..n {
                dry_l[i] = self.dry_line.left(l[i]);
                dry_r[i] = self.dry_line.right(r[i]);
            }
            let mid_side = ctl.routing == 4;
            for i in 0..n {
                let (a, b) = if mid_side {
                    encode_ms(l[i], r[i])
                } else {
                    (l[i], r[i])
                };
                l[i] = a * drive;
                r[i] = b * drive;
            }
            let [c0, c1] = &mut self.channels;
            let [os0, os1] = &mut self.oversamplers;
            for (ch, chan, os, block) in [
                (0usize, &mut *c0, &mut *os0, &mut *l),
                (1, &mut *c1, &mut *os1, &mut *r),
            ] {
                chan.tune(&ctl);
                os.process_in_place(block, |hi| {
                    for s in hi.iter_mut() {
                        *s = chan.tick(*s, &ctl, ch);
                    }
                });
            }
            // With nothing engaged the wet path is only a delay, and is written as exactly that:
            // the oversampling filters would otherwise ring a little around a transient. The
            // path above still runs, so the filters are warm when something is turned up.
            let transparent = ctl.is_transparent();
            for i in 0..n {
                let (a, b) = if transparent {
                    (dry_l[i] * drive, dry_r[i] * drive)
                } else if mid_side {
                    decode_ms(l[i], r[i])
                } else {
                    (l[i], r[i])
                };
                let wet_l = (a * output).clamp(-1.0, 1.0);
                let wet_r = (b * output).clamp(-1.0, 1.0);
                l[i] = dry_l[i] * (1.0 - mix) + wet_l * mix;
                r[i] = dry_r[i] * (1.0 - mix) + wet_r * mix;
            }
            let mut listen = AudioBuffer {
                left: &mut *l,
                right: &mut *r,
            };
            self.detector.listen(&mut listen);
            pos += n;
        }
        self.sidechain_pos += frames;
    }

    fn midi(&mut self, m: MidiMessage) {
        match m {
            MidiMessage::NoteOn { key, velocity, .. } if velocity > 0.0 => {
                self.held_note = Some(key);
            }
            MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => {
                if self.held_note == Some(key) {
                    self.held_note = None;
                }
            }
            MidiMessage::AllNotesOff => self.held_note = None,
            _ => {}
        }
    }
}

impl Processor for Roar {
    fn activate(&mut self, sample_rate: f64, max_block: usize) {
        let multiplier = OVERSAMPLE.multiplier();
        let rate = sample_rate * multiplier as f64;
        self.sr = sample_rate as f32;
        self.max_block = max_block.max(CONTROL_FRAMES);
        let capacity = (rate * MAX_DELAY_SECONDS as f64) as usize + 8;
        let comb = (rate / 15.0) as usize + 8;
        self.channels = [
            Channel::new(10, comb, capacity),
            Channel::new(20, comb, capacity),
        ];
        self.oversamplers = [
            Oversampler::new(OVERSAMPLE, self.max_block),
            Oversampler::new(OVERSAMPLE, self.max_block),
        ];
        for chan in &mut self.channels {
            chan.prepare(rate);
        }
        self.latency = self.oversamplers[0].latency();
        self.dry_line = DryLine::new(self.latency as usize);
        self.detector.prepare(sample_rate, self.max_block);
        self.update_detector();
        for lfo in &mut self.lfos {
            lfo.lfo.prepare(sample_rate);
        }
        self.clock = BeatClock::new(120.0, sample_rate);
        self.env_state = 0.0;
        self.env_held = 0;
        self.gate_level = 1.0;
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        self.clock = BeatClock::from_transport(ctx.transport);
        for lfo in &mut self.lfos {
            lfo.lfo.set_clock(&self.clock);
        }
        self.sidechain_pos = 0;
        let sidechain = ctx.sidechain;
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => self.set_param(id, value),
            BlockStep::Audio(audio) => self.render(audio, sidechain),
            BlockStep::Midi(m) => self.midi(m),
        });
        if ctx.view.is_active() {
            ctx.view.publish(MOD_LEVELS, &self.sources);
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        let Some(v) = self.v.get_mut(id as usize) else {
            return;
        };
        *v = value;
        self.detector_stale |= matches!(
            id,
            ENV_FREQ | ENV_WIDTH | ENV_LISTEN | EXT_SC | SC_MIX | SC_GAIN | SC_LISTEN
        );
        if id >= MATRIX_BASE {
            let cell = (id - MATRIX_BASE) as usize;
            if cell < CELLS {
                self.set_cell(cell / DEST_COUNT, cell % DEST_COUNT, value as f32);
            }
        }
    }

    fn reset(&mut self) {
        for chan in &mut self.channels {
            chan.reset();
        }
        for os in &mut self.oversamplers {
            os.reset();
        }
        self.dry_line.clear();
        self.detector.reset();
        self.env_state = 0.0;
        self.env_held = 0;
        for lfo in &mut self.lfos {
            lfo.lfo.retrigger();
            lfo.smooth.value = 0.0;
            lfo.value = 0.0;
        }
        self.noise.reset();
        self.sources = [0.0; SOURCE_COUNT];
        self.held_note = None;
        self.gate_level = 1.0;
    }

    fn latency(&self) -> u32 {
        self.latency
    }

    fn tail(&self) -> Tail {
        if self.v[FB_AMOUNT as usize] > 0.0 && !self.on(FB_GATE) {
            Tail::Infinite
        } else if self.val(ROUTING).round() >= 5.0 || self.v[FB_AMOUNT as usize] > 0.0 {
            Tail::Frames((MAX_DELAY_SECONDS * self.sr) as u32)
        } else {
            Tail::None
        }
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.roar",
    name: "Roar",
    role: DeviceRole::AudioEffect,
    category: "Distortion",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        ports: Ports {
            sidechain: true,
            ..Default::default()
        },
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::<Roar>::default(),
};
