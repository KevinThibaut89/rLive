//! Meld's oscillator types (manual pp. 747-749).
//!
//! The manual says twenty-four but describes twenty-five, and all twenty-five are here. It names
//! each type's macros; what they do in numbers (ranges, intervals, event rates) is ours.

use super::common::{
    edge_distance, major_scale, poly_blep, pulse, saw, transpose_degrees, triangle,
};
use crate::dsp::util::{lerp, sanitize, Rng};
use rlive_core::ScaleInfo;
use std::f32::consts::TAU;

pub const OSC_TYPES: [&str; 25] = [
    "Basic Shapes",
    "Dual Basic Shapes",
    "Noisy Shapes",
    "Square Sync",
    "Square 5th",
    "Sub",
    "Swarm Sine",
    "Swarm Triangle",
    "Swarm Saw",
    "Swarm Square",
    "Harmonic Fm",
    "Fold Fm",
    "Squelch",
    "Simple Fm",
    "Chip",
    "Shepard's Pi",
    "Tarp",
    "Extratone",
    "Noise Loop",
    "Filtered Noise",
    "Bitgrunge",
    "Crackle",
    "Rain",
    "Bubble",
    "Chord",
];

/// The labels of each type's two macros, as the manual names them.
pub const OSC_MACROS: [(&str, &str); 25] = [
    ("Shape", "Tone"),
    ("Shape", "Detune"),
    ("Shape", "Rough"),
    ("Freq 1", "Freq 2"),
    ("5th Amt", "P Width"),
    ("Tone", "Aux"),
    ("Motion", "Spacing"),
    ("Motion", "Spacing"),
    ("Motion", "Spacing"),
    ("Motion", "Spacing"),
    ("Amount", "Ratio"),
    ("Amount", "Shape"),
    ("Amount", "Feedback"),
    ("Amount", "Ratio"),
    ("Tone", "Rate"),
    ("Rate", "Width"),
    ("Decay", "Tone"),
    ("Pitch", "Env Amount"),
    ("Rate", "Fade"),
    ("Freq", "Width"),
    ("Freq", "Mult"),
    ("Density", "Intensity"),
    ("Tone", "Rate"),
    ("Density", "Spread"),
    ("Shape", "Inversion"),
];

pub const SWARM_SINE: usize = 6;
pub const CHIP: usize = 14;
pub const CHORD: usize = 24;
pub const DUAL_BASIC: usize = 1;

/// The types that follow the scale, marked with a flat-sharp in the manual.
pub fn is_scale_aware(kind: usize) -> bool {
    kind == DUAL_BASIC || (SWARM_SINE..SWARM_SINE + 4).contains(&kind) || kind == CHIP
}

/// What an oscillator is told at control rate.
#[derive(Clone, Copy)]
pub struct OscControl {
    pub kind: usize,
    pub note: f32,
    pub hz: f32,
    pub sample_rate: f32,
    pub macro1: f32,
    pub macro2: f32,
    /// The scale, when this oscillator should follow one.
    pub scale: Option<ScaleInfo>,
    /// The scale set in the song, for Chord, which follows it whenever Use Current Scale is on.
    pub song_scale: Option<ScaleInfo>,
    /// Whether Use Current Scale is on, which is what makes Chord scale-aware.
    pub use_current_scale: bool,
}

