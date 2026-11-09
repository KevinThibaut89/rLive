//! Saturator: the original drive-and-tanh path, kept sample for sample while every parameter
//! the original had is all that's in play, and Live's curves, post clip, colour filters,
//! waveshaper and Hi-Quality behind appended parameters.
//!
//! From the manual: the eight curve names, Soft/Hard post clip, the mirrored colour filter, Bass
//! Shaper Threshold from 0 to -50 dB, the six Waveshaper controls, Hi-Quality and Pre-DC Filter.
//! The manual gives no numbers for curve formulas, colour filter frequencies, ranges or Q.
//! Those are ours, including Analog Clip being `tanh`: the original path this device started
//! with, which the golden project needs bit for bit at default settings.

use super::defaults;
use crate::devices::b5::common::{DcBlock, DryLine, HI_QUALITY_FACTOR};
use crate::dsp::oversample::Oversampler;
use crate::dsp::{soft_clip, Biquad, BiquadKind};
use rlive_core::{
    db_to_gain, AudioBuffer, BlockStep, ModMode, ParamInfo, ProcessContext, Processor,
};
use std::f32::consts::FRAC_PI_2;

// ---------------------------------------------------------------- Saturator

pub const DRIVE: u32 = 0;
pub const OUTPUT: u32 = 1;
pub const MIX: u32 = 2;
// Appended; each default leaves the original path.
pub const CURVE: u32 = 3;
pub const POST_CLIP: u32 = 4;
pub const COLOR: u32 = 5;
pub const AMT_LO: u32 = 6;
pub const AMT_HI: u32 = 7;
pub const FREQ: u32 = 8;
pub const WIDTH: u32 = 9;
pub const BASS_THRESHOLD: u32 = 10;
pub const WS_DRIVE: u32 = 11;
pub const WS_CURVE: u32 = 12;
pub const WS_DEPTH: u32 = 13;
pub const WS_LIN: u32 = 14;
pub const WS_DAMP: u32 = 15;
pub const WS_PERIOD: u32 = 16;
pub const HI_QUALITY: u32 = 17;
pub const PRE_DC: u32 = 18;

pub const CURVES: [&str; 8] = [
    "Analog Clip",
    "Soft Sine",
    "Bass Shaper",
    "Medium Curve",
    "Hard Curve",
    "Sinoid Fold",
    "Digital Clip",
    "Waveshaper",
];

const PARAMS: usize = 19;

pub fn saturator_params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::continuous(DRIVE, "Drive", -36.0, 36.0, 6.0, "dB"),
        ParamInfo::continuous(OUTPUT, "Output", -36.0, 6.0, -3.0, "dB")
            .with_mod_mode(ModMode::Scale),
        ParamInfo::continuous(MIX, "Dry/Wet", 0.0, 1.0, 1.0, ""),
        ParamInfo::stepped(CURVE, "Curve Type", &CURVES, 0),
        ParamInfo::stepped(
            POST_CLIP,
            "Post Clip",
            &["Off", "Soft Clip", "Hard Clip"],
            0,
        ),
        ParamInfo::toggle(COLOR, "Color", false),
        ParamInfo::continuous(AMT_LO, "Amt Lo", -1.0, 1.0, 0.0, ""),
        ParamInfo::continuous(AMT_HI, "Amt Hi", -1.0, 1.0, 0.0, ""),
        ParamInfo::skewed(FREQ, "Frequency", 30.0, 18_000.0, 1_000.0, "Hz", 2.5),
        ParamInfo::continuous(WIDTH, "Width", 0.0, 1.0, 0.5, ""),
        ParamInfo::continuous(
            BASS_THRESHOLD,
            "Bass Shaper Threshold",
            -50.0,
            0.0,
            -50.0,
            "dB",
        ),
        ParamInfo::continuous(WS_DRIVE, "WS Drive", 0.0, 1.0, 1.0, ""),
        ParamInfo::continuous(WS_CURVE, "WS Curve", 0.0, 1.0, 0.5, ""),
        ParamInfo::continuous(WS_DEPTH, "WS Depth", 0.0, 1.0, 0.0, ""),
        ParamInfo::continuous(WS_LIN, "WS Lin", 0.0, 1.0, 0.5, ""),
        ParamInfo::continuous(WS_DAMP, "WS Damp", 0.0, 1.0, 0.0, ""),
        ParamInfo::continuous(WS_PERIOD, "WS Period", 0.0, 1.0, 0.0, ""),
        ParamInfo::toggle(HI_QUALITY, "Hi-Quality", false),
        ParamInfo::toggle(PRE_DC, "Pre-DC Filter", false),
    ]
}

