//! Complex and Complex Pro: a phase vocoder that reads the source at random.
//!
//! Each grain is the spectrum of the source around the place the timeline says that moment of the
//! output should be, so the stretcher reads ahead of the output instead of behind it: nothing
//! accumulates from grain to grain, a tempo change can't move the clip off the timeline, and
//! there is no latency to compensate. A grain reads the source a second time one synthesis hop
//! earlier, and the difference between those two spectra is how far each partial's phase turns in
//! one hop, so the phase advance is right at any speed and needs no history.
//!
//! Transposition is done by reading the grain at the transposed rate before analysing it: the
//! vocoder only ever stretches time. Complex Pro adds formant correction: the spectral envelope
//! of each grain, found by liftering its cepstrum, is moved back towards where it was before
//! transposition.
//!
//! An attack doesn't survive a phase vocoder, so where the clip knows its transients (or the
//! spectrum says one is arriving) grains cut themselves off around it and the source plays
//! through at its own speed across the attack. The same path carries the moments after a seek,
//! before enough grains overlap: it fills whatever part of the window sum is missing.

use crate::crossfade_frames;
use crate::interp::Sinc;
use realfft::num_complex::Complex32;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use rlive_core::warp::{StretchSpan, Stretcher, WarpMode};
use rlive_core::SampleBuffer;
use std::collections::HashMap;
use std::f64::consts::TAU;
use std::sync::{Arc, Mutex, OnceLock};

/// How far either side of a transient the grains are cut off, in output frames.
const TRANSIENT_MARGIN: usize = 144;
/// How long after a transient the grains stay out of the way, in output frames.
const ATTACK_HOLD: usize = 240;
/// A jump in spectral flux this big starts a new phase history, as a transient would.
const TRANSIENT_FLUX: f32 = 2.5;
/// The smallest change that counts as a transposition, as a frequency ratio.
const NO_TRANSPOSE: f64 = 1e-4;
/// Formant correction never boosts or cuts a bin more than this, as a linear gain.
const MAX_FORMANT_GAIN: f32 = 64.0;
/// The fewest cepstral coefficients a spectral envelope is made from.
const MIN_ENVELOPE: f32 = 8.0;

/// The window, and the FFT plans for it, shared by every stretcher of one size.
struct Plan {
    window: Vec<f32>,
    forward: Arc<dyn RealToComplex<f32>>,
    inverse: Arc<dyn ComplexToReal<f32>>,
    scratch: usize,
}

fn plan(n: usize) -> Arc<Plan> {
    static PLANS: OnceLock<Mutex<HashMap<usize, Arc<Plan>>>> = OnceLock::new();
    let mut plans = PLANS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    plans
        .entry(n)
        .or_insert_with(|| {
            let mut planner = RealFftPlanner::<f32>::new();
            let forward = planner.plan_fft_forward(n);
            let inverse = planner.plan_fft_inverse(n);
            let scratch = forward.get_scratch_len().max(inverse.get_scratch_len());
            Arc::new(Plan {
                window: (0..n)
                    .map(|i| (0.5 - 0.5 * (TAU * i as f64 / n as f64).cos()) as f32)
                    .collect(),
                forward,
                inverse,
                scratch,
            })
        })
        .clone()
}

/// Which of the two modes a [`PhaseVocoder`] plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    Complex,
    ComplexPro,
}

impl Flavor {
    fn window_frames(self) -> usize {
        match self {
            Flavor::Complex => 2048,
            Flavor::ComplexPro => 4096,
        }
    }
}

/// The transient closest to a grain and which side of it the grain sits.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Cut {
    None,
    /// The grain's centre is before the transient: it stops short of it.
    Before(f64),
    /// The grain's centre is after the transient: it starts past the attack.
    After(f64),
}

/// A stretcher for mixed material: whole songs, chords, anything polyphonic.
pub struct PhaseVocoder {
    flavor: Flavor,
    plan: Arc<Plan>,
    sinc: &'static Sinc,
    sample_rate: f64,
    n: usize,
    hop: usize,
    /// What the squared windows of the grains sum to once enough of them overlap.
    cola: f32,

