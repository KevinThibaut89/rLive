use super::defaults;
use crate::devices::b3::common::Lr4;
use crate::devices::b4::common::{gain_or_silence, Ramp, CONTROL_FRAMES, SILENCE_DB};
use crate::dsp::{Biquad, BiquadKind, Smoother};
use rlive_core::{
    db_to_gain, AudioBuffer, BlockStep, ModMode, ParamInfo, ProcessContext, Processor, ViewId,
};

// ---------------------------------------------------------------- EQ Three

pub const LOW_GAIN: u32 = 0;
pub const LOW_FREQ: u32 = 1;
pub const MID_GAIN: u32 = 2;
pub const MID_FREQ: u32 = 3;
pub const MID_Q: u32 = 4;
pub const HIGH_GAIN: u32 = 5;
pub const HIGH_FREQ: u32 = 6;
pub const OUTPUT: u32 = 7;
// Added after the first release. Every default leaves the sound as it was.
pub const SLOPE: u32 = 8;
pub const LOW_ON: u32 = 9;
pub const MID_ON: u32 = 10;
pub const HIGH_ON: u32 = 11;

/// [`SLOPE`]'s settings. Classic is the shelving EQ this device was before it had crossovers.
pub const SLOPE_CLASSIC: usize = 0;
pub const SLOPE_24: usize = 1;
pub const SLOPE_48: usize = 2;

/// The most a band can be boosted in the crossover modes, in dB.
pub const CROSSOVER_MAX_DB: f64 = 6.0;
/// The level in a band that lights its LED, in dB.
pub const LED_THRESHOLD_DB: f32 = -24.0;
/// One value per band, 1 when lit, for the `eq_three_leds` panel.
pub const LEDS: ViewId = ViewId(0);
pub const VIEW_LEN: usize = 3;

pub fn eq_params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::continuous(LOW_GAIN, "Low", SILENCE_DB, 12.0, 0.0, "dB"),
        ParamInfo::skewed(LOW_FREQ, "Low Freq", 30.0, 1_000.0, 200.0, "Hz", 2.5),
        ParamInfo::continuous(MID_GAIN, "Mid", SILENCE_DB, 12.0, 0.0, "dB"),
        ParamInfo::skewed(MID_FREQ, "Mid Freq", 100.0, 10_000.0, 1_000.0, "Hz", 2.5),
        ParamInfo::skewed(MID_Q, "Mid Q", 0.1, 10.0, 0.8, "", 2.0),
        ParamInfo::continuous(HIGH_GAIN, "High", SILENCE_DB, 12.0, 0.0, "dB"),
        ParamInfo::skewed(
            HIGH_FREQ,
            "High Freq",
            1_000.0,
            18_000.0,
            4_000.0,
            "Hz",
            2.5,
        ),
        ParamInfo::continuous(OUTPUT, "Output", -24.0, 12.0, 0.0, "dB")
            .with_mod_mode(ModMode::Scale),
        ParamInfo::stepped(
            SLOPE,
            "Slope",
            &["Classic", "24 dB", "48 dB"],
            SLOPE_CLASSIC,
        ),
        ParamInfo::toggle(LOW_ON, "Low On", true),
        ParamInfo::toggle(MID_ON, "Mid On", true),
        ParamInfo::toggle(HIGH_ON, "High On", true),
    ]
}

/// How far the 48 dB mode's Qs are from a perfect Linkwitz-Riley. The kind of mismatch analog
/// parts have, so that mode doesn't sum flat. The manual asks for this but gives no numbers.
pub const Q_48_MISMATCH: f32 = 1.02;

// The sections of an 8th-order Linkwitz-Riley filter are those of two 4th-order Butterworth
// filters, each with these Qs.
const Q_24: [f32; 2] = [std::f32::consts::FRAC_1_SQRT_2; 2];
const Q_48: [f32; 4] = [
    0.541_196 * Q_48_MISMATCH,
    1.306_563 * Q_48_MISMATCH,
    0.541_196 * Q_48_MISMATCH,
    1.306_563 * Q_48_MISMATCH,
];

/// A cascade of up to four second-order sections.
#[derive(Clone, Copy, Default)]
struct Cut {
    sections: [Biquad; 4],
    count: usize,
}

impl Cut {
    fn set(&mut self, kind: BiquadKind, freq: f32, qs: &[f32], sr: f32) {
        self.count = qs.len();
        for (section, &q) in self.sections.iter_mut().zip(qs) {
            section.set(kind, freq, q, 0.0, sr);
        }
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        self.sections[..self.count]
            .iter_mut()
            .fold(x, |y, s| s.process(y))
    }