/// A curve as a pure function, along with what it depends on. Takes the signal after Drive.
#[derive(Clone, Copy, Debug)]
pub struct CurveShape {
    pub kind: usize,
    pub threshold: f32,
    pub drive: f32,
    pub curve: f32,
    pub depth: f32,
    pub lin: f32,
    pub damp: f32,
    pub period: f32,
}

impl CurveShape {
    fn from_values(v: &[f64; PARAMS]) -> Self {
        Self {
            kind: v[CURVE as usize].round().clamp(0.0, 7.0) as usize,
            threshold: db_to_gain(v[BASS_THRESHOLD as usize] as f32),
            drive: v[WS_DRIVE as usize] as f32,
            curve: v[WS_CURVE as usize] as f32,
            depth: v[WS_DEPTH as usize] as f32,
            lin: v[WS_LIN as usize] as f32,
            damp: v[WS_DAMP as usize] as f32,
            period: v[WS_PERIOD as usize] as f32,
        }
    }

    /// The transfer curve at `x`.
    #[inline]
    pub fn apply(&self, x: f32) -> f32 {
        match self.kind {
            0 => soft_clip(x),
            1 => (x.clamp(-1.0, 1.0) * FRAC_PI_2).sin(),
            2 => bass_shaper(x, self.threshold),
            3 => x / (1.0 + x * x).sqrt(),
            4 => x / (1.0 + x * x * x * x).sqrt().sqrt(),
            5 => (x * FRAC_PI_2).sin(),
            6 => x.clamp(-1.0, 1.0),
            _ => waveshaper(x, self),
        }
    }
}

/// Linear below `threshold`, then bending smoothly into full scale. A threshold of 0 dB is a
/// hard clip; the lower it is, the sooner and softer the bend.
#[inline]
pub fn bass_shaper(x: f32, threshold: f32) -> f32 {
    let t = threshold.clamp(0.0, 1.0);
    let a = x.abs();
    if a <= t {
        x
    } else if t >= 1.0 {
        x.signum()
    } else {
        x.signum() * (t + (1.0 - t) * ((a - t) / (1.0 - t)).tanh())
    }
}

#[inline]
fn waveshaper(x: f32, s: &CurveShape) -> f32 {
    let u = x.clamp(-1.0, 1.0);
    if s.drive <= 0.0 {
        return u;
    }
    // Curve adds mostly third-order harmonics; the rest pull the linear part around.
    let curved = s.lin * u + (s.curve - s.lin * 0.5) * u * u * u;
    let ripples = 1.0 + s.period * 15.0;
    let sine = s.depth * (u * std::f32::consts::PI * ripples).sin();
    let shaped = u + s.drive * (curved + sine - u);
    // Damp is a very fast gate around zero.
    let k = 0.1 * s.damp * s.damp;
    let gated = if k > 0.0 {
        shaped * (u * u) / (u * u + k)
    } else {
        shaped
    };
    gated.clamp(-1.5, 1.5)
}

#[derive(Clone, Debug)]
struct ChanFilters {
    dc: DcBlock,
    pre: [Biquad; 2],
    post: [Biquad; 2],
}