    /// `n + hop` frames read from the source for one grain.
    read_l: Vec<f32>,
    read_r: Vec<f32>,
    /// The cut applied to the frames read, by read frame.
    mask: Vec<f32>,
    /// A frame on its way into or out of the FFT.
    frame: Vec<f32>,
    /// The spectra of the grain: left, right, and the sum and earlier sum the phases come from.
    spec_l: Vec<Complex32>,
    spec_r: Vec<Complex32>,
    spec_sum: Vec<Complex32>,
    spec_prev: Vec<Complex32>,
    scratch: Vec<Complex32>,
    /// The unit phasor each bin ended the last grain with.
    phase: Vec<Complex32>,
    /// Bin indexes of the spectral peaks of the grain.
    peaks: Vec<usize>,
    /// The sign the right channel enters the sum with.
    sum_sign: f32,
    /// The log magnitude of the spectral envelope, and what it's worked out in.
    envelope: Vec<f32>,
    cepstrum: Vec<f32>,

    /// The sum of overlapping grains; `ring_w` is the sum of their squared windows.
    ring_l: Vec<f32>,
    ring_r: Vec<f32>,
    ring_w: Vec<f32>,
    ring_at: usize,
    until_grain: usize,
    has_phase: bool,
    /// The grain before last started a phase history, so this one can't.
    just_reset: bool,
    /// The transient the source path is playing through, if any.
    active: Option<f64>,
    /// The transient that last started a phase history.
    reset_for: Option<f64>,
    /// Where the source path reads next.
    direct: f64,
    last: Option<f64>,
    fresh: bool,
    seek_to: Option<f64>,
    crossfade: u32,
    fade_in_left: u32,
    /// What the reading before a seek still had to say.
    tail_l: Vec<f32>,
    tail_r: Vec<f32>,
    tail_at: usize,
    tail_len: usize,
}

impl PhaseVocoder {
    pub fn new(flavor: Flavor) -> Self {
        let n = flavor.window_frames();
        let hop = 512;
        let bins = n / 2 + 1;
        let plan = plan(n);
        let tail = crossfade_frames(192_000.0) as usize;
        Self {
            flavor,
            sinc: Sinc::shared(),
            sample_rate: 48_000.0,
            n,
            hop,
            cola: (0.375 * n as f64 / hop as f64) as f32,
            read_l: vec![0.0; n + hop],
            read_r: vec![0.0; n + hop],
            mask: vec![1.0; n + hop],
            frame: vec![0.0; n],
            spec_l: vec![Complex32::default(); bins],
            spec_r: vec![Complex32::default(); bins],
            spec_sum: vec![Complex32::default(); bins],
            spec_prev: vec![Complex32::default(); bins],
            scratch: vec![Complex32::default(); plan.scratch],
            phase: vec![Complex32::default(); bins],
            peaks: Vec::with_capacity(bins),
            sum_sign: 1.0,
            envelope: vec![0.0; bins],
            cepstrum: vec![0.0; n],
            ring_l: vec![0.0; n],
            ring_r: vec![0.0; n],
            ring_w: vec![0.0; n],
            ring_at: 0,
            until_grain: 0,
            has_phase: false,
            just_reset: false,
            active: None,
            reset_for: None,
            direct: 0.0,
            last: None,
            fresh: true,
            seek_to: None,
            crossfade: crossfade_frames(48_000.0),
            fade_in_left: 0,
            tail_l: vec![0.0; tail],
            tail_r: vec![0.0; tail],
            tail_at: 0,
            tail_len: 0,
            plan,
        }
    }

    pub fn complex() -> Self {
        Self::new(Flavor::Complex)
    }

    pub fn complex_pro() -> Self {
        Self::new(Flavor::ComplexPro)
    }

