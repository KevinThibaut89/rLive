//! Built-in audio effects.

use crate::dsp::{soft_clip, Biquad, BiquadKind, DelayLine, Smoother};
use rlive_core::{db_to_gain, gain_to_db, ParamInfo, ProcessContext, Processor};

fn defaults(params: &[ParamInfo]) -> Vec<f64> {
    let max = params.iter().map(|p| p.id as usize).max().unwrap_or(0);
    let mut v = vec![0.0; max + 1];
    for p in params {
        v[p.id as usize] = p.default;
    }
    v
}

// ---------------------------------------------------------------- Delay

pub mod delay {
    pub const SYNC: u32 = 0;
    pub const TIME: u32 = 1;
    pub const DIVISION: u32 = 2;
    pub const FEEDBACK: u32 = 3;
    pub const MIX: u32 = 4;
    pub const PING_PONG: u32 = 5;
    pub const TONE: u32 = 6;
    pub const DIVISIONS: [&str; 7] = ["1/16", "1/8", "1/8.", "1/4", "1/4.", "1/2", "1 Bar"];
    /// Division lengths in quarter notes.
    pub const DIVISION_BEATS: [f64; 7] = [0.25, 0.5, 0.75, 1.0, 1.5, 2.0, 4.0];
}

pub fn delay_params() -> Vec<ParamInfo> {
    use delay::*;
    vec![
        ParamInfo::toggle(SYNC, "Sync", true),
        ParamInfo::skewed(TIME, "Time", 1.0, 2_000.0, 375.0, "ms", 2.0),
        ParamInfo::stepped(DIVISION, "Division", &DIVISIONS, 2),
        ParamInfo::continuous(FEEDBACK, "Feedback", 0.0, 0.95, 0.4, ""),
        ParamInfo::continuous(MIX, "Dry/Wet", 0.0, 1.0, 0.3, ""),
        ParamInfo::toggle(PING_PONG, "Ping Pong", true),
        ParamInfo::skewed(TONE, "Tone", 200.0, 20_000.0, 6_000.0, "Hz", 3.0),
    ]
}

pub struct Delay {
    v: Vec<f64>,
    sr: f32,
    lines: [DelayLine; 2],
    time: Smoother,
    tone: [Biquad; 2],
    tone_dirty: bool,
}

impl Default for Delay {
    fn default() -> Self {
        Self {
            v: defaults(&delay_params()),
            sr: 48_000.0,
            lines: [DelayLine::default(), DelayLine::default()],
            time: Smoother::new(0.0),
            tone: [Biquad::default(); 2],
            tone_dirty: true,
        }
    }
}

impl Processor for Delay {
    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sr = sample_rate as f32;
        let max = (sample_rate * 4.5) as usize;
        self.lines = [DelayLine::new(max), DelayLine::new(max)];
        self.time.set_time(0.08, self.sr);
        self.tone_dirty = true;
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        use delay::*;
        if self.tone_dirty {
            for f in self.tone.iter_mut() {
                f.set(
                    BiquadKind::LowPass,
                    self.v[TONE as usize] as f32,
                    0.707,
                    0.0,
                    self.sr,
                );
            }
            self.tone_dirty = false;
        }
        let seconds = if self.v[SYNC as usize] >= 0.5 {
            let div = (self.v[DIVISION as usize].round() as usize).min(DIVISION_BEATS.len() - 1);
            DIVISION_BEATS[div] * 60.0 / ctx.transport.tempo
        } else {
            self.v[TIME as usize] / 1000.0
        };
        self.time.target = (seconds as f32 * self.sr).min(self.sr * 4.4);
        if self.time.value == 0.0 {
            self.time.reset();
        }
        let fb = self.v[FEEDBACK as usize] as f32;
        let mix = self.v[MIX as usize] as f32;
        let ping = self.v[PING_PONG as usize] >= 0.5;
        let (left, right) = (&mut *ctx.audio.left, &mut *ctx.audio.right);
        for i in 0..left.len() {
            let d = self.time.next();
            let dl = self.tone[0].process(self.lines[0].read(d));
            let dr = self.tone[1].process(self.lines[1].read(d));
            let (inl, inr) = (left[i], right[i]);
            if ping {
                let mono = (inl + inr) * 0.5;
                self.lines[0].write(mono + dr * fb);
                self.lines[1].write(dl * fb);
            } else {
                self.lines[0].write(inl + dl * fb);
                self.lines[1].write(inr + dr * fb);
            }
            left[i] = inl * (1.0 - mix) + dl * mix;
            right[i] = inr * (1.0 - mix) + dr * mix;
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
        }
        if id == delay::TONE {
            self.tone_dirty = true;
        }
    }

    fn reset(&mut self) {
        for l in self.lines.iter_mut() {
            l.clear();
        }
    }
}

