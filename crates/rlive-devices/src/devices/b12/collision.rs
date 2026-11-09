//! Collision: mallet percussion from exciters and two stereo resonators (manual pages 713-722).
//!
//! Beam, Marimba, String, Membrane and Plate are modal banks with textbook ratios and mode shapes;
//! Pipe and Tube are tuned waveguides. These are our own designs: Collision's models are
//! proprietary and the manual describes behaviour only. Its ranges, except the percentages it gives
//! for Hit, Pos., Opening, Note Off and Key, are ours, as is the destination list for the LFOs and
//! MIDI tab, which it describes only as "mallet, noise and resonator" parameters.

use super::common::{
    knob, limit, menu, note_hz, scale_note_bend, toggle, ModalBank, StealFade, CONTROL, MAX_MODES,
};
use crate::dsp::delay::{InterpDelay, Interpolation};
use crate::dsp::envelope::{Envelope, EnvelopeSettings};
use crate::dsp::filter::{Circuit, Filter, FilterKind, FilterSettings, FilterSlope};
use crate::dsp::lfo::{Lfo, LfoRate, LfoSettings, LfoShape};
use crate::dsp::modmatrix::{ModBus, ModMatrix, Route, RouteKind};
use crate::dsp::tempo::{BeatClock, SyncRate};
use crate::dsp::util::{db_to_gain, sanitize, Rng};
use crate::dsp::voice::{NoteInfo, VoiceAllocator, VoiceEvent, VoiceSettings, VoiceState};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    BlockStep, DeviceDesc, Item, MidiMessage, ParamInfo, ProcessContext, Processor, Section,
    Status, Tail, ViewId,
};
use std::f32::consts::{PI, TAU};

pub const M_ON: u32 = 0;
pub const M_VOL: u32 = 1;
pub const M_STIFF: u32 = 2;
pub const M_NOISE: u32 = 3;
pub const M_COLOR: u32 = 4;
pub const M_VOL_KEY: u32 = 5;
pub const M_VOL_VEL: u32 = 6;
pub const M_STIFF_KEY: u32 = 7;
pub const M_STIFF_VEL: u32 = 8;
pub const M_NOISE_KEY: u32 = 9;
pub const M_NOISE_VEL: u32 = 10;
pub const N_ON: u32 = 11;
pub const N_TYPE: u32 = 12;
pub const N_FREQ: u32 = 13;
pub const N_RES: u32 = 14;
pub const N_VOL: u32 = 15;
pub const N_ENV_AMT: u32 = 16;
pub const N_A: u32 = 17;
pub const N_D: u32 = 18;
pub const N_S: u32 = 19;
pub const N_R: u32 = 20;
pub const N_FREQ_KEY: u32 = 21;
pub const N_FREQ_VEL: u32 = 22;
pub const N_VOL_KEY: u32 = 23;
pub const N_VOL_VEL: u32 = 24;

pub const RES_BASE: [u32; 2] = [30, 70];
pub const R_ON: u32 = 0;
pub const R_TYPE: u32 = 1;
pub const R_QUALITY: u32 = 2;
pub const R_DECAY: u32 = 3;
pub const R_MATERIAL: u32 = 4;
pub const R_RADIUS: u32 = 5;
pub const R_RATIO: u32 = 6;
pub const R_BRIGHT: u32 = 7;
pub const R_INHARM: u32 = 8;
pub const R_OPENING: u32 = 9;
pub const R_HIT: u32 = 10;
pub const R_RND: u32 = 11;
pub const R_NOTE_OFF: u32 = 12;
pub const R_POS_L: u32 = 13;
pub const R_POS_R: u32 = 14;
pub const R_TUNE: u32 = 15;
pub const R_FINE: u32 = 16;
pub const R_TUNE_KEY: u32 = 17;
pub const R_PENV: u32 = 18;
pub const R_PENV_TIME: u32 = 19;
pub const R_BLEED: u32 = 20;
pub const R_GAIN: u32 = 21;
pub const R_PAN: u32 = 22;
pub const R_DECAY_KEY: u32 = 23;
pub const R_DECAY_VEL: u32 = 24;
pub const R_MAT_KEY: u32 = 25;
pub const R_MAT_VEL: u32 = 26;
pub const R_INHARM_VEL: u32 = 27;
pub const R_OPEN_VEL: u32 = 28;
pub const R_PENV_VEL: u32 = 29;
pub const R_PAN_KEY: u32 = 30;
pub const R_PARAMS: u32 = 31;

pub const LFO_BASE: [u32; 2] = [110, 125];
pub const L_ON: u32 = 0;
pub const L_WAVE: u32 = 1;
pub const L_OFFSET: u32 = 2;
pub const L_RETRIG: u32 = 3;
pub const L_SYNC: u32 = 4;
pub const L_RATE: u32 = 5;
pub const L_DIV: u32 = 6;
pub const L_AMOUNT: u32 = 7;
pub const L_DEST_A: u32 = 8;
pub const L_AMT_A: u32 = 9;
pub const L_DEST_B: u32 = 10;
pub const L_AMT_B: u32 = 11;
pub const L_RATE_KEY: u32 = 12;
pub const L_AMT_VEL: u32 = 13;

/// `[dest A, amount A, dest B, amount B]` each, for pitch bend, wheel, pressure and slide.
pub const MIDI_BASE: [u32; 4] = [140, 144, 148, 152];
pub const VOICES: u32 = 160;
pub const RETRIG: u32 = 161;
pub const STRUCTURE: u32 = 162;
pub const VOLUME: u32 = 163;
const PARAMS: usize = 164;

pub const BEAM: usize = 0;
pub const MARIMBA: usize = 1;
pub const STRING: usize = 2;
pub const MEMBRANE: usize = 3;
pub const PLATE: usize = 4;
pub const PIPE: usize = 5;
pub const TUBE: usize = 6;

pub const STRUCT_SERIAL: usize = 0;
pub const STRUCT_PARALLEL: usize = 1;

pub const RESONANCE_TYPES: [&str; 7] = [
    "Beam", "Marimba", "String", "Membrane", "Plate", "Pipe", "Tube",
];
/// Partials per Quality setting. The manual names only the two ends, Eco and High.
pub const QUALITY_MODES: [usize; 4] = [8, 14, 22, MAX_MODES];
const VOICE_COUNTS: [usize; 8] = [1, 2, 4, 6, 8, 12, 16, 24];
const WAVES: [&str; 7] = [
    "Sine", "Square", "Triangle", "Saw Up", "Saw Down", "Noise 1", "Noise 2",
];

