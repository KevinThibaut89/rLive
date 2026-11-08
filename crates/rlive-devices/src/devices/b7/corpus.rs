//! Corpus: seven resonant bodies, two per channel (manual pages 589-595).
//!
//! Beam, Marimba, String, Membrane and Plate are banks of decaying oscillators, one per overtone,
//! from each body's modal frequencies. Pipe and Tube are waveguides. These are our own designs,
//! based on textbook vibration formulas; the manual describes behaviours, not models, and the only
//! figures it gives are percentages for Opening, Hit, Width, Pos. L/R and Off Decay and the
//! 0/180/360 degree Phase points. The ranges in the parameter list and the constants below are
//! ours.
//!
//! Tune is the fundamental of every body, at every Opening: Pipe's closed-to-open change shows up
//! as a change of timbre (odd harmonics to all harmonics), with its fundamental held in place.
//! Opening crossfades between a closed-end and an open waveguide tuned to the same pitch.

use super::tank::soft_limit;
use crate::dsp::delay::{InterpDelay, Interpolation};
use crate::dsp::lfo::{Lfo, LfoRate, LfoSettings, LfoShape};
use crate::dsp::tempo::{BeatClock, SyncRate};
use crate::dsp::util::{midi_to_hz, sanitize};
use crate::dsp::{Biquad, BiquadKind};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{
    db_to_gain, AudioBuffer, BlockStep, DeviceDesc, Item, MidiMessage, ParamId, ParamInfo, Ports,
    ProcessContext, Processor, Section, Status, Tail, ViewId,
};
use std::f32::consts::{PI, TAU};

pub const BODY: u32 = 0;
pub const QUALITY: u32 = 1;
pub const DECAY: u32 = 2;
pub const MATERIAL: u32 = 3;
pub const RADIUS: u32 = 4;
pub const BRIGHT: u32 = 5;
pub const INHARM: u32 = 6;
pub const OPENING: u32 = 7;
pub const RATIO: u32 = 8;
pub const HIT: u32 = 9;
pub const WIDTH: u32 = 10;
pub const POS_L: u32 = 11;
pub const POS_R: u32 = 12;
pub const TUNE: u32 = 13;
pub const TRANSPOSE: u32 = 14;
pub const FINE: u32 = 15;
pub const SPREAD: u32 = 16;
pub const LFO_ON: u32 = 17;
pub const LFO_AMOUNT: u32 = 18;
pub const LFO_SYNC: u32 = 19;
pub const LFO_RATE: u32 = 20;
pub const LFO_DIVISION: u32 = 21;
pub const LFO_WAVE: u32 = 22;
pub const LFO_STEREO: u32 = 23;
pub const LFO_PHASE: u32 = 24;
pub const LFO_SPIN: u32 = 25;
pub const LFO_OFFSET: u32 = 26;
pub const FILTER_ON: u32 = 27;
pub const FILTER_FREQ: u32 = 28;
pub const FILTER_BW: u32 = 29;
pub const BLEED: u32 = 30;
pub const GAIN: u32 = 31;
pub const MIX: u32 = 32;
pub const MIDI_FREQ: u32 = 33;
pub const PRIORITY: u32 = 34;
pub const PB_RANGE: u32 = 35;
pub const OFF_DECAY: u32 = 36;
pub const OFF_AMOUNT: u32 = 37;
const PARAMS: usize = 38;

pub const BEAM: usize = 0;
pub const MARIMBA: usize = 1;
pub const STRING: usize = 2;
pub const MEMBRANE: usize = 3;
pub const PLATE: usize = 4;
pub const PIPE: usize = 5;
pub const TUBE: usize = 6;

