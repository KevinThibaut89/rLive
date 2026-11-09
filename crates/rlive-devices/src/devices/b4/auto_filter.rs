//! Auto Filter: a filter with a stereo LFO and an envelope follower moving its cutoff.
//!
//! The filter types and circuits are the shared library's, for the five types that have
//! circuits. The manual gives no ranges for Freq, Res, the modulation amounts, or the LFO and
//! envelope times, so those here are our own, as are the [`LFO_OCTAVES`] and [`ENV_OCTAVES`]
//! that say how far each one moves the cutoff at full amount.

use super::common::{
    saw_down, saw_up, sine, square, triangle, Ramp, RandomWave, StepGrid, CONTROL_FRAMES,
};
use crate::devices::b3::common::sidechain_slice;
use crate::dsp::delay::{InterpDelay, Interpolation};
use crate::dsp::filter::{Circuit, Filter, FilterKind, FilterSettings, FilterSlope};
use crate::dsp::lfo::{Lfo, LfoRate, LfoSettings};
use crate::dsp::sidechain::{SidechainDetector, SidechainEq, SidechainSettings};
use crate::dsp::spectrum::SpectrumFeed;
use crate::dsp::tempo::{BeatClock, SyncRate};
use crate::dsp::util::{lerp, sanitize};
use crate::dsp::{Biquad, BiquadKind, Smoother};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    db_to_gain, AudioBuffer, AudioInput, BlockStep, DeviceDesc, Item, ParamId, ParamInfo, Ports,
    ProcessContext, Processor, Section, Status, ViewId, SCOPE_VALUES,
};
use std::f32::consts::{PI, TAU};

pub const TYPE: u32 = 0;
pub const FREQ: u32 = 1;
pub const RES: u32 = 2;
pub const MORPH: u32 = 3;
pub const CONTROL: u32 = 4;
pub const PITCH: u32 = 5;
pub const FORMANT: u32 = 6;
pub const SLOPE: u32 = 7;
pub const MORPH_SLOPE: u32 = 8;
pub const CIRCUIT: u32 = 9;
pub const DRIVE: u32 = 10;
pub const CLIP: u32 = 11;
pub const OUTPUT: u32 = 12;
pub const DRY_WET: u32 = 13;

pub const LFO_AMOUNT: u32 = 14;
pub const LFO_MODE: u32 = 15;
pub const LFO_RATE: u32 = 16;
pub const LFO_TIME: u32 = 17;
pub const LFO_BEATS: u32 = 18;
pub const LFO_SIXTEENTHS: u32 = 19;
pub const LFO_WAVE: u32 = 20;
pub const LFO_MORPH: u32 = 21;
pub const LFO_STEREO: u32 = 22;
pub const LFO_PHASE: u32 = 23;
pub const LFO_SPIN: u32 = 24;
pub const LFO_OFFSET: u32 = 25;
pub const LFO_QUANT: u32 = 26;
pub const LFO_STEPS: u32 = 27;
pub const LFO_SH_RATE: u32 = 28;

pub const ENV_AMOUNT: u32 = 29;
pub const ENV_ATTACK: u32 = 30;
pub const ENV_HOLD: u32 = 31;
pub const ENV_RELEASE: u32 = 32;
pub const ENV_SH_ON: u32 = 33;
pub const ENV_SH_RATE: u32 = 34;

pub const SC_EXTERNAL: u32 = 35;
pub const SC_MIX: u32 = 36;
pub const SC_GAIN: u32 = 37;
pub const SC_LISTEN: u32 = 38;
pub const SC_FILTER_ON: u32 = 39;
pub const SC_FILTER_TYPE: u32 = 40;
pub const SC_FILTER_FREQ: u32 = 41;
pub const SC_FILTER_Q: u32 = 42;
pub const SC_SHELF_GAIN: u32 = 43;
pub const SC_PEAK_GAIN: u32 = 44;
pub const SC_MONO: u32 = 45;

pub const PARAM_COUNT: usize = 46;

/// Indices of [`TYPE_LABELS`].
pub const T_LOW_PASS: usize = 0;
pub const T_HIGH_PASS: usize = 1;
pub const T_BAND_PASS: usize = 2;
pub const T_NOTCH: usize = 3;
pub const T_MORPH: usize = 4;
pub const T_DJ: usize = 5;
pub const T_COMB: usize = 6;
pub const T_RESAMPLING: usize = 7;
pub const T_NOTCH_LP: usize = 8;
pub const T_VOWEL: usize = 9;

pub const TYPE_LABELS: [&str; 10] = [
    "Low-pass",
    "High-pass",
    "Band-pass",
    "Notch",
    "Morph",
    "DJ",
    "Comb",
    "Resampling",
    "Notch + LP",
    "Vowel",
];

pub const WAVE_LABELS: [&str; 8] = [
    "Sine",
    "Triangle",
    "Saw",
    "Square",
    "Ramp Up",
    "Ramp Down",
    "Wander",
    "S&H",
];
const WAVE_RAMP_UP: usize = 4;
const WAVE_RAMP_DOWN: usize = 5;
const WAVE_WANDER: usize = 6;
const WAVE_SH: usize = 7;

pub const LFO_MODE_LABELS: [&str; 4] = ["Freq", "Time", "Beats", "16th"];
pub const LM_FREQ: usize = 0;
pub const LM_TIME: usize = 1;
pub const LM_BEATS: usize = 2;
pub const LM_16TH: usize = 3;

pub const QUANT_LABELS: [&str; 3] = ["Off", "Steps", "S&H"];
pub const QUANT_STEPS: usize = 1;
pub const QUANT_SH: usize = 2;

/// How far the LFO moves the cutoff at full Amt, up and down, in octaves. Ours.
pub const LFO_OCTAVES: f32 = 4.0;
/// How far the envelope moves it at full Envelope and full level, in octaves. Ours.
pub const ENV_OCTAVES: f32 = 6.0;
/// How far below the cutoff the Notch + LP notch sits at Morph 0, in octaves. Ours.
pub const NOTCH_LP_RANGE: f32 = 3.0;
/// The DJ filter's control range in octaves, from a cutoff at the edge of hearing. Ours.
pub const DJ_OCTAVES: f32 = 9.0;
pub const DJ_LOW_HZ: f32 = 20.0;
pub const DJ_HIGH_HZ: f32 = 20_000.0;
/// Where the Clip softening starts. The manual doesn't say.
pub const CLIP_KNEE: f32 = 0.8;