// ---------------------------------------------------------------- Reverb (Freeverb)

pub mod reverb {
    pub const SIZE: u32 = 0;
    pub const DAMPING: u32 = 1;
    pub const WIDTH: u32 = 2;
    pub const MIX: u32 = 3;
    pub const PREDELAY: u32 = 4;
}

pub fn reverb_params() -> Vec<ParamInfo> {
    use reverb::*;
    vec![
        ParamInfo::continuous(SIZE, "Size", 0.0, 1.0, 0.7, ""),
        ParamInfo::continuous(DAMPING, "Damping", 0.0, 1.0, 0.5, ""),
        ParamInfo::continuous(WIDTH, "Width", 0.0, 1.0, 1.0, ""),
        ParamInfo::continuous(MIX, "Dry/Wet", 0.0, 1.0, 0.25, ""),
        ParamInfo::continuous(PREDELAY, "Pre-delay", 0.0, 200.0, 10.0, "ms"),
    ]
}

const COMB_TUNING: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const ALLPASS_TUNING: [usize; 4] = [556, 441, 341, 225];
const STEREO_SPREAD: usize = 23;

#[derive(Default)]
struct Comb {
    buf: Vec<f32>,
    pos: usize,
    store: f32,
}

impl Comb {
    #[inline]
    fn process(&mut self, x: f32, feedback: f32, damp: f32) -> f32 {
        let y = self.buf[self.pos];
        self.store = y * (1.0 - damp) + self.store * damp;
        self.buf[self.pos] = x + self.store * feedback;
        self.pos = (self.pos + 1) % self.buf.len();
        y
    }
}

#[derive(Default)]
struct Allpass {
    buf: Vec<f32>,
    pos: usize,
}

impl Allpass {
    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let b = self.buf[self.pos];
        let y = b - x;
        self.buf[self.pos] = x + b * 0.5;
        self.pos = (self.pos + 1) % self.buf.len();
        y
    }
}

pub struct Reverb {
    v: Vec<f64>,
    sr: f32,
    combs: [Vec<Comb>; 2],
    allpasses: [Vec<Allpass>; 2],
    predelay: [DelayLine; 2],
}

impl Default for Reverb {
    fn default() -> Self {
        Self {
            v: defaults(&reverb_params()),
            sr: 48_000.0,
            combs: [Vec::new(), Vec::new()],
            allpasses: [Vec::new(), Vec::new()],
            predelay: [DelayLine::default(), DelayLine::default()],
        }
    }
}