    fn reset(&mut self) {
        self.sections.iter_mut().for_each(Biquad::reset);
    }
}

/// One channel's split into three bands, at either slope.
#[derive(Clone, Copy, Default)]
struct Crossover {
    low_lp: Cut,
    low_hp: Cut,
    high_lp: Cut,
    high_hp: Cut,
    align_lp: Cut,
    align_hp: Cut,
}

impl Crossover {
    fn set(&mut self, low: f32, high: f32, qs: &[f32], sr: f32) {
        self.low_lp.set(BiquadKind::LowPass, low, qs, sr);
        self.low_hp.set(BiquadKind::HighPass, low, qs, sr);
        self.high_lp.set(BiquadKind::LowPass, high, qs, sr);
        self.high_hp.set(BiquadKind::HighPass, high, qs, sr);
        self.align_lp.set(BiquadKind::LowPass, high, qs, sr);
        self.align_hp.set(BiquadKind::HighPass, high, qs, sr);
    }

    #[inline]
    fn split(&mut self, x: f32) -> [f32; 3] {
        let low = self.low_lp.process(x);
        let upper = self.low_hp.process(x);
        let mid = self.high_lp.process(upper);
        let high = self.high_hp.process(upper);
        let aligned = self.align_lp.process(low) + self.align_hp.process(low);
        [aligned, mid, high]
    }

    fn reset(&mut self) {
        for cut in [
            &mut self.low_lp,
            &mut self.low_hp,
            &mut self.high_lp,
            &mut self.high_hp,
            &mut self.align_lp,
            &mut self.align_hp,
        ] {
            cut.reset();
        }
    }
}

/// Finds out what's in each band for the LEDs. A view only: never touches the sound.
#[derive(Clone, Copy, Default)]
struct LedSplit {
    low: Lr4,
    high: Lr4,
}

pub struct Eq3 {
    v: Vec<f64>,
    sr: f32,
    bands: [[Biquad; 3]; 2],
    dirty: bool,
    crossover: [Crossover; 2],
    /// `log2` of the crossover frequencies, eased so moving them is smooth.
    low_log: Ramp,
    high_log: Ramp,
    band_gain: [Smoother; 3],
    crossover_stale: bool,
    /// Whether the next crossover block is the first since activation or a reset.
    fresh: bool,
    last_slope: usize,
    led_split: [LedSplit; 2],
}

impl Default for Eq3 {
    fn default() -> Self {
        Self {
            v: defaults(&eq_params()),
            sr: 48_000.0,
            bands: [[Biquad::default(); 3]; 2],
            dirty: true,
            crossover: [Crossover::default(); 2],
            low_log: Ramp::new(200f32.log2()),
            high_log: Ramp::new(4_000f32.log2()),
            band_gain: [Smoother::new(1.0); 3],
            crossover_stale: true,
            fresh: true,
            last_slope: SLOPE_CLASSIC,
            led_split: [LedSplit::default(); 2],
        }
    }
}

impl Eq3 {
    fn slope(&self) -> usize {
        (self.v[SLOPE as usize].round() as usize).min(SLOPE_48)
    }

    fn band_on(&self, id: u32) -> bool {
        self.v[id as usize] >= 0.5
    }

    fn render(&mut self, audio: AudioBuffer) {
        if self.slope() != SLOPE_CLASSIC {
            self.render_crossover(audio);
            return;
        }
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
        let on = [
            self.band_on(LOW_ON),
            self.band_on(MID_ON),
            self.band_on(HIGH_ON),
        ];
        let out = db_to_gain(self.v[OUTPUT as usize] as f32);
        for (ch, buf) in [&mut *audio.left, &mut *audio.right]
            .into_iter()
            .enumerate()
        {
            for s in buf.iter_mut() {
                let mut x = *s;
                for (b, band_on) in self.bands[ch].iter_mut().zip(on) {
                    if band_on {
                        x = b.process(x);
                    }
                }
                *s = x * out;
            }
        }
    }