/// How many partials each Quality gives: the manual names only the ends of the chooser, Eco and High.
pub const QUALITY_MODES: [usize; 4] = [8, 14, 20, 32];
pub const MAX_MODES: usize = 32;
/// Free-free beam mode ratios (Euler-Bernoulli), the first five.
pub const BEAM_RATIOS: [f32; 5] = [1.0, 2.7565, 5.4039, 8.9329, 13.3444];
/// A marimba bar's tuned first partials: the fundamental, two octaves, and the tenth above that.
pub const MARIMBA_RATIOS: [f32; 3] = [1.0, 4.0, 10.0];
/// Full Spread moves each resonator this many semitones, in opposite directions.
pub const SPREAD_SEMITONES: f32 = 1.0;
/// Full LFO Amount, in semitones.
pub const LFO_SEMITONES: f32 = 12.0;
/// The Material exponent: a partial's decay time scales as `ratio ^ (material * MATERIAL_SLOPE)`.
pub const MATERIAL_SLOPE: f32 = 1.0;
/// The Bright exponent on a partial's amplitude, as `ratio ^ (bright * BRIGHT_SLOPE)`.
pub const BRIGHT_SLOPE: f32 = 0.7;
/// The Inharm exponent on a partial's ratio, as `ratio ^ (1 + inharm * INHARM_SLOPE)`.
pub const INHARM_SLOPE: f32 = 0.25;
/// How far Radius pulls Pipe and Tube's pitch down at its very top, as a fraction.
pub const RADIUS_PITCH_PULL: f32 = 0.2;
/// At full Off Decay, what the decay time is multiplied by once a note is released.
pub const OFF_DECAY_FLOOR: f32 = 0.002;
/// The limiter's ceiling, as a sample value.
pub const LIMIT_CEILING: f32 = 0.98;
const LIMIT_RELEASE_S: f32 = 0.15;
const CONTROL_FRAMES: usize = 16;
const MAX_HELD_NOTES: usize = 16;
const MODE_GRID: usize = 12;
const MAX_DECAY_S: f32 = 60.0;

pub const VIEW: ViewId = ViewId(0);
/// `[limiter active, tune MIDI note, 2 x MAX_MODES partial (ratio, level)]`.
pub const VIEW_LEN: usize = 2 + MAX_MODES * 2;

const BODIES: [&str; 7] = [
    "Beam", "Marimba", "String", "Membrane", "Plate", "Pipe", "Tube",
];
const WAVES: [&str; 7] = [
    "Sine",
    "Square",
    "Triangle",
    "Saw Up",
    "Saw Down",
    "Noise Stepped",
    "Noise Smooth",
];

fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::stepped(BODY, "Resonance Type", &BODIES, BEAM),
        ParamInfo::stepped(QUALITY, "Quality", &["Eco", "Low", "Mid", "High"], 2),
        ParamInfo::skewed(DECAY, "Decay", 10.0, 20_000.0, 800.0, "ms", 4.0),
        ParamInfo::continuous(MATERIAL, "Material", -100.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(RADIUS, "Radius", 0.0, 100.0, 50.0, "%"),
        ParamInfo::continuous(BRIGHT, "Bright", -100.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(INHARM, "Inharm", -100.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(OPENING, "Opening", 0.0, 100.0, 100.0, "%"),
        ParamInfo::skewed(RATIO, "Ratio", 0.2, 5.0, 1.0, "", 2.0),
        ParamInfo::continuous(HIT, "Hit", 0.0, 100.0, 20.0, "%"),
        ParamInfo::continuous(WIDTH, "Width", 0.0, 100.0, 100.0, "%"),
        ParamInfo::continuous(POS_L, "Pos. L", 0.0, 100.0, 20.0, "%"),
        ParamInfo::continuous(POS_R, "Pos. R", 0.0, 100.0, 35.0, "%"),
        ParamInfo::skewed(TUNE, "Tune", 20.0, 8_000.0, 220.0, "Hz", 3.0),
        ParamInfo::continuous(TRANSPOSE, "Transpose", -48.0, 48.0, 0.0, "st"),
        ParamInfo::continuous(FINE, "Fine", -100.0, 100.0, 0.0, "ct"),
        ParamInfo::continuous(SPREAD, "Spread", -100.0, 100.0, 0.0, "%"),
        ParamInfo::toggle(LFO_ON, "LFO", false),
        ParamInfo::continuous(LFO_AMOUNT, "LFO Amount", 0.0, 100.0, 0.0, "%"),
        ParamInfo::toggle(LFO_SYNC, "LFO Sync", false),
        ParamInfo::skewed(LFO_RATE, "LFO Rate", 0.01, 30.0, 1.0, "Hz", 3.0),
        ParamInfo::stepped(LFO_DIVISION, "LFO Division", &SyncRate::LABELS, 10),
        ParamInfo::stepped(LFO_WAVE, "LFO Wave", &WAVES, 0),
        ParamInfo::stepped(LFO_STEREO, "LFO Stereo", &["Phase", "Spin"], 0),
        ParamInfo::continuous(LFO_PHASE, "LFO Phase", 0.0, 360.0, 0.0, "deg"),
        ParamInfo::continuous(LFO_SPIN, "LFO Spin", 0.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(LFO_OFFSET, "LFO Offset", 0.0, 360.0, 0.0, "deg"),
        ParamInfo::toggle(FILTER_ON, "Filter", false),
        ParamInfo::skewed(FILTER_FREQ, "Freq", 20.0, 20_000.0, 1_000.0, "Hz", 3.0),
        ParamInfo::continuous(FILTER_BW, "Bdwidth", 0.1, 8.0, 3.0, "oct"),
        ParamInfo::continuous(BLEED, "Bleed", 0.0, 100.0, 0.0, "%"),
        ParamInfo::continuous(GAIN, "Gain", -24.0, 24.0, 0.0, "dB"),
        ParamInfo::continuous(MIX, "Dry/Wet", 0.0, 100.0, 100.0, "%"),
        ParamInfo::toggle(MIDI_FREQ, "Frequency", false),
        ParamInfo::stepped(PRIORITY, "Last/Low", &["Last", "Low"], 0),
        ParamInfo::continuous(PB_RANGE, "PB Range", 0.0, 48.0, 2.0, "st"),
        ParamInfo::toggle(OFF_DECAY, "Off Decay", false),
        ParamInfo::continuous(OFF_AMOUNT, "Off Decay Amount", 0.0, 100.0, 100.0, "%"),
    ]
}

fn knob(id: u32) -> Item {
    Item::Knob(ParamId(id))
}

fn toggle(id: u32) -> Item {
    Item::Toggle(ParamId(id))
}

fn menu(id: u32) -> Item {
    Item::Menu(ParamId(id))
}

fn layout() -> Vec<Section> {
    vec![
        Section {
            title: "Resonator",
            items: vec![
                Item::Panel("corpus_display"),
                menu(BODY),
                menu(QUALITY),
                knob(DECAY),
                knob(MATERIAL),
                knob(RADIUS),
                knob(BRIGHT),
                knob(INHARM),
                knob(OPENING),
                knob(RATIO),
                knob(HIT),
                knob(WIDTH),
                knob(POS_L),
                knob(POS_R),
                knob(TUNE),
                knob(TRANSPOSE),
                knob(FINE),
                knob(SPREAD),
            ],
        },
        Section {
            title: "LFO",
            items: vec![
                toggle(LFO_ON),
                knob(LFO_AMOUNT),
                toggle(LFO_SYNC),
                knob(LFO_RATE),
                menu(LFO_DIVISION),
                menu(LFO_WAVE),
                menu(LFO_STEREO),
                knob(LFO_PHASE),
                knob(LFO_SPIN),
                knob(LFO_OFFSET),
            ],
        },
        Section {
            title: "Filter",
            items: vec![toggle(FILTER_ON), knob(FILTER_FREQ), knob(FILTER_BW)],
        },
        Section {
            title: "Global",
            items: vec![knob(BLEED), knob(GAIN), knob(MIX)],
        },
        Section {
            title: "Sidechain",
            items: vec![
                toggle(MIDI_FREQ),
                menu(PRIORITY),
                knob(PB_RANGE),
                toggle(OFF_DECAY),
                knob(OFF_AMOUNT),
            ],
        },
    ]
}

/// One channel's bank of decaying oscillators, `z[k] = a[k] z[k] + g[k] x`.
struct Modes {
    re: [f32; MAX_MODES],
    im: [f32; MAX_MODES],
    a_re: [f32; MAX_MODES],
    a_im: [f32; MAX_MODES],
    excite: [f32; MAX_MODES],
    listen: [f32; MAX_MODES],
    count: usize,
}

impl Default for Modes {
    fn default() -> Self {
        Self {
            re: [0.0; MAX_MODES],
            im: [0.0; MAX_MODES],
            a_re: [0.0; MAX_MODES],
            a_im: [0.0; MAX_MODES],
            excite: [0.0; MAX_MODES],
            listen: [0.0; MAX_MODES],
            count: 0,
        }
    }
}

impl Modes {
    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let n = self.count;
        let mut out = 0.0;
        for k in 0..n {
            let re = self.a_re[k] * self.re[k] - self.a_im[k] * self.im[k] + self.excite[k] * x;
            let im = self.a_re[k] * self.im[k] + self.a_im[k] * self.re[k];
            self.re[k] = re;
            self.im[k] = im;
            out += re * self.listen[k];
        }
        out
    }

    fn clear(&mut self) {
        self.re = [0.0; MAX_MODES];
        self.im = [0.0; MAX_MODES];
    }
}

/// A waveguide loop: a delay and a loss filter closing back on itself, tuned so its fundamental is
/// exactly on pitch whatever the filter does. A negative loop `half` a period long has odd
/// harmonics only, which is how a pipe closed at one end sounds.
struct Comb {
    line: InterpDelay,
    loss: f32,
    state: f32,
    delay: f32,
    delay_target: f32,
    feedback: f32,
    input: f32,
    /// True for the half-length, sign-flipped loop.
    negative: bool,
}

impl Comb {
    fn new(negative: bool) -> Self {
        Self {
            line: InterpDelay::new(16),
            loss: 0.0,
            state: 0.0,
            delay: 16.0,
            delay_target: 16.0,
            feedback: 0.0,
            input: 1.0,
            negative,
        }
    }

    /// Tunes the loop to `hz`, with the given one-pole loss in it and a `ring` seconds decay.
    fn tune(&mut self, hz: f32, sr: f32, loss: f32, ring: f32) {
        let hz = hz.clamp(10.0, sr * 0.4);
        let w = TAU * hz / sr;
        let (filter_gain, filter_delay) = one_pole_response(loss, w);
        let length = if self.negative {
            sr / hz * 0.5
        } else {
            sr / hz
        };
        self.loss = loss;
        self.delay_target = (length - filter_delay).clamp(1.0, sr * 0.5);
        let pass = 10f32.powf(-3.0 * (length / sr) / ring.max(1e-3));
        let feedback = (pass / filter_gain.max(1e-3)).min(0.9995);
        self.feedback = if self.negative { -feedback } else { feedback };
        self.input = (1.0 - pass * pass).max(1e-6).sqrt();
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        self.delay += (self.delay_target - self.delay) * 0.05;
        let y = self
            .line
            .read((self.delay - 1.0).max(0.0), Interpolation::Cubic);
        self.state += (y - self.state) * (1.0 - self.loss);
        let next = sanitize(soft_limit(x * self.input + self.feedback * self.state));
        self.line.write(next);
        next
    }

    fn clear(&mut self) {
        self.line.clear();
        self.state = 0.0;
    }
}

/// Pipe and Tube: an open loop and a closed-end loop, each fully in tune, blended by Opening.
struct Waveguide {
    open: Comb,
    closed: Comb,
    open_gain: f32,
    closed_gain: f32,
}

impl Default for Waveguide {
    fn default() -> Self {
        Self {
            open: Comb::new(false),
            closed: Comb::new(true),
            open_gain: 1.0,
            closed_gain: 0.0,
        }
    }
}

impl Waveguide {
    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let mut out = 0.0;
        if self.open_gain > 0.0 {
            out += self.open.process(x) * self.open_gain;
        }
        if self.closed_gain > 0.0 {
            out += self.closed.process(x) * self.closed_gain;
        }
        out
    }

    fn clear(&mut self) {
        self.open.clear();
        self.closed.clear();
    }
}