impl Processor for Reverb {
    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sr = sample_rate as f32;
        let scale = sample_rate / 44_100.0;
        for ch in 0..2 {
            let spread = ch * STEREO_SPREAD;
            self.combs[ch] = COMB_TUNING
                .iter()
                .map(|&t| Comb {
                    buf: vec![0.0; (((t + spread) as f64) * scale) as usize],
                    pos: 0,
                    store: 0.0,
                })
                .collect();
            self.allpasses[ch] = ALLPASS_TUNING
                .iter()
                .map(|&t| Allpass {
                    buf: vec![0.0; (((t + spread) as f64) * scale) as usize],
                    pos: 0,
                })
                .collect();
            self.predelay[ch] = DelayLine::new((sample_rate * 0.25) as usize);
        }
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        use reverb::*;
        if self.combs[0].is_empty() {
            return;
        }
        let feedback = 0.7 + self.v[SIZE as usize] as f32 * 0.28;
        let damp = self.v[DAMPING as usize] as f32 * 0.4;
        let width = self.v[WIDTH as usize] as f32;
        let mix = self.v[MIX as usize] as f32;
        let pre = (self.v[PREDELAY as usize] as f32 * 0.001 * self.sr).max(1.0);
        let wet1 = width / 2.0 + 0.5;
        let wet2 = (1.0 - width) / 2.0;
        let (left, right) = (&mut *ctx.audio.left, &mut *ctx.audio.right);
        for i in 0..left.len() {
            let (inl, inr) = (left[i], right[i]);
            let input = (inl + inr) * 0.015;
            self.predelay[0].write(input);
            let x = self.predelay[0].read(pre);
            let mut out = [0f32; 2];
            for ch in 0..2 {
                let mut acc = 0.0;
                for c in self.combs[ch].iter_mut() {
                    acc += c.process(x, feedback, damp);
                }
                for a in self.allpasses[ch].iter_mut() {
                    acc = a.process(acc);
                }
                out[ch] = acc;
            }
            let wl = out[0] * wet1 + out[1] * wet2;
            let wr = out[1] * wet1 + out[0] * wet2;
            left[i] = inl * (1.0 - mix) + wl * mix;
            right[i] = inr * (1.0 - mix) + wr * mix;
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
        }
    }

    fn reset(&mut self) {
        for ch in 0..2 {
            for c in self.combs[ch].iter_mut() {
                c.buf.fill(0.0);
                c.store = 0.0;
            }
            for a in self.allpasses[ch].iter_mut() {
                a.buf.fill(0.0);
            }
            self.predelay[ch].clear();
        }
    }
}

// ---------------------------------------------------------------- Compressor

pub mod comp {
    pub const THRESHOLD: u32 = 0;
    pub const RATIO: u32 = 1;
    pub const ATTACK: u32 = 2;
    pub const RELEASE: u32 = 3;
    pub const KNEE: u32 = 4;
    pub const MAKEUP: u32 = 5;
    pub const MIX: u32 = 6;
}

pub fn compressor_params() -> Vec<ParamInfo> {
    use comp::*;
    vec![
        ParamInfo::continuous(THRESHOLD, "Threshold", -60.0, 0.0, -18.0, "dB"),
        ParamInfo::skewed(RATIO, "Ratio", 1.0, 20.0, 4.0, ":1", 2.0),
        ParamInfo::skewed(ATTACK, "Attack", 0.1, 200.0, 10.0, "ms", 3.0),
        ParamInfo::skewed(RELEASE, "Release", 5.0, 2_000.0, 120.0, "ms", 3.0),
        ParamInfo::continuous(KNEE, "Knee", 0.0, 18.0, 6.0, "dB"),
        ParamInfo::continuous(MAKEUP, "Makeup", 0.0, 24.0, 0.0, "dB"),
        ParamInfo::continuous(MIX, "Dry/Wet", 0.0, 1.0, 1.0, ""),
    ]
}

pub struct Compressor {
    v: Vec<f64>,
    sr: f32,
    env_db: f32,
}

impl Default for Compressor {
    fn default() -> Self {
        Self {
            v: defaults(&compressor_params()),
            sr: 48_000.0,
            env_db: 0.0,
        }
    }
}

impl Compressor {
    fn gain_reduction(&self, level_db: f32) -> f32 {
        use comp::*;
        let t = self.v[THRESHOLD as usize] as f32;
        let r = self.v[RATIO as usize] as f32;
        let k = self.v[KNEE as usize] as f32;
        let over = level_db - t;
        let slope = 1.0 - 1.0 / r.max(1.0);
        if k > 0.0 && over.abs() <= k / 2.0 {
            slope * (over + k / 2.0).powi(2) / (2.0 * k)
        } else if over > 0.0 {
            slope * over
        } else {
            0.0
        }
    }
}

