//! Tension: a string instrument from physical parts (manual pages 802-813).
//!
//! The string is a digital waveguide: one loop of delay holding the travelling velocity wave,
//! with damping and stiffness filters at the bridge. Exciter and damper act at points along it
//! as scattering junctions, and the pickup reads the same points. This is our own design from the
//! waveguide literature (a bow by Smith's friction junction, hammer and plectrum as contact springs
//! integrated per sample), not a copy of Tension's model. The manual describes behaviours and gives
//! no figures beyond percentages, so the ranges, the body types and resonances, and the MPE
//! destinations are ours.

use super::common::{
    knob, limit, menu, note_hz, scale_note_bend, stretch_semitones, toggle, StealFade, CONTROL,
};
use crate::dsp::envelope::{Envelope, EnvelopeSettings};
use crate::dsp::filter::{Circuit, Filter, FilterKind, FilterSettings, FilterSlope};
use crate::dsp::lfo::{Lfo, LfoRate, LfoSettings, LfoShape};
use crate::dsp::modmatrix::{ModBus, ModMatrix, Route, RouteKind};
use crate::dsp::tempo::{BeatClock, SyncRate};
use crate::dsp::util::{cubic, db_to_gain, sanitize, Rng};
use crate::dsp::voice::{
    GlideCurve, GlideMode, NoteInfo, StealMode, VoiceAllocator, VoiceEvent, VoiceMode,
    VoiceSettings, VoiceState,
};
use crate::dsp::{Biquad, BiquadKind};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    BlockStep, DeviceDesc, ExprKind, Item, MidiMessage, ParamInfo, ProcessContext, Processor,
    Section, Status, Tail, ViewId,
};
use std::f32::consts::{PI, TAU};

macro_rules! ids {
    ($($name:ident),* $(,)?) => { ids!(@ 0u32, $($name,)*); };
    (@ $at:expr, $name:ident, $($rest:ident,)*) => {
        pub const $name: u32 = $at;
        ids!(@ $at + 1, $($rest,)*);
    };
    (@ $at:expr,) => { const PARAMS: usize = $at as usize; };
}

ids!(
    EX_ON,
    EX_TYPE,
    BOW_FORCE,
    BOW_FRICTION,
    HAMMER_MASS,
    HAMMER_STIFF,
    PLECTRUM_PROTRUSION,
    PLECTRUM_STIFF,
    EX_VELOCITY,
    EX_POSITION,
    EX_DAMPING,
    EX_FIX_POS,
    P1_VEL,
    P1_KEY,
    P2_VEL,
    P2_KEY,
    VELOCITY_VEL,
    VELOCITY_KEY,
    POSITION_VEL,
    POSITION_KEY,
    DAMPING_VEL,
    DAMPING_KEY,
    D_ON,
    D_MASS,
    D_STIFF,
    D_VELOCITY,
    D_POSITION,
    D_DAMPING,
    D_GATED,
    D_FIX_POS,
    D_MASS_KEY,
    D_STIFF_KEY,
    D_VELOCITY_KEY,
    D_POSITION_KEY,
    S_DECAY,
    S_DECAY_KEY,
    S_RATIO,
    S_INHARM,
    S_DAMPING,
    S_DAMPING_KEY,
    V_ON,
    V_DELAY,
    V_ATTACK,
    V_RATE,
    V_AMOUNT,
    V_MOD,
    V_ERROR,
    T_ON,
    T_FINGER_MASS,
    T_FINGER_STIFF,
    T_FRET_STIFF,
    T_MASS_VEL,
    T_MASS_KEY,
    P_ON,
    P_POSITION,
    B_ON,
    B_TYPE,
    B_SIZE,
    B_DECAY,
    B_MIX,
    B_LOW_CUT,
    B_HIGH_CUT,
    VOLUME,
    F_ON,
    F_TYPE,
    F_FREQ,
    F_RES,
    F_FREQ_LFO,
    F_FREQ_ENV,
    F_FREQ_KEY,
    F_RES_LFO,
    F_RES_ENV,
    F_RES_KEY,
    FE_ON,
    FE_ATTACK,
    FE_ATTACK_VEL,
    FE_DECAY,
    FE_SUSTAIN,
    FE_SUSTAIN_VEL,
    FE_RELEASE,
    FL_ON,
    FL_WAVE,
    FL_SYNC,
    FL_RATE,
    FL_DIV,
    FL_ATTACK,
    FL_DELAY,
    PRESS_DEST_A,
    PRESS_AMT_A,
    PRESS_DEST_B,
    PRESS_AMT_B,
    SLIDE_DEST_A,
    SLIDE_AMT_A,
    SLIDE_DEST_B,
    SLIDE_AMT_B,
    PITCH_BEND,
    NOTE_PB,
    OCTAVE,
    SEMI,
    DETUNE,
    VOICES,
    STRETCH,
    ERROR,
    PRIORITY,
    PO_ON,
    PO_TIME,
    PO_LEGATO,
    PO_PROP,
    UN_ON,
    UN_VOICES,
    UN_DETUNE,
    UN_DELAY,
);

pub const BOW: usize = 0;
pub const HAMMER: usize = 1;
pub const HAMMER_BOUNCE: usize = 2;
pub const PLECTRUM: usize = 3;

pub const EXCITERS: [&str; 4] = ["Bow", "Hammer", "Hammer (bouncing)", "Plectrum"];
/// The manual says only that the types are "modeled after physical instruments".
pub const BODY_TYPES: [&str; 6] = ["Piano", "Guitar", "Violin", "Viola", "Cello", "Bass"];
const SIZES: [&str; 5] = ["XS", "S", "M", "L", "XL"];
const SIZE_SCALE: [f32; 5] = [1.6, 1.25, 1.0, 0.8, 0.62];
const FILTER_TYPES: [&str; 10] = [
    "LP 2",
    "LP 4",
    "BP 2",
    "BP 4",
    "Notch 2",
    "Notch 4",
    "HP 2",
    "HP 4",
    "Formant 2",
    "Formant 4",
];
const LFO_WAVES: [&str; 5] = ["Sine", "Triangle", "Rectangular", "Random 1", "Random 2"];
const VOICE_COUNTS: [usize; 9] = [1, 2, 4, 6, 8, 12, 16, 24, 32];
const VOICE_LABELS: [&str; 9] = ["1", "2", "4", "6", "8", "12", "16", "24", "32"];

const DEST_LABELS: [&str; 14] = [
    "Off",
    "Exciter P1",
    "Exciter P2",
    "Exciter Velocity",
    "Exciter Position",
    "Damper Mass",
    "Damper Stiffness",
    "String Decay",
    "String Damping",
    "Vibrato Rate",
    "Vibrato Amount",
    "Filter Freq",
    "Filter Res",
    "Volume",
];
const D_P1: usize = 1;
const D_P2: usize = 2;
const D_VELOCITY_DEST: usize = 3;
const D_POSITION_DEST: usize = 4;
const D_DAMPER_MASS: usize = 5;
const D_DAMPER_STIFF: usize = 6;
const D_STRING_DECAY: usize = 7;
const D_STRING_DAMPING: usize = 8;
const D_VIB_RATE: usize = 9;
const D_VIB_AMOUNT: usize = 10;
const D_FILTER_FREQ: usize = 11;
const D_FILTER_RES: usize = 12;
const D_VOLUME: usize = 13;

const SRC_PRESSURE: usize = 0;
const SRC_SLIDE: usize = 1;

const MAX_VOICES: usize = 32;
const STAGES: usize = 4;
const BODY_MODES: usize = 5;

pub const VIEW: ViewId = ViewId(0);
/// `[voices playing, excitation point, damper point, pickup point, the string's motion at 32
/// points, then the pressure, slide and per-note-bend activity]`.
pub const VIEW_LEN: usize = 4 + SHAPE_POINTS + 3;
pub const SHAPE_POINTS: usize = 32;
/// Where in the view the three MPE activity values start.
pub const VIEW_ACTIVITY: usize = 4 + SHAPE_POINTS;