pub struct Corpus {
    v: [f64; PARAMS],
    sr: f32,
    modes: [Modes; 2],
    guides: [Waveguide; 2],
    ratio: [f32; MAX_MODES],
    ring: [f32; MAX_MODES],
    excite_shape: [f32; MAX_MODES],
    listen_shape: [[f32; MAX_MODES]; 2],
    /// Each partial's level as it reaches the display: Bright and the normalisation included.
    level: [f32; MAX_MODES],
    mode_count: usize,
    lfo: Lfo,
    lfo_value: [f32; 2],
    bandpass: [Biquad; 2],
    limit_env: f32,
    limit_active: f32,
    limit_release: f32,
    frames: usize,
    dirty: bool,
    held: [u8; MAX_HELD_NOTES],
    held_count: usize,
    bend: f32,
    midi_note: Option<f32>,
    /// True once every note is up, for Off Decay.
    released: bool,
}

impl Default for Corpus {
    fn default() -> Self {
        let params = params();
        let mut v = [0.0; PARAMS];
        for p in &params {
            v[p.id as usize] = p.default;
        }
        Self {
            v,
            sr: 48_000.0,
            modes: Default::default(),
            guides: Default::default(),
            ratio: [1.0; MAX_MODES],
            ring: [0.1; MAX_MODES],
            excite_shape: [0.0; MAX_MODES],
            listen_shape: [[0.0; MAX_MODES]; 2],
            level: [0.0; MAX_MODES],
            mode_count: 0,
            lfo: Lfo::new(0xC0DE),
            lfo_value: [0.0; 2],
            bandpass: [Biquad::default(); 2],
            limit_env: 0.0,
            limit_active: 0.0,
            limit_release: 0.999,
            frames: 0,
            dirty: true,
            held: [0; MAX_HELD_NOTES],
            held_count: 0,
            bend: 0.0,
            midi_note: None,
            released: true,
        }
    }
}