impl Processor for Compressor {
    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sr = sample_rate as f32;
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        use comp::*;
        let att = (-1.0 / (self.v[ATTACK as usize] as f32 * 0.001 * self.sr)).exp();
        let rel = (-1.0 / (self.v[RELEASE as usize] as f32 * 0.001 * self.sr)).exp();
        let makeup = self.v[MAKEUP as usize] as f32;
        let mix = self.v[MIX as usize] as f32;
        let (left, right) = (&mut *ctx.audio.left, &mut *ctx.audio.right);
        for i in 0..left.len() {
            let (l, r) = (left[i], right[i]);
            let level = gain_to_db(l.abs().max(r.abs()));
            let gr = self.gain_reduction(level);
            let coeff = if gr > self.env_db { att } else { rel };
            self.env_db = gr + (self.env_db - gr) * coeff;
            let g = db_to_gain(makeup - self.env_db);
            left[i] = l * (1.0 - mix) + l * g * mix;
            right[i] = r * (1.0 - mix) + r * g * mix;
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
        }
    }

    fn reset(&mut self) {
        self.env_db = 0.0;
    }
}

// ---------------------------------------------------------------- EQ Three

pub mod eq {
    pub const LOW_GAIN: u32 = 0;
    pub const LOW_FREQ: u32 = 1;
    pub const MID_GAIN: u32 = 2;
    pub const MID_FREQ: u32 = 3;
    pub const MID_Q: u32 = 4;
    pub const HIGH_GAIN: u32 = 5;
    pub const HIGH_FREQ: u32 = 6;
    pub const OUTPUT: u32 = 7;
}

pub fn eq_params() -> Vec<ParamInfo> {
    use eq::*;
    vec![
        ParamInfo::continuous(LOW_GAIN, "Low", -24.0, 12.0, 0.0, "dB"),
        ParamInfo::skewed(LOW_FREQ, "Low Freq", 30.0, 1_000.0, 200.0, "Hz", 2.5),
        ParamInfo::continuous(MID_GAIN, "Mid", -24.0, 12.0, 0.0, "dB"),
        ParamInfo::skewed(MID_FREQ, "Mid Freq", 100.0, 10_000.0, 1_000.0, "Hz", 2.5),
        ParamInfo::skewed(MID_Q, "Mid Q", 0.1, 10.0, 0.8, "", 2.0),
        ParamInfo::continuous(HIGH_GAIN, "High", -24.0, 12.0, 0.0, "dB"),
        ParamInfo::skewed(
            HIGH_FREQ,
            "High Freq",
            1_000.0,
            18_000.0,
            4_000.0,
            "Hz",
            2.5,
        ),
        ParamInfo::continuous(OUTPUT, "Output", -24.0, 12.0, 0.0, "dB"),
    ]
}

pub struct Eq3 {
    v: Vec<f64>,
    sr: f32,
    bands: [[Biquad; 3]; 2],
    dirty: bool,
}

impl Default for Eq3 {
    fn default() -> Self {
        Self {
            v: defaults(&eq_params()),
            sr: 48_000.0,
            bands: [[Biquad::default(); 3]; 2],
            dirty: true,
        }
    }
}

impl Processor for Eq3 {
    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sr = sample_rate as f32;
        self.dirty = true;
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        use eq::*;
        if self.dirty {
            let g = |id: u32| self.v[id as usize] as f32;
            for ch in 0..2 {
                self.bands[ch][0].set(
                    BiquadKind::LowShelf,
                    g(LOW_FREQ),
                    0.707,
                    g(LOW_GAIN),
                    self.sr,
                );
                self.bands[ch][1].set(
                    BiquadKind::Peak,
                    g(MID_FREQ),
                    g(MID_Q),
                    g(MID_GAIN),
                    self.sr,
                );
                self.bands[ch][2].set(
                    BiquadKind::HighShelf,
                    g(HIGH_FREQ),
                    0.707,
                    g(HIGH_GAIN),
                    self.sr,
                );
            }
            self.dirty = false;
        }
        let out = db_to_gain(self.v[OUTPUT as usize] as f32);
        for (ch, buf) in [&mut *ctx.audio.left, &mut *ctx.audio.right]
            .into_iter()
            .enumerate()
        {
            for s in buf.iter_mut() {
                let mut x = *s;
                for b in self.bands[ch].iter_mut() {
                    x = b.process(x);
                }
                *s = x * out;
            }
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
            self.dirty = true;
        }
    }

    fn reset(&mut self) {
        for ch in self.bands.iter_mut() {
            for b in ch.iter_mut() {
                b.reset();
            }
        }
    }
}