pub struct Saturator {
    v: [f64; PARAMS],
    sr: f32,
    max_block: usize,
    os: [Oversampler; 2],
    dry_line: DryLine,
    dry: [Vec<f32>; 2],
    chans: [ChanFilters; 2],
    /// `(low, high, freq, width)` the filters are tuned to, so they aren't re-tuned every chunk.
    tuned: (f32, f32, f32, f32),
    latency: u32,
}

impl Default for Saturator {
    fn default() -> Self {
        let mut v = [0.0; PARAMS];
        v.copy_from_slice(&defaults(&saturator_params()));
        let chan = ChanFilters {
            dc: DcBlock::default(),
            pre: [Biquad::default(); 2],
            post: [Biquad::default(); 2],
        };
        Self {
            v,
            sr: 48_000.0,
            max_block: 512,
            os: [
                Oversampler::new(HI_QUALITY_FACTOR, 512),
                Oversampler::new(HI_QUALITY_FACTOR, 512),
            ],
            dry_line: DryLine::new(0),
            dry: [Vec::new(), Vec::new()],
            chans: [chan.clone(), chan],
            tuned: (f32::NAN, 0.0, 0.0, 0.0),
            latency: 0,
        }
    }
}

impl Saturator {
    fn val(&self, id: u32) -> f32 {
        self.v[id as usize] as f32
    }

    fn on(&self, id: u32) -> bool {
        self.v[id as usize] >= 0.5
    }

    /// Whether every appended feature is out of the signal.
    fn original_path(&self) -> bool {
        self.val(CURVE).round() == 0.0
            && self.val(POST_CLIP).round() == 0.0
            && !self.on(COLOR)
            && !self.on(HI_QUALITY)
            && !self.on(PRE_DC)
    }

    /// The original algorithm, kept as it was: this is what the golden project hears.
    fn render_original(&mut self, audio: AudioBuffer) {
        let drive = db_to_gain(self.v[DRIVE as usize] as f32);
        let out = db_to_gain(self.v[OUTPUT as usize] as f32);
        let mix = self.v[MIX as usize] as f32;
        for s in audio.left.iter_mut().chain(audio.right.iter_mut()) {
            let wet = soft_clip(*s * drive) * out;
            *s = *s * (1.0 - mix) + wet * mix;
        }
    }

    fn retune(&mut self) {
        let tuned = (
            self.val(AMT_LO),
            self.val(AMT_HI),
            self.val(FREQ),
            self.val(WIDTH),
        );
        if tuned == self.tuned {
            return;
        }
        self.tuned = tuned;
        let (lo, hi, freq, width) = tuned;
        let (lo_db, hi_db) = (lo * COLOR_RANGE_DB, hi * COLOR_RANGE_DB);
        let q = 3.0 * (0.15f32).powf(width);
        for chan in &mut self.chans {
            for (filters, sign) in [(&mut chan.pre, 1.0f32), (&mut chan.post, -1.0)] {
                filters[0].set(
                    BiquadKind::LowShelf,
                    COLOR_LOW_HZ,
                    0.707,
                    lo_db * sign,
                    self.sr,
                );
                filters[1].set(BiquadKind::Peak, freq, q, hi_db * sign, self.sr);
            }
        }
    }