/// The body's resonances for a body type: Hz at Size M and level, ours.
pub const BODY_DATA: [[(f32, f32); BODY_MODES]; 6] = [
    [
        (90.0, 0.6),
        (180.0, 0.8),
        (340.0, 0.6),
        (680.0, 0.4),
        (1400.0, 0.3),
    ],
    [
        (100.0, 0.9),
        (190.0, 0.8),
        (380.0, 0.5),
        (700.0, 0.3),
        (1500.0, 0.2),
    ],
    [
        (275.0, 0.8),
        (460.0, 0.9),
        (700.0, 0.6),
        (2500.0, 0.5),
        (3500.0, 0.3),
    ],
    [
        (230.0, 0.8),
        (390.0, 0.9),
        (600.0, 0.6),
        (2000.0, 0.5),
        (3000.0, 0.3),
    ],
    [
        (110.0, 0.8),
        (190.0, 0.9),
        (290.0, 0.6),
        (1000.0, 0.4),
        (2000.0, 0.3),
    ],
    [
        (60.0, 0.9),
        (110.0, 0.8),
        (200.0, 0.6),
        (500.0, 0.3),
        (1000.0, 0.2),
    ],
];

/// Formants of five vowels, F1 to F3.
const VOWELS: [[f32; 3]; 5] = [
    [800.0, 1150.0, 2900.0],
    [400.0, 1600.0, 2700.0],
    [350.0, 2000.0, 2800.0],
    [450.0, 800.0, 2830.0],
    [325.0, 700.0, 2700.0],
];

fn params() -> Vec<ParamInfo> {
    let pct = |id, name: &str, d| ParamInfo::continuous(id, name, 0.0, 100.0, d, "%");
    let bi = |id, name: &str, d| ParamInfo::continuous(id, name, -100.0, 100.0, d, "%");
    let ms = |id, name: &str, min, max, d| ParamInfo::skewed(id, name, min, max, d, "ms", 3.0);
    let dest = |id, name: &str, d| ParamInfo::stepped(id, name, &DEST_LABELS, d);
    vec![
        ParamInfo::toggle(EX_ON, "Exciter", true),
        ParamInfo::stepped(EX_TYPE, "Exciter Type", &EXCITERS, HAMMER),
        pct(BOW_FORCE, "Force", 40.0),
        pct(BOW_FRICTION, "Friction", 50.0),
        pct(HAMMER_MASS, "Mass", 40.0),
        pct(HAMMER_STIFF, "Hammer Stiffness", 50.0),
        pct(PLECTRUM_PROTRUSION, "Protrusion", 50.0),
        pct(PLECTRUM_STIFF, "Plectrum Stiffness", 50.0),
        pct(EX_VELOCITY, "Velocity", 60.0),
        pct(EX_POSITION, "Position", 14.0),
        pct(EX_DAMPING, "Damping", 30.0),
        ParamInfo::toggle(EX_FIX_POS, "Fix. Pos", false),
        bi(P1_VEL, "P1 Vel", 0.0),
        bi(P1_KEY, "P1 Key", 0.0),
        bi(P2_VEL, "P2 Vel", 0.0),
        bi(P2_KEY, "P2 Key", 0.0),
        bi(VELOCITY_VEL, "Velocity Vel", 80.0),
        bi(VELOCITY_KEY, "Velocity Key", 0.0),
        bi(POSITION_VEL, "Position Vel", 0.0),
        bi(POSITION_KEY, "Position Key", 0.0),
        bi(DAMPING_VEL, "Damping Vel", 0.0),
        bi(DAMPING_KEY, "Damping Key", 0.0),
        ParamInfo::toggle(D_ON, "Damper", true),
        pct(D_MASS, "Damper Mass", 50.0),
        pct(D_STIFF, "Damper Stiffness", 30.0),
        pct(D_VELOCITY, "Damper Velocity", 20.0),
        pct(D_POSITION, "Damper Position", 25.0),
        pct(D_DAMPING, "Damper Damping", 30.0),
        ParamInfo::toggle(D_GATED, "Gated", true),
        ParamInfo::toggle(D_FIX_POS, "Damper Fix. Pos", false),
        bi(D_MASS_KEY, "Damper Mass Key", 0.0),
        bi(D_STIFF_KEY, "Damper Stiffness Key", 0.0),
        bi(D_VELOCITY_KEY, "Damper Velocity Key", 0.0),
        bi(D_POSITION_KEY, "Damper Position Key", 0.0),
        ParamInfo::skewed(S_DECAY, "Decay", 10.0, 30_000.0, 3_000.0, "ms", 4.0),
        bi(S_DECAY_KEY, "Decay Key", -30.0),
        pct(S_RATIO, "Ratio", 70.0),
        pct(S_INHARM, "Inharm", 5.0),
        pct(S_DAMPING, "String Damping", 70.0),
        bi(S_DAMPING_KEY, "String Damping Key", 0.0),
        ParamInfo::toggle(V_ON, "Vibrato", false),
        ms(V_DELAY, "Vibrato Delay", 0.0, 5_000.0, 0.0),
        ms(V_ATTACK, "Vibrato Attack", 1.0, 5_000.0, 400.0),
        ParamInfo::skewed(V_RATE, "Vibrato Rate", 0.5, 20.0, 5.5, "Hz", 2.0),
        pct(V_AMOUNT, "Vibrato Amount", 30.0),
        pct(V_MOD, "< Mod", 0.0),
        pct(V_ERROR, "Vibrato Error", 0.0),
        ParamInfo::toggle(T_ON, "Termination", false),
        pct(T_FINGER_MASS, "Finger Mass", 50.0),
        pct(T_FINGER_STIFF, "Finger Stiff", 50.0),
        pct(T_FRET_STIFF, "Fret Stiff", 50.0),
        bi(T_MASS_VEL, "Finger Mass Vel", 0.0),
        bi(T_MASS_KEY, "Finger Mass Key", 0.0),
        ParamInfo::toggle(P_ON, "Pickup", false),
        pct(P_POSITION, "Pickup Position", 25.0),
        ParamInfo::toggle(B_ON, "Body", true),
        ParamInfo::stepped(B_TYPE, "Body Type", &BODY_TYPES, 1),
        ParamInfo::stepped(B_SIZE, "Body Size", &SIZES, 2),
        pct(B_DECAY, "Body Decay", 40.0),
        pct(B_MIX, "Str/Body", 40.0),
        ParamInfo::skewed(B_LOW_CUT, "Low Cut", 20.0, 2_000.0, 40.0, "Hz", 3.0),
        ParamInfo::skewed(B_HIGH_CUT, "High Cut", 500.0, 20_000.0, 12_000.0, "Hz", 3.0),
        ParamInfo::continuous(VOLUME, "Volume", -48.0, 12.0, -6.0, "dB"),
        ParamInfo::toggle(F_ON, "Filter", false),
        ParamInfo::stepped(F_TYPE, "Filter Type", &FILTER_TYPES, 1),
        ParamInfo::skewed(F_FREQ, "Freq", 20.0, 20_000.0, 2_000.0, "Hz", 3.0),
        pct(F_RES, "Res", 20.0),
        bi(F_FREQ_LFO, "Freq LFO", 0.0),
        bi(F_FREQ_ENV, "Freq Env", 0.0),
        bi(F_FREQ_KEY, "Freq Key", 0.0),
        bi(F_RES_LFO, "Res LFO", 0.0),
        bi(F_RES_ENV, "Res Env", 0.0),
        bi(F_RES_KEY, "Res Key", 0.0),
        ParamInfo::toggle(FE_ON, "Filter Envelope", false),
        ms(FE_ATTACK, "Attack", 0.5, 10_000.0, 5.0),
        pct(FE_ATTACK_VEL, "Attack Vel", 0.0),
        ms(FE_DECAY, "Filter Decay", 1.0, 20_000.0, 300.0),
        pct(FE_SUSTAIN, "Sustain", 30.0),
        pct(FE_SUSTAIN_VEL, "Sustain Vel", 0.0),
        ms(FE_RELEASE, "Release", 1.0, 20_000.0, 200.0),
        ParamInfo::toggle(FL_ON, "Filter LFO", false),
        ParamInfo::stepped(FL_WAVE, "LFO Wave", &LFO_WAVES, 0),
        ParamInfo::toggle(FL_SYNC, "LFO Sync", false),
        ParamInfo::skewed(FL_RATE, "LFO Rate", 0.01, 60.0, 2.0, "Hz", 3.0),
        ParamInfo::stepped(FL_DIV, "LFO Beat Rate", &SyncRate::LABELS, 12),
        ms(FL_ATTACK, "LFO Attack", 1.0, 10_000.0, 1.0),
        ms(FL_DELAY, "LFO Delay", 0.0, 10_000.0, 0.0),
        dest(PRESS_DEST_A, "Pressure Destination A", 0),
        bi(PRESS_AMT_A, "Pressure Amount A", 0.0),
        dest(PRESS_DEST_B, "Pressure Destination B", 0),
        bi(PRESS_AMT_B, "Pressure Amount B", 0.0),
        dest(SLIDE_DEST_A, "Slide Destination A", 0),
        bi(SLIDE_AMT_A, "Slide Amount A", 0.0),
        dest(SLIDE_DEST_B, "Slide Destination B", 0),
        bi(SLIDE_AMT_B, "Slide Amount B", 0.0),
        ParamInfo::continuous(PITCH_BEND, "Pitch Bend", 0.0, 24.0, 2.0, "st"),
        ParamInfo::continuous(NOTE_PB, "Note PB", 0.0, 48.0, 48.0, "st"),
        ParamInfo::continuous(OCTAVE, "Octave", -3.0, 3.0, 0.0, "oct"),
        ParamInfo::continuous(SEMI, "Semi", -12.0, 12.0, 0.0, "st"),
        ParamInfo::continuous(DETUNE, "Detune", -50.0, 50.0, 0.0, "ct"),
        ParamInfo::stepped(VOICES, "Voices", &VOICE_LABELS, 4),
        bi(STRETCH, "Stretch", 0.0),
        pct(ERROR, "Error", 0.0),
        ParamInfo::stepped(PRIORITY, "Priority", &["High", "Low", "Last"], 2),
        ParamInfo::toggle(PO_ON, "Portamento", false),
        ms(PO_TIME, "Time", 1.0, 5_000.0, 150.0),
        ParamInfo::toggle(PO_LEGATO, "Legato", false),
        ParamInfo::toggle(PO_PROP, "Prop.", false),
        ParamInfo::toggle(UN_ON, "Unison", false),
        ParamInfo::stepped(UN_VOICES, "Unison Voices", &["2", "4"], 0),
        pct(UN_DETUNE, "Unison Detune", 20.0),
        ms(UN_DELAY, "Unison Delay", 0.0, 200.0, 0.0),
    ]
}