    /// Fills `read_l` and `read_r` starting at source position `first`, one frame per `step`.
    fn read_source(&mut self, s: &SampleBuffer, first: f64, step: f64) {
        let len = self.read_l.len();
        if (step - 1.0).abs() < 1e-9 {
            // A rational reading on whole frames is a copy, which also keeps the identity exact.
            let at = first.round() as i64;
            let n = s.len() as i64;
            for i in 0..len {
                let p = at + i as i64;
                let inside = (0..n).contains(&p);
                self.read_l[i] = if inside { s.left[p as usize] } else { 0.0 };
                self.read_r[i] = if inside { s.right[p as usize] } else { 0.0 };
            }
            return;
        }
        let band = Sinc::band_for(step);
        for i in 0..len {
            let (a, b) = self.sinc.read(s, first + i as f64 * step, band);
            self.read_l[i] = a;
            self.read_r[i] = b;
        }
    }

    /// Finds the transient to cut a grain against, and writes the cut into `mask`. The second
    /// value says whether the source path should go to it now.
    fn make_mask(
        &mut self,
        first: f64,
        step: f64,
        slices: &[f64],
        ring_deficit: f32,
    ) -> (Cut, bool) {
        self.mask.fill(1.0);
        let len = self.read_l.len();
        let half = self.n as f64 * 0.5 * step;
        let centre = first + (self.hop as f64 + self.n as f64 * 0.5) * step;
        let (start, end) = (centre - half, centre + half);
        let i = slices.partition_point(|t| *t <= start);
        let Some(&t) = slices.get(i).filter(|t| **t < end) else {
            self.active = None;
            return (Cut::None, false);
        };
        // Another transient's path may still be sounding; leave this one to the grains.
        let newly = self.active != Some(t);
        if newly && ring_deficit >= 0.05 {
            return (Cut::None, false);
        }
        self.active = Some(t);
        let margin = TRANSIENT_MARGIN as f64 * step;
        let hold = ATTACK_HOLD as f64 * step;
        if centre < t {
            for (j, m) in self.mask.iter_mut().enumerate().take(len) {
                let q = first + j as f64 * step;
                if q >= t {
                    *m = 0.0;
                } else if q > t - margin {
                    *m = ((t - q) / margin) as f32;
                }
            }
            (Cut::Before(t), newly)
        } else {
            for (j, m) in self.mask.iter_mut().enumerate().take(len) {
                let q = first + j as f64 * step;
                if q <= t + hold {
                    *m = 0.0;
                } else if q < t + hold + margin {
                    *m = ((q - t - hold) / margin) as f32;
                }
            }
            (Cut::After(t), newly)
        }
    }

    /// The spectra of the grain, and of the same grain a hop earlier.
    #[allow(clippy::needless_range_loop)]
    fn analyse(&mut self) {
        let (n, hop) = (self.n, self.hop);
        let window = &self.plan.window;
        // The right channel is summed with the sign that doesn't cancel it.
        let correlation: f32 = self.read_l[hop..hop + n]
            .iter()
            .zip(&self.read_r[hop..hop + n])
            .map(|(a, b)| a * b)
            .sum();
        let sign = if correlation < 0.0 { -1.0 } else { 1.0 };
        if sign != self.sum_sign {
            self.sum_sign = sign;
            self.has_phase = false;
        }
        for i in 0..n {
            let at = hop + i;
            self.frame[i] = self.read_l[at] * window[i] * self.mask[at];
        }
        let _ = self.plan.forward.process_with_scratch(
            &mut self.frame,
            &mut self.spec_l,
            &mut self.scratch,
        );
        for i in 0..n {
            let at = hop + i;
            self.frame[i] = self.read_r[at] * window[i] * self.mask[at];
        }
        let _ = self.plan.forward.process_with_scratch(
            &mut self.frame,
            &mut self.spec_r,
            &mut self.scratch,
        );
        for (out, (l, r)) in self
            .spec_sum
            .iter_mut()
            .zip(self.spec_l.iter().zip(&self.spec_r))
        {
            *out = *l + *r * sign;
        }
        for i in 0..n {
            self.frame[i] = (self.read_l[i] + self.read_r[i] * sign) * window[i] * self.mask[i];
        }
        let _ = self.plan.forward.process_with_scratch(
            &mut self.frame,
            &mut self.spec_prev,
            &mut self.scratch,
        );
    }