    fn render_full(&mut self, audio: AudioBuffer) {
        if self.on(COLOR) {
            self.retune();
        }
        let drive = db_to_gain(self.val(DRIVE));
        let out = db_to_gain(self.val(OUTPUT));
        let mix = self.val(MIX);
        let curve = CurveShape::from_values(&self.v);
        let (color, pre_dc, hq) = (self.on(COLOR), self.on(PRE_DC), self.on(HI_QUALITY));
        let post_clip = self.val(POST_CLIP).round() as i32;

        let frames = audio.frames();
        let (left, right) = (audio.left, audio.right);
        let mut pos = 0;
        while pos < frames {
            let n = (frames - pos).min(self.max_block);
            let mut channels = [&mut left[pos..pos + n], &mut right[pos..pos + n]];
            for (ch, block) in channels.iter_mut().enumerate() {
                self.dry[ch][..n].copy_from_slice(block);
                let chan = &mut self.chans[ch];
                for s in block.iter_mut() {
                    let mut x = *s;
                    if pre_dc {
                        x = chan.dc.process(x);
                    }
                    if color {
                        let first = chan.pre[0].process(x);
                        x = chan.pre[1].process(first);
                    }
                    *s = x * drive;
                }
                if hq {
                    self.os[ch].process_in_place(block, |hi| {
                        for s in hi.iter_mut() {
                            *s = curve.apply(*s);
                        }
                    });
                } else {
                    for s in block.iter_mut() {
                        *s = curve.apply(*s);
                    }
                }
                for s in block.iter_mut() {
                    let mut x = *s;
                    if color {
                        let first = chan.post[0].process(x);
                        x = chan.post[1].process(first);
                    }
                    *s = match post_clip {
                        1 => soft_clip(x) * out,
                        2 => x.clamp(-1.0, 1.0) * out,
                        _ => x * out,
                    };
                }
            }
            let [wet_l, wet_r] = channels;
            let [dry_l, dry_r] = &mut self.dry;
            if hq {
                let line = &mut self.dry_line;
                for (y, x) in wet_l.iter_mut().zip(&dry_l[..n]) {
                    *y = *y * mix + line.left(*x) * (1.0 - mix);
                }
                for (y, x) in wet_r.iter_mut().zip(&dry_r[..n]) {
                    *y = *y * mix + line.right(*x) * (1.0 - mix);
                }
            } else {
                for (y, x) in wet_l.iter_mut().zip(&dry_l[..n]) {
                    *y = *y * mix + *x * (1.0 - mix);
                }
                for (y, x) in wet_r.iter_mut().zip(&dry_r[..n]) {
                    *y = *y * mix + *x * (1.0 - mix);
                }
            }
            pos += n;
        }
    }

    fn render(&mut self, audio: AudioBuffer) {
        if self.original_path() {
            self.render_original(audio);
        } else {
            self.render_full(audio);
        }
    }
}

/// Ours: the manual gives no ranges for the colour filters.
pub const COLOR_RANGE_DB: f32 = 18.0;
pub const COLOR_LOW_HZ: f32 = 300.0;

impl Processor for Saturator {
    fn sleeps_when_silent(&self) -> bool {
        true
    }

    fn activate(&mut self, sample_rate: f64, max_block: usize) {
        self.sr = sample_rate as f32;
        self.max_block = max_block.max(1);
        self.os = [
            Oversampler::new(HI_QUALITY_FACTOR, self.max_block),
            Oversampler::new(HI_QUALITY_FACTOR, self.max_block),
        ];
        self.latency = self.os[0].latency();
        self.dry_line = DryLine::new(self.latency as usize);
        self.dry = [vec![0.0; self.max_block], vec![0.0; self.max_block]];
        for chan in &mut self.chans {
            chan.dc.set(5.0, self.sr);
            chan.dc.reset();
        }
        self.tuned = (f32::NAN, 0.0, 0.0, 0.0);
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => self.set_param(id, value),
            BlockStep::Audio(audio) => self.render(audio),
            BlockStep::Midi(_) => {}
        });
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.v.get_mut(id as usize) {
            *v = value;
        }
    }

    fn reset(&mut self) {
        for os in &mut self.os {
            os.reset();
        }
        self.dry_line.clear();
        for chan in &mut self.chans {
            chan.dc.reset();
            chan.pre
                .iter_mut()
                .chain(chan.post.iter_mut())
                .for_each(Biquad::reset);
        }
    }

    fn latency(&self) -> u32 {
        if self.on(HI_QUALITY) {
            self.latency
        } else {
            0
        }
    }
}