fn layout() -> Vec<Section> {
    let knobs = |ids: &[u32]| ids.iter().map(|&id| knob(id)).collect::<Vec<_>>();
    let section = |title, first: Vec<Item>, rest: &[u32]| {
        let mut items = first;
        items.extend(knobs(rest));
        Section { title, items }
    };
    vec![
        section(
            "Exciter",
            vec![
                Item::Panel("tension_display"),
                toggle(EX_ON),
                menu(EX_TYPE),
                toggle(EX_FIX_POS),
            ],
            &[
                BOW_FORCE,
                BOW_FRICTION,
                HAMMER_MASS,
                HAMMER_STIFF,
                PLECTRUM_PROTRUSION,
                PLECTRUM_STIFF,
                EX_VELOCITY,
                EX_POSITION,
                EX_DAMPING,
                P1_VEL,
                P1_KEY,
                P2_VEL,
                P2_KEY,
                VELOCITY_VEL,
                VELOCITY_KEY,
                POSITION_VEL,
                POSITION_KEY,
                DAMPING_VEL,
                DAMPING_KEY,
            ],
        ),
        section(
            "Damper",
            vec![toggle(D_ON), toggle(D_GATED), toggle(D_FIX_POS)],
            &[
                D_MASS,
                D_STIFF,
                D_VELOCITY,
                D_POSITION,
                D_DAMPING,
                D_MASS_KEY,
                D_STIFF_KEY,
                D_VELOCITY_KEY,
                D_POSITION_KEY,
            ],
        ),
        section(
            "String",
            vec![],
            &[
                S_DECAY,
                S_DECAY_KEY,
                S_RATIO,
                S_INHARM,
                S_DAMPING,
                S_DAMPING_KEY,
            ],
        ),
        section(
            "Vibrato",
            vec![toggle(V_ON)],
            &[V_DELAY, V_ATTACK, V_RATE, V_AMOUNT, V_MOD, V_ERROR],
        ),
        section(
            "Termination",
            vec![toggle(T_ON)],
            &[
                T_FINGER_MASS,
                T_FINGER_STIFF,
                T_FRET_STIFF,
                T_MASS_VEL,
                T_MASS_KEY,
            ],
        ),
        section("Pickup", vec![toggle(P_ON)], &[P_POSITION]),
        section(
            "Body",
            vec![toggle(B_ON), menu(B_TYPE), menu(B_SIZE)],
            &[B_DECAY, B_MIX, B_LOW_CUT, B_HIGH_CUT, VOLUME],
        ),
        section(
            "Filter",
            vec![toggle(F_ON), menu(F_TYPE)],
            &[
                F_FREQ, F_RES, F_FREQ_LFO, F_FREQ_ENV, F_FREQ_KEY, F_RES_LFO, F_RES_ENV, F_RES_KEY,
            ],
        ),
        section(
            "Filter Envelope",
            vec![toggle(FE_ON)],
            &[
                FE_ATTACK,
                FE_ATTACK_VEL,
                FE_DECAY,
                FE_SUSTAIN,
                FE_SUSTAIN_VEL,
                FE_RELEASE,
            ],
        ),
        section(
            "Filter LFO",
            vec![toggle(FL_ON), menu(FL_WAVE), toggle(FL_SYNC), menu(FL_DIV)],
            &[FL_RATE, FL_ATTACK, FL_DELAY],
        ),
        section(
            "MPE",
            vec![
                menu(PRESS_DEST_A),
                knob(PRESS_AMT_A),
                menu(PRESS_DEST_B),
                knob(PRESS_AMT_B),
                menu(SLIDE_DEST_A),
                knob(SLIDE_AMT_A),
                menu(SLIDE_DEST_B),
                knob(SLIDE_AMT_B),
            ],
            &[PITCH_BEND, NOTE_PB],
        ),
        section(
            "Keyboard",
            vec![menu(VOICES), menu(PRIORITY)],
            &[OCTAVE, SEMI, DETUNE, STRETCH, ERROR],
        ),
        section(
            "Portamento",
            vec![toggle(PO_ON), toggle(PO_LEGATO), toggle(PO_PROP)],
            &[PO_TIME],
        ),
        section(
            "Unison",
            vec![toggle(UN_ON), menu(UN_VOICES)],
            &[UN_DETUNE, UN_DELAY],
        ),
    ]
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.tension",
    name: "Tension",
    role: DeviceRole::Instrument,
    category: "Physical Models",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::new(Tension::new()),
};

// ---------------------------------------------------------------------------------- string

/// A bow's reflection from the velocity difference across its hair. Smith's friction curve:
/// stuck near zero difference and slipping away from it.
#[inline]
pub fn bow_table(delta: f32, slope: f32, grip: f32) -> f32 {
    (((delta * slope).abs() + grip).powi(-4)).min(1.0)
}

/// The loop's filters' delay and gain at the fundamental, from which the delay is tuned.
#[derive(Clone, Copy, Default)]
struct LoopFilters {
    lowpass: f32,
    allpass: f32,
}

impl LoopFilters {
    /// Delay in samples at `w` radians per sample, and gain.
    fn at(&self, w: f32) -> (f32, f32) {
        let c = self.lowpass;
        let delay_lp = if c.abs() < 1e-5 {
            0.0
        } else {
            (c * w.sin()).atan2(1.0 - c * w.cos()) / w
        };
        let gain_lp = (1.0 - c) / (1.0 - 2.0 * c * w.cos() + c * c).max(1e-9).sqrt();
        let a = self.allpass;
        let delay_ap = if a.abs() < 1e-6 {
            0.0
        } else {
            STAGES as f32 * (1.0 - 2.0 * (a * w.sin()).atan2(1.0 + a * w.cos()) / w)
        };
        (delay_lp + delay_ap, gain_lp)
    }
}

/// The waveguide loop. A wave written at age 0 leaves the bridge; age `t` is `t` samples of travel,
/// out to the nut at half the period and back to the bridge at the period.
#[derive(Clone, Debug)]
struct Waveguide {
    buffer: Box<[f32]>,
    mask: usize,
    write: usize,
    period: f32,
    gain: f32,
    lowpass: f32,
    lowpass_state: f32,
    allpass: f32,
    ap_x: [f32; STAGES],
    ap_y: [f32; STAGES],
    buzz_threshold: f32,
    buzz_amount: f32,
}