    /// How much the grain has grown over the grain a hop earlier, as a share of that one.
    fn spectral_flux(&self) -> f32 {
        let (mut rise, mut total) = (0.0f32, 0.0f32);
        for (now, before) in self.spec_sum.iter().zip(&self.spec_prev) {
            let (a, b) = (now.norm_sqr().sqrt(), before.norm_sqr().sqrt());
            rise += (a - b).max(0.0);
            total += b;
        }
        if total <= 1e-9 {
            0.0
        } else {
            rise / total
        }
    }

    /// Turns the phases of the grain to where this hop takes them, each bin locked to its peak.
    fn propagate(&mut self, reset: bool) {
        let bins = self.spec_sum.len();
        self.peaks.clear();
        for k in 2..bins - 2 {
            let m = self.spec_sum[k].norm_sqr();
            if m > 1e-20
                && m >= self.spec_sum[k - 1].norm_sqr()
                && m >= self.spec_sum[k + 1].norm_sqr()
                && m >= self.spec_sum[k - 2].norm_sqr()
                && m >= self.spec_sum[k + 2].norm_sqr()
            {
                self.peaks.push(k);
            }
        }
        let rotations = self.peaks.len();
        let mut region_end = 0usize;
        for p in 0..rotations.max(1) {
            let peak = self.peaks.get(p).copied();
            let next_end = match (peak, self.peaks.get(p + 1)) {
                (Some(a), Some(&b)) => (a + b) / 2,
                _ => bins - 1,
            };
            // The rotation every bin of this region is turned by.
            let rotation = match peak {
                Some(k) if !reset => {
                    let z = self.spec_sum[k];
                    let advance = z * self.spec_prev[k].conj();
                    let advance_norm = advance.norm_sqr().sqrt();
                    let z_norm = z.norm_sqr().sqrt();
                    if advance_norm > 1e-18 && z_norm > 1e-18 {
                        let target = self.phase[k] * (advance / advance_norm);
                        target * z.conj() / z_norm
                    } else {
                        Complex32::new(1.0, 0.0)
                    }
                }
                _ => Complex32::new(1.0, 0.0),
            };
            for k in region_end..=next_end.min(bins - 1) {
                let z = self.spec_sum[k];
                let turned = z * rotation;
                let norm = z.norm_sqr().sqrt();
                self.phase[k] = if norm > 1e-18 {
                    turned / norm
                } else {
                    Complex32::new(1.0, 0.0)
                };
                self.spec_l[k] *= rotation;
                self.spec_r[k] *= rotation;
            }
            region_end = next_end + 1;
        }
    }

    /// Replaces `envelope` with the smooth log-magnitude envelope of the grain.
    fn spectral_envelope(&mut self, quefrency: usize) {
        let n = self.n;
        let bins = self.spec_sum.len();
        for k in 0..bins {
            let log_magnitude = 0.5 * (self.spec_sum[k].norm_sqr() + 1e-20).ln();
            self.cepstrum[k] = log_magnitude;
            self.cepstrum[(n - k) % n] = log_magnitude;
        }
        let _ = self.plan.forward.process_with_scratch(
            &mut self.cepstrum,
            &mut self.spec_prev,
            &mut self.scratch,
        );
        let scale = 1.0 / n as f32;
        self.cepstrum[0] = self.spec_prev[0].re * scale;
        for q in 1..bins {
            let keep = if q < quefrency {
                0.5 + 0.5 * (std::f32::consts::PI * q as f32 / quefrency as f32).cos()
            } else {
                0.0
            };
            let c = self.spec_prev[q].re * scale * keep;
            self.cepstrum[q] = c;
            self.cepstrum[(n - q) % n] = c;
        }
        let _ = self.plan.forward.process_with_scratch(
            &mut self.cepstrum,
            &mut self.spec_prev,
            &mut self.scratch,
        );
        for (out, value) in self.envelope.iter_mut().zip(&self.spec_prev) {
            *out = value.re;
        }
    }