/// A 1D mode shape where `x` runs from the centre (0) to the edge (1): the sine modes of a string
/// that's held at its ends.
#[inline]
fn shape_1d(k: usize, x: f32) -> f32 {
    (k as f32 * PI * (1.0 - x) * 0.5).sin()
}

/// How far Hit and Pos. go: 100% is very near the edge, so the body is still heard.
fn position(percent: f64) -> f32 {
    (percent as f32 * 0.01).clamp(0.0, 1.0) * 0.92
}

/// Fills `out` with the partial ratios of `body`, as many as fit, ascending.
fn body_ratios(
    body: usize,
    ratio_xy: f32,
    count: usize,
    out: &mut [f32; MAX_MODES],
    mn: &mut [(u8, u8); MAX_MODES],
) -> usize {
    match body {
        BEAM | MARIMBA | STRING => {
            for k in 0..count {
                let n = (k + 1) as f32;
                out[k] = match body {
                    BEAM => *BEAM_RATIOS
                        .get(k)
                        .unwrap_or(&((2.0 * n + 1.0) * (2.0 * n + 1.0) / 9.0)),
                    MARIMBA => {
                        if k < MARIMBA_RATIOS.len() {
                            MARIMBA_RATIOS[k]
                        } else {
                            10.0 + (n - 3.0).powf(1.6) * 8.0
                        }
                    }
                    _ => n * (1.0 + 0.000_5 * n * n).sqrt(),
                };
                mn[k] = (k as u8 + 1, 1);
            }
            count
        }
        _ => {
            // The lowest modes of a rectangle, by (m, n).
            let r2 = ratio_xy * ratio_xy;
            let f = |m: usize, n: usize| {
                let (m2, n2) = ((m * m) as f32, (n * n) as f32);
                let squared = (m2 + r2 * n2) / (1.0 + r2);
                if body == MEMBRANE {
                    squared.sqrt()
                } else {
                    squared
                }
            };
            let mut filled = 0usize;
            for m in 1..=MODE_GRID {
                for n in 1..=MODE_GRID {
                    let v = f(m, n);
                    let at = out[..filled].iter().position(|o| *o > v).unwrap_or(filled);
                    if at >= count {
                        continue;
                    }
                    let end = (filled + 1).min(count);
                    out.copy_within(at..end - 1, at + 1);
                    mn.copy_within(at..end - 1, at + 1);
                    out[at] = v;
                    mn[at] = (m as u8, n as u8);
                    filled = end;
                }
            }
            filled
        }
    }
}

impl Corpus {
    fn value(&self, id: u32) -> f32 {
        self.v[id as usize] as f32
    }

    fn flag(&self, id: u32) -> bool {
        self.v[id as usize] >= 0.5
    }

    fn body(&self) -> usize {
        (self.v[BODY as usize].round() as usize).min(TUBE)
    }

    fn is_tube(&self) -> bool {
        matches!(self.body(), PIPE | TUBE)
    }

    /// The resonator's fundamental before LFO and Spread, in Hz.
    fn base_hz(&self) -> f32 {
        match self.midi_note.filter(|_| self.flag(MIDI_FREQ)) {
            Some(note) => {
                let semis = self.value(TRANSPOSE)
                    + self.value(FINE) * 0.01
                    + self.bend * self.value(PB_RANGE);
                midi_to_hz(note + semis)
            }
            None => self.value(TUNE),
        }
    }

    /// The fundamental of each channel, after Spread and the LFO. For Pipe and Tube, also after
    /// Radius's pull on the pitch.
    fn channel_hz(&self) -> [f32; 2] {
        let base = self.base_hz();
        let spread = self.value(SPREAD) * 0.01 * SPREAD_SEMITONES;
        let amount = if self.flag(LFO_ON) {
            self.value(LFO_AMOUNT) * 0.01 * LFO_SEMITONES
        } else {
            0.0
        };
        let radius_factor = if self.is_tube() {
            let r = self.value(RADIUS) * 0.01;
            // Only very wide tubes: the pitch pull grows steeply near the top of the range.
            1.0 / (1.0 + RADIUS_PITCH_PULL * r.powi(12))
        } else {
            1.0
        };
        [
            base * radius_factor * 2f32.powf((spread + self.lfo_value[0] * amount) / 12.0),
            base * radius_factor * 2f32.powf((-spread + self.lfo_value[1] * amount) / 12.0),
        ]
    }