const DEST_LABELS: [&str; 27] = [
    "Off",
    "Mallet Volume",
    "Mallet Stiffness",
    "Mallet Noise",
    "Noise Volume",
    "Noise Freq",
    "Noise Res",
    "Res 1 Decay",
    "Res 1 Material",
    "Res 1 Brightness",
    "Res 1 Inharm",
    "Res 1 Opening",
    "Res 1 Tune",
    "Res 1 Pan",
    "Res 1 Gain",
    "Res 2 Decay",
    "Res 2 Material",
    "Res 2 Brightness",
    "Res 2 Inharm",
    "Res 2 Opening",
    "Res 2 Tune",
    "Res 2 Pan",
    "Res 2 Gain",
    "LFO 1 Rate",
    "LFO 1 Amount",
    "LFO 2 Rate",
    "LFO 2 Amount",
];
const D_MALLET_VOL: usize = 1;
const D_MALLET_STIFF: usize = 2;
const D_MALLET_NOISE: usize = 3;
const D_NOISE_VOL: usize = 4;
const D_NOISE_FREQ: usize = 5;
const D_NOISE_RES: usize = 6;
const D_RES: [usize; 2] = [7, 15];
const DR_DECAY: usize = 0;
const DR_MATERIAL: usize = 1;
const DR_BRIGHT: usize = 2;
const DR_INHARM: usize = 3;
const DR_OPENING: usize = 4;
const DR_TUNE: usize = 5;
const DR_PAN: usize = 6;
const DR_GAIN: usize = 7;
const D_LFO_RATE: [usize; 2] = [23, 25];
const D_LFO_AMOUNT: [usize; 2] = [24, 26];

/// Semitones at 100% for the Tune destinations.
const DEST_TUNE_SEMITONES: f32 = 12.0;

const SRC_LFO: [usize; 2] = [0, 1];
const SRC_BEND: usize = 2;
const SRC_WHEEL: usize = 3;
const SRC_PRESSURE: usize = 4;
const SRC_SLIDE: usize = 5;
const SRC_NOTE_BEND: usize = 6;
const SOURCES: usize = 7;
const ROUTES: usize = 14;

const MAX_VOICES: usize = 24;
const MAX_DECAY_S: f32 = 30.0;

pub const VIEW: ViewId = ViewId(0);
/// `[voices playing]`.
pub const VIEW_LEN: usize = 1;