    /// Puts the envelope back where the transposition took it from, by `amount` of the way.
    fn correct_formants(&mut self, ratio: f64, amount: f64, envelope: f32) {
        let quefrency = envelope
            .clamp(MIN_ENVELOPE, (self.n / 2 - 1) as f32)
            .round() as usize;
        self.spectral_envelope(quefrency);
        let shift = ratio.powf(amount.clamp(0.0, 1.0));
        let bins = self.envelope.len();
        let last = (bins - 1) as f64;
        for k in 0..bins {
            let at = (k as f64 * shift).min(last);
            let whole = (at as usize).min(bins - 2);
            let frac = (at - whole as f64) as f32;
            let target =
                self.envelope[whole] + (self.envelope[whole + 1] - self.envelope[whole]) * frac;
            let gain = (target - self.envelope[k])
                .exp()
                .clamp(1.0 / MAX_FORMANT_GAIN, MAX_FORMANT_GAIN);
            self.spec_l[k] *= gain;
            self.spec_r[k] *= gain;
        }
    }

    /// Back to the time domain and into the overlap buffer, under the window and the cut.
    #[allow(clippy::needless_range_loop)]
    fn synthesise(&mut self) {
        let (n, hop) = (self.n, self.hop);
        let scale = 1.0 / n as f32;
        let window = &self.plan.window;
        for spec in [&mut self.spec_l, &mut self.spec_r] {
            spec[0].im = 0.0;
            spec[n / 2].im = 0.0;
        }
        let _ = self.plan.inverse.process_with_scratch(
            &mut self.spec_l,
            &mut self.frame,
            &mut self.scratch,
        );
        for i in 0..n {
            let at = (self.ring_at + i) % n;
            let m = window[i] * self.mask[hop + i];
            self.ring_l[at] += self.frame[i] * m * scale;
            self.ring_w[at] += m * window[i] * self.mask[hop + i];
        }
        let _ = self.plan.inverse.process_with_scratch(
            &mut self.spec_r,
            &mut self.frame,
            &mut self.scratch,
        );
        for i in 0..n {
            let at = (self.ring_at + i) % n;
            self.ring_r[at] += self.frame[i] * window[i] * self.mask[hop + i] * scale;
        }
    }

    /// Makes the grain starting now.
    #[allow(clippy::too_many_arguments)]
    fn emit_grain(
        &mut self,
        s: &SampleBuffer,
        now: f64,
        rate: f64,
        step: f64,
        ratio: f64,
        span: &StretchSpan<'_>,
    ) {
        let half = self.n as f64 * 0.5;
        // The timeline's own place for the middle of this grain.
        let centre = now + rate * half;
        let first = centre - step * (half + self.hop as f64);
        let ring_deficit = 1.0 - self.ring_w[self.ring_at] / self.cola;
        self.read_source(s, first, step);
        let (cut, newly) = self.make_mask(first, step, span.slices, ring_deficit);
        if let (Cut::Before(t) | Cut::After(t), true) = (cut, newly) {
            // The source path reads the attack from where the timeline will have it.
            let frames_until = (t - now) / rate.max(1e-9);
            self.direct = t - frames_until * step;
        }
        self.analyse();
        // Phases start again at a transient, unless a grain just did.
        let forced = match cut {
            Cut::After(t) if self.reset_for != Some(t) => {
                self.reset_for = Some(t);
                true
            }
            _ => false,
        };
        let flux_reset =
            self.has_phase && !self.just_reset && self.spectral_flux() > TRANSIENT_FLUX;
        let reset = !self.has_phase || forced || flux_reset;
        self.propagate(reset);
        self.has_phase = true;
        self.just_reset = reset;
        if let WarpMode::ComplexPro { formants, envelope } = span.mode {
            if formants > 0.0 && (ratio - 1.0).abs() > NO_TRANSPOSE {
                self.correct_formants(ratio, f64::from(formants) / 100.0, envelope);
            }
        }
        self.synthesise();
    }