impl Waveguide {
    fn new(capacity: usize) -> Self {
        let size = capacity.next_power_of_two();
        Self {
            buffer: vec![0.0; size].into_boxed_slice(),
            mask: size - 1,
            write: 0,
            period: 100.0,
            gain: 0.0,
            lowpass: 0.0,
            lowpass_state: 0.0,
            allpass: 0.0,
            ap_x: [0.0; STAGES],
            ap_y: [0.0; STAGES],
            buzz_threshold: f32::MAX,
            buzz_amount: 0.0,
        }
    }

    fn clear(&mut self) {
        self.buffer.fill(0.0);
        self.lowpass_state = 0.0;
        self.ap_x = [0.0; STAGES];
        self.ap_y = [0.0; STAGES];
    }

    #[inline]
    fn at(&self, age: usize) -> f32 {
        self.buffer[self.write.wrapping_sub(age) & self.mask]
    }

    #[inline]
    fn add(&mut self, age: usize, x: f32) {
        let i = self.write.wrapping_sub(age) & self.mask;
        self.buffer[i] += x;
    }

    /// The wave arriving back at the bridge.
    #[inline]
    fn read(&self) -> f32 {
        let i = self.period as usize;
        let t = self.period - i as f32;
        cubic(
            self.at(i - 1),
            self.at(i),
            self.at(i + 1),
            self.at(i + 2),
            t,
        )
    }

    /// The velocity at a point `age` samples of travel from the bridge.
    #[inline]
    fn velocity_at(&self, age: usize) -> f32 {
        let far = (self.period as usize).saturating_sub(age).max(1);
        self.at(age) - self.at(far)
    }

    /// Adds velocity to both directions at the point `age`.
    #[inline]
    fn inject(&mut self, age: usize, f: f32) {
        let far = (self.period as usize).saturating_sub(age).max(1);
        self.add(age, f);
        self.add(far, -f);
    }

    /// Filters the arriving wave and writes it back into the loop.
    #[inline]
    fn write_back(&mut self, y: f32) {
        self.lowpass_state = sanitize((1.0 - self.lowpass) * y + self.lowpass * self.lowpass_state);
        let mut x = self.lowpass_state;
        if self.allpass != 0.0 {
            for s in 0..STAGES {
                let out = self.allpass * x + self.ap_x[s] - self.allpass * self.ap_y[s];
                self.ap_x[s] = x;
                self.ap_y[s] = sanitize(out);
                x = out;
            }
        }
        if x.abs() > self.buzz_threshold {
            x -= (x.abs() - self.buzz_threshold) * self.buzz_amount * x.signum();
        }
        let i = self.write & self.mask;
        self.buffer[i] = (x * self.gain).clamp(-8.0, 8.0);
        self.write = self.write.wrapping_add(1);
    }
}

/// An exciter's state, in units of string displacement and velocity per sample.
#[derive(Clone, Copy, Debug, Default)]
struct Exciter {
    kind: usize,
    active: bool,
    age: usize,
    // Hammer.
    hammer_y: f32,
    hammer_v: f32,
    string_y: f32,
    stiffness: f32,
    mass: f32,
    damping: f32,
    gravity: f32,
    touching: bool,
    ttl: u32,
    // Plectrum.
    pick_y: f32,
    pick_v: f32,
    pick_limit: f32,
    // Bow.
    bow_target: f32,
    bow_speed: f32,
    bow_slope: f32,
    bow_grip: f32,
}

impl Exciter {
    /// The velocity to inject at the contact point, given the wave velocity there.
    #[inline]
    fn tick(&mut self, v: f32) -> f32 {
        if !self.active {
            return 0.0;
        }
        match self.kind {
            BOW => {
                self.bow_speed += (self.bow_target - self.bow_speed) * 0.01;
                let delta = self.bow_speed - v;
                delta * bow_table(delta, self.bow_slope, self.bow_grip)
            }
            HAMMER | HAMMER_BOUNCE => {
                let compression = self.hammer_y - self.string_y;
                let mut force = 0.0;
                if compression > 0.0 {
                    force = self.stiffness * compression * compression.sqrt();
                    force = (force + self.damping * (self.hammer_v - v) * compression.min(1.0))
                        .max(0.0);
                    force = force.min(2.0);
                    self.touching = true;
                } else if self.touching && self.kind == HAMMER {
                    self.active = false;
                    return 0.0;
                }
                self.hammer_v += self.gravity - force / self.mass;
                self.hammer_y += self.hammer_v;
                if self.kind == HAMMER_BOUNCE {
                    self.ttl = self.ttl.saturating_sub(1);
                    if self.ttl == 0 {
                        self.active = false;
                    }
                }
                let f = force * 0.5;
                self.string_y += v + f;
                f
            }
            _ => {
                self.pick_y += self.pick_v;
                let force = self.stiffness * (self.pick_y - self.string_y);
                if force.abs() > self.pick_limit {
                    self.active = false;
                    return 0.0;
                }
                let f = force * 0.5;
                self.string_y += v + f;
                f
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Damper {
    enabled: bool,
    gated: bool,
    contact: f32,
    target: f32,
    speed: f32,
    resistance: f32,
    hit_left: u32,
    hit_gain: f32,
    age: usize,
    bounce_phase: f32,
    bounce_step: f32,
    duty: f32,
}

impl Damper {
    #[inline]
    fn tick(&mut self, v: f32) -> f32 {
        if !self.enabled {
            return 0.0;
        }
        self.contact += (self.target - self.contact).clamp(-self.speed, self.speed);
        let mut f = 0.0;
        if self.contact > 1e-5 {
            self.bounce_phase = (self.bounce_phase + self.bounce_step).fract();
            if self.bounce_phase < self.duty {
                let r = self.resistance * self.contact;
                f = -v * r / (2.0 + r);
            }
        }
        if self.hit_left > 0 {
            self.hit_left -= 1;
            f += self.hit_gain;
        }
        f
    }
}

#[derive(Clone)]
struct Voice {
    active: bool,
    note: NoteInfo,
    key: f32,
    velocity: f32,
    released: bool,
    delay_left: u32,
    waveguide: Waveguide,
    exciter: Exciter,
    damper: Damper,
    pickup_age: usize,
    pickup_on: bool,
    period_ref: f32,
    dc: f32,
    filter: [Filter; 3],
    filter_gain: f32,
    filter_on: bool,
    filter_env: Envelope,
    filter_lfo: Lfo,
    filter_lfo_time: f32,
    vibrato: Lfo,
    vibrato_time: f32,
    vib_rate: f32,
    vib_amount: f32,
    vib_delay: f32,
    vib_attack: f32,
    tune_error: f32,
    bus: ModBus,
    rng: Rng,
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
            delay_left: 0,
            waveguide: Waveguide::new(capacity),
            exciter: Exciter::default(),
            damper: Damper::default(),
            pickup_age: 1,
            pickup_on: false,
            period_ref: 100.0,
            dc: 0.0,
            filter: [Filter::new(); 3],
            filter_gain: 1.0,
            filter_on: false,
            filter_env: Envelope::new(),
            filter_lfo: Lfo::new(seed.wrapping_mul(7)),
            filter_lfo_time: 0.0,
            vibrato: Lfo::new(seed.wrapping_mul(13)),
            vibrato_time: 0.0,
            vib_rate: 5.0,
            vib_amount: 0.0,
            vib_delay: 0.0,
            vib_attack: 0.0,
            tune_error: 0.0,
            bus: matrix.bus(),
            rng: Rng::new(seed),
            fade: StealFade::default(),
            quiet: 0,
        }
    }

    fn silence(&mut self) {
        self.active = false;
        self.waveguide.clear();
        self.exciter = Exciter::default();
        self.damper = Damper::default();
        self.filter_env.reset();
        for f in &mut self.filter {
            f.reset();
        }
        self.dc = 0.0;
        self.fade.clear();
    }
}

/// Tension. See the [module docs](self).
pub struct Tension {
    values: [f64; PARAMS],
    sample_rate: f32,
    alloc: VoiceAllocator,
    voices: Vec<Voice>,
    matrix: ModMatrix,
    routes_dirty: bool,
    wheel: f32,
    clock: BeatClock,
    body: [Biquad; BODY_MODES],
    body_gains: [f32; BODY_MODES],
    body_cuts: [Biquad; 2],
    body_key: Option<(usize, usize, u32)>,
    scratch: Vec<f32>,
    shape_at: [f32; 3],
    note_bend_seen: f32,
}

impl Default for Tension {
    fn default() -> Self {
        Self::new()
    }
}

impl Tension {
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
            matrix: ModMatrix::new(2, DEST_LABELS.len(), 4),
            routes_dirty: true,
            wheel: 0.0,
            clock: BeatClock::default(),
            body: [Biquad::default(); BODY_MODES],
            body_gains: [0.0; BODY_MODES],
            body_cuts: [Biquad::default(); 2],
            body_key: None,
            scratch: Vec::new(),
            shape_at: [0.0; 3],
            note_bend_seen: 0.0,
        }
    }