    fn decay_seconds(&self) -> f32 {
        let mut t = self.value(DECAY) * 0.001;
        if self.flag(OFF_DECAY) && self.released {
            let a = (self.value(OFF_AMOUNT) * 0.01).clamp(0.0, 1.0);
            t *= 1.0 - a * (1.0 - OFF_DECAY_FLOOR);
        }
        t.max(1e-3)
    }

    /// Worked out when a control moves: ratios, ring times and excitation shapes.
    fn analyse(&mut self) {
        let body = self.body();
        let quality = (self.v[QUALITY as usize].round() as usize).min(3);
        let count = QUALITY_MODES[quality];
        let mut ratios = [1.0f32; MAX_MODES];
        let mut mn = [(1u8, 1u8); MAX_MODES];
        let n = body_ratios(body, self.value(RATIO), count, &mut ratios, &mut mn);
        self.mode_count = n;
        let decay = self.decay_seconds();
        let material = (self.value(MATERIAL) * 0.01).clamp(-1.0, 1.0);
        let bright = (self.value(BRIGHT) * 0.01).clamp(-1.0, 1.0);
        let inharm = (self.value(INHARM) * 0.01).clamp(-1.0, 1.0);
        let hit = position(self.v[HIT as usize]);
        let pos = [
            position(self.v[POS_L as usize]),
            position(self.v[POS_R as usize]),
        ];
        let mut level = [0.0f32; MAX_MODES];
        let two_d = matches!(body, MEMBRANE | PLATE);
        let mut energy = [0.0f32; 2];
        for k in 0..n {
            let r = ratios[k].max(1e-3);
            let warped = r.powf(1.0 + inharm * INHARM_SLOPE);
            self.ratio[k] = warped;
            self.ring[k] = (decay * r.powf(material * MATERIAL_SLOPE)).min(MAX_DECAY_S);
            let (m, nn) = mn[k];
            let shape = |x: f32| {
                if two_d {
                    shape_1d(m as usize, x) * shape_1d(nn as usize, x)
                } else {
                    shape_1d(m as usize, x)
                }
            };
            let amp = if two_d {
                1.0 / (m as f32 * nn as f32).sqrt()
            } else {
                1.0 / (k as f32 + 1.0).sqrt()
            };
            level[k] = amp * r.powf(bright * BRIGHT_SLOPE);
            self.excite_shape[k] = shape(hit);
            for ch in 0..2 {
                self.listen_shape[ch][k] = shape(pos[ch]);
                let joint = level[k] * self.excite_shape[k] * self.listen_shape[ch][k];
                energy[ch] += joint * joint;
            }
        }
        // Fixed loudness whatever the shapes: Hit and Pos. change colour, not level.
        for (ch, e) in energy.iter().enumerate() {
            let g = 0.7 / e.max(1e-9).sqrt();
            for (k, l) in level.iter().enumerate().take(n) {
                self.modes[ch].excite[k] = l * self.excite_shape[k] * g;
                self.modes[ch].listen[k] = self.listen_shape[ch][k];
            }
            self.modes[ch].count = n;
        }
        self.level = level;
        for (f, _) in self.bandpass.iter_mut().zip(0..2) {
            let bw = self.v[FILTER_BW as usize] as f32;
            let q = 1.0 / (2.0 * (std::f32::consts::LN_2 * 0.5 * bw).sinh());
            f.set(
                BiquadKind::BandPass,
                self.v[FILTER_FREQ as usize] as f32,
                q,
                0.0,
                self.sr,
            );
        }
        self.dirty = false;
    }

    /// The modal coefficients for this control step.
    fn update_modes(&mut self, hz: [f32; 2]) {
        let nyquist = self.sr * 0.47;
        for (ch, modes) in self.modes.iter_mut().enumerate() {
            for k in 0..self.mode_count {
                let f = hz[ch] * self.ratio[k];
                if f >= nyquist {
                    modes.a_re[k] = 0.0;
                    modes.a_im[k] = 0.0;
                    continue;
                }
                let w = TAU * f / self.sr;
                let r = (-6.907_755 / (self.ring[k] * self.sr)).exp();
                let (s, c) = w.sin_cos();
                modes.a_re[k] = r * c;
                modes.a_im[k] = r * s;
            }
        }
    }