fn res_params(base: u32, n: usize) -> Vec<ParamInfo> {
    let name = |s: &str| format!("Res {n} {s}");
    let is_first = n == 1;
    vec![
        ParamInfo::toggle(base + R_ON, &name("On"), is_first),
        ParamInfo::stepped(
            base + R_TYPE,
            &name("Type"),
            &RESONANCE_TYPES,
            if is_first { BEAM } else { TUBE },
        ),
        ParamInfo::stepped(
            base + R_QUALITY,
            &name("Quality"),
            &["Eco", "Low", "Mid", "High"],
            2,
        ),
        ParamInfo::skewed(
            base + R_DECAY,
            &name("Decay"),
            10.0,
            30_000.0,
            800.0,
            "ms",
            4.0,
        ),
        ParamInfo::continuous(
            base + R_MATERIAL,
            &name("Material"),
            -100.0,
            100.0,
            0.0,
            "%",
        ),
        ParamInfo::continuous(base + R_RADIUS, &name("Radius"), 0.0, 100.0, 50.0, "%"),
        ParamInfo::skewed(base + R_RATIO, &name("Ratio"), 0.25, 4.0, 1.0, "", 2.0),
        ParamInfo::continuous(
            base + R_BRIGHT,
            &name("Brightness"),
            -100.0,
            100.0,
            0.0,
            "%",
        ),
        ParamInfo::continuous(base + R_INHARM, &name("Inharm"), -100.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(base + R_OPENING, &name("Opening"), 0.0, 100.0, 100.0, "%"),
        ParamInfo::continuous(base + R_HIT, &name("Hit"), 0.0, 100.0, 25.0, "%"),
        ParamInfo::continuous(base + R_RND, &name("Rnd"), 0.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(base + R_NOTE_OFF, &name("Note Off"), 0.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(base + R_POS_L, &name("Pos. L"), 0.0, 100.0, 20.0, "%"),
        ParamInfo::continuous(base + R_POS_R, &name("Pos. R"), 0.0, 100.0, 35.0, "%"),
        ParamInfo::continuous(base + R_TUNE, &name("Tune"), -48.0, 48.0, 0.0, "st"),
        ParamInfo::continuous(base + R_FINE, &name("Fine"), -50.0, 50.0, 0.0, "ct"),
        ParamInfo::continuous(
            base + R_TUNE_KEY,
            &name("Tune Key"),
            -200.0,
            200.0,
            100.0,
            "%",
        ),
        ParamInfo::continuous(base + R_PENV, &name("Pitch Env"), -48.0, 48.0, 0.0, "st"),
        ParamInfo::skewed(
            base + R_PENV_TIME,
            &name("Time"),
            1.0,
            5_000.0,
            100.0,
            "ms",
            3.0,
        ),
        ParamInfo::continuous(base + R_BLEED, &name("Bleed"), 0.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(base + R_GAIN, &name("Gain"), -24.0, 24.0, 0.0, "dB"),
        ParamInfo::continuous(base + R_PAN, &name("Pan"), -100.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(
            base + R_DECAY_KEY,
            &name("Decay Key"),
            -100.0,
            100.0,
            0.0,
            "%",
        ),
        ParamInfo::continuous(
            base + R_DECAY_VEL,
            &name("Decay Vel"),
            -100.0,
            100.0,
            0.0,
            "%",
        ),
        ParamInfo::continuous(
            base + R_MAT_KEY,
            &name("Material Key"),
            -100.0,
            100.0,
            0.0,
            "%",
        ),
        ParamInfo::continuous(
            base + R_MAT_VEL,
            &name("Material Vel"),
            -100.0,
            100.0,
            0.0,
            "%",
        ),
        ParamInfo::continuous(
            base + R_INHARM_VEL,
            &name("Inharm Vel"),
            -100.0,
            100.0,
            0.0,
            "%",
        ),
        ParamInfo::continuous(
            base + R_OPEN_VEL,
            &name("Opening Vel"),
            -100.0,
            100.0,
            0.0,
            "%",
        ),
        ParamInfo::continuous(
            base + R_PENV_VEL,
            &name("Pitch Env Vel"),
            -100.0,
            100.0,
            0.0,
            "%",
        ),
        ParamInfo::continuous(base + R_PAN_KEY, &name("Pan Key"), -100.0, 100.0, 0.0, "%"),
    ]
}

fn lfo_params(base: u32, n: usize) -> Vec<ParamInfo> {
    let name = |s: &str| format!("LFO {n} {s}");
    vec![
        ParamInfo::toggle(base + L_ON, &name("On"), false),
        ParamInfo::stepped(base + L_WAVE, &name("Wave"), &WAVES, 0),
        ParamInfo::continuous(base + L_OFFSET, &name("Offs."), 0.0, 360.0, 0.0, "deg"),
        ParamInfo::toggle(base + L_RETRIG, &name("Retrigger"), false),
        ParamInfo::toggle(base + L_SYNC, &name("Sync"), false),
        ParamInfo::skewed(base + L_RATE, &name("Rate"), 0.01, 60.0, 2.0, "Hz", 3.0),
        ParamInfo::stepped(base + L_DIV, &name("Beat Rate"), &SyncRate::LABELS, 12),
        ParamInfo::continuous(base + L_AMOUNT, &name("Amount"), 0.0, 100.0, 100.0, "%"),
        ParamInfo::stepped(base + L_DEST_A, &name("Destination A"), &DEST_LABELS, 0),
        ParamInfo::continuous(base + L_AMT_A, &name("Amount A"), -100.0, 100.0, 0.0, "%"),
        ParamInfo::stepped(base + L_DEST_B, &name("Destination B"), &DEST_LABELS, 0),
        ParamInfo::continuous(base + L_AMT_B, &name("Amount B"), -100.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(
            base + L_RATE_KEY,
            &name("Rate Key"),
            -100.0,
            100.0,
            0.0,
            "%",
        ),
        ParamInfo::continuous(
            base + L_AMT_VEL,
            &name("Amount Vel"),
            -100.0,
            100.0,
            0.0,
            "%",
        ),
    ]
}

fn midi_params() -> Vec<ParamInfo> {
    let mut out = Vec::new();
    let defaults = [
        (12, 17.0, 20, 17.0),
        (0, 0.0, 0, 0.0),
        (0, 0.0, 0, 0.0),
        (0, 0.0, 0, 0.0),
    ];
    for ((base, label), (dest_a, amount_a, dest_b, amount_b)) in MIDI_BASE
        .into_iter()
        .zip(["PB", "Mod", "Press", "Slide"])
        .zip(defaults)
    {
        out.push(ParamInfo::stepped(
            base,
            &format!("{label} Destination A"),
            &DEST_LABELS,
            dest_a,
        ));
        out.push(ParamInfo::continuous(
            base + 1,
            &format!("{label} Amount A"),
            -100.0,
            100.0,
            amount_a,
            "%",
        ));
        out.push(ParamInfo::stepped(
            base + 2,
            &format!("{label} Destination B"),
            &DEST_LABELS,
            dest_b,
        ));
        out.push(ParamInfo::continuous(
            base + 3,
            &format!("{label} Amount B"),
            -100.0,
            100.0,
            amount_b,
            "%",
        ));
    }
    out
}

fn params() -> Vec<ParamInfo> {
    let pct = |id, name: &str, d| ParamInfo::continuous(id, name, 0.0, 100.0, d, "%");
    let bi = |id, name: &str, d| ParamInfo::continuous(id, name, -100.0, 100.0, d, "%");
    let mut v = vec![
        ParamInfo::toggle(M_ON, "Mallet", true),
        ParamInfo::continuous(M_VOL, "Mallet Volume", -48.0, 12.0, 0.0, "dB"),
        pct(M_STIFF, "Stiffness", 40.0),
        pct(M_NOISE, "Mallet Noise", 0.0),
        pct(M_COLOR, "Color", 50.0),
        bi(M_VOL_KEY, "Mallet Volume Key", 0.0),
        bi(M_VOL_VEL, "Mallet Volume Vel", 80.0),
        bi(M_STIFF_KEY, "Stiffness Key", 0.0),
        bi(M_STIFF_VEL, "Stiffness Vel", 30.0),
        bi(M_NOISE_KEY, "Mallet Noise Key", 0.0),
        bi(M_NOISE_VEL, "Mallet Noise Vel", 0.0),
        ParamInfo::toggle(N_ON, "Noise", false),
        ParamInfo::stepped(N_TYPE, "Noise Type", &["LP", "HP", "BP", "LP+HP"], 0),
        ParamInfo::skewed(N_FREQ, "Noise Freq", 20.0, 20_000.0, 4_000.0, "Hz", 3.0),
        pct(N_RES, "Noise Res", 20.0),
        ParamInfo::continuous(N_VOL, "Noise Volume", -48.0, 12.0, -12.0, "dB"),
        bi(N_ENV_AMT, "Env Amt", 0.0),
        ParamInfo::skewed(N_A, "Noise A", 0.5, 10_000.0, 1.0, "ms", 3.0),
        ParamInfo::skewed(N_D, "Noise D", 1.0, 20_000.0, 200.0, "ms", 3.0),
        pct(N_S, "Noise S", 0.0),
        ParamInfo::skewed(N_R, "Noise R", 1.0, 20_000.0, 100.0, "ms", 3.0),
        bi(N_FREQ_KEY, "Noise Freq Key", 0.0),
        bi(N_FREQ_VEL, "Noise Freq Vel", 0.0),
        bi(N_VOL_KEY, "Noise Volume Key", 0.0),
        bi(N_VOL_VEL, "Noise Volume Vel", 50.0),
    ];
    v.extend(res_params(RES_BASE[0], 1));
    v.extend(res_params(RES_BASE[1], 2));
    v.extend(lfo_params(LFO_BASE[0], 1));
    v.extend(lfo_params(LFO_BASE[1], 2));
    v.extend(midi_params());
    v.push(ParamInfo::stepped(
        VOICES,
        "Voices",
        &["1", "2", "4", "6", "8", "12", "16", "24"],
        4,
    ));
    v.push(ParamInfo::toggle(RETRIG, "Retrig.", false));
    v.push(ParamInfo::stepped(
        STRUCTURE,
        "Structure",
        &["1 > 2", "1 + 2"],
        STRUCT_SERIAL,
    ));
    v.push(ParamInfo::continuous(
        VOLUME, "Volume", -48.0, 12.0, -6.0, "dB",
    ));
    v
}

fn res_section(title: &'static str, base: u32, panel: &'static str) -> Section {
    Section {
        title,
        items: vec![
            Item::Panel(panel),
            toggle(base + R_ON),
            menu(base + R_TYPE),
            menu(base + R_QUALITY),
            knob(base + R_DECAY),
            knob(base + R_MATERIAL),
            knob(base + R_RADIUS),
            knob(base + R_RATIO),
            knob(base + R_BRIGHT),
            knob(base + R_INHARM),
            knob(base + R_OPENING),
            knob(base + R_HIT),
            knob(base + R_RND),
            knob(base + R_NOTE_OFF),
            knob(base + R_POS_L),
            knob(base + R_POS_R),
            knob(base + R_TUNE),
            knob(base + R_FINE),
            knob(base + R_TUNE_KEY),
            knob(base + R_PENV),
            knob(base + R_PENV_TIME),
            knob(base + R_BLEED),
            knob(base + R_GAIN),
            knob(base + R_PAN),
            knob(base + R_DECAY_KEY),
            knob(base + R_DECAY_VEL),
            knob(base + R_MAT_KEY),
            knob(base + R_MAT_VEL),
            knob(base + R_INHARM_VEL),
            knob(base + R_OPEN_VEL),
            knob(base + R_PENV_VEL),
            knob(base + R_PAN_KEY),
        ],
    }
}

fn lfo_section(title: &'static str, base: u32) -> Section {
    Section {
        title,
        items: vec![
            toggle(base + L_ON),
            menu(base + L_WAVE),
            knob(base + L_OFFSET),
            toggle(base + L_RETRIG),
            toggle(base + L_SYNC),
            knob(base + L_RATE),
            menu(base + L_DIV),
            knob(base + L_AMOUNT),
            menu(base + L_DEST_A),
            knob(base + L_AMT_A),
            menu(base + L_DEST_B),
            knob(base + L_AMT_B),
            knob(base + L_RATE_KEY),
            knob(base + L_AMT_VEL),
        ],
    }
}

fn layout() -> Vec<Section> {
    let midi = |title, base: u32| Section {
        title,
        items: vec![menu(base), knob(base + 1), menu(base + 2), knob(base + 3)],
    };
    vec![
        Section {
            title: "Mallet",
            items: [M_ON, M_VOL, M_STIFF, M_NOISE, M_COLOR, M_VOL_KEY, M_VOL_VEL]
                .iter()
                .chain(&[M_STIFF_KEY, M_STIFF_VEL, M_NOISE_KEY, M_NOISE_VEL])
                .map(|&id| if id == M_ON { toggle(id) } else { knob(id) })
                .collect(),
        },
        Section {
            title: "Noise",
            items: vec![
                toggle(N_ON),
                menu(N_TYPE),
                knob(N_FREQ),
                knob(N_RES),
                knob(N_VOL),
                knob(N_ENV_AMT),
                knob(N_A),
                knob(N_D),
                knob(N_S),
                knob(N_R),
                knob(N_FREQ_KEY),
                knob(N_FREQ_VEL),
                knob(N_VOL_KEY),
                knob(N_VOL_VEL),
            ],
        },
        res_section("Resonator 1", RES_BASE[0], "collision_res_1"),
        res_section("Resonator 2", RES_BASE[1], "collision_res_2"),
        lfo_section("LFO 1", LFO_BASE[0]),
        lfo_section("LFO 2", LFO_BASE[1]),
        midi("Pitch Bend", MIDI_BASE[0]),
        midi("Mod Wheel", MIDI_BASE[1]),
        midi("Pressure", MIDI_BASE[2]),
        midi("Slide", MIDI_BASE[3]),
        Section {
            title: "Global",
            items: vec![menu(VOICES), toggle(RETRIG), menu(STRUCTURE), knob(VOLUME)],
        },
    ]
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.collision",
    name: "Collision",
    role: DeviceRole::Instrument,
    category: "Physical Models",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::new(Collision::new()),
};

// ------------------------------------------------------------------------------- mode shapes

/// One overtone: a ratio to the fundamental and its mode numbers along two axes.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Mode {
    pub ratio: f32,
    pub m: u8,
    pub n: u8,
}

const GRID: usize = 8;

/// Free-free beam, Euler-Bernoulli: the k-th overtone of `(beta_k / beta_1)^2`.
pub fn beam_ratio(k: usize) -> f32 {
    const FIRST: [f32; 5] = [1.0, 2.7565, 5.4039, 8.9329, 13.3444];
    FIRST.get(k - 1).copied().unwrap_or_else(|| {
        let beta = (k as f32 + 0.5) * PI;
        (beta / 4.730_04).powi(2)
    })
}

fn marimba_ratio(k: usize) -> f32 {
    match k {
        1 => 1.0,
        2 => 4.0,
        3 => 10.0,
        _ => beam_ratio(k) * 10.0 / beam_ratio(3),
    }
}

/// The overtones of `kind`, ascending. `ratio` is Membrane's and Plate's side ratio.
pub fn modes(kind: usize, count: usize, ratio: f32, out: &mut [Mode; MAX_MODES]) {
    let count = count.min(MAX_MODES);
    match kind {
        MEMBRANE | PLATE => {
            let mut grid = [Mode::default(); GRID * GRID];
            let plate = kind == PLATE;
            let f = |m: usize, n: usize| {
                let v = (m * m) as f32 + ((n as f32) / ratio).powi(2);
                if plate {
                    v
                } else {
                    v.sqrt()
                }
            };
            let base = f(1, 1);
            for m in 1..=GRID {
                for n in 1..=GRID {
                    grid[(m - 1) * GRID + n - 1] = Mode {
                        ratio: f(m, n) / base,
                        m: m as u8,
                        n: n as u8,
                    };
                }
            }
            grid.sort_unstable_by(|a, b| a.ratio.total_cmp(&b.ratio));
            out[..count].copy_from_slice(&grid[..count]);
        }
        STRING => {
            for (k, mode) in out.iter_mut().enumerate().take(count) {
                *mode = Mode {
                    ratio: (k + 1) as f32,
                    m: (k + 1) as u8,
                    n: 1,
                };
            }
        }
        MARIMBA => {
            for (k, mode) in out.iter_mut().enumerate().take(count) {
                *mode = Mode {
                    ratio: marimba_ratio(k + 1),
                    m: (k + 1) as u8,
                    n: 1,
                };
            }
        }
        _ => {
            for (k, mode) in out.iter_mut().enumerate().take(count) {
                *mode = Mode {
                    ratio: beam_ratio(k + 1),
                    m: (k + 1) as u8,
                    n: 1,
                };
            }
        }
    }
}

/// How strongly a mode moves at `at`: 0 is the object's centre, 1 its edge.
pub fn shape(kind: usize, mode: Mode, at: f32) -> f32 {
    let at = at.clamp(0.0, 1.0);
    match kind {
        STRING => (mode.m as f32 * PI * 0.5 * (1.0 - at).max(0.02)).sin(),
        MEMBRANE | PLATE => {
            let x = 0.5 * (1.0 - at).max(0.02);
            let y = 0.5 * (1.0 - 0.6 * at).max(0.02);
            (mode.m as f32 * PI * x).sin() * (mode.n as f32 * PI * y).sin()
        }
        _ => {
            let beta = (mode.m as f32 + 0.5) * PI * 0.5 * at;
            if mode.m % 2 == 1 {
                beta.cos()
            } else {
                beta.sin()
            }
        }
    }
}

/// A delay line with a one-pole loss filter in its feedback path.
#[derive(Clone, Debug)]
struct Comb {
    delay: InterpDelay,
    time: f32,
    feedback: f32,
    lowpass: f32,
    state: f32,
}

impl Comb {
    fn new(capacity: usize) -> Self {
        Self {
            delay: InterpDelay::new(capacity),
            time: 2.0,
            feedback: 0.0,
            lowpass: 0.0,
            state: 0.0,
        }
    }

    #[inline]
    fn tick(&mut self, x: f32) -> f32 {
        let y = self.delay.read(self.time, Interpolation::Cubic);
        self.state = sanitize((1.0 - self.lowpass) * y + self.lowpass * self.state);
        self.delay.write(x + self.feedback * self.state);
        y
    }

    fn clear(&mut self) {
        self.delay.clear();
        self.state = 0.0;
    }
}

/// What a resonator's overtones depend on. When nothing here changes, nothing is recalculated.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ResControl {
    kind: usize,
    count: usize,
    ratio: f32,
    pitch: f32,
    decay: f32,
    material: f32,
    radius: f32,
    bright: f32,
    inharm: f32,
    opening: f32,
    pos: [f32; 2],
    released: bool,
    note_off: f32,
}

#[derive(Clone, Debug)]
struct ResVoice {
    bank: ModalBank,
    closed: Comb,
    open: Comb,
    opening: f32,
    table: [Mode; MAX_MODES],
    table_key: (usize, usize, u32),
    control: Option<ResControl>,
    hit: f32,
    env_left: f32,
    tubular: bool,
}

impl ResVoice {
    fn new(capacity: usize) -> Self {
        Self {
            bank: ModalBank::default(),
            closed: Comb::new(capacity),
            open: Comb::new(capacity),
            opening: 1.0,
            table: [Mode::default(); MAX_MODES],
            table_key: (usize::MAX, 0, 0),
            control: None,
            hit: 0.0,
            env_left: 0.0,
            tubular: false,
        }
    }

    fn clear(&mut self) {
        self.bank.clear();
        self.closed.clear();
        self.open.clear();
        self.control = None;
    }

    fn apply(&mut self, c: ResControl, sr: f32) {
        if self.control == Some(c) {
            return;
        }
        self.control = Some(c);
        self.tubular = matches!(c.kind, PIPE | TUBE);
        let hz = note_hz(c.pitch).clamp(4.0, sr * 0.45);
        let released_time = |t: f32| {
            if c.released {
                t.max(1e-3).powf(1.0 - c.note_off) * 0.005f32.powf(c.note_off)
            } else {
                t
            }
        };
        if self.tubular {
            self.apply_tube(c, hz, released_time(c.decay), sr);
            return;
        }
        let key = (c.kind, c.count, c.ratio.to_bits());
        if self.table_key != key {
            modes(c.kind, c.count, c.ratio, &mut self.table);
            self.table_key = key;
        }
        self.bank.count = c.count;
        let stretch = 1.0 + c.inharm * 0.25;
        let mut weights = [0.0f32; MAX_MODES];
        let mut total = 0.0f32;
        for (i, m) in self.table[..c.count].iter().enumerate() {
            let ratio = m.ratio.max(1.0).powf(stretch);
            let f = hz * ratio;
            if f >= sr * 0.45 {
                continue;
            }
            let w = shape(c.kind, *m, self.hit) * ratio.powf(0.7 * c.bright) / ratio.powf(0.5);
            weights[i] = w;
            let listen = shape(c.kind, *m, c.pos[0])
                .abs()
                .max(shape(c.kind, *m, c.pos[1]).abs());
            total += (w * listen).abs();
        }
        let norm = 1.0 / total.max(0.3);
        for (i, m) in self.table[..c.count].iter().enumerate() {
            let ratio = m.ratio.max(1.0).powf(stretch);
            let t60 = released_time(c.decay * ratio.powf(c.material)).min(MAX_DECAY_S);
            self.bank.tune(i, hz * ratio, t60, sr);
            let (a, b) = (shape(c.kind, *m, c.pos[0]), shape(c.kind, *m, c.pos[1]));
            self.bank.weights(i, weights[i] * norm, a, b);
        }
    }

    fn apply_tube(&mut self, c: ResControl, hz: f32, decay: f32, sr: f32) {
        let hz = if c.radius > 0.8 {
            hz * (1.0 - 0.2 * ((c.radius - 0.8) / 0.2).powi(2))
        } else {
            hz
        };
        let t60 = (decay * (0.3 + 1.7 * c.radius)).clamp(1e-3, MAX_DECAY_S);
        let lowpass = 0.7 * (1.0 - c.radius).powi(2);
        let path = lowpass / (1.0 - lowpass);
        let set = |comb: &mut Comb, samples: f32, sign: f32| {
            // The delay's read-then-write loop adds one sample of its own.
            comb.time = (samples - path - 1.0).clamp(2.0, (comb.delay.capacity() - 4) as f32);
            comb.feedback = sign * 10f32.powf(-3.0 * samples / (sr * t60)).min(0.99995);
            comb.lowpass = lowpass;
        };
        // A tube closed at one end has only odd harmonics of a half-wave loop that inverts on each
        // round trip; closed at both, or open at both, every harmonic of a whole-wave loop.
        set(&mut self.closed, sr / (2.0 * hz), -1.0);
        set(&mut self.open, sr / hz, 1.0);
        self.opening = if c.kind == TUBE { 1.0 } else { c.opening };
    }

    #[inline]
    fn tick(&mut self, x: f32) -> [f32; 2] {
        if self.tubular {
            let closed = self.closed.tick(x * (1.0 - self.opening));
            let open = self.open.tick(x * self.opening);
            let y = closed + open;
            [y, y]
        } else {
            self.bank.tick(x)
        }
    }

    fn level(&self) -> f32 {
        if self.tubular {
            self.open.state.abs().max(self.closed.state.abs())
        } else {
            self.bank.level()
        }
    }
}

#[derive(Clone)]
struct Voice {
    active: bool,
    note: NoteInfo,
    key: f32,
    velocity: f32,
    released: bool,
    mallet_pending: Option<f32>,
    mallet_at: u32,
    mallet_len: u32,
    mallet_gain: f32,
    mallet_noise: f32,
    chiff_left: u32,
    chiff_len: u32,
    color_state: f32,
    color_coef: f32,
    noise_gain: f32,
    rng: Rng,
    noise_env: Envelope,
    noise_on: bool,
    filter: [Filter; 2],
    res: [ResVoice; 2],
    gains: [[f32; 2]; 2],
    bleed: [f32; 2],
    lfo: [Lfo; 2],
    lfo_value: [f32; 2],
    bus: ModBus,
    fade: StealFade,
    quiet: u32,
}

impl Voice {
    fn new(seed: u32, capacity: usize, matrix: &ModMatrix) -> Self {
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
            key: 0.0,
            velocity: 0.0,
            released: false,
            mallet_pending: None,
            mallet_at: 0,
            mallet_len: 0,
            mallet_gain: 0.0,
            mallet_noise: 0.0,
            chiff_left: 0,
            chiff_len: 1,
            color_state: 0.0,
            color_coef: 0.0,
            noise_gain: 0.0,
            rng: Rng::new(seed),
            noise_env: Envelope::new(),
            noise_on: false,
            filter: [Filter::new(), Filter::new()],
            res: [ResVoice::new(capacity), ResVoice::new(capacity)],
            gains: [[0.0; 2]; 2],
            bleed: [0.0; 2],
            lfo: [
                Lfo::new(seed.wrapping_mul(3)),
                Lfo::new(seed.wrapping_mul(5)),
            ],
            lfo_value: [0.0; 2],
            bus: matrix.bus(),
            fade: StealFade::default(),
            quiet: 0,
        }
    }

    fn silence(&mut self) {
        self.active = false;
        self.mallet_pending = None;
        self.mallet_at = 0;
        self.mallet_len = 0;
        self.chiff_left = 0;
        self.noise_on = false;
        self.noise_env.reset();
        self.fade.clear();
        for f in &mut self.filter {
            f.reset();
        }
        for r in &mut self.res {
            r.clear();
        }
    }
}

/// Collision. See the [module docs](self).
pub struct Collision {
    values: [f64; PARAMS],
    sample_rate: f32,
    alloc: VoiceAllocator,
    voices: Vec<Voice>,
    matrix: ModMatrix,
    routes_dirty: bool,
    bend: f32,
    wheel: f32,
    clock: BeatClock,
}

impl Default for Collision {
    fn default() -> Self {
        Self::new()
    }
}

impl Collision {
    pub fn new() -> Self {
        let mut values = [0.0; PARAMS];
        for p in params() {
            values[p.id as usize] = p.default;
        }
        Self {
            values,
            sample_rate: 48_000.0,
            alloc: VoiceAllocator::new(MAX_VOICES),
            voices: Vec::new(),
            matrix: ModMatrix::new(SOURCES, DEST_LABELS.len(), ROUTES),
            routes_dirty: true,
            bend: 0.0,
            wheel: 0.0,
            clock: BeatClock::default(),
        }
    }

    fn v(&self, id: u32) -> f32 {
        self.values[id as usize] as f32
    }

    fn sync_settings(&mut self) {
        let index = self.v(VOICES).round() as usize;
        let voices = VOICE_COUNTS[index.min(VOICE_COUNTS.len() - 1)];
        self.alloc.set(&VoiceSettings {
            polyphony: voices,
            retrigger: self.v(RETRIG) >= 0.5,
            bend_range: 0.0,
            ..Default::default()
        });
    }

    fn sync_routes(&mut self) {
        let route = |source, dest: f32, amount: f32| {
            let dest = dest.round() as usize;
            (dest != 0).then_some(Route {
                source,
                dest,
                amount: amount / 100.0,
                kind: RouteKind::Add,
                via: None,
            })
        };
        let mut slot = 0;
        for (i, base) in LFO_BASE.into_iter().enumerate() {
            for (d, a) in [(L_DEST_A, L_AMT_A), (L_DEST_B, L_AMT_B)] {
                self.matrix
                    .set_route(slot, route(SRC_LFO[i], self.v(base + d), self.v(base + a)));
                slot += 1;
            }
        }
        for (source, base) in [SRC_BEND, SRC_WHEEL, SRC_PRESSURE, SRC_SLIDE]
            .into_iter()
            .zip(MIDI_BASE)
        {
            for (d, a) in [(0, 1), (2, 3)] {
                self.matrix
                    .set_route(slot, route(source, self.v(base + d), self.v(base + a)));
                slot += 1;
            }
        }
        // Per-note bend always tunes the voice, so its route only exists for the other targets.
        for (d, a) in [(0, 1), (2, 3)] {
            let dest = self.v(MIDI_BASE[0] + d).round() as usize;
            let tune = D_RES.iter().any(|base| dest == base + DR_TUNE);
            let r = route(SRC_NOTE_BEND, dest as f32, self.v(MIDI_BASE[0] + a));
            self.matrix.set_route(slot, r.filter(|_| !tune));
            slot += 1;
        }
    }

    fn start(&mut self, v: usize, note: NoteInfo) {
        let sr = self.sample_rate;
        let key = (note.key as f32 - 60.0) / 60.0;
        let velocity = note.velocity;
        let values = self.values;
        let g = |id: u32| values[id as usize] as f32;
        let pc = |id: u32| values[id as usize] as f32 * 0.01;
        let voice = &mut self.voices[v];
        voice.silence();
        voice.active = true;
        voice.note = note;
        voice.key = key;
        voice.velocity = velocity;
        voice.released = false;
        voice.quiet = 0;
        let vol = |base: u32, key_amt: u32, vel_amt: u32| {
            (db_to_gain(g(base))
                * (1.0 + pc(key_amt) * key)
                * (1.0 + pc(vel_amt) * (velocity - 1.0)))
                .max(0.0)
        };
        if g(M_ON) >= 0.5 {
            let stiff = (pc(M_STIFF) + pc(M_STIFF_KEY) * key + pc(M_STIFF_VEL) * (velocity - 1.0))
                .clamp(0.0, 1.0);
            voice.mallet_pending = Some(stiff);
            voice.mallet_at = 0;
            voice.mallet_len = 0;
            voice.mallet_gain = vol(M_VOL, M_VOL_KEY, M_VOL_VEL);
            voice.mallet_noise =
                (pc(M_NOISE) + pc(M_NOISE_KEY) * key + pc(M_NOISE_VEL) * (velocity - 1.0)).max(0.0);
            voice.chiff_len = ((0.04 * sr) as u32).max(1);
            voice.chiff_left = voice.chiff_len;
            let hz = 200.0 * 2f32.powf(pc(M_COLOR) * 6.0);
            voice.color_coef = (-TAU * hz / sr).exp();
            voice.color_state = 0.0;
        }
        voice.noise_on = g(N_ON) >= 0.5;
        if voice.noise_on {
            voice.noise_gain = vol(N_VOL, N_VOL_KEY, N_VOL_VEL);
            voice.noise_env.gate_on();
        }
        for (i, base) in RES_BASE.into_iter().enumerate() {
            let rnd = pc(base + R_RND) * voice.rng.unipolar();
            voice.res[i].hit = (pc(base + R_HIT) + rnd).clamp(0.0, 1.0);
            voice.res[i].env_left = 1.0;
        }
        for (i, base) in LFO_BASE.into_iter().enumerate() {
            if values[(base + L_RETRIG) as usize] >= 0.5 {
                voice.lfo[i].retrigger();
            }
        }
    }

    fn lfo_settings(&self, base: u32, rate_mod: f32, key: f32) -> LfoSettings {
        let shape = [
            LfoShape::Sine,
            LfoShape::Square,
            LfoShape::Triangle,
            LfoShape::SawUp,
            LfoShape::SawDown,
            LfoShape::SampleHold,
            LfoShape::SmoothRandom,
        ][self.v(base + L_WAVE).round() as usize % WAVES.len()];
        let key_octaves = self.v(base + L_RATE_KEY) * 0.01 * key * 5.0;
        let rate = if self.v(base + L_SYNC) >= 0.5 {
            LfoRate::Beats(SyncRate::from_index(self.values[(base + L_DIV) as usize]).beats())
        } else {
            LfoRate::Hz(self.v(base + L_RATE) * 2f32.powf(key_octaves + rate_mod * 5.0))
        };
        LfoSettings {
            shape,
            rate,
            offset: self.v(base + L_OFFSET) / 360.0,
            ..Default::default()
        }
    }

    /// Does one voice's control-rate work for the `frames` about to be rendered.
    fn control(&mut self, i: usize, frames: usize) {
        let sr = self.sample_rate;
        let (key, velocity, note_key) = {
            let v = &self.voices[i];
            (v.key, v.velocity, v.note.key as f32)
        };
        let alloc_voice = *self.alloc.voice(i);
        let channel_pressure = self.alloc.channel_pressure(i);
        let note_bend = self.alloc.pitch(i) - note_key;
        let mut sources = [0.0f32; SOURCES];
        for (slot, base) in LFO_BASE.into_iter().enumerate() {
            let scale = (self.v(base + L_AMOUNT)
                * 0.01
                * (1.0 + self.v(base + L_AMT_VEL) * 0.01 * (velocity - 1.0)))
                .max(0.0);
            sources[SRC_LFO[slot]] = self.voices[i].lfo_value[slot] * scale;
        }
        sources[SRC_BEND] = self.bend;
        sources[SRC_WHEEL] = self.wheel;
        sources[SRC_PRESSURE] = alloc_voice.pressure.max(channel_pressure);
        sources[SRC_SLIDE] = alloc_voice.slide;
        sources[SRC_NOTE_BEND] = note_bend / rlive_core::MPE_BEND_RANGE;
        {
            let (matrix, voices) = (&self.matrix, &mut self.voices);
            matrix.evaluate(&sources, &mut voices[i].bus);
        }

        let values = self.values;
        let g = |id: u32| values[id as usize] as f32;
        let pc = |id: u32| values[id as usize] as f32 * 0.01;

        // LFOs, each modulating the other's rate and amount through the matrix.
        for (slot, base) in LFO_BASE.into_iter().enumerate() {
            if g(base + L_ON) < 0.5 {
                self.voices[i].lfo_value[slot] = 0.0;
                continue;
            }
            let rate_mod = self.voices[i].bus.offset(D_LFO_RATE[slot]);
            let amount_mod = 1.0 + self.voices[i].bus.offset(D_LFO_AMOUNT[slot]);
            let settings = self.lfo_settings(base, rate_mod, key);
            let clock = self.clock;
            let voice = &mut self.voices[i];
            voice.lfo[slot].set(&settings);
            voice.lfo[slot].set_clock(&clock);
            voice.lfo_value[slot] = voice.lfo[slot].advance(frames)[0] * amount_mod.max(0.0);
        }

        let voice = &mut self.voices[i];
        let bus = &voice.bus;

        // The mallet's stiffness is fixed at the strike, once the modulation is known.
        if let Some(stiff) = voice.mallet_pending.take() {
            let stiff = (stiff + bus.offset(D_MALLET_STIFF)).clamp(0.0, 1.0);
            // A soft mallet stays in contact for milliseconds, a hard one for a fraction of one.
            let seconds = 0.012 * (0.000_12f32 / 0.012).powf(stiff);
            voice.mallet_len = ((seconds * sr) as u32).max(2);
        }

        // Noise exciter.
        if voice.noise_on {
            let settings = EnvelopeSettings {
                attack: g(N_A) * 0.001,
                decay: g(N_D) * 0.001,
                sustain: pc(N_S),
                release: g(N_R) * 0.001,
                ..Default::default()
            };
            voice.noise_env.set(&settings);
            let octaves = pc(N_FREQ_KEY) * key * 5.0
                + pc(N_FREQ_VEL) * (velocity - 1.0) * 5.0
                + pc(N_ENV_AMT) * voice.noise_env.level() * 6.0
                + bus.offset(D_NOISE_FREQ) * 5.0;
            let cutoff = (g(N_FREQ) * 2f32.powf(octaves)).clamp(20.0, sr * 0.45);
            let res = (pc(N_RES) + bus.offset(D_NOISE_RES)).clamp(0.0, 1.0);
            let kind = g(N_TYPE).round() as usize;
            let set = |filter: &mut Filter, kind: FilterKind, cutoff: f32, resonance: f32| {
                filter.set(&FilterSettings {
                    circuit: Circuit::Svf,
                    kind,
                    slope: FilterSlope::Db12,
                    cutoff: cutoff.clamp(20.0, sr * 0.45),
                    resonance,
                    ..Default::default()
                });
            };
            match kind {
                1 => set(
                    &mut voice.filter[0],
                    FilterKind::HighPass,
                    cutoff,
                    res * 0.9,
                ),
                2 => set(
                    &mut voice.filter[0],
                    FilterKind::BandPass,
                    cutoff,
                    res * 0.9,
                ),
                3 => {
                    // LP+HP: Res sets the width of the pass band around Freq.
                    let width = 2f32.powf(0.1 + 3.0 * res);
                    set(
                        &mut voice.filter[0],
                        FilterKind::HighPass,
                        cutoff / width,
                        0.0,
                    );
                    set(
                        &mut voice.filter[1],
                        FilterKind::LowPass,
                        cutoff * width,
                        0.0,
                    );
                }
                _ => set(&mut voice.filter[0], FilterKind::LowPass, cutoff, res * 0.9),
            }
        }

        // Resonators.
        let mut level = 0.0f32;
        for (slot, base) in RES_BASE.into_iter().enumerate() {
            if g(base + R_ON) < 0.5 {
                voice.gains[slot] = [0.0; 2];
                voice.bleed[slot] = 0.0;
                continue;
            }
            let d = D_RES[slot];
            let dest = |n: usize| bus.offset(d + n);
            let kind = g(base + R_TYPE).round() as usize % RESONANCE_TYPES.len();

            // Decay and material follow key and velocity; both are ratios or offsets.
            let decay_octaves = pc(base + R_DECAY_KEY) * key * 3.0
                + pc(base + R_DECAY_VEL) * (velocity - 1.0) * 3.0
                + dest(DR_DECAY) * 4.0;
            let decay = (g(base + R_DECAY) * 0.001 * 2f32.powf(decay_octaves)).min(MAX_DECAY_S);
            let material_offset = pc(base + R_MAT_KEY) * key
                + pc(base + R_MAT_VEL) * (velocity - 1.0)
                + dest(DR_MATERIAL);
            let material = (pc(base + R_MATERIAL) + material_offset).clamp(-1.0, 1.0);
            let radius = (pc(base + R_RADIUS) + material_offset).clamp(0.0, 1.0);
            let inharm = (pc(base + R_INHARM)
                + pc(base + R_INHARM_VEL) * (velocity - 1.0)
                + dest(DR_INHARM))
            .clamp(-1.0, 1.0);
            let opening = (pc(base + R_OPENING)
                + pc(base + R_OPEN_VEL) * (velocity - 1.0)
                + dest(DR_OPENING))
            .clamp(0.0, 1.0);
            let bright = (pc(base + R_BRIGHT) + dest(DR_BRIGHT)).clamp(-1.0, 1.0);

            // Pitch: the key follows Tune Key around C3, with the pitch envelope gliding home.
            let r = &mut voice.res[slot];
            let env_seconds = (g(base + R_PENV_TIME) * 0.001).max(1e-3);
            let start = g(base + R_PENV) * (1.0 + pc(base + R_PENV_VEL) * (velocity - 1.0));
            let envelope = start * r.env_left;
            r.env_left = (r.env_left - frames as f32 / (env_seconds * sr)).max(0.0);
            let pitch = 60.0
                + (note_key - 60.0) * pc(base + R_TUNE_KEY)
                + g(base + R_TUNE)
                + g(base + R_FINE) * 0.01
                + envelope
                + note_bend
                + dest(DR_TUNE) * DEST_TUNE_SEMITONES;

            let quality = g(base + R_QUALITY).round() as usize;
            let control = ResControl {
                kind,
                count: QUALITY_MODES[quality.min(QUALITY_MODES.len() - 1)],
                ratio: g(base + R_RATIO),
                pitch,
                decay,
                material,
                radius,
                bright,
                inharm,
                opening,
                pos: [pc(base + R_POS_L), pc(base + R_POS_R)],
                released: voice.released,
                note_off: pc(base + R_NOTE_OFF),
            };
            r.apply(control, sr);
            r.bank.sanitize();
            level = level.max(r.level());

            let pan =
                (pc(base + R_PAN) + pc(base + R_PAN_KEY) * key + dest(DR_PAN)).clamp(-1.0, 1.0);
            let gain = db_to_gain(g(base + R_GAIN) + dest(DR_GAIN) * 24.0);
            let angle = (pan + 1.0) * PI * 0.25;
            voice.gains[slot] = [gain * angle.cos() * 1.414, gain * angle.sin() * 1.414];
            voice.bleed[slot] = pc(base + R_BLEED) * gain;
        }

        // A voice with nothing left ringing or being driven can go.
        let driving = voice.mallet_pending.is_some()
            || voice.mallet_at < voice.mallet_len
            || voice.chiff_left > 0
            || (voice.noise_on && voice.noise_env.is_active());
        if !driving && level < 3e-6 {
            voice.quiet += 1;
        } else {
            voice.quiet = 0;
        }
    }

    /// Renders one voice, at most [`CONTROL`] frames, adding to the output.
    fn render_voice(&mut self, i: usize, left: &mut [f32], right: &mut [f32]) {
        let bus_mallet_vol = 1.0 + self.voices[i].bus.offset(D_MALLET_VOL);
        let bus_mallet_noise = 1.0 + self.voices[i].bus.offset(D_MALLET_NOISE);
        let bus_noise_vol = 1.0 + self.voices[i].bus.offset(D_NOISE_VOL);
        let lp_hp = self.v(N_TYPE).round() as usize == 3;
        let serial = self.v(STRUCTURE).round() as usize == STRUCT_SERIAL;
        let on = [
            self.v(RES_BASE[0] + R_ON) >= 0.5,
            self.v(RES_BASE[1] + R_ON) >= 0.5,
        ];
        let volume = db_to_gain(self.v(VOLUME)) * 2.5;
        let voice = &mut self.voices[i];
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            // Mallet: a raised-cosine contact of unit area, so every stiffness has the same energy
            // at low frequencies, with a burst of coloured noise on top.
            let mut x = 0.0f32;
            if voice.mallet_at < voice.mallet_len {
                let t = voice.mallet_at as f32 / voice.mallet_len as f32;
                x += voice.mallet_gain * bus_mallet_vol.max(0.0) * (1.0 - (TAU * t).cos())
                    / voice.mallet_len as f32;
                voice.mallet_at += 1;
            }
            if voice.chiff_left > 0 {
                let t = voice.chiff_left as f32 / voice.chiff_len as f32;
                let noise = voice.rng.bipolar();
                voice.color_state =
                    voice.color_coef * voice.color_state + (1.0 - voice.color_coef) * noise;
                x += (noise - voice.color_state)
                    * t
                    * t
                    * voice.mallet_noise
                    * bus_mallet_noise.max(0.0)
                    * voice.mallet_gain
                    * 0.25;
                voice.chiff_left -= 1;
            }
            if voice.noise_on && voice.noise_env.is_active() {
                let env = voice.noise_env.next();
                let mut n = voice.filter[0].process(voice.rng.bipolar());
                if lp_hp {
                    n = voice.filter[1].process(n);
                }
                x += n * env * voice.noise_gain * bus_noise_vol.max(0.0) * 0.1;
            }
            let [a0, a1] = if on[0] {
                voice.res[0].tick(x)
            } else {
                [0.0; 2]
            };
            // In series the second resonator hears the first, mixed down; with the first off
            // it hears the exciters, as it does in parallel.
            let drive = if serial && on[0] { (a0 + a1) * 0.25 } else { x };
            let [b0, b1] = if on[1] {
                voice.res[1].tick(drive)
            } else {
                [0.0; 2]
            };
            let fade = voice.fade.gain();
            let dry = x * (voice.bleed[0] + voice.bleed[1]);
            *l += sanitize((a0 * voice.gains[0][0] + b0 * voice.gains[1][0] + dry) * fade * volume);
            *r += sanitize((a1 * voice.gains[0][1] + b1 * voice.gains[1][1] + dry) * fade * volume);
        }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        if self.routes_dirty {
            self.routes_dirty = false;
            self.sync_routes();
        }
        let mut at = 0;
        let frames = left.len().min(right.len());
        while at < frames {
            let n = (frames - at).min(CONTROL);
            self.alloc.advance(n);
            let (mut mix_l, mut mix_r) = ([0.0f32; CONTROL], [0.0f32; CONTROL]);
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
                self.control(i, n);
                self.render_voice(i, &mut mix_l[..n], &mut mix_r[..n]);
                let voice = &mut self.voices[i];
                let held = self.alloc.voice(i).state == VoiceState::Held;
                if !voice.fade.is_fading() && voice.quiet > 8 && !held {
                    voice.silence();
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

    fn midi(&mut self, message: MidiMessage) {
        match message {
            MidiMessage::PitchBend { value, .. } => self.bend = value,
            MidiMessage::ControlChange { cc: 1, value, .. } => self.wheel = value as f32 / 127.0,
            MidiMessage::AllNotesOff => self.bend = 0.0,
            _ => {}
        }
        // Note PB isn't one of Collision's parameters: per-note bend arrives in semitones as sent.
        let message = scale_note_bend(message, rlive_core::MPE_BEND_RANGE);
        let mut starts: [Option<(usize, NoteInfo, bool)>; 16] = [None; 16];
        let mut count = 0;
        let (alloc, voices) = (&mut self.alloc, &mut self.voices);
        alloc.handle(message, |event| match event {
            VoiceEvent::Start {
                voice,
                note,
                stolen,
            } => {
                if count < starts.len() {
                    starts[count] = Some((voice, note, stolen));
                    count += 1;
                }
            }
            VoiceEvent::Release { voice } => {
                let v = &mut voices[voice];
                v.released = true;
                v.noise_env.gate_off();
            }
            VoiceEvent::Kill { voice } => voices[voice].fade.start(None),
            VoiceEvent::Legato { .. } => {}
        });
        for (voice, note, stolen) in starts.into_iter().flatten() {
            if stolen && self.voices[voice].active {
                self.voices[voice].fade.start(Some(note));
            } else {
                self.start(voice, note);
            }
        }
    }
}

impl Processor for Collision {
    fn activate(&mut self, sample_rate: f64, _max_block: usize) {
        self.sample_rate = sample_rate as f32;
        self.alloc.prepare(sample_rate);
        let capacity = (sample_rate as usize / 8).max(64);
        let matrix = &self.matrix;
        self.voices = (0..MAX_VOICES)
            .map(|i| Voice::new(i as u32 + 1, capacity, matrix))
            .collect();
        for v in &mut self.voices {
            v.noise_env.prepare(sample_rate);
            for f in &mut v.filter {
                f.prepare(sample_rate);
            }
            for l in &mut v.lfo {
                l.prepare(sample_rate);
            }
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
            let playing = self.voices.iter().filter(|v| v.active).count();
            ctx.view.publish(VIEW, &[playing as f32]);
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.values.get_mut(id as usize) {
            *v = value;
            self.routes_dirty = true;
            if matches!(id, VOICES | RETRIG) {
                self.sync_settings();
            }
        }
    }

    fn reset(&mut self) {
        self.alloc.reset();
        self.bend = 0.0;
        self.wheel = 0.0;
        for v in &mut self.voices {
            v.silence();
        }
    }

    fn tail(&self) -> Tail {
        let longest = RES_BASE
            .into_iter()
            .map(|base| self.v(base + R_DECAY) * 0.001)
            .fold(0.0f32, f32::max);
        let release = self.v(N_R) * 0.001;
        let seconds = (longest.max(release) * 8.0).min(MAX_DECAY_S * 2.0) + 0.1;
        Tail::Frames((seconds * self.sample_rate) as u32)
    }
}