    fn begin_seek(&mut self, s: &SampleBuffer, src: f64) {
        // What's already been summed for the frames ahead carries the old reading out.
        let mut had_signal = false;
        if let Some(pos) = self.last {
            if pos < s.len() as f64 {
                let cola = self.cola;
                let (n, frames) = (self.n, (self.crossfade as usize).min(self.tail_l.len()));
                for i in 0..frames {
                    let at = (self.ring_at + i) % n;
                    self.tail_l[i] = self.ring_l[at] / cola;
                    self.tail_r[i] = self.ring_r[at] / cola;
                }
                self.tail_at = 0;
                self.tail_len = frames;
                had_signal = true;
            }
        }
        if !had_signal {
            self.tail_at = 0;
            self.tail_len = 0;
        }
        self.fade_in_left = if had_signal || self.fresh {
            self.crossfade
        } else {
            0
        };
        self.ring_l.fill(0.0);
        self.ring_r.fill(0.0);
        self.ring_w.fill(0.0);
        self.until_grain = 0;
        self.has_phase = false;
        self.just_reset = false;
        self.active = None;
        self.reset_for = None;
        self.direct = src;
    }
}

impl Stretcher for PhaseVocoder {
    fn prepare(&mut self, sample_rate: f64) {
        self.sample_rate = sample_rate;
        self.crossfade = crossfade_frames(sample_rate).min(self.tail_l.len() as u32);
    }

    fn seek(&mut self, src: f64) {
        self.seek_to = Some(src);
    }

    fn reset(&mut self) {
        self.ring_l.fill(0.0);
        self.ring_r.fill(0.0);
        self.ring_w.fill(0.0);
        self.ring_at = 0;
        self.until_grain = 0;
        self.has_phase = false;
        self.just_reset = false;
        self.active = None;
        self.reset_for = None;
        self.seek_to = None;
        self.last = None;
        self.tail_len = 0;
        self.fade_in_left = 0;
        self.fresh = true;
    }

    fn declicks(&self) -> bool {
        true
    }

    fn render(&mut self, s: &SampleBuffer, span: StretchSpan<'_>, l: &mut [f32], r: &mut [f32]) {
        let frames = l.len().min(r.len());
        if frames == 0 {
            return;
        }
        if let Some(src) = self.seek_to.take() {
            self.begin_seek(s, src);
        }
        self.fresh = false;
        let rate = (span.src_end - span.src_start) / frames as f64;
        let ratio = 2f64.powf(f64::from(span.transpose) / 12.0);
        let step = ratio * s.sample_rate / self.sample_rate;
        let band = Sinc::band_for(step);
        let crossfade = self.crossfade.max(1) as f32;
        for i in 0..frames {
            if self.until_grain == 0 {
                let now = span.src_start + rate * i as f64;
                self.emit_grain(s, now, rate, step, ratio, &span);
                self.until_grain = self.hop;
            }
            self.until_grain -= 1;

            let at = self.ring_at;
            let deficit = (1.0 - self.ring_w[at] / self.cola).clamp(0.0, 1.0);
            let mut out_l = self.ring_l[at] / self.cola;
            let mut out_r = self.ring_r[at] / self.cola;
            self.ring_l[at] = 0.0;
            self.ring_r[at] = 0.0;
            self.ring_w[at] = 0.0;
            self.ring_at = (at + 1) % self.n;
            if deficit > 0.0005 {
                let (a, b) = self.sinc.read(s, self.direct, band);
                out_l += a * deficit;
                out_r += b * deficit;
            }
            self.direct += step;

            if self.fade_in_left > 0 {
                let gain = 1.0 - self.fade_in_left as f32 / crossfade;
                out_l *= gain;
                out_r *= gain;
                self.fade_in_left -= 1;
            }
            if self.tail_at < self.tail_len {
                let gain = 1.0 - self.tail_at as f32 / self.tail_len as f32;
                out_l += self.tail_l[self.tail_at] * gain;
                out_r += self.tail_r[self.tail_at] * gain;
                self.tail_at += 1;
            }
            l[i] = out_l;
            r[i] = out_r;
        }
        self.last = Some(span.src_end);
    }

    fn latency(&self) -> u32 {
        0
    }
}

impl std::fmt::Debug for PhaseVocoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhaseVocoder")
            .field("flavor", &self.flavor)
            .field("n", &self.n)
            .finish()
    }
}