    /// The Live-style device: three bands from a crossover, which can each be cut completely.
    fn render_crossover(&mut self, audio: AudioBuffer) {
        let slope = self.slope();
        if slope != self.last_slope || self.dirty {
            if slope != self.last_slope {
                self.crossover.iter_mut().for_each(Crossover::reset);
            }
            self.last_slope = slope;
            self.crossover_stale = true;
            self.dirty = false;
        }
        let (low, high) = {
            let a = self.v[LOW_FREQ as usize] as f32;
            let b = self.v[HIGH_FREQ as usize] as f32;
            (a.min(b), a.max(b))
        };
        self.low_log.target = low.log2();
        self.high_log.target = high.log2();
        let gains =
            [(LOW_GAIN, LOW_ON), (MID_GAIN, MID_ON), (HIGH_GAIN, HIGH_ON)].map(|(gain, on)| {
                if self.band_on(on) {
                    gain_or_silence(self.v[gain as usize].min(CROSSOVER_MAX_DB))
                } else {
                    0.0
                }
            });
        for (smoother, gain) in self.band_gain.iter_mut().zip(gains) {
            smoother.target = gain;
        }
        if self.fresh {
            self.fresh = false;
            self.low_log.snap(self.low_log.target);
            self.high_log.snap(self.high_log.target);
            self.band_gain.iter_mut().for_each(Smoother::reset);
            self.crossover_stale = true;
        }
        let out = db_to_gain(self.v[OUTPUT as usize] as f32);
        let qs: &[f32] = if slope == SLOPE_48 { &Q_48 } else { &Q_24 };
        let frames = audio.frames();
        let (left, right) = (&mut *audio.left, &mut *audio.right);
        let mut pos = 0;
        while pos < frames {
            let n = (frames - pos).min(CONTROL_FRAMES);
            let moved = self.low_log.tick(n, 0.02, self.sr) | self.high_log.tick(n, 0.02, self.sr);
            if moved || self.crossover_stale {
                let (lo, hi) = (
                    2f32.powf(self.low_log.value),
                    2f32.powf(self.high_log.value),
                );
                for c in &mut self.crossover {
                    c.set(lo, hi, qs, self.sr);
                }
                self.crossover_stale = false;
            }
            for i in pos..pos + n {
                let gain = [
                    self.band_gain[0].next(),
                    self.band_gain[1].next(),
                    self.band_gain[2].next(),
                ];
                let [l0, l1, l2] = self.crossover[0].split(left[i]);
                let [r0, r1, r2] = self.crossover[1].split(right[i]);
                left[i] = (l0 * gain[0] + l1 * gain[1] + l2 * gain[2]) * out;
                right[i] = (r0 * gain[0] + r1 * gain[1] + r2 * gain[2]) * out;
            }
            pos += n;
        }
    }

    /// Measures the input for the LEDs. They light even for a band that's off.
    fn measure_leds(&mut self, left: &[f32], right: &[f32]) -> [f32; 3] {
        let g = |id: u32| self.v[id as usize] as f32;
        let (lo, hi) = (g(LOW_FREQ), g(HIGH_FREQ));
        let sr = self.sr;
        for s in &mut self.led_split {
            s.low.set(BiquadKind::LowPass, lo, sr);
            s.high.set(BiquadKind::HighPass, hi, sr);
        }
        let mut peaks = [0.0f32; 3];
        for (ch, buf) in [left, right].into_iter().enumerate() {
            let split = &mut self.led_split[ch];
            for &x in buf {
                let low = split.low.process(x);
                let high = split.high.process(x);
                let mid = x - low - high;
                peaks[0] = peaks[0].max(low.abs());
                peaks[1] = peaks[1].max(mid.abs());
                peaks[2] = peaks[2].max(high.abs());
            }
        }
        let threshold = db_to_gain(LED_THRESHOLD_DB);
        peaks.map(|p| if p >= threshold { 1.0 } else { 0.0 })
    }
}

impl Processor for Eq3 {
    fn sleeps_when_silent(&self) -> bool {
        true
    }

    fn activate(&mut self, sample_rate: f64, _: usize) {
        self.sr = sample_rate as f32;
        self.dirty = true;
        self.crossover_stale = true;
        self.fresh = true;
        for smoother in &mut self.band_gain {
            smoother.set_time(0.01, self.sr);
        }
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        let leds = ctx
            .view
            .is_active()
            .then(|| self.measure_leds(ctx.audio.left, ctx.audio.right));
        ctx.for_each_step(|step| match step {
            BlockStep::Param { id, value } => self.set_param(id, value),
            BlockStep::Audio(audio) => self.render(audio),
            BlockStep::Midi(_) => {}
        });
        if let Some(leds) = leds {
            ctx.view.publish(LEDS, &leds);
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
        self.crossover.iter_mut().for_each(Crossover::reset);
        self.fresh = true;
        for split in &mut self.led_split {
            split.low.reset();
            split.high.reset();
        }
    }
}