// ---------------------------------------------------------------- Saturator

pub mod sat {
    pub const DRIVE: u32 = 0;
    pub const OUTPUT: u32 = 1;
    pub const MIX: u32 = 2;
}

pub fn saturator_params() -> Vec<ParamInfo> {
    use sat::*;
    vec![
        ParamInfo::continuous(DRIVE, "Drive", 0.0, 36.0, 6.0, "dB"),
        ParamInfo::continuous(OUTPUT, "Output", -36.0, 6.0, -3.0, "dB"),
        ParamInfo::continuous(MIX, "Dry/Wet", 0.0, 1.0, 1.0, ""),
    ]
}

pub struct Saturator {
    v: Vec<f64>,
}

impl Default for Saturator {
    fn default() -> Self {
        Self {
            v: defaults(&saturator_params()),
        }
    }
}

impl Processor for Saturator {
    fn activate(&mut self, _: f64, _: usize) {}

    fn process(&mut self, ctx: &mut ProcessContext) {
        use sat::*;
        let drive = db_to_gain(self.v[DRIVE as usize] as f32);
        let out = db_to_gain(self.v[OUTPUT as usize] as f32);
        let mix = self.v[MIX as usize] as f32;
        for s in ctx.audio.left.iter_mut().chain(ctx.audio.right.iter_mut()) {
            let wet = soft_clip(*s * drive) * out;
            *s = *s * (1.0 - mix) + wet * mix;
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
        }
    }
}

// ---------------------------------------------------------------- Utility

pub mod util {
    pub const GAIN: u32 = 0;
    pub const PAN: u32 = 1;
    pub const WIDTH: u32 = 2;
    pub const MONO: u32 = 3;
    pub const INVERT_L: u32 = 4;
    pub const INVERT_R: u32 = 5;
}

pub fn utility_params() -> Vec<ParamInfo> {
    use util::*;
    vec![
        ParamInfo::continuous(GAIN, "Gain", -48.0, 24.0, 0.0, "dB"),
        ParamInfo::continuous(PAN, "Pan", -1.0, 1.0, 0.0, ""),
        ParamInfo::continuous(WIDTH, "Width", 0.0, 2.0, 1.0, ""),
        ParamInfo::toggle(MONO, "Mono", false),
        ParamInfo::toggle(INVERT_L, "Phase L", false),
        ParamInfo::toggle(INVERT_R, "Phase R", false),
    ]
}

pub struct Utility {
    v: Vec<f64>,
    gain_l: Smoother,
    gain_r: Smoother,
}

impl Default for Utility {
    fn default() -> Self {
        Self {
            v: defaults(&utility_params()),
            gain_l: Smoother::new(1.0),
            gain_r: Smoother::new(1.0),
        }
    }
}

impl Processor for Utility {
    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.gain_l.set_time(0.01, sample_rate as f32);
        self.gain_r.set_time(0.01, sample_rate as f32);
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        use util::*;
        let g = db_to_gain(self.v[GAIN as usize] as f32);
        let pan = self.v[PAN as usize] as f32;
        let width = if self.v[MONO as usize] >= 0.5 {
            0.0
        } else {
            self.v[WIDTH as usize] as f32
        };
        let inv_l = if self.v[INVERT_L as usize] >= 0.5 {
            -1.0
        } else {
            1.0
        };
        let inv_r = if self.v[INVERT_R as usize] >= 0.5 {
            -1.0
        } else {
            1.0
        };
        self.gain_l.target = g * (1.0 - pan).min(1.0) * inv_l;
        self.gain_r.target = g * (1.0 + pan).min(1.0) * inv_r;
        let (left, right) = (&mut *ctx.audio.left, &mut *ctx.audio.right);
        for i in 0..left.len() {
            let mid = (left[i] + right[i]) * 0.5;
            let side = (left[i] - right[i]) * 0.5 * width;
            left[i] = (mid + side) * self.gain_l.next();
            right[i] = (mid - side) * self.gain_r.next();
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
        }
    }
}