pub const CLIP_LED: ViewId = ViewId(3);
pub const SPECTRUM: ViewId = ViewId(4);
/// The left and right cutoffs after modulation, in Hz.
pub const CUTOFFS: ViewId = ViewId(4 + SCOPE_VALUES as u32);
pub const SAMPLE_RATE: ViewId = ViewId(6 + SCOPE_VALUES as u32);
pub const VIEW_LEN: usize = 7 + SCOPE_VALUES;

fn params() -> Vec<ParamInfo> {
    let sync_default = 12; // 1/4.
    vec![
        ParamInfo::stepped(TYPE, "Filter Type", &TYPE_LABELS, 0),
        ParamInfo::skewed(FREQ, "Freq", 20.0, 20_000.0, 1_000.0, "Hz", 3.0),
        ParamInfo::continuous(RES, "Res", 0.0, 100.0, 20.0, "%"),
        ParamInfo::continuous(MORPH, "Morph", 0.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(CONTROL, "Control", -100.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(PITCH, "Pitch", -24.0, 24.0, 0.0, "st"),
        ParamInfo::continuous(FORMANT, "Formant", 0.0, 100.0, 0.0, "%"),
        ParamInfo::stepped(SLOPE, "Slope", &["12 dB", "24 dB"], 0),
        ParamInfo::stepped(
            MORPH_SLOPE,
            "Morph Slope",
            &["6 dB", "12 dB", "24 dB", "48 dB"],
            1,
        ),
        ParamInfo::stepped(CIRCUIT, "Circuit", &["SVF", "DFM", "MS2", "PRD"], 0),
        ParamInfo::continuous(DRIVE, "Drive", 0.0, 100.0, 0.0, "%"),
        ParamInfo::toggle(CLIP, "Clip", false),
        ParamInfo::continuous(OUTPUT, "Output", -24.0, 24.0, 0.0, "dB"),
        ParamInfo::continuous(DRY_WET, "Dry/Wet", 0.0, 100.0, 100.0, "%"),
        ParamInfo::continuous(LFO_AMOUNT, "LFO Amt", 0.0, 100.0, 0.0, "%"),
        ParamInfo::stepped(LFO_MODE, "LFO Mode", &LFO_MODE_LABELS, LM_FREQ),
        ParamInfo::skewed(LFO_RATE, "LFO Rate", 0.01, 40.0, 1.0, "Hz", 3.0),
        ParamInfo::skewed(LFO_TIME, "LFO Time", 0.01, 60.0, 1.0, "s", 3.0),
        ParamInfo::stepped(LFO_BEATS, "LFO Beats", &SyncRate::LABELS, sync_default),
        ParamInfo::continuous(LFO_SIXTEENTHS, "LFO 16ths", 1.0, 64.0, 4.0, ""),
        ParamInfo::stepped(LFO_WAVE, "LFO Wave", &WAVE_LABELS, 0),
        ParamInfo::continuous(LFO_MORPH, "LFO Morph", 0.0, 100.0, 0.0, "%"),
        ParamInfo::stepped(LFO_STEREO, "LFO Stereo Mode", &["Phase", "Spin"], 0),
        ParamInfo::continuous(LFO_PHASE, "LFO Phase", 0.0, 360.0, 0.0, "deg"),
        ParamInfo::continuous(LFO_SPIN, "LFO Spin", 0.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(LFO_OFFSET, "LFO Phase Offset", 0.0, 360.0, 0.0, "deg"),
        ParamInfo::stepped(LFO_QUANT, "LFO Quantization", &QUANT_LABELS, 0),
        ParamInfo::continuous(LFO_STEPS, "LFO Steps", 2.0, 32.0, 8.0, ""),
        ParamInfo::stepped(LFO_SH_RATE, "LFO S&H", &SyncRate::LABELS, 6),
        ParamInfo::continuous(ENV_AMOUNT, "Envelope", -100.0, 100.0, 0.0, "%"),
        ParamInfo::skewed(ENV_ATTACK, "Attack", 0.1, 1_000.0, 6.0, "ms", 3.0),
        ParamInfo::toggle(ENV_HOLD, "Hold", false),
        ParamInfo::skewed(ENV_RELEASE, "Release", 1.0, 3_000.0, 200.0, "ms", 3.0),
        ParamInfo::toggle(ENV_SH_ON, "Envelope S&H", false),
        ParamInfo::stepped(ENV_SH_RATE, "Envelope S&H Rate", &SyncRate::LABELS, 6),
        ParamInfo::toggle(SC_EXTERNAL, "External", false),
        ParamInfo::continuous(SC_MIX, "S/C Mix", 0.0, 100.0, 100.0, "%"),
        ParamInfo::continuous(SC_GAIN, "S/C Gain", -24.0, 24.0, 0.0, "dB"),
        ParamInfo::toggle(SC_LISTEN, "S/C Listen", false),
        ParamInfo::toggle(SC_FILTER_ON, "S/C Filter", false),
        ParamInfo::stepped(
            SC_FILTER_TYPE,
            "S/C Filter Type",
            &[
                "Low Shelf",
                "Peak",
                "High Shelf",
                "Low-pass",
                "Band-pass",
                "Notch",
                "High-pass",
            ],
            4,
        ),
        ParamInfo::skewed(
            SC_FILTER_FREQ,
            "S/C Filter Freq",
            20.0,
            20_000.0,
            1_000.0,
            "Hz",
            3.0,
        ),
        ParamInfo::skewed(SC_FILTER_Q, "S/C Filter Q", 0.1, 12.0, 0.71, "", 2.0),
        ParamInfo::continuous(SC_SHELF_GAIN, "S/C Shelf Gain", -15.0, 15.0, 0.0, "dB"),
        ParamInfo::continuous(SC_PEAK_GAIN, "S/C Peak Gain", -15.0, 15.0, 0.0, "dB"),
        ParamInfo::toggle(SC_MONO, "Mono Sidechain", true),
    ]
}

fn layout() -> Vec<Section> {
    let knob = |id| Item::Knob(ParamId(id));
    let menu = |id| Item::Menu(ParamId(id));
    let toggle = |id| Item::Toggle(ParamId(id));
    vec![
        Section {
            title: "Display",
            items: vec![Item::Panel("auto_filter_display")],
        },
        Section {
            title: "Filter",
            items: vec![
                menu(TYPE),
                menu(CIRCUIT),
                menu(SLOPE),
                menu(MORPH_SLOPE),
                knob(FREQ),
                knob(RES),
                knob(MORPH),
                knob(CONTROL),
                knob(PITCH),
                knob(FORMANT),
                knob(DRIVE),
            ],
        },
        Section {
            title: "LFO",
            items: vec![
                knob(LFO_AMOUNT),
                menu(LFO_MODE),
                knob(LFO_RATE),
                knob(LFO_TIME),
                menu(LFO_BEATS),
                knob(LFO_SIXTEENTHS),
                menu(LFO_WAVE),
                knob(LFO_MORPH),
                menu(LFO_STEREO),
                knob(LFO_PHASE),
                knob(LFO_SPIN),
                knob(LFO_OFFSET),
                menu(LFO_QUANT),
                knob(LFO_STEPS),
                menu(LFO_SH_RATE),
            ],
        },
        Section {
            title: "Envelope",
            items: vec![
                knob(ENV_AMOUNT),
                knob(ENV_ATTACK),
                toggle(ENV_HOLD),
                knob(ENV_RELEASE),
                toggle(ENV_SH_ON),
                menu(ENV_SH_RATE),
            ],
        },
        Section {
            title: "Sidechain",
            items: vec![
                toggle(SC_EXTERNAL),
                knob(SC_MIX),
                knob(SC_GAIN),
                toggle(SC_LISTEN),
                toggle(SC_FILTER_ON),
                menu(SC_FILTER_TYPE),
                knob(SC_FILTER_FREQ),
                knob(SC_FILTER_Q),
                knob(SC_SHELF_GAIN),
                knob(SC_PEAK_GAIN),
                toggle(SC_MONO),
            ],
        },
        Section {
            title: "Global",
            items: vec![
                toggle(CLIP),
                Item::Panel("clip_led"),
                knob(OUTPUT),
                knob(DRY_WET),
            ],
        },
    ]
}

/// The controls a filter's shape depends on, apart from its cutoff.
#[derive(Clone, Copy, Debug)]
pub struct FilterShape {
    pub filter_type: usize,
    pub resonance: f32,
    pub morph: f32,
    pub control: f32,
    pub pitch: f32,
    pub formant: f32,
    pub slope: FilterSlope,
    pub morph_slope: usize,
    pub circuit: Circuit,
}

impl FilterShape {
    /// Reads the shape from the parameters.
    pub fn read(get: impl Fn(u32) -> f64) -> Self {
        Self {
            filter_type: (get(TYPE).round() as usize).min(T_VOWEL),
            resonance: (get(RES) as f32 * 0.01).clamp(0.0, 1.0),
            morph: (get(MORPH) as f32 * 0.01).clamp(0.0, 1.0),
            control: (get(CONTROL) as f32 * 0.01).clamp(-1.0, 1.0),
            pitch: get(PITCH) as f32,
            formant: (get(FORMANT) as f32 * 0.01).clamp(0.0, 1.0),
            slope: if get(SLOPE) >= 0.5 {
                FilterSlope::Db24
            } else {
                FilterSlope::Db12
            },
            morph_slope: (get(MORPH_SLOPE).round() as usize).min(3),
            circuit: match get(CIRCUIT).round() as usize {
                1 => Circuit::Dfm,
                2 => Circuit::Ms2,
                3 => Circuit::Prd,
                _ => Circuit::Svf,
            },
        }
    }

    /// The kind of a filter type that has one, for the library's filters. The Morph control
    /// runs low-pass, band-pass, high-pass, which is the first half of the library's cycle.
    fn library_kind(&self) -> Option<FilterKind> {
        match self.filter_type {
            T_LOW_PASS => Some(FilterKind::LowPass),
            T_HIGH_PASS => Some(FilterKind::HighPass),
            T_BAND_PASS => Some(FilterKind::BandPass),
            T_NOTCH => Some(FilterKind::Notch),
            T_MORPH if self.morph_slope > 0 => Some(FilterKind::Morph),
            _ => None,
        }
    }

    /// Whether the circuit and drive of the library's filters apply.
    fn uses_circuit(&self) -> bool {
        self.library_kind().is_some()
    }

    fn library_settings(&self, cutoff: f32, drive_db: f32) -> FilterSettings {
        let is_morph = self.filter_type == T_MORPH;
        FilterSettings {
            circuit: self.circuit,
            kind: self.library_kind().unwrap_or_default(),
            slope: if is_morph {
                if self.morph_slope <= 1 {
                    FilterSlope::Db12
                } else {
                    FilterSlope::Db24
                }
            } else {
                self.slope
            },
            cutoff,
            resonance: self.resonance,
            drive: drive_db,
            morph: self.morph * 0.5,
        }
    }

    /// Whether the 48 dB morph runs two filters in series.
    fn doubled(&self) -> bool {
        self.filter_type == T_MORPH && self.morph_slope == 3
    }

    /// The DJ filter's kind, cutoff and resonance at a control position.
    fn dj(&self, control: f32) -> (FilterKind, f32, f32) {
        let amount = control.abs().min(1.0);
        // The resonance's boost starts small and grows towards the ends of the control.
        let resonance = self.resonance * (0.2 + 0.8 * amount);
        if control > 0.0 {
            (
                FilterKind::HighPass,
                DJ_LOW_HZ * 2f32.powf(amount * DJ_OCTAVES),
                resonance,
            )
        } else {
            (
                FilterKind::LowPass,
                DJ_HIGH_HZ * 2f32.powf(-amount * DJ_OCTAVES),
                resonance,
            )
        }
    }

    fn comb_feedback(&self) -> f32 {
        lerp(0.4, 0.97, self.resonance)
    }

    /// The linear small-signal gain at `hz` with the cutoff at `cutoff`. For drawing.
    pub fn magnitude(&self, cutoff: f32, hz: f32, sr: f32) -> f32 {
        match self.filter_type {
            T_DJ => {
                if self.control.abs() < 0.001 {
                    1.0
                } else {
                    let (kind, freq, resonance) = self.dj(self.control);
                    FilterSettings {
                        kind,
                        cutoff: freq,
                        resonance,
                        slope: FilterSlope::Db24,
                        ..Default::default()
                    }
                    .magnitude(hz, sr)
                }
            }
            T_COMB => {
                let g = self.comb_feedback();
                let w = TAU * hz / sr;
                let d = (sr / cutoff.clamp(20.0, sr * 0.25)).max(2.0);
                let (s, c) = (w * d).sin_cos();
                let (re, im) = (g * c, -g * s);
                // Feedback comb: (1 - g) / (1 - g z), feedforward comb: (1 - g z) / (1 + g).
                let (dr, di) = (1.0 - re, -im);
                let den = (dr * dr + di * di).max(1e-9);
                let peak = ((1.0 - g) * dr / den, -(1.0 - g) * di / den);
                let notch = ((1.0 - re) / (1.0 + g), -im / (1.0 + g));
                let re = lerp(peak.0, notch.0, self.morph);
                let im = lerp(peak.1, notch.1, self.morph);
                re.hypot(im)
            }
            T_RESAMPLING => 1.0,
            T_NOTCH_LP => {
                let notch = FilterSettings {
                    kind: FilterKind::Notch,
                    cutoff: notch_lp_notch(cutoff, self.morph),
                    resonance: 0.5,
                    ..Default::default()
                };
                let low = FilterSettings {
                    kind: FilterKind::LowPass,
                    cutoff,
                    resonance: self.resonance,
                    slope: self.slope,
                    ..Default::default()
                };
                notch.magnitude(hz, sr) * low.magnitude(hz, sr)
            }
            T_VOWEL => {
                let (freqs, gains, qs) = vowel_targets(self.formant, self.pitch, self.morph);
                let mut b = Biquad::default();
                (0..3)
                    .map(|i| {
                        b.set(BiquadKind::BandPass, freqs[i], qs[i], 0.0, sr);
                        b.magnitude(hz, sr) * gains[i]
                    })
                    .sum()
            }
            T_MORPH if self.morph_slope == 0 => one_pole_morph_gain(cutoff, self.morph, hz, sr),
            _ => {
                let settings = self.library_settings(cutoff, 0.0);
                let once = settings.magnitude(hz, sr);
                if self.doubled() {
                    once * once
                } else {
                    once
                }
            }
        }
    }
}

/// Where the notch of Notch + LP sits for a low-pass at `cutoff`.
pub fn notch_lp_notch(cutoff: f32, morph: f32) -> f32 {
    cutoff * 2f32.powf(-(1.0 - morph) * NOTCH_LP_RANGE)
}

fn one_pole_morph_gain(cutoff: f32, morph: f32, hz: f32, sr: f32) -> f32 {
    let g = (PI * cutoff.clamp(10.0, sr * 0.49) / sr).tan();
    let w = (PI * hz.clamp(0.0, sr * 0.499) / sr).tan() / g;
    // lp = 1/(1+s), hp = s/(1+s), bp = hp lp = s/(1+s)^2, at s = jw.
    let den = 1.0 + w * w;
    let lp = (1.0 / den, -w / den);
    let hp = (w * w / den, w / den);
    let bp = (hp.0 * lp.0 - hp.1 * lp.1, hp.0 * lp.1 + hp.1 * lp.0);
    let (a, b, t) = if morph < 0.5 {
        (lp, bp, morph * 2.0)
    } else {
        (bp, hp, morph * 2.0 - 1.0)
    };
    lerp(a.0, b.0, t).hypot(lerp(a.1, b.1, t))
}

// Published averages for the vowels a, e, i, o, u: three formants' frequencies, gains in dB and
// bandwidths in Hz.
const FORMANT_HZ: [[f32; 3]; 5] = [
    [800.0, 1_150.0, 2_900.0],
    [350.0, 2_000.0, 2_800.0],
    [270.0, 2_140.0, 2_950.0],
    [450.0, 800.0, 2_830.0],
    [325.0, 700.0, 2_700.0],
];
const FORMANT_DB: [[f32; 3]; 5] = [
    [0.0, -6.0, -32.0],
    [0.0, -20.0, -15.0],
    [0.0, -12.0, -26.0],
    [0.0, -11.0, -22.0],
    [0.0, -16.0, -35.0],
];
const FORMANT_BW: [[f32; 3]; 5] = [
    [80.0, 90.0, 120.0],
    [60.0, 100.0, 120.0],
    [60.0, 90.0, 100.0],
    [70.0, 80.0, 100.0],
    [50.0, 60.0, 170.0],
];

/// The vowel filter's three band-pass frequencies, linear gains and Qs. `formant` runs through
/// a, e, i, o, u. `morph` flattens the gains towards unity.
pub fn vowel_targets(formant: f32, pitch: f32, morph: f32) -> ([f32; 3], [f32; 3], [f32; 3]) {
    let pos = (formant.clamp(0.0, 1.0) * 4.0).min(3.999_9);
    let i = pos as usize;
    let t = pos - i as f32;
    let (mut hz, mut gain, mut q) = ([0.0; 3], [0.0; 3], [0.0; 3]);
    let shift = 2f32.powf(pitch / 12.0);
    for f in 0..3 {
        let freq = lerp(FORMANT_HZ[i][f], FORMANT_HZ[i + 1][f], t);
        let bw = lerp(FORMANT_BW[i][f], FORMANT_BW[i + 1][f], t);
        let db = lerp(FORMANT_DB[i][f], FORMANT_DB[i + 1][f], t);
        hz[f] = freq * shift;
        gain[f] = db_to_gain(db * (1.0 - morph));
        q[f] = freq / bw;
    }
    (hz, gain, q)
}

// ------------------------------------------------------------- channel filters

#[derive(Clone)]
struct ChannelFilter {
    a: Filter,
    b: Filter,
    lp1: f32,
    lp2: f32,
    g1: f32,
    comb_feedback: InterpDelay,
    comb_feedforward: InterpDelay,
    comb_delay: f32,
    comb_delay_target: f32,
    comb_step: f32,
    held: f32,
    resample_phase: f64,
    resample_rate: f64,
    formants: [Biquad; 3],
    formant_gain: [f32; 3],
    dj_bypass: bool,
}

impl ChannelFilter {
    fn new(max_comb: usize) -> Self {
        Self {
            a: Filter::new(),
            b: Filter::new(),
            lp1: 0.0,
            lp2: 0.0,
            g1: 0.0,
            comb_feedback: InterpDelay::new(max_comb),
            comb_feedforward: InterpDelay::new(max_comb),
            comb_delay: 48.0,
            comb_delay_target: 48.0,
            comb_step: 0.0,
            held: 0.0,
            resample_phase: 0.0,
            resample_rate: 0.0,
            formants: [Biquad::default(); 3],
            formant_gain: [1.0; 3],
            dj_bypass: true,
        }
    }

    fn prepare(&mut self, sample_rate: f64) {
        self.a.prepare(sample_rate);
        self.b.prepare(sample_rate);
    }

    fn reset(&mut self) {
        self.a.reset();
        self.b.reset();
        self.lp1 = 0.0;
        self.lp2 = 0.0;
        self.comb_feedback.clear();
        self.comb_feedforward.clear();
        self.held = 0.0;
        self.resample_phase = 0.0;
        self.formants.iter_mut().for_each(Biquad::reset);
    }

    /// Tunes the channel's filter for the next `frames` samples.
    fn retune(
        &mut self,
        shape: &FilterShape,
        cutoff: f32,
        octaves: f32,
        drive_db: f32,
        sr: f32,
        frames: usize,
    ) {
        match shape.filter_type {
            T_DJ => {
                let control = shape.control + octaves / DJ_OCTAVES;
                self.dj_bypass = control.abs() < 0.001;
                let (kind, freq, resonance) = shape.dj(control.clamp(-1.0, 1.0));
                self.a.set(&FilterSettings {
                    kind,
                    cutoff: freq,
                    resonance,
                    slope: FilterSlope::Db24,
                    ..Default::default()
                });
            }
            T_COMB => {
                self.comb_delay_target =
                    (sr / cutoff).clamp(2.0, self.comb_feedback.capacity() as f32);
                self.comb_step = (self.comb_delay_target - self.comb_delay) / frames as f32;
            }
            T_RESAMPLING => self.resample_rate = 2.0 * cutoff as f64 / sr as f64,
            T_NOTCH_LP => {
                self.a.set(&FilterSettings {
                    kind: FilterKind::Notch,
                    cutoff: notch_lp_notch(cutoff, shape.morph),
                    resonance: 0.5,
                    ..Default::default()
                });
                self.b.set(&FilterSettings {
                    kind: FilterKind::LowPass,
                    cutoff,
                    resonance: shape.resonance,
                    slope: shape.slope,
                    ..Default::default()
                });
            }
            T_VOWEL => {
                let (hz, gain, q) =
                    vowel_targets(shape.formant, shape.pitch + octaves * 12.0, shape.morph);
                for i in 0..3 {
                    self.formants[i].set(BiquadKind::BandPass, hz[i], q[i], 0.0, sr);
                }
                self.formant_gain = gain;
            }
            T_MORPH if shape.morph_slope == 0 => {
                let g = (PI * cutoff.clamp(10.0, sr * 0.49) / sr).tan();
                self.g1 = g / (1.0 + g);
            }
            _ => {
                let settings = shape.library_settings(cutoff, drive_db);
                self.a.set(&settings);
                if shape.doubled() {
                    self.b.set(&FilterSettings {
                        drive: 0.0,
                        ..settings
                    });
                }
            }
        }
    }

    #[inline]
    fn tick(&mut self, shape: &FilterShape, x: f32) -> f32 {
        match shape.filter_type {
            T_DJ => {
                if self.dj_bypass {
                    x
                } else {
                    self.a.process(x)
                }
            }
            T_COMB => {
                self.comb_delay += self.comb_step;
                let g = shape.comb_feedback();
                let delayed_output = self
                    .comb_feedback
                    .read(self.comb_delay - 1.0, Interpolation::Cubic);
                let delayed_input = self
                    .comb_feedforward
                    .read(self.comb_delay - 1.0, Interpolation::Cubic);
                let y = sanitize(x + g * delayed_output);
                self.comb_feedback.write(y);
                self.comb_feedforward.write(x);
                let peaks = y * (1.0 - g);
                let notches = (x - g * delayed_input) / (1.0 + g);
                lerp(peaks, notches, shape.morph)
            }
            T_RESAMPLING => {
                self.resample_phase += self.resample_rate;
                if self.resample_phase >= 1.0 {
                    self.resample_phase -= 1.0;
                    self.held = x;
                }
                self.held
            }
            T_NOTCH_LP => self.b.process(self.a.process(x)),
            T_VOWEL => {
                let mut y = 0.0;
                for (formant, gain) in self.formants.iter_mut().zip(self.formant_gain) {
                    y += formant.process(x) * gain;
                }
                y
            }
            T_MORPH if shape.morph_slope == 0 => {
                let v = (x - self.lp1) * self.g1;
                let lp = v + self.lp1;
                self.lp1 = sanitize(lp + v);
                let hp = x - lp;
                let v2 = (hp - self.lp2) * self.g1;
                let bp = v2 + self.lp2;
                self.lp2 = sanitize(bp + v2);
                if shape.morph < 0.5 {
                    lerp(lp, bp, shape.morph * 2.0)
                } else {
                    lerp(bp, hp, shape.morph * 2.0 - 1.0)
                }
            }
            _ => {
                let y = self.a.process(x);
                if shape.doubled() {
                    self.b.process(y)
                } else {
                    y
                }
            }
        }
    }
}

/// The envelope state of one channel, or of both with a mono sidechain.
#[derive(Clone, Copy, Debug, Default)]
struct EnvState {
    level: f32,
    target: f32,
    attack_left: u32,
    held: f32,
}

/// How the envelope follower behaves, from its controls.
#[derive(Clone, Copy, Debug)]
struct EnvSetup {
    attack_coeff: f32,
    release_coeff: f32,
    attack_frames: u32,
    hold: bool,
}

impl EnvState {
    #[inline]
    fn follow(&mut self, x: f32, setup: &EnvSetup) {
        if setup.hold {
            if x > self.level && self.attack_left == 0 {
                self.attack_left = setup.attack_frames;
                self.target = x;
            }
            if self.attack_left > 0 {
                self.target = self.target.max(x);
                self.level = x_towards(self.level, self.target, setup.attack_coeff);
                self.attack_left -= 1;
                return;
            }
        }
        let coeff = if x > self.level {
            setup.attack_coeff
        } else {
            setup.release_coeff
        };
        self.level = x_towards(self.level, x, coeff);
    }
}

#[inline]
fn x_towards(level: f32, target: f32, coeff: f32) -> f32 {
    sanitize(target + (level - target) * coeff)
}

/// A soft clip that leaves the signal alone below [`CLIP_KNEE`] and meets 1.0 asymptotically.
#[inline]
pub fn soft_clip_output(x: f32) -> f32 {
    let a = x.abs();
    if a <= CLIP_KNEE {
        x
    } else {
        let range = 1.0 - CLIP_KNEE;
        x.signum() * (CLIP_KNEE + range * ((a - CLIP_KNEE) / range).tanh())
    }
}

/// The controls that are worked out from parameters only when one changes.
#[derive(Clone, Copy, Debug)]
struct Setup {
    shape: FilterShape,
    drive_db: f32,
    drive_gain: f32,
    clip: bool,
    lfo_amount: f32,
    lfo_wave: usize,
    lfo_morph: f32,
    lfo_cycle_s: f32,
    env_amount: f32,
    env_sh: Option<f64>,
    lfo_quant: usize,
    lfo_steps: f32,
    lfo_sh_beats: f64,
    use_external: bool,
    mono_key: bool,
    listen: bool,
    settings: LfoSettings,
    env: EnvSetup,
}

pub struct AutoFilter {
    v: [f64; PARAM_COUNT],
    sr: f32,
    filters: [ChannelFilter; 2],
    freq_log: Ramp,
    cut_log: [Ramp; 2],
    output: Smoother,
    mix: Smoother,
    lfo: Lfo,
    random: [RandomWave; 2],
    lfo_held: [f32; 2],
    lfo_smooth: [f32; 2],
    lfo_grid: StepGrid,
    env_grid: StepGrid,
    envs: [EnvState; 2],
    detector: SidechainDetector,
    setup: Option<Setup>,
    free_beat: f64,
    spectrum: SpectrumFeed,
    clipped: bool,
    fresh: bool,
    cut_hz: [f32; 2],
}

const MAX_COMB_SECONDS: f64 = 1.0 / 20.0;

impl Default for AutoFilter {
    fn default() -> Self {
        let mut v = [0.0; PARAM_COUNT];
        for p in params() {
            v[p.id as usize] = p.default;
        }
        let max_comb = (48_000.0 * MAX_COMB_SECONDS) as usize + 8;
        let mut detector = SidechainDetector::new();
        detector.prepare(48_000.0, CONTROL_FRAMES);
        Self {
            v,
            sr: 48_000.0,
            filters: [ChannelFilter::new(max_comb), ChannelFilter::new(max_comb)],
            freq_log: Ramp::new(1_000f32.log2()),
            cut_log: [Ramp::new(1_000f32.log2()); 2],
            output: Smoother::new(1.0),
            mix: Smoother::new(1.0),
            lfo: Lfo::new(1),
            random: [RandomWave::new(3); 2],
            lfo_held: [0.0; 2],
            lfo_smooth: [0.0; 2],
            lfo_grid: StepGrid::default(),
            env_grid: StepGrid::default(),
            envs: [EnvState::default(); 2],
            detector,
            setup: None,
            free_beat: 0.0,
            spectrum: SpectrumFeed::new(48_000.0, 4096, 1024),
            clipped: false,
            fresh: true,
            cut_hz: [1_000.0; 2],
        }
    }
}

impl AutoFilter {
    fn build_setup(&mut self, tempo: f64) -> Setup {
        let get = |id: u32| self.v[id as usize];
        let shape = FilterShape::read(get);
        let drive_percent = get(DRIVE) as f32;
        let drive_db = drive_percent * 0.36;
        let lfo_mode = (get(LFO_MODE).round() as usize).min(LM_16TH);
        let lfo_beats_rate = SyncRate::from_index(get(LFO_BEATS)).beats();
        let lfo_16th_beats = get(LFO_SIXTEENTHS).round().max(1.0) * 0.25;
        let hz = match lfo_mode {
            LM_TIME => 1.0 / get(LFO_TIME).max(1e-3) as f32,
            _ => get(LFO_RATE).max(0.0) as f32,
        };
        let beats = match lfo_mode {
            LM_BEATS => Some(lfo_beats_rate),
            LM_16TH => Some(lfo_16th_beats),
            _ => None,
        };
        let wave = (get(LFO_WAVE).round() as usize).min(WAVE_SH);
        let random = wave == WAVE_WANDER || wave == WAVE_SH;
        let spin_mode = !random && get(LFO_STEREO).round() as usize == 1;
        let lfo_cycle_s = match beats {
            Some(b) => (b * 60.0 / tempo.max(1.0)) as f32,
            None => 1.0 / hz.max(1e-3),
        };
        let attack_ms = get(ENV_ATTACK) as f32;
        let release_ms = get(ENV_RELEASE) as f32;
        let sr = self.sr;
        let coeff = |ms: f32| (-1.0 / (ms.max(0.001) * 0.001 * sr)).exp();
        let env_sh =
            (get(ENV_SH_ON) >= 0.5).then(|| SyncRate::from_index(get(ENV_SH_RATE)).beats());
        Setup {
            shape,
            drive_db,
            drive_gain: db_to_gain(drive_db),
            clip: get(CLIP) >= 0.5,
            lfo_amount: (get(LFO_AMOUNT) as f32 * 0.01).clamp(0.0, 1.0),
            lfo_wave: wave,
            lfo_morph: (get(LFO_MORPH) as f32 * 0.01).clamp(0.0, 1.0),
            lfo_cycle_s,
            env_amount: (get(ENV_AMOUNT) as f32 * 0.01).clamp(-1.0, 1.0),
            env_sh,
            lfo_quant: (get(LFO_QUANT).round() as usize).min(QUANT_SH),
            lfo_steps: get(LFO_STEPS).round().max(2.0) as f32,
            lfo_sh_beats: SyncRate::from_index(get(LFO_SH_RATE)).beats(),
            use_external: get(SC_EXTERNAL) >= 0.5,
            mono_key: get(SC_MONO) >= 0.5,
            listen: get(SC_LISTEN) >= 0.5,
            settings: LfoSettings {
                rate: match beats {
                    Some(b) => LfoRate::Beats(b),
                    None => LfoRate::Hz(hz),
                },
                offset: if beats.is_some() {
                    get(LFO_OFFSET) as f32 / 360.0
                } else {
                    0.0
                },
                phase: if spin_mode {
                    0.0
                } else {
                    get(LFO_PHASE) as f32 / 360.0
                },
                spin: if spin_mode {
                    get(LFO_SPIN) as f32 * 0.01
                } else {
                    0.0
                },
                ..Default::default()
            },
            env: EnvSetup {
                attack_coeff: coeff(attack_ms),
                release_coeff: coeff(release_ms),
                attack_frames: (5.0 * attack_ms * 0.001 * sr) as u32,
                hold: get(ENV_HOLD) >= 0.5,
            },
        }
    }

    fn sidechain_settings(&self) -> SidechainSettings {
        let get = |id: u32| self.v[id as usize];
        let (eq, gain) = if get(SC_FILTER_ON) >= 0.5 {
            match get(SC_FILTER_TYPE).round() as usize {
                0 => (SidechainEq::LowShelf, get(SC_SHELF_GAIN)),
                1 => (SidechainEq::Peak, get(SC_PEAK_GAIN)),
                2 => (SidechainEq::HighShelf, get(SC_SHELF_GAIN)),
                3 => (SidechainEq::LowPass, 0.0),
                4 => (SidechainEq::BandPass, 0.0),
                5 => (SidechainEq::Notch, 0.0),
                _ => (SidechainEq::HighPass, 0.0),
            }
        } else {
            (SidechainEq::Off, 0.0)
        };
        SidechainSettings {
            gain_db: get(SC_GAIN) as f32,
            mix: get(SC_MIX) as f32 * 0.01,
            eq,
            eq_freq: get(SC_FILTER_FREQ) as f32,
            eq_q: get(SC_FILTER_Q) as f32,
            eq_gain_db: gain as f32,
            listen: get(SC_LISTEN) >= 0.5,
            ..Default::default()
        }
    }

    /// The LFO's bipolar output for a channel, at cycle position `phase`.
    fn lfo_wave(setup: &Setup, random: &mut RandomWave, phase: f32) -> f32 {
        let mut p = phase;
        if setup.lfo_quant == QUANT_STEPS {
            p = (p * setup.lfo_steps).floor() / setup.lfo_steps;
        }
        if setup.lfo_wave == WAVE_WANDER || setup.lfo_wave == WAVE_SH {
            random.update(p);
            return if setup.lfo_wave == WAVE_WANDER {
                random.glide(p)
            } else {
                random.held()
            };
        }
        if setup.lfo_morph > 0.0 {
            p = p.powf(1.0 + 3.0 * setup.lfo_morph);
        }
        match setup.lfo_wave {
            0 => sine(p),
            1 => triangle(p),
            2 => saw_down(p),
            3 => square(p),
            WAVE_RAMP_UP => p,
            WAVE_RAMP_DOWN => 1.0 - p,
            _ => saw_up(p),
        }
    }

    fn render(&mut self, audio: AudioBuffer, external: Option<AudioInput>, clock: &mut BeatClock) {
        let setup = self
            .setup
            .unwrap_or_else(|| self.build_setup(clock.beats_per_sample * 60.0 * self.sr as f64));
        if self.setup.is_none() {
            self.lfo.set(&setup.settings);
            self.detector.set(&self.sidechain_settings());
            self.setup = Some(setup);
        }
        self.freq_log.target = (self.v[FREQ as usize] as f32).log2();
        self.output.target = db_to_gain(self.v[OUTPUT as usize] as f32);
        self.mix.target = self.v[DRY_WET as usize] as f32 * 0.01;
        let shape = setup.shape;
        let use_circuit = shape.uses_circuit();
        let drive_active = !use_circuit && setup.drive_gain > 1.0;
        let drive_out = 1.0 / setup.drive_gain.sqrt();
        let needs_key = setup.env_amount != 0.0 || setup.listen;
        let frames = audio.frames();
        let (left, right) = (&mut *audio.left, &mut *audio.right);
        if self.fresh {
            self.fresh = false;
            self.freq_log.snap(self.freq_log.target);
            self.cut_log = [Ramp::new(self.freq_log.value); 2];
            self.output.reset();
            self.mix.reset();
            self.detector.reset();
        }
        let mut pos = 0;
        while pos < frames {
            let n = (frames - pos).min(CONTROL_FRAMES);
            self.freq_log.tick(n, 0.02, self.sr);
            let beat = if clock.playing {
                clock.beat
            } else {
                self.free_beat
            };

            // Follow the key signal.
            let (key_l, key_r) = (&left[pos..pos + n], &right[pos..pos + n]);
            let mut listen_ready = false;
            if needs_key {
                let ext = if setup.use_external {
                    sidechain_slice(external, pos, n)
                } else {
                    None
                };
                let key = self.detector.process(key_l, key_r, ext);
                listen_ready = true;
                for i in 0..n {
                    let (a, b) = (key.left[i].abs(), key.right[i].abs());
                    if setup.mono_key {
                        let peak = a.max(b);
                        self.envs[0].follow(peak, &setup.env);
                        self.envs[1] = self.envs[0];
                    } else {
                        self.envs[0].follow(a, &setup.env);
                        self.envs[1].follow(b, &setup.env);
                    }
                }
            }

            // The modulators.
            self.lfo.set_clock(clock);
            let phases = self.lfo.advance_phases(n);
            let lfo_due =
                setup.lfo_quant == QUANT_SH && self.lfo_grid.crossed(beat, setup.lfo_sh_beats);
            let env_due = setup
                .env_sh
                .is_some_and(|period| self.env_grid.crossed(beat, period));
            let smooth =
                1.0 - (-(n as f32) / (setup.lfo_morph * setup.lfo_cycle_s * self.sr + 1.0)).exp();

            for (ch, phase) in phases.into_iter().enumerate() {
                let mut wave = Self::lfo_wave(&setup, &mut self.random[ch], phase);
                if setup.lfo_quant == QUANT_SH {
                    if lfo_due {
                        self.lfo_held[ch] = wave;
                    }
                    wave = self.lfo_held[ch];
                }
                if setup.lfo_wave == WAVE_SH {
                    // The Morph control is Smooth for this shape.
                    self.lfo_smooth[ch] += (wave - self.lfo_smooth[ch]) * smooth;
                    wave = self.lfo_smooth[ch];
                }
                if env_due {
                    self.envs[ch].held = self.envs[ch].level;
                }
                let env = if setup.env_sh.is_some() {
                    self.envs[ch].held
                } else {
                    self.envs[ch].level
                }
                .min(1.0);

                let octaves =
                    wave * setup.lfo_amount * LFO_OCTAVES + env * setup.env_amount * ENV_OCTAVES;
                let cutoff_log = self.freq_log.value + octaves;
                let ramp = &mut self.cut_log[ch];
                ramp.target = cutoff_log;
                ramp.tick(n, 0.001, self.sr);
                let cutoff = 2f32.powf(ramp.value).clamp(20.0, self.sr * 0.49);
                self.cut_hz[ch] = cutoff;
                self.filters[ch].retune(&shape, cutoff, octaves, setup.drive_db, self.sr, n);
            }

            clock.advance(n);
            if !clock.playing {
                self.free_beat += clock.beats_per_sample * n as f64;
            }

            // The audio.
            let (filter_l, filter_r) = {
                let (a, b) = self.filters.split_at_mut(1);
                (&mut a[0], &mut b[0])
            };
            for i in pos..pos + n {
                let gain = self.output.next();
                let wet_mix = self.mix.next();
                for (filter, sample) in [
                    (&mut *filter_l, &mut left[i]),
                    (&mut *filter_r, &mut right[i]),
                ] {
                    let dry = *sample;
                    let x = if drive_active {
                        (dry * setup.drive_gain).tanh() * drive_out
                    } else {
                        dry
                    };
                    let mut y = filter.tick(&shape, x) * gain;
                    if setup.clip {
                        let clipped = soft_clip_output(y);
                        if clipped != y {
                            self.clipped = true;
                        }
                        y = clipped;
                    }
                    *sample = dry + (y - dry) * wet_mix;
                }
            }

            if listen_ready {
                self.detector.listen(&mut AudioBuffer {
                    left: &mut left[pos..pos + n],
                    right: &mut right[pos..pos + n],
                });
            }
            pos += n;
        }
    }
}

impl Processor for AutoFilter {
    fn sleeps_when_silent(&self) -> bool {
        true
    }

    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sr = sample_rate as f32;
        let max_comb = (sample_rate * MAX_COMB_SECONDS) as usize + 8;
        self.filters = [ChannelFilter::new(max_comb), ChannelFilter::new(max_comb)];
        for f in &mut self.filters {
            f.prepare(sample_rate);
        }
        self.output.set_time(0.01, self.sr);
        self.mix.set_time(0.01, self.sr);
        self.lfo.prepare(sample_rate);
        self.detector.prepare(sample_rate, CONTROL_FRAMES);
        self.spectrum = SpectrumFeed::new(sample_rate, 4096, 1024);
        self.spectrum.set_smoothing(0.7);
        self.setup = None;
        self.fresh = true;
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        let sidechain = ctx.sidechain;
        let mut clock = BeatClock::from_transport(ctx.transport);
        let mut pos = 0;
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => self.set_param(id, value),
            BlockStep::Audio(audio) => {
                let frames = audio.frames();
                self.render(audio, sidechain_slice(sidechain, pos, frames), &mut clock);
                pos += frames;
            }
            BlockStep::Midi(_) => {}
        });
        if ctx.view.is_active() {
            self.spectrum
                .process_stereo(ctx.audio.left, ctx.audio.right, ctx.view, SPECTRUM);
            ctx.view
                .publish(CLIP_LED, &[if self.clipped { 1.0 } else { 0.0 }]);
            ctx.view.publish(CUTOFFS, &self.cut_hz);
            ctx.view.publish(SAMPLE_RATE, &[self.sr]);
        }
        self.clipped = false;
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
            self.setup = None;
        }
    }

    fn reset(&mut self) {
        for filter in &mut self.filters {
            filter.reset();
        }
        self.envs = [EnvState::default(); 2];
        self.detector.reset();
        self.lfo.retrigger();
        self.lfo_smooth = [0.0; 2];
        self.lfo_held = [0.0; 2];
        self.spectrum.reset();
        self.fresh = true;
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.auto_filter",
    name: "Auto Filter",
    role: DeviceRole::AudioEffect,
    category: "EQ & Filters",
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
    create: |_| Box::<AutoFilter>::default(),
};