    fn update_guides(&mut self, hz: [f32; 2]) {
        let body = self.body();
        let radius = (self.value(RADIUS) * 0.01).clamp(0.0, 1.0);
        // A wider tube rings longer and keeps more of its top end.
        let ring = self.decay_seconds() * (0.35 + 1.65 * radius);
        let loss = (1.0 - radius) * 0.85;
        let opening = if body == TUBE {
            1.0
        } else {
            (self.value(OPENING) * 0.01).clamp(0.0, 1.0)
        };
        let sr = self.sr;
        for (ch, guide) in self.guides.iter_mut().enumerate() {
            guide.open_gain = opening;
            guide.closed_gain = 1.0 - opening;
            guide.open.tune(hz[ch], sr, loss, ring);
            guide.closed.tune(hz[ch], sr, loss, ring);
        }
    }

    fn lfo_settings(&self) -> LfoSettings {
        let shape = match self.v[LFO_WAVE as usize].round() as i32 {
            1 => LfoShape::Square,
            2 => LfoShape::Triangle,
            3 => LfoShape::SawUp,
            4 => LfoShape::SawDown,
            5 => LfoShape::SampleHold,
            6 => LfoShape::SmoothRandom,
            _ => LfoShape::Sine,
        };
        let noise = matches!(shape, LfoShape::SampleHold | LfoShape::SmoothRandom);
        let sync = self.flag(LFO_SYNC);
        let spinning = !sync && !noise && self.v[LFO_STEREO as usize].round() as i32 == 1;
        LfoSettings {
            shape,
            rate: if sync {
                LfoRate::Beats(SyncRate::from_index(self.v[LFO_DIVISION as usize]).beats())
            } else {
                LfoRate::Hz(self.value(LFO_RATE))
            },
            offset: if sync {
                self.value(LFO_OFFSET) / 360.0
            } else {
                0.0
            },
            phase: if spinning || noise {
                0.0
            } else {
                self.value(LFO_PHASE) / 360.0
            },
            spin: if spinning {
                self.value(LFO_SPIN) * 0.01
            } else {
                0.0
            },
        }
    }

    fn control(&mut self) {
        if self.flag(LFO_ON) {
            self.lfo.set(&self.lfo_settings());
            self.lfo_value = self.lfo.advance(CONTROL_FRAMES);
        } else {
            self.lfo_value = [0.0; 2];
        }
        let hz = self.channel_hz();
        if self.is_tube() {
            self.update_guides(hz);
        } else {
            self.update_modes(hz);
        }
    }

    fn render(&mut self, audio: AudioBuffer) {
        if self.dirty {
            self.analyse();
            self.control();
        }
        let tube = self.is_tube();
        let width = (self.value(WIDTH) * 0.01).clamp(0.0, 1.0);
        let bleed = if tube {
            0.0
        } else {
            (self.value(BLEED) * 0.01).clamp(0.0, 1.0)
        };
        let gain = db_to_gain(self.value(GAIN));
        let mix = (self.value(MIX) * 0.01).clamp(0.0, 1.0);
        let filter = self.flag(FILTER_ON);
        let (left, right) = (&mut *audio.left, &mut *audio.right);
        for i in 0..left.len() {
            self.frames += 1;
            if self.frames >= CONTROL_FRAMES {
                self.frames = 0;
                self.control();
            }
            let (dry_l, dry_r) = (left[i], right[i]);
            // Dry/Wet stops new input from going in; what's already ringing carries on.
            let input = [dry_l * mix, dry_r * mix];
            let mut wet = [0.0f32; 2];
            for ch in 0..2 {
                let mut y = if tube {
                    self.guides[ch].process(input[ch])
                } else {
                    self.modes[ch].process(input[ch])
                };
                if filter {
                    y = self.bandpass[ch].process(y);
                }
                wet[ch] = (y + input[ch] * bleed) * gain;
            }
            let (mid, side) = (0.5 * (wet[0] + wet[1]), 0.5 * (wet[0] - wet[1]) * width);
            let (mut l, mut r) = (mid + side, mid - side);
            // A peak limiter with an instant attack: it can't be overshot.
            let peak = l.abs().max(r.abs());
            self.limit_env = peak.max(self.limit_env * self.limit_release);
            if self.limit_env > LIMIT_CEILING {
                let g = LIMIT_CEILING / self.limit_env;
                l *= g;
                r *= g;
                self.limit_active = 1.0;
            }
            left[i] = dry_l * (1.0 - mix) + sanitize(l);
            right[i] = dry_r * (1.0 - mix) + sanitize(r);
        }
    }