    fn v(&self, id: u32) -> f32 {
        self.values[id as usize] as f32
    }

    fn pc(&self, id: u32) -> f32 {
        self.v(id) * 0.01
    }

    fn sync_settings(&mut self) {
        let voices = VOICE_COUNTS[(self.v(VOICES).round() as usize).min(VOICE_COUNTS.len() - 1)];
        let portamento = self.v(PO_ON) >= 0.5;
        let legato = self.v(PO_LEGATO) >= 0.5;
        let unison = if self.v(UN_ON) >= 0.5 {
            2 << (self.v(UN_VOICES).round() as usize).min(1)
        } else {
            1
        };
        self.alloc.set(&VoiceSettings {
            mode: if voices == 1 && portamento && legato {
                VoiceMode::Legato
            } else {
                VoiceMode::Poly
            },
            polyphony: voices,
            unison,
            steal: match self.v(PRIORITY).round() as usize {
                0 => StealMode::Lowest,
                1 => StealMode::Highest,
                _ => StealMode::Oldest,
            },
            glide: match (portamento, legato) {
                (false, _) => GlideMode::Off,
                (true, false) => GlideMode::Always,
                (true, true) => GlideMode::LegatoOnly,
            },
            glide_curve: if self.v(PO_PROP) >= 0.5 {
                GlideCurve::ConstantRate
            } else {
                GlideCurve::ConstantTime
            },
            glide_time: self.v(PO_TIME) * 0.001,
            bend_range: self.v(PITCH_BEND),
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
        self.matrix.set_route(
            0,
            route(SRC_PRESSURE, self.v(PRESS_DEST_A), self.v(PRESS_AMT_A)),
        );
        self.matrix.set_route(
            1,
            route(SRC_PRESSURE, self.v(PRESS_DEST_B), self.v(PRESS_AMT_B)),
        );
        self.matrix.set_route(
            2,
            route(SRC_SLIDE, self.v(SLIDE_DEST_A), self.v(SLIDE_AMT_A)),
        );
        self.matrix.set_route(
            3,
            route(SRC_SLIDE, self.v(SLIDE_DEST_B), self.v(SLIDE_AMT_B)),
        );
    }

    fn sync_body(&mut self) {
        let sr = self.sample_rate;
        let kind = (self.v(B_TYPE).round() as usize).min(BODY_DATA.len() - 1);
        let size = (self.v(B_SIZE).round() as usize).min(SIZE_SCALE.len() - 1);
        let decay = self.v(B_DECAY).to_bits();
        let key = Some((
            kind,
            size,
            decay ^ self.v(B_LOW_CUT).to_bits() ^ self.v(B_HIGH_CUT).to_bits(),
        ));
        if self.body_key == key {
            return;
        }
        self.body_key = key;
        // Longer decays are narrower resonances: Q is pi f t60 / ln(1000).
        let t60 = 0.01 * 2f32.powf(self.pc(B_DECAY) * 7.0);
        for (i, (hz, gain)) in BODY_DATA[kind].iter().enumerate() {
            let f = (hz * SIZE_SCALE[size]).min(sr * 0.45);
            let q = (PI * f * t60 / 6.9).clamp(0.5, 200.0);
            self.body[i].set(BiquadKind::BandPass, f, q, 0.0, sr);
            self.body_gains[i] = *gain;
        }
        self.body_cuts[0].set(BiquadKind::HighPass, self.v(B_LOW_CUT), 0.7, 0.0, sr);
        self.body_cuts[1].set(
            BiquadKind::LowPass,
            self.v(B_HIGH_CUT).min(sr * 0.45),
            0.7,
            0.0,
            sr,
        );
    }

    fn start(&mut self, i: usize, note: NoteInfo) {
        let sr = self.sample_rate;
        let scale = 48_000.0 / sr;
        let key = (note.key as f32 - 60.0) / 60.0;
        let velocity = note.velocity;
        let values = self.values;
        let g = |id: u32| values[id as usize] as f32;
        let pc = |id: u32| values[id as usize] as f32 * 0.01;
        let exciter_on = g(EX_ON) >= 0.5;
        let kind = (g(EX_TYPE).round() as usize).min(3);
        // Each knob takes velocity and key in the same way.
        let modded = |base: f32, vel: u32, k: u32| {
            (base * (1.0 + pc(vel) * (velocity - 1.0)) * (1.0 + pc(k) * key)).clamp(0.0, 1.0)
        };
        let (p1, p2) = match kind {
            BOW => (pc(BOW_FORCE), pc(BOW_FRICTION)),
            PLECTRUM => (pc(PLECTRUM_PROTRUSION), pc(PLECTRUM_STIFF)),
            _ => (pc(HAMMER_MASS), pc(HAMMER_STIFF)),
        };
        let p1 = modded(p1, P1_VEL, P1_KEY);
        let p2 = modded(p2, P2_VEL, P2_KEY);
        let speed = modded(pc(EX_VELOCITY), VELOCITY_VEL, VELOCITY_KEY);
        let damping = modded(pc(EX_DAMPING), DAMPING_VEL, DAMPING_KEY);

        let voice = &mut self.voices[i];
        voice.silence();
        voice.active = true;
        voice.note = note;
        voice.key = key;
        voice.velocity = velocity;
        voice.released = false;
        voice.quiet = 0;
        voice.tune_error = voice.rng.bipolar() * pc(ERROR) * 0.5;
        voice.delay_left = (note.unison_index as f32 * g(UN_DELAY) * 0.001 * sr) as u32;

        let error = pc(V_ERROR);
        let mut wobble = || 1.0 + voice.rng.bipolar() * error * 0.5;
        voice.vib_rate = g(V_RATE) * wobble();
        voice.vib_amount = pc(V_AMOUNT) * wobble();
        voice.vib_delay = g(V_DELAY) * 0.001 * wobble();
        voice.vib_attack = (g(V_ATTACK) * 0.001 * wobble()).max(1e-3);
        voice.vibrato_time = 0.0;
        voice.vibrato.retrigger();
        voice.filter_lfo_time = 0.0;
        voice.filter_lfo.retrigger();
        voice.filter_env.gate_on();

        let mut e = Exciter {
            kind,
            active: exciter_on,
            ..Default::default()
        };
        let k_scale = scale * scale;
        match kind {
            BOW => {
                e.bow_target = speed * 0.25 * scale;
                e.bow_slope = 5.0 - 4.0 * p1;
                e.bow_grip = 0.9 - 0.4 * p2;
            }
            PLECTRUM => {
                e.stiffness = (0.02 * 2f32.powf(p2 * 6.0) * k_scale).min(1.5);
                e.pick_v = (0.002 + 0.03 * speed) * scale;
                e.pick_limit = 0.02 + 0.5 * p1 * (0.3 + p2);
            }
            _ => {
                e.mass = 0.2 * 2f32.powf(p1 * 6.0);
                let mut stiff = 0.002 * 2f32.powf(p2 * 9.0);
                if kind == HAMMER_BOUNCE {
                    // Damping is the spring between the hammer and what drives it: stiffer, shorter.
                    stiff *= 1.0 + 4.0 * damping;
                } else {
                    e.damping = damping * 0.3 * scale;
                }
                e.stiffness = (stiff * k_scale).min(1.5);
                e.hammer_v = speed * 0.4 * scale;
                e.hammer_y = -e.hammer_v * 4.0;
                if kind == HAMMER_BOUNCE {
                    e.gravity = 2.0 * e.hammer_v / (0.035 * sr);
                    e.ttl = (0.5 * sr) as u32;
                }
            }
        }
        voice.exciter = e;

        let damper_on = g(D_ON) >= 0.5;
        let gated = g(D_GATED) >= 0.5;
        voice.damper = Damper {
            enabled: damper_on,
            gated,
            contact: if damper_on { 1.0 } else { 0.0 },
            target: if gated { 0.0 } else { 1.0 },
            ..Default::default()
        };
        if damper_on && gated {
            // Lifting the damper off the string nudges it, which is all the sound there is with
            // the Exciter off.
            voice.damper.hit_left = (0.001 * sr) as u32;
            voice.damper.hit_gain = pc(D_VELOCITY) * 0.02 * (0.3 + pc(D_MASS)) * scale;
        }
        if !exciter_on && !damper_on {
            voice.exciter.active = false;
        }
    }

    fn release(&mut self, i: usize) {
        let g = |id: u32| self.values[id as usize] as f32;
        let sr = self.sample_rate;
        let scale = 48_000.0 / sr;
        let velocity_amount =
            (g(D_VELOCITY) * 0.01 * (1.0 + g(D_VELOCITY_KEY) * 0.01 * self.voices[i].key))
                .clamp(0.0, 1.0);
        let mass = g(D_MASS) * 0.01;
        let voice = &mut self.voices[i];
        voice.released = true;
        voice.filter_env.gate_off();
        if voice.exciter.kind == BOW {
            voice.exciter.bow_target = 0.0;
        } else if voice.exciter.kind == PLECTRUM {
            voice.exciter.active = false;
        }
        if voice.damper.enabled && voice.damper.gated {
            // The harder the damper comes down, the louder the thump.
            voice.damper.target = 1.0;
            voice.damper.hit_left = (0.0015 * sr) as u32;
            voice.damper.hit_gain = -velocity_amount.powi(2) * (0.3 + mass) * 0.3 * scale;
        }
    }

    /// The control-rate work for one voice.
    fn control(&mut self, i: usize, frames: usize) {
        let sr = self.sample_rate;
        let scale = 48_000.0 / sr;
        let alloc_voice = *self.alloc.voice(i);
        let channel_pressure = self.alloc.channel_pressure(i);
        let sources = [
            alloc_voice.pressure.max(channel_pressure),
            alloc_voice.slide,
        ];
        {
            let (matrix, voices) = (&self.matrix, &mut self.voices);
            matrix.evaluate(&sources, &mut voices[i].bus);
        }
        let values = self.values;
        let g = |id: u32| values[id as usize] as f32;
        let pc = |id: u32| values[id as usize] as f32 * 0.01;
        let pitch_played = self.alloc.pitch(i);
        let clock = self.clock;
        let wheel = self.wheel;

        let voice = &mut self.voices[i];
        let key = voice.key;
        let velocity = voice.velocity;

        // Vibrato: delay, then a ramp up to Amount, the wheel scaling it.
        let mut vibrato = 0.0;
        if g(V_ON) >= 0.5 {
            let rate = voice.vib_rate * 2f32.powf(voice.bus.offset(D_VIB_RATE) * 2.0);
            voice.vibrato.set(&LfoSettings {
                shape: LfoShape::Sine,
                rate: LfoRate::Hz(rate),
                ..Default::default()
            });
            let wave = voice.vibrato.advance(frames)[0];
            let elapsed = voice.vibrato_time - voice.vib_delay;
            let ramp = (elapsed / voice.vib_attack).clamp(0.0, 1.0);
            let wheel_gain = 1.0 - pc(V_MOD) * (1.0 - wheel);
            vibrato = wave
                * ramp
                * (voice.vib_amount + voice.bus.offset(D_VIB_AMOUNT)).max(0.0)
                * wheel_gain
                * 0.5;
            voice.vibrato_time += frames as f32 / sr;
        }

        // Pitch and the loop.
        let unison = if g(UN_ON) >= 0.5 {
            voice.note.spread * pc(UN_DETUNE) * 0.5
        } else {
            0.0
        };
        let termination = g(T_ON) >= 0.5;
        let mut finger = 0.0;
        if termination {
            finger = (pc(T_FINGER_MASS)
                * (1.0 + pc(T_MASS_VEL) * (velocity - 1.0))
                * (1.0 + pc(T_MASS_KEY) * key))
                .clamp(0.0, 1.0);
        }
        let pitch = pitch_played
            + g(OCTAVE) * 12.0
            + g(SEMI)
            + g(DETUNE) * 0.01
            + voice.tune_error
            + stretch_semitones(pitch_played, g(STRETCH))
            + vibrato
            + unison
            // Pressing the string down onto the fret raises the pitch a little.
            + finger * pc(T_FRET_STIFF) * 0.08;
        let hz = note_hz(pitch).clamp(20.0, sr * 0.2);
        let w = TAU * hz / sr;
        let target_period = sr / hz;

        let damp = (pc(S_DAMPING) + pc(S_DAMPING_KEY) * key + voice.bus.offset(D_STRING_DAMPING))
            .clamp(0.0, 1.0);
        let mut lowpass = 0.92 * (1.0 - damp).powi(2);
        let mut extra_loss = 1.0;
        if termination {
            // A soft finger or fret soaks up the top of the spectrum at the end of the string.
            let softness = 1.0 - 0.5 * (pc(T_FINGER_STIFF) + pc(T_FRET_STIFF));
            lowpass = lowpass.max(0.5 * softness);
            extra_loss = 1.0 - 0.003 * softness;
            voice.waveguide.buzz_threshold = 0.05 + 2.0 * finger;
            voice.waveguide.buzz_amount = 0.5;
        } else {
            voice.waveguide.buzz_threshold = f32::MAX;
        }

        let inharm = pc(S_INHARM);
        // Only poles close to z = 1 bend the phase within the audible band.
        let mut allpass = -0.95 * inharm.powf(0.4);
        // Stiffness filters are long at low frequencies: never let them eat the whole loop.
        let limit_delay = 0.4 * target_period;
        let stage_delay = |a: f32| (1.0 - a) / (1.0 + a);
        while allpass < 0.0 && STAGES as f32 * stage_delay(allpass) > limit_delay {
            allpass *= 0.8;
            if allpass > -1e-3 {
                allpass = 0.0;
            }
        }
        if inharm <= 0.0 {
            allpass = 0.0;
        }

        let filters = LoopFilters { lowpass, allpass };
        let (delay, gain_lp) = filters.at(w);
        voice.waveguide.lowpass = lowpass;
        voice.waveguide.allpass = allpass;
        let max_delay = voice.waveguide.buffer.len().saturating_sub(4) as f32;
        voice.waveguide.period = (target_period - delay).clamp(4.0, max_delay);
        voice.period_ref = sr / note_hz(60.0);

        let decay_octaves = pc(S_DECAY_KEY) * key * 4.0 + voice.bus.offset(D_STRING_DECAY) * 4.0;
        let decay = g(S_DECAY) * 0.001 * 2f32.powf(decay_octaves);
        let t60 = if voice.released {
            decay * (1.0 - pc(S_RATIO).min(0.995)).powi(2).max(1e-4)
        } else {
            decay
        };
        // The cubic read loses a little of the fundamental; the loop gain makes up for it.
        let t = voice.waveguide.period.fract();
        let interp = interpolation_gain(t, w);
        let target = 10f32.powf(-3.0 / (hz * t60.max(1e-4)));
        voice.waveguide.gain = (target / (gain_lp * interp).max(1e-3)).min(0.9999) * extra_loss;

        // Where the exciter, the damper and the pickup sit along the string.
        let span = voice.waveguide.period * 0.5;
        // A position as a fraction of the string, 50% being its midpoint and past that its mirror.
        let point = |percent: f32, fixed: bool, reference: f32| -> usize {
            let along = 0.5 - (percent.clamp(0.0, 1.0) - 0.5).abs();
            let at = along * if fixed { reference } else { span };
            (at.max(1.0) as usize).min((span as usize).saturating_sub(1).max(1))
        };
        let reference = voice.period_ref * 0.5;
        let position_mod = voice.bus.offset(D_POSITION_DEST);
        let position = (pc(EX_POSITION)
            * (1.0 + pc(POSITION_VEL) * (velocity - 1.0))
            * (1.0 + pc(POSITION_KEY) * key))
            + position_mod;
        voice.exciter.age = point(position, g(EX_FIX_POS) >= 0.5, reference);
        if voice.exciter.kind == BOW {
            let force_mod = voice.bus.offset(D_P1) + voice.bus.offset(D_P2) * 0.5;
            let speed_mod = voice.bus.offset(D_VELOCITY_DEST);
            let p1 = (pc(BOW_FORCE) + force_mod).clamp(0.0, 1.0);
            voice.exciter.bow_slope = 5.0 - 4.0 * p1;
            voice.exciter.bow_grip =
                0.9 - 0.4 * (pc(BOW_FRICTION) + voice.bus.offset(D_P2)).clamp(0.0, 1.0);
            if !voice.released {
                let speed = (pc(EX_VELOCITY) * (1.0 + pc(VELOCITY_VEL) * (velocity - 1.0))
                    + speed_mod)
                    .clamp(0.0, 1.0);
                voice.exciter.bow_target = speed * 0.25 * scale;
            }
        }

        // The damper: the contact point, how fast it moves and how hard it holds.
        let d = &mut voice.damper;
        if d.enabled {
            let d_position = pc(D_POSITION) * (1.0 + pc(D_POSITION_KEY) * key);
            d.age = point(d_position, g(D_FIX_POS) >= 0.5, reference);
            let mass = (pc(D_MASS) * (1.0 + pc(D_MASS_KEY) * key)
                + voice.bus.offset(D_DAMPER_MASS))
            .clamp(0.0, 1.0);
            let stiffness = (pc(D_STIFF) * (1.0 + pc(D_STIFF_KEY) * key)
                + voice.bus.offset(D_DAMPER_STIFF))
            .clamp(0.0, 1.0);
            let velocity_amount = pc(D_VELOCITY) * (1.0 + pc(D_VELOCITY_KEY) * key);
            d.speed = if d.gated {
                ((0.001 + 0.03 * velocity_amount) * scale).clamp(1e-4, 1.0)
            } else {
                1.0
            };
            let damping = pc(D_DAMPING);
            // Below half the damper simply absorbs more as it goes up; above half it gets so
            // stiff that it bounces, spending less and less of the time on the string.
            let absorb = (damping / 0.5).min(1.0);
            d.duty = if damping <= 0.5 {
                1.0
            } else {
                1.0 - (damping - 0.5) * 1.8
            };
            d.bounce_step = 45.0 / sr;
            d.resistance =
                0.02 * 2f32.powf(mass * 8.0) * (0.3 + absorb) + 6000.0 * (mass * stiffness).powi(3);
            d.resistance *= scale;
        }

        voice.pickup_on = g(P_ON) >= 0.5;
        voice.pickup_age = point(pc(P_POSITION), false, reference);

        // The filter.
        voice.filter_on = g(F_ON) >= 0.5;
        if voice.filter_on {
            let env = if g(FE_ON) >= 0.5 {
                voice.filter_env.set(&EnvelopeSettings {
                    attack: g(FE_ATTACK) * 0.001 * (1.0 - pc(FE_ATTACK_VEL) * velocity * 0.95),
                    decay: g(FE_DECAY) * 0.001,
                    sustain: (pc(FE_SUSTAIN) + pc(FE_SUSTAIN_VEL) * velocity).min(1.0),
                    release: g(FE_RELEASE) * 0.001,
                    ..Default::default()
                });
                voice.filter_env.advance(frames)
            } else {
                0.0
            };
            let mut lfo = 0.0;
            if g(FL_ON) >= 0.5 {
                let shape = [
                    LfoShape::Sine,
                    LfoShape::Triangle,
                    LfoShape::Square,
                    LfoShape::SampleHold,
                    LfoShape::SmoothRandom,
                ][(g(FL_WAVE).round() as usize).min(4)];
                let rate = if g(FL_SYNC) >= 0.5 {
                    LfoRate::Beats(SyncRate::from_index(values[FL_DIV as usize]).beats())
                } else {
                    LfoRate::Hz(g(FL_RATE))
                };
                voice.filter_lfo.set(&LfoSettings {
                    shape,
                    rate,
                    ..Default::default()
                });
                voice.filter_lfo.set_clock(&clock);
                let wave = voice.filter_lfo.advance(frames)[0];
                let elapsed = voice.filter_lfo_time - g(FL_DELAY) * 0.001;
                let ramp = (elapsed / (g(FL_ATTACK) * 0.001).max(1e-3)).clamp(0.0, 1.0);
                lfo = wave * ramp;
                voice.filter_lfo_time += frames as f32 / sr;
            }
            let key_st = (pitch_played - 60.0) / 12.0;
            let octaves = pc(F_FREQ_LFO) * lfo * 4.0
                + pc(F_FREQ_ENV) * env * 8.0
                + pc(F_FREQ_KEY) * key_st
                + voice.bus.offset(D_FILTER_FREQ) * 5.0;
            let cutoff = (g(F_FREQ) * 2f32.powf(octaves)).clamp(20.0, sr * 0.45);
            let res = (pc(F_RES)
                + pc(F_RES_LFO) * lfo
                + pc(F_RES_ENV) * env
                + pc(F_RES_KEY) * key_st * 0.1
                + voice.bus.offset(D_FILTER_RES))
            .clamp(0.0, 1.0);
            let kind = (g(F_TYPE).round() as usize).min(FILTER_TYPES.len() - 1);
            let (filter_kind, slope) = match kind {
                0 => (FilterKind::LowPass, FilterSlope::Db12),
                1 => (FilterKind::LowPass, FilterSlope::Db24),
                2 => (FilterKind::BandPass, FilterSlope::Db12),
                3 => (FilterKind::BandPass, FilterSlope::Db24),
                4 => (FilterKind::Notch, FilterSlope::Db12),
                5 => (FilterKind::Notch, FilterSlope::Db24),
                6 => (FilterKind::HighPass, FilterSlope::Db12),
                7 => (FilterKind::HighPass, FilterSlope::Db24),
                8 => (FilterKind::BandPass, FilterSlope::Db12),
                _ => (FilterKind::BandPass, FilterSlope::Db24),
            };
            if kind >= 8 {
                // Formant: Res moves through the vowels, Freq shifts them all together.
                let t = res * (VOWELS.len() - 1) as f32;
                let (a, b) = (
                    t.floor() as usize,
                    (t.ceil() as usize).min(VOWELS.len() - 1),
                );
                let frac = t.fract();
                let shift = cutoff / 1_000.0;
                for (band, f) in voice.filter.iter_mut().enumerate() {
                    let hz = VOWELS[a][band] + (VOWELS[b][band] - VOWELS[a][band]) * frac;
                    f.set(&FilterSettings {
                        circuit: Circuit::Svf,
                        kind: filter_kind,
                        slope,
                        cutoff: (hz * shift).clamp(20.0, sr * 0.45),
                        resonance: 0.85,
                        ..Default::default()
                    });
                }
                voice.filter_gain = 1.0;
            } else {
                voice.filter[0].set(&FilterSettings {
                    circuit: Circuit::Svf,
                    kind: filter_kind,
                    slope,
                    cutoff,
                    resonance: res * 0.97,
                    ..Default::default()
                });
                voice.filter_gain = 0.0;
            }
        }

        let done = !voice.exciter.active && voice.damper.hit_left == 0 && voice.delay_left == 0;
        if !done {
            voice.quiet = 0;
        }
    }

    fn render_voice(&mut self, i: usize, out: &mut [f32]) {
        let volume_mod = 1.0 + self.voices[i].bus.offset(D_VOLUME);
        let filtered = self.voices[i].filter_on;
        let formant = self.voices[i].filter_gain == 1.0;
        let voice = &mut self.voices[i];
        let mut loudest = 0.0f32;
        for s in out.iter_mut() {
            if voice.delay_left > 0 {
                voice.delay_left -= 1;
                continue;
            }
            let arriving = voice.waveguide.read();
            voice.waveguide.write_back(arriving);
            let (exciter_age, damper_age) = (voice.exciter.age, voice.damper.age);
            let wave = voice.waveguide.velocity_at(exciter_age.max(1));
            let f = voice.exciter.tick(wave);
            if f != 0.0 {
                voice.waveguide.inject(exciter_age, f);
            }
            let damper_wave = voice.waveguide.velocity_at(damper_age.max(1));
            let d = voice.damper.tick(damper_wave);
            if d != 0.0 {
                voice.waveguide.inject(damper_age, d);
            }
            let raw = if voice.pickup_on {
                voice.waveguide.velocity_at(voice.pickup_age)
            } else {
                arriving
            };
            voice.dc += (raw - voice.dc) * 0.0005;
            let mut y = raw - voice.dc;
            if filtered {
                y = if formant {
                    voice.filter[0].process(y)
                        + voice.filter[1].process(y)
                        + voice.filter[2].process(y)
                } else {
                    voice.filter[0].process(y)
                };
            }
            let y = sanitize(y * volume_mod.max(0.0) * voice.fade.gain());
            *s += y;
            loudest = loudest.max(y.abs());
        }
        voice.quiet = if loudest < 2e-6 { voice.quiet + 1 } else { 0 };
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        if self.routes_dirty {
            self.routes_dirty = false;
            self.sync_routes();
        }
        self.sync_body();
        let frames = left.len().min(right.len());
        let body_on = self.v(B_ON) >= 0.5;
        let body_mix = self.pc(B_MIX);
        // How loud the string's waves come out of each exciter, levelled against each other.
        let trim = match self.v(EX_TYPE).round() as usize {
            BOW => 0.5,
            PLECTRUM => 2.5,
            _ => 6.0,
        };
        let volume = db_to_gain(self.v(VOLUME)) * trim;
        let mut at = 0;
        while at < frames {
            let n = (frames - at).min(CONTROL);
            self.alloc.advance(n);
            self.scratch[..n].fill(0.0);
            let mut scratch = std::mem::take(&mut self.scratch);
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
                self.render_voice(i, &mut scratch[..n]);
                let held = self.alloc.voice(i).state == VoiceState::Held;
                let voice = &mut self.voices[i];
                if !voice.fade.is_fading() && voice.quiet > 16 && !held {
                    voice.silence();
                    self.alloc.finished(i);
                }
            }
            for (s, (l, r)) in scratch[..n].iter().zip(
                left[at..at + n]
                    .iter_mut()
                    .zip(right[at..at + n].iter_mut()),
            ) {
                let dry = *s;
                let mut y = dry;
                if body_on {
                    let mut wet = 0.0;
                    for (band, gain) in self.body.iter_mut().zip(self.body_gains) {
                        wet += band.process(dry) * gain;
                    }
                    let low_cut = self.body_cuts[0].process(wet * 2.0);
                    let wet = self.body_cuts[1].process(low_cut);
                    y = dry * (1.0 - body_mix) + wet * body_mix;
                }
                let out = limit(sanitize(y * volume));
                *l += out;
                *r += out;
            }
            self.scratch = scratch;
            self.clock.advance(n);
            at += n;
        }
    }

    /// Whether Priority turns a new note away: with High, one lower than everything sounding
    /// when the voices are full; with Low, one higher.
    fn refused_by_priority(&self, key: u8) -> bool {
        let settings = *self.alloc.settings();
        if settings.steal == StealMode::Oldest {
            return false;
        }
        let voices = self.alloc.voices();
        let held = voices
            .iter()
            .filter(|v| v.state != VoiceState::Free)
            .count();
        if held < settings.polyphony * settings.unison.max(1) {
            return false;
        }
        let mut sounding = voices.iter().filter(|v| v.state != VoiceState::Free);
        match settings.steal {
            StealMode::Lowest => sounding.all(|v| key < v.note.key),
            StealMode::Highest => sounding.all(|v| key > v.note.key),
            _ => false,
        }
    }

    fn midi(&mut self, message: MidiMessage) {
        if let MidiMessage::ControlChange { cc: 1, value, .. } = message {
            self.wheel = value as f32 / 127.0;
        }
        if let MidiMessage::NoteOn { key, velocity, .. } = message {
            if velocity > 0.0 && self.refused_by_priority(key) {
                return;
            }
        }
        if let MidiMessage::NoteExpr {
            kind: ExprKind::Pitch,
            ..
        } = message
        {
            self.note_bend_seen = 1.0;
        }
        let message = scale_note_bend(message, self.v(NOTE_PB));
        let mut starts: [Option<(usize, NoteInfo, bool)>; 16] = [None; 16];
        let mut releases: [Option<usize>; 32] = [None; 32];
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
                voices[voice].key = (note.key as f32 - 60.0) / 60.0;
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

    fn publish(&mut self, ctx: &ProcessContext) {
        let playing = self.voices.iter().filter(|v| v.active).count();
        let mut view = [0.0f32; VIEW_LEN];
        view[0] = playing as f32;
        let voice = self.voices.iter().find(|v| v.active);
        if let Some(v) = voice {
            let span = (v.waveguide.period * 0.5).max(1.0);
            self.shape_at = [
                v.exciter.age as f32 / span,
                v.damper.age as f32 / span,
                v.pickup_age as f32 / span,
            ];
            for p in 0..SHAPE_POINTS {
                let age = ((p as f32 + 0.5) / SHAPE_POINTS as f32 * span) as usize;
                view[4 + p] = v.waveguide.velocity_at(age.max(1)).clamp(-1.0, 1.0);
            }
        }
        view[1..4].copy_from_slice(&self.shape_at);
        for v in self
            .alloc
            .voices()
            .iter()
            .filter(|v| v.state != VoiceState::Free)
        {
            view[VIEW_ACTIVITY] = view[VIEW_ACTIVITY].max(v.pressure);
            view[VIEW_ACTIVITY + 1] = view[VIEW_ACTIVITY + 1].max(v.slide);
        }
        view[VIEW_ACTIVITY + 2] = self.note_bend_seen;
        ctx.view.publish(VIEW, &view);
    }
}

/// The magnitude of the cubic read at `w` radians per sample, for a fractional position `t`.
fn interpolation_gain(t: f32, w: f32) -> f32 {
    let (t2, t3) = (t * t, t * t * t);
    let c = [
        -0.5 * t + t2 - 0.5 * t3,
        1.0 - 2.5 * t2 + 1.5 * t3,
        0.5 * t + 2.0 * t2 - 1.5 * t3,
        -0.5 * t2 + 0.5 * t3,
    ];
    let (mut re, mut im) = (0.0f32, 0.0f32);
    for (k, a) in c.iter().enumerate() {
        let angle = -(k as f32 - 1.0) * w;
        re += a * angle.cos();
        im += a * angle.sin();
    }
    (re * re + im * im).sqrt()
}

impl Processor for Tension {
    fn sleeps_when_silent(&self) -> bool {
        true
    }

    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sample_rate = sample_rate as f32;
        self.alloc.prepare(sample_rate);
        self.scratch = vec![0.0; CONTROL];
        let capacity = (sample_rate as usize / 16).max(256);
        let matrix = &self.matrix;
        self.voices = (0..MAX_VOICES)
            .map(|i| Voice::new(i as u32 + 21, capacity, matrix))
            .collect();
        for v in &mut self.voices {
            v.filter_env.prepare(sample_rate);
            v.filter_lfo.prepare(sample_rate);
            v.vibrato.prepare(sample_rate);
            for f in &mut v.filter {
                f.prepare(sample_rate);
            }
        }
        self.sync_settings();
        self.routes_dirty = true;
        self.body_key = None;
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
            self.note_bend_seen = 0.0;
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.values.get_mut(id as usize) {
            *v = value;
            self.routes_dirty = true;
            if matches!(
                id,
                VOICES
                    | PITCH_BEND
                    | PO_ON
                    | PO_TIME
                    | PO_LEGATO
                    | PO_PROP
                    | UN_ON
                    | UN_VOICES
                    | PRIORITY
            ) {
                self.sync_settings();
            }
        }
    }

    fn reset(&mut self) {
        self.alloc.reset();
        self.wheel = 0.0;
        for v in &mut self.voices {
            v.silence();
        }
        for b in &mut self.body {
            b.reset();
        }
        for b in &mut self.body_cuts {
            b.reset();
        }
    }

    fn tail(&self) -> Tail {
        let decay = self.v(S_DECAY) * 0.001;
        let seconds = decay * 2.0 + 1.0;
        Tail::Frames((seconds * self.sample_rate) as u32)
    }
}