const SWARM_SETS: [[f32; 8]; 5] = [
    [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
    [0.0, 0.0, 12.0, 12.0, 0.0, 0.0, 12.0, 12.0],
    [0.0, 7.0, 12.0, 19.0, 0.0, 7.0, 12.0, 19.0],
    [0.0, 7.0, 12.0, 16.0, 19.0, 24.0, 28.0, 31.0],
    [0.0, 4.0, 7.0, 11.0, 14.0, 17.0, 21.0, 24.0],
];
const CHORD_SHAPES: [[i32; 4]; 6] = [
    [0, 4, 7, 12],
    [0, 3, 7, 10],
    [0, 4, 7, 11],
    [0, 5, 7, 12],
    [0, 7, 14, 21],
    [0, 3, 6, 9],
];
const CHIP_STEPS: [i32; 4] = [0, 4, 7, 12];
const POOL: usize = 12;

#[derive(Clone, Copy, Default)]
struct Blip {
    active: bool,
    phase: f32,
    hz: f32,
    rise: f32,
    amp: f32,
    decay: f32,
}

/// One engine's oscillator in one voice: every type's state, sized at construction.
#[derive(Clone)]
pub struct MeldOsc {
    phase: [f32; 8],
    detune: [f32; 8],
    gain: [f32; 8],
    ratios: [f32; 8],
    noise_table: Box<[f32; NOISE_LEN]>,
    rng: Rng,
    a: f32,
    b: f32,
    c: f32,
    feedback: f32,
    motion: f32,
    counter: f32,
    step: usize,
    shepard: f32,
    pool: [Blip; POOL],
    svf: [f32; 2],
    width: f32,
    amount: f32,
}

const NOISE_LEN: usize = 2048;

/// Sine through triangle and saw to square, with `shape` 0..1. `width` is a pulse width in 0..0.5.
#[inline]
pub fn morph_shape(phase: f32, dt: f32, shape: f32, width: f32) -> f32 {
    let x = shape.clamp(0.0, 1.0) * 3.0;
    let i = (x as usize).min(2);
    let t = x - i as f32;
    let at = |n: usize| match n {
        0 => (TAU * phase).sin(),
        1 => triangle(phase, dt, width.clamp(0.02, 0.98)),
        2 => {
            let w = width.clamp(0.02, 0.98);
            (saw(phase, dt) + saw((phase + w).fract(), dt)) * 0.5 + (1.0 - 2.0 * w) * 0.5
        }
        _ => pulse(phase, dt, width),
    };
    lerp(at(i), at(i + 1), t)
}

impl MeldOsc {
    pub fn new(seed: u32) -> Self {
        let mut rng = Rng::new(seed);
        let mut table = Box::new([0.0; NOISE_LEN]);
        for s in table.iter_mut() {
            *s = rng.bipolar();
        }
        let mut detune = [0.0; 8];
        for (i, d) in detune.iter_mut().enumerate() {
            *d = ((i as f32 + 1.0) * 0.618).fract();
        }
        Self {
            phase: [0.0; 8],
            detune,
            gain: [1.0; 8],
            ratios: [1.0; 8],
            noise_table: table,
            rng,
            a: 0.0,
            b: 0.0,
            c: 0.0,
            feedback: 0.0,
            motion: 0.0,
            counter: 0.0,
            step: 0,
            shepard: 0.0,
            pool: [Blip::default(); POOL],
            svf: [0.0; 2],
            width: 0.5,
            amount: 0.0,
        }
    }

    pub fn reset_phase(&mut self, start: f32) {
        for p in self.phase.iter_mut() {
            *p = start.rem_euclid(1.0);
        }
    }

    pub fn clear(&mut self) {
        self.svf = [0.0; 2];
        self.feedback = 0.0;
        self.pool = [Blip::default(); POOL];
        self.counter = 0.0;
        self.step = 0;
    }

    fn scale_ratio(c: &OscControl, semitones: f32) -> f32 {
        match c.scale {
            Some(scale) => {
                let degrees = (semitones * 7.0 / 12.0).round() as i32;
                let note = c.note.round() as i32;
                2f32.powf((transpose_degrees(note, degrees, scale) - note) as f32 / 12.0)
            }
            None => 2f32.powf(semitones / 12.0),
        }
    }

    /// What can be worked out once per control tick.
    #[allow(clippy::needless_range_loop)]
    pub fn control(&mut self, c: &OscControl, frames: f32) {
        let m1 = c.macro1.clamp(0.0, 1.0);
        let m2 = c.macro2.clamp(0.0, 1.0);
        let sr = c.sample_rate;
        match c.kind {
            DUAL_BASIC => {
                let ratio = if c.scale.is_some() {
                    Self::scale_ratio(c, m2 * 12.0)
                } else {
                    2f32.powf(m2 * m2 / 12.0)
                };
                self.ratios[0] = 1.0;
                self.ratios[1] = ratio;
            }
            3 => {
                self.ratios[0] = 2f32.powf(m1 * 2.0);
                self.ratios[1] = 2f32.powf(m2 * 3.0);
            }
            4 => self.width = 0.5 - 0.45 * m2,
            6..=9 => {
                let set = m2 * 4.0;
                let i = (set as usize).min(3);
                let t = set - i as f32;
                for v in 0..8 {
                    let from = SWARM_SETS[i][v];
                    let to = SWARM_SETS[i + 1][v];
                    let semi = if v % 2 == 0 { from } else { to };
                    self.ratios[v] = Self::scale_ratio(c, semi);
                    self.gain[v] = if v % 2 == 0 {
                        1.0 - 0.5 * t
                    } else {
                        0.5 + 0.5 * t
                    };
                }
                self.motion += frames / sr;
            }
            CHIP => {
                self.width = lerp(0.5, 0.125, m1);
                let rate = lerp(2.0, 60.0, m2 * m2);
                self.counter += rate * frames / sr;
                if self.counter >= 1.0 {
                    self.counter -= 1.0;
                    self.step = (self.step + 1) % CHIP_STEPS.len();
                }
                let semi = CHIP_STEPS[self.step] as f32 + 12.0 * (m1 * 2.0 - 1.0).round();
                self.ratios[0] = Self::scale_ratio(c, semi);
            }
            15 => {
                let rate = (m1 - 0.5) * 2.0;
                self.shepard += rate * frames / sr;
                self.shepard -= self.shepard.floor();
            }
            CHORD => {
                let shape = m1 * (CHORD_SHAPES.len() as f32 - 1.0);
                let i = shape.round() as usize;
                let inversion = (m2 * 3.0).round() as usize;
                let note = c.note.round() as i32;
                let scale = if c.use_current_scale {
                    Some(
                        c.song_scale
                            .unwrap_or(major_scale(note.rem_euclid(12) as u8)),
                    )
                } else {
                    None
                };
                for v in 0..4 {
                    let interval = CHORD_SHAPES[i][v];
                    let octave = if v < inversion { 12 } else { 0 };
                    let semis = match scale {
                        Some(s) => {
                            let degrees = (interval as f32 * 7.0 / 12.0).round() as i32;
                            transpose_degrees(note, degrees, s) - note + octave
                        }
                        None => interval + octave,
                    };
                    self.ratios[v] = 2f32.powf(semis as f32 / 12.0);
                }
            }
            _ => {}
        }
        self.amount = m1;
    }

    fn spawn(&mut self, hz: f32, rise: f32, amp: f32, seconds: f32, sr: f32) {
        if let Some(b) = self.pool.iter_mut().find(|b| !b.active) {
            *b = Blip {
                active: true,
                phase: 0.0,
                hz,
                rise,
                amp,
                decay: (-1.0 / (seconds.max(0.0005) * sr)).exp(),
            };
        }
    }

    #[inline]
    fn blips(&mut self, sr: f32) -> f32 {
        let mut sum = 0.0;
        for b in self.pool.iter_mut().filter(|b| b.active) {
            sum += (TAU * b.phase).sin() * b.amp;
            b.hz *= b.rise;
            b.phase = (b.phase + b.hz / sr).fract();
            b.amp *= b.decay;
            if b.amp < 1e-4 {
                b.active = false;
            }
        }
        sum
    }

    /// The next sample.
    #[inline]
    pub fn next(&mut self, c: &OscControl) -> f32 {
        let sr = c.sample_rate;
        let dt = (c.hz / sr).clamp(1e-6, 0.49);
        let m1 = c.macro1.clamp(0.0, 1.0);
        let m2 = c.macro2.clamp(0.0, 1.0);
        let out = match c.kind {
            0 => {
                let p = self.advance(0, dt);
                morph_shape(p, dt, m1, 0.5 - 0.45 * m2)
            }
            1 => {
                let (a, b) = (self.advance(0, dt), self.advance_ratio(1, dt));
                let d1 = dt * self.ratios[1];
                (morph_shape(a, dt, m1, 0.5) + morph_shape(b, d1, m1, 0.5)) * 0.5
            }
            2 => {
                let p = self.advance(0, dt);
                let w = morph_shape(p, dt, m1, 0.5);
                let n = self.rng.bipolar();
                w * (1.0 - m2) + w * n * m2
            }
            3 => self.square_sync(dt),
            4 => {
                let (a, b) = (self.advance(0, dt), self.advance_at(1, dt * 1.498_307));
                lerp(
                    pulse(a, dt, self.width),
                    pulse(b, dt * 1.498_307, self.width),
                    m1,
                )
            }
            5 => {
                let (p, sub) = (self.advance(0, dt), self.advance_at(1, dt * 0.5));
                lerp((TAU * p).sin(), pulse(p, dt, 0.5), m1) + m2 * (TAU * sub).sin()
            }
            6..=9 => self.swarm(c.kind - 6, dt, m1),
            10 => {
                let ratio = 1.0 + (m2 * 7.0).round();
                self.fm(dt, ratio, m1 * 8.0, 0.0, 0.0)
            }
            11 => {
                let ratio = 2.0;
                self.fm(dt, ratio, m1 * 8.0, m2, 0.0)
            }
            12 => self.fm(dt, 1.0, m1 * 8.0, 0.0, m2),
            13 => self.fm(dt, lerp(0.5, 8.0, m2), m1 * 6.0, 0.0, 0.0),
            CHIP => {
                let d = dt * self.ratios[0];
                let p = self.advance_at(0, d);
                pulse(p, d, self.width)
            }
            15 => self.shepard(dt, m2),
            16 => self.tarp(dt, m1, m2, sr),
            17 => self.extratone(dt, m1, m2),
            18 => self.noise_loop(dt, m1, m2),
            19 => self.filtered_noise(dt, m1, m2),
            20 => self.bitgrunge(dt, m1, m2),
            21 => self.crackle(m1, m2, sr),
            22 => self.rain(c, m1, m2, sr),
            23 => self.bubble(c, m1, m2, sr),
            CHORD => {
                let mut s = 0.0;
                for v in 0..4 {
                    let d = dt * self.ratios[v];
                    s += saw(self.advance_at(v, d), d);
                }
                s * 0.35
            }
            _ => 0.0,
        };
        sanitize(out)
    }

    #[inline]
    fn advance(&mut self, i: usize, dt: f32) -> f32 {
        self.advance_at(i, dt)
    }

    #[inline]
    fn advance_ratio(&mut self, i: usize, dt: f32) -> f32 {
        self.advance_at(i, dt * self.ratios[i])
    }

    #[inline]
    fn advance_at(&mut self, i: usize, dt: f32) -> f32 {
        self.phase[i] += dt;
        if self.phase[i] >= 1.0 {
            self.phase[i] -= 1.0;
        }
        self.phase[i]
    }

    #[inline]
    fn square_sync(&mut self, dt: f32) -> f32 {
        let dm = (dt * self.ratios[0]).min(0.45);
        let ds = (dt * self.ratios[1]).min(0.45);
        let (pm, ps) = (self.phase[0], self.phase[1]);
        let mut y = pulse(ps, ds, 0.5);
        let d = edge_distance(pm, 0.0, dm);
        if d.abs() < 2.0 && self.ratios[1] > 1.001 {
            let from = if d < 0.0 {
                (ps - d * ds).fract()
            } else {
                self.c
            };
            let before = if from < 0.5 { 1.0 } else { -1.0 };
            y += (1.0 - before) * poly_blep(d);
        }
        self.phase[1] += ds;
        if self.phase[1] >= 1.0 {
            self.phase[1] -= 1.0;
        }
        self.phase[0] += dm;
        if self.phase[0] >= 1.0 {
            self.phase[0] -= 1.0;
            self.c = self.phase[1];
            self.phase[1] = self.phase[0] / dm * ds;
        }
        y
    }

    #[inline]
    fn swarm(&mut self, wave: usize, dt: f32, motion: f32) -> f32 {
        let mut sum = 0.0;
        for v in 0..8 {
            let drift = 1.0
                + motion
                    * 0.02
                    * (TAU * (self.motion * (0.2 + self.detune[v]) + self.detune[v])).sin()
                + (self.detune[v] - 0.5) * 0.006;
            let d = dt * self.ratios[v] * drift;
            let p = self.advance_at(v, d);
            let w = match wave {
                0 => (TAU * p).sin(),
                1 => triangle(p, d, 0.5),
                2 => saw(p, d),
                _ => pulse(p, d, 0.5),
            };
            sum += w * self.gain[v];
        }
        sum * 0.2
    }

    /// Two-operator FM. The index is held back so the sidebands stay below Nyquist.
    #[inline]
    fn fm(&mut self, dt: f32, ratio: f32, index: f32, fold: f32, feedback: f32) -> f32 {
        let dm = dt * ratio;
        let cap = (((0.45 - dt) / dm.max(1e-6)) - 1.0).max(0.0);
        let idx = index.min(cap);
        let carrier = self.advance_at(0, dt);
        let modulator = self.advance_at(1, dm);
        let phase = TAU * modulator + feedback * 3.0 * self.feedback;
        self.feedback = 0.5 * (self.feedback + phase.sin());
        let y = (TAU * carrier + idx * self.feedback).sin();
        if fold > 0.0 {
            (y * (1.0 + 3.0 * fold)).sin()
        } else {
            y
        }
    }

    #[inline]
    fn shepard(&mut self, dt: f32, width: f32) -> f32 {
        let octaves = 2.0 + (width * 6.0).round();
        let count = (octaves as usize).min(8);
        let mut sum = 0.0;
        for i in 0..count {
            let t = (self.shepard + i as f32) / octaves;
            let d = (dt * (2f32.powf(t * octaves - octaves * 0.5))).min(0.45);
            let amp = 0.5 - 0.5 * (TAU * t).cos();
            let p = self.advance_at(i, d);
            sum += (TAU * p).sin() * amp;
        }
        sum * 0.5
    }

    #[inline]
    fn tarp(&mut self, dt: f32, decay: f32, tone: f32, sr: f32) -> f32 {
        // A noise-excited string of two resonances.
        self.counter += 1.0;
        let period = lerp(0.3, 0.05, tone) * sr;
        if self.counter > period {
            self.counter = 0.0;
            self.a = 1.0;
        }
        self.a *= 1.0 - 0.002;
        let excite = self.rng.bipolar() * self.a;
        let r = lerp(0.99, 0.9997, decay);
        let w = TAU * dt;
        let cos = 2.0 * r * w.cos();
        let y = cos * self.svf[0] - r * r * self.svf[1] + excite * (1.0 - r);
        self.svf[1] = self.svf[0];
        self.svf[0] = sanitize(y);
        let body = self.svf[0];
        lerp(body * 4.0, body * 4.0 + excite * 0.2, tone).clamp(-2.0, 2.0)
    }

    #[inline]
    fn extratone(&mut self, dt: f32, pitch: f32, env_amt: f32) -> f32 {
        let rate = lerp(0.002, 0.02, pitch);
        self.counter += rate;
        if self.counter >= 1.0 {
            self.counter -= 1.0;
        }
        let env = 1.0 - self.counter;
        let shaped = env * env;
        let d = (dt * 2f32.powf(env_amt * shaped * 4.0)).min(0.45);
        let p = self.advance_at(0, d);
        (TAU * p).sin() * shaped
    }

    #[inline]
    fn noise_loop(&mut self, dt: f32, rate: f32, fade: f32) -> f32 {
        let p = self.advance_at(0, dt);
        let at = p * NOISE_LEN as f32;
        let i = at as usize % NOISE_LEN;
        let t = at - at.floor();
        let (a, b) = (self.noise_table[i], self.noise_table[(i + 1) % NOISE_LEN]);
        let y = lerp(a, b, t);
        self.counter += rate * rate * 0.01;
        if self.counter >= 1.0 {
            self.counter -= 1.0;
            let chunk = (self.rng.unipolar() * NOISE_LEN as f32) as usize;
            self.b = self.rng.bipolar();
            for k in 0..16 {
                self.noise_table[(chunk + k) % NOISE_LEN] = self.rng.bipolar();
            }
        }
        lerp(y, y * (1.0 + self.b).abs() + self.rng.bipolar() * 0.3, fade)
    }

    #[inline]
    fn filtered_noise(&mut self, dt: f32, freq: f32, width: f32) -> f32 {
        let centre = (dt * 2f32.powf((freq - 0.5) * 8.0)).clamp(0.0005, 0.45);
        let g = (std::f32::consts::PI * centre).tan();
        let k = lerp(0.05, 1.8, width);
        let n = self.rng.bipolar();
        let a1 = 1.0 / (1.0 + g * (g + k));
        let v1 = a1 * self.svf[0] + a1 * g * (n - self.svf[1]);
        let v2 = self.svf[1] + g * v1;
        self.svf[0] = 2.0 * v1 - self.svf[0];
        self.svf[1] = 2.0 * v2 - self.svf[1];
        sanitize(v1) * 4.0 * (k + 0.3).sqrt()
    }

    #[inline]
    fn bitgrunge(&mut self, dt: f32, freq: f32, mult: f32) -> f32 {
        let base = (dt * 2f32.powf((freq - 0.5) * 4.0)).min(0.45);
        // At the top no sub-octaves, at the bottom as many as there are slots.
        let subs = ((1.0 - mult) * 7.0).round() as usize;
        let mut sum = 0.0;
        for i in 0..=subs.min(7) {
            let d = base / (1u32 << i) as f32;
            sum += pulse(self.advance_at(i, d), d, 0.5);
        }
        sum / (subs.min(7) + 1) as f32
    }

    #[inline]
    fn crackle(&mut self, density: f32, intensity: f32, sr: f32) -> f32 {
        let rate = lerp(1.0, 800.0, density * density);
        if self.rng.unipolar() < rate / sr {
            let scale = 1.0 - intensity * self.rng.unipolar();
            self.a = self.rng.bipolar() * scale;
            self.b = lerp(0.2, 0.95, self.rng.unipolar() * (1.0 - intensity * 0.8));
        }
        self.c = self.c * self.b + self.a;
        self.a = 0.0;
        sanitize(self.c)
    }

    #[inline]
    fn rain(&mut self, c: &OscControl, tone: f32, rate: f32, sr: f32) -> f32 {
        let density = lerp(4.0, 240.0, rate * rate);
        if self.rng.unipolar() < density / sr {
            let tuned = c.hz * (1.0 + (self.rng.unipolar() * 4.0).floor());
            let hz = lerp(lerp(800.0, 5_000.0, self.rng.unipolar()), tuned, tone);
            self.spawn(hz, 1.0, 0.5, lerp(0.004, 0.05, tone), sr);
        }
        let wind = self.rng.bipolar();
        self.svf[0] += 0.06 * (wind - self.svf[0]);
        self.blips(sr) + self.svf[0] * lerp(0.4, 0.15, tone)
    }

    #[inline]
    fn bubble(&mut self, c: &OscControl, density: f32, spread: f32, sr: f32) -> f32 {
        let rate = lerp(1.0, 60.0, density * density);
        if self.rng.unipolar() < rate / sr {
            let size = 1.0 + spread * (self.rng.unipolar() * 3.0);
            self.spawn(c.hz * size, 1.0 + 8.0 / sr, 0.5, 0.05 + 0.06 * spread, sr);
        }
        self.blips(sr)
    }
}