    fn note_off(&mut self, key: u8) {
        if let Some(at) = self.held[..self.held_count].iter().position(|k| *k == key) {
            self.held.copy_within(at + 1..self.held_count, at);
            self.held_count -= 1;
        }
    }

    fn update_note(&mut self) {
        if self.held_count == 0 {
            return;
        }
        let keys = &self.held[..self.held_count];
        let note = if self.v[PRIORITY as usize].round() as i32 == 1 {
            keys.iter().copied().min().unwrap_or(keys[0])
        } else {
            keys[self.held_count - 1]
        };
        self.midi_note = Some(note as f32);
    }

    fn midi(&mut self, message: MidiMessage) {
        match message {
            MidiMessage::NoteOn { key, .. } => {
                self.note_off(key);
                if self.held_count == MAX_HELD_NOTES {
                    self.held.copy_within(1.., 0);
                    self.held_count -= 1;
                }
                self.held[self.held_count] = key;
                self.held_count += 1;
                self.update_note();
            }
            MidiMessage::NoteOff { key, .. } => {
                self.note_off(key);
                self.update_note();
            }
            MidiMessage::PitchBend { value, .. } => self.bend = value,
            MidiMessage::AllNotesOff => self.held_count = 0,
            _ => return,
        }
        let released = self.held_count == 0;
        if released != self.released {
            self.released = released;
            self.dirty = true;
        }
    }

    fn publish(&mut self, ctx: &ProcessContext) {
        if !ctx.view.is_active() {
            return;
        }
        let mut values = [0.0f32; VIEW_LEN];
        values[0] = self.limit_active;
        values[1] = self.midi_note.unwrap_or(f32::NAN);
        let peak = self.level[..self.mode_count]
            .iter()
            .fold(1e-6f32, |a, b| a.max(b.abs()));
        for k in 0..self.mode_count {
            values[2 + k * 2] = self.ratio[k];
            values[3 + k * 2] = (self.level[k] / peak).clamp(0.0, 1.0);
        }
        ctx.view.publish(VIEW, &values);
        self.limit_active = 0.0;
    }
}

/// `(gain, phase delay in samples)` of the one-pole `(1-a) / (1 - a z^-1)` at `w` rad/sample.
fn one_pole_response(a: f32, w: f32) -> (f32, f32) {
    let (s, c) = w.sin_cos();
    let denominator = (1.0 - 2.0 * a * c + a * a).max(1e-6);
    (
        (1.0 - a) / denominator.sqrt(),
        (a * s).atan2(1.0 - a * c) / w,
    )
}

impl Processor for Corpus {
    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sr = sample_rate as f32;
        self.limit_release = (-1.0 / (LIMIT_RELEASE_S * self.sr)).exp();
        let longest = (sample_rate as f32 / 5.0) as usize + 16;
        for g in self.guides.iter_mut() {
            g.open.line = InterpDelay::new(longest);
            g.closed.line = InterpDelay::new(longest);
        }
        self.lfo.prepare(sample_rate);
        self.dirty = true;
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        self.lfo
            .set_clock(&BeatClock::from_transport(ctx.transport));
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => {
                if let Some(v) = self.v.get_mut(id as usize) {
                    *v = value;
                    self.dirty = true;
                }
            }
            BlockStep::Midi(message) => self.midi(message),
            BlockStep::Audio(audio) => self.render(audio),
        });
        self.publish(ctx);
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
            self.dirty = true;
        }
    }

    fn reset(&mut self) {
        for ch in 0..2 {
            self.modes[ch].clear();
            self.guides[ch].clear();
            self.bandpass[ch].reset();
        }
        self.limit_env = 0.0;
        self.held_count = 0;
        self.released = true;
        self.dirty = true;
    }

    fn tail(&self) -> Tail {
        Tail::Infinite
    }
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.corpus",
    name: "Corpus",
    role: DeviceRole::AudioEffect,
    category: "Reverb & Resonance",
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
    create: |_| Box::<Corpus>::default(),
};
