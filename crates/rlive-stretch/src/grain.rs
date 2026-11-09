//! Tones and Texture: overlapping grains of the source, each started where the timeline is.
//!
//! A grain is centred on the place the timeline will be when the grain's middle sounds, so a tempo
//! change can't move the clip and there is no latency to compensate. Grains overlap by half under
//! a window that sums to one, so reading at the timeline's own speed plays the source unchanged.
//!
//! Tones looks for the pitch around each grain. The grain is cut on whole periods of that pitch
//! and starts where the last grain's waveform would have got to, which is what keeps a held note
//! from beating against itself. Texture ignores pitch and scatters its grains instead.

use crate::interp::Sinc;
use crate::{crossfade_frames, is_native_rate, source_frame};
use rlive_core::warp::{StretchSpan, Stretcher, WarpMode};
use rlive_core::SampleBuffer;
use std::f64::consts::TAU;
use std::sync::OnceLock;

/// Two grains overlapping and the rest fading out of the way of a seek.
const GRAINS: usize = 8;
const WINDOW_POINTS: usize = 1024;
/// Grain Size for a mode whose settings never reached the stretcher.
const DEFAULT_GRAIN_MS: f32 = 30.0;
/// The range Grain Size means, in milliseconds.
const GRAIN_MS: (f32, f32) = (2.0, 250.0);

/// A grain long enough that what fades out of the way stays at the top of its window.
const TAIL_GRAIN_LEN: u32 = 1 << 20;

/// The pitch search works on a copy of the signal at this fraction of the source's rate.
const PITCH_DECIMATE: usize = 4;
const PITCH_WINDOW: usize = 480;
const MIN_PITCH_HZ: f64 = 50.0;
const MAX_PITCH_HZ: f64 = 1_200.0;
/// How clearly periodic the source must be before it counts as having a pitch.
const MIN_CLARITY: f32 = 0.55;
/// Pitched grains are at least this many periods long.
const MIN_PERIODS: f64 = 2.0;

/// The two window shapes: `hann[i]` overlaps to a constant sum for coherent grains,
/// `power[i]` overlaps to a constant power for incoherent ones.
struct Windows {
    hann: Vec<f32>,
    power: Vec<f32>,
}

fn windows() -> &'static Windows {
    static WINDOWS: OnceLock<Windows> = OnceLock::new();
    WINDOWS.get_or_init(|| {
        let hann: Vec<f32> = (0..=WINDOW_POINTS)
            .map(|i| (0.5 - 0.5 * (TAU * i as f64 / WINDOW_POINTS as f64).cos()) as f32)
            .collect();
        let power = hann.iter().map(|v| v.sqrt()).collect();
        Windows { hann, power }
    })
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Grain {
    on: bool,
    pos: f64,
    step: f64,
    age: u32,
    len: u32,
    /// Output frames of fade left if the grain has been told to go.
    release: u32,
    /// Frames of fade-in, when it starts over silence.
    fade_in: u32,
    /// When set, the grain starts at the top of its window instead of the bottom.
    half: bool,
}

const FREE: Grain = Grain {
    on: false,
    pos: 0.0,
    step: 1.0,
    age: 0,
    len: 1,
    release: 0,
    fade_in: 0,
    half: false,
};

/// What a [`Granular`] listens to in the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Tones,
    Texture,
}

/// The shared granular engine. Use it through [`Tones`] and [`Texture`].
pub struct Granular {
    kind: Kind,
    sinc: &'static Sinc,
    windows: &'static Windows,
    sample_rate: f64,
    crossfade: u32,
    grains: [Grain; GRAINS],
    until_grain: u32,
    current_len: u32,
    /// Where the newest grain is reading, so the next can start from there.
    newest: Option<usize>,
    /// The decimated source around the read head; preallocated to the longest search.
    search: Vec<f32>,
    /// `(period, clarity)` of the source at the head, in source frames.
    last_pitch: (f64, f32),
    random: u64,
    seek_to: Option<f64>,
    last: Option<f64>,
    fresh: bool,
    /// Where the source is being read straight off, at the clip's own tempo. No grains then.
    straight: Option<Straight>,
}

/// Reading the source straight off.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Straight {
    /// The next source frame.
    frame: f64,
    /// Frames of fade-in left, when it starts over silence or something else.
    fade_in: u32,
}

impl Granular {
    fn new(kind: Kind) -> Self {
        Self {
            kind,
            sinc: Sinc::shared(),
            windows: windows(),
            sample_rate: 48_000.0,
            crossfade: crossfade_frames(48_000.0),
            grains: [FREE; GRAINS],
            until_grain: 0,
            current_len: 1,
            newest: None,
            search: vec![0.0; PITCH_WINDOW + 1_200],
            last_pitch: (0.0, 0.0),
            random: 0x9E37_79B9_7F4A_7C15,
            seek_to: None,
            last: None,
            fresh: true,
            straight: None,
        }
    }

    /// The length of the grain being played, in output frames. For tests.
    pub fn grain_frames(&self) -> u32 {
        self.current_len
    }

    /// The pitch period the search last found, in source frames. For tests.
    pub fn found_period(&self) -> f64 {
        self.last_pitch.0
    }

    fn next_random(&mut self) -> f32 {
        // xorshift64*: cheap, deterministic, and good enough to scatter grains.
        let mut x = self.random;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.random = x;
        let bits = x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40;
        (bits as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    }

    fn claim(&mut self) -> usize {
        self.grains
            .iter()
            .position(|g| !g.on)
            .or_else(|| {
                (0..GRAINS)
                    .filter(|&i| self.grains[i].release > 0)
                    .min_by_key(|&i| self.grains[i].release)
            })
            .unwrap_or(0)
    }

    /// Normalised autocorrelation over a decimated stretch of the source: the pitch period in
    /// source frames, and how periodic it is, if anything at all.
    fn detect_pitch(&mut self, s: &SampleBuffer, centre: f64) -> (f64, f32) {
        let decimated_rate = s.sample_rate / PITCH_DECIMATE as f64;
        let max_lag = ((decimated_rate / MIN_PITCH_HZ) as usize).min(self.search.len() / 3);
        let min_lag = ((decimated_rate / MAX_PITCH_HZ) as usize).max(2);
        let window = PITCH_WINDOW.min(self.search.len() - max_lag - 1);
        let span = window + max_lag;
        let n = s.len() as i64;
        let start = centre as i64 - (span * PITCH_DECIMATE) as i64 / 2;
        for (i, out) in self.search.iter_mut().enumerate().take(span) {
            let mut sum = 0.0;
            for d in 0..PITCH_DECIMATE {
                let at = start + (i * PITCH_DECIMATE + d) as i64;
                if (0..n).contains(&at) {
                    sum += s.left[at as usize] + s.right[at as usize];
                }
            }
            *out = sum;
        }
        let energy_at = |search: &[f32], lag: usize| -> f32 {
            search[lag..lag + window].iter().map(|v| v * v).sum()
        };
        let base = energy_at(&self.search, 0);
        if base <= 1e-9 {
            return (0.0, 0.0);
        }
        let (mut best_lag, mut best) = (0usize, 0.0f32);
        let mut previous = 0.0f32;
        let mut rising = false;
        // The first strong peak is the fundamental; a later one is only a multiple of it.
        for lag in min_lag..=max_lag {
            let cross: f32 = self.search[..window]
                .iter()
                .zip(&self.search[lag..lag + window])
                .map(|(a, b)| a * b)
                .sum();
            let denominator = (base * energy_at(&self.search, lag)).sqrt().max(1e-9);
            let value = cross / denominator;
            if rising && value < previous && previous > best + 0.01 {
                best = previous;
                best_lag = lag - 1;
                if best > 0.85 {
                    break;
                }
            }
            rising = value >= previous;
            previous = value;
        }
        if best_lag == 0 {
            return (0.0, best);
        }
        // Refine the peak on the undecimated autocorrelation by a parabola through its top.
        let cross_at = |lag: usize| -> f32 {
            self.search[..window]
                .iter()
                .zip(&self.search[lag..lag + window])
                .map(|(a, b)| a * b)
                .sum()
        };
        let (a, b, c) = (
            cross_at(best_lag - 1),
            cross_at(best_lag),
            cross_at(best_lag + 1),
        );
        let divide = a - 2.0 * b + c;
        let offset = if divide.abs() > 1e-9 {
            (0.5 * (a - c) / divide).clamp(-0.5, 0.5)
        } else {
            0.0
        };
        (
            (best_lag as f64 + f64::from(offset)) * PITCH_DECIMATE as f64,
            best,
        )
    }

    /// How long the next grain should be, in output frames. Pitched sources are cut on whole
    /// periods of their pitch, as Grain Size asks for as near as a whole period can be.
    fn grain_length(&self, base: f64, period: f64, clarity: f32, step: f64) -> u32 {
        let base = base.max(32.0);
        if self.kind == Kind::Tones && period > 0.0 && clarity >= MIN_CLARITY {
            let period_out = period / step.max(1e-6);
            let periods = (base / period_out).round().max(MIN_PERIODS);
            (periods * period_out).round().max(16.0) as u32
        } else {
            base.round() as u32
        }
    }

    /// Starts the grain whose first frame is now.
    #[allow(clippy::too_many_arguments)]
    fn start_grain(
        &mut self,
        s: &SampleBuffer,
        now: f64,
        rate: f64,
        step: f64,
        base_len: f64,
        flux: f32,
    ) {
        // Where this grain should be heard: its centre is where the timeline will be.
        let (period, clarity) = if self.kind == Kind::Tones {
            let at = now + rate * base_len * 0.5;
            self.detect_pitch(s, at)
        } else {
            (0.0, 0.0)
        };
        self.last_pitch = (period, clarity);
        let mut length = self.grain_length(base_len, period, clarity, step);
        let flux = flux.clamp(0.0, 100.0) / 100.0;
        if self.kind == Kind::Texture && flux > 0.0 {
            let shaped = 1.0 + 0.35 * flux * self.next_random();
            length = (length as f32 * shaped).max(16.0) as u32;
        }
        let half_len = f64::from(length) * 0.5;
        let centre = now + rate * half_len;
        let ideal = centre - step * half_len;
        let start = match self.kind {
            Kind::Tones => self.aligned_start(s, ideal, period, clarity),
            Kind::Texture if flux > 0.0 => {
                let scatter = self.next_random() as f64 * flux as f64 * half_len * step;
                ideal + scatter
            }
            Kind::Texture => ideal,
        };
        let slot = self.claim();
        self.grains[slot] = Grain {
            on: true,
            pos: start,
            step,
            age: 0,
            len: length,
            release: 0,
            fade_in: 0,
            half: false,
        };
        self.current_len = length;
        self.newest = Some(slot);
        self.until_grain = (length / 2).max(1);
    }

    /// Where the grain should start so its waveform carries on from the last one's: a whole
    /// number of pitch periods from where the last grain has reached, as near `ideal` as that
    /// gets. A source with no clear pitch, or a head that is already there, starts on `ideal`.
    fn aligned_start(&self, s: &SampleBuffer, ideal: f64, period: f64, clarity: f32) -> f64 {
        let Some(head) = self.newest.map(|i| self.grains[i].pos) else {
            return ideal;
        };
        if (ideal - head).abs() < 0.5 {
            return head;
        }
        if clarity < MIN_CLARITY || period <= 0.0 {
            return ideal;
        }
        let periods = ((ideal - head) / period).round();
        let candidate = head + periods * period;
        // A little search around the whole-period place, in case the pitch was slightly off.
        let reach = (period * 0.125).clamp(2.0, 48.0);
        let (mut best, mut best_score) = (candidate, f32::MIN);
        let mut offset = -reach;
        let band = 0;
        while offset <= reach {
            let at = candidate + offset;
            let score: f32 = (0..32)
                .map(|i| {
                    let d = i as f64 * 4.0;
                    let (a, _) = self.sinc.read(s, head + d, band);
                    let (b, _) = self.sinc.read(s, at + d, band);
                    a * b
                })
                .sum();
            if score > best_score {
                best_score = score;
                best = at;
            }
            offset += 2.0;
        }
        best
    }

    /// Stops reading straight off, leaving a grain to fade out what was being read.
    fn leave_straight(&mut self) {
        if let Some(head) = self.straight.take() {
            let slot = self.claim();
            let top = TAIL_GRAIN_LEN / 2;
            self.grains[slot] = Grain {
                on: true,
                pos: head.frame,
                step: 1.0,
                age: top,
                len: TAIL_GRAIN_LEN,
                release: self.crossfade,
                fade_in: 0,
                half: false,
            };
        }
    }

    /// Plays the source as it is, at the clip's own tempo (manual p. 990). Grains exist so a
    /// stretch can happen between them, and there is no stretch here.
    fn render_straight(
        &mut self,
        s: &SampleBuffer,
        span: &StretchSpan<'_>,
        l: &mut [f32],
        r: &mut [f32],
    ) {
        let seek = self.seek_to.take().is_some();
        let lost = self
            .straight
            .is_none_or(|h| (h.frame - span.src_start).abs() >= 1.0);
        if seek || lost {
            let sounding = self.last.is_some_and(|p| p < s.len() as f64);
            let had_any = self.straight.is_some() || self.grains.iter().any(|g| g.on);
            self.leave_straight();
            let crossfade = self.crossfade;
            for g in self.grains.iter_mut().filter(|g| g.on && g.release == 0) {
                g.release = crossfade;
            }
            let fade_in = if had_any && sounding || self.fresh {
                crossfade
            } else {
                0
            };
            self.straight = Some(Straight {
                frame: span.src_start.round(),
                fade_in,
            });
        }
        self.fresh = false;
        self.newest = None;
        self.until_grain = 0;
        let crossfade = self.crossfade as f32;
        let windows = self.windows;
        let Some(mut head) = self.straight else {
            return;
        };
        for (out_l, out_r) in l.iter_mut().zip(r.iter_mut()) {
            let (mut a, mut b) = source_frame(s, head.frame);
            head.frame += 1.0;
            if head.fade_in > 0 {
                let gain = 1.0 - head.fade_in as f32 / crossfade;
                a *= gain;
                b *= gain;
                head.fade_in -= 1;
            }
            for g in self.grains.iter_mut().filter(|g| g.on) {
                let at = if g.half { g.age + g.len / 2 } else { g.age };
                if at >= g.len || g.release == 0 {
                    g.on = false;
                    continue;
                }
                let window = window_value(&windows.hann, at, g.len);
                let gain = window * g.release as f32 / crossfade;
                let (x, y) = self.sinc.read(s, g.pos, 0);
                g.pos += g.step;
                g.age += 1;
                g.release -= 1;
                g.on = g.release > 0;
                a += x * gain;
                b += y * gain;
            }
            *out_l = a;
            *out_r = b;
        }
        self.straight = Some(head);
        self.last = Some(span.src_end);
    }

    fn begin_seek(&mut self, s: &SampleBuffer, src: f64, base_len: f64, step: f64) {
        let sounding = self.last.is_some_and(|p| p < s.len() as f64);
        let crossfade = self.crossfade;
        let had_any = self.grains.iter().any(|g| g.on);
        for g in &mut self.grains {
            if g.on {
                // Only the grains still sounding have anything to fade out of the way of.
                g.release = crossfade;
            }
        }
        self.newest = None;
        let fade_in = if had_any && sounding || self.fresh {
            crossfade
        } else {
            0
        };
        // The first grain starts at the top of its window, right on the new place, so the next
        // can rise as it falls.
        let length = base_len.round().max(32.0) as u32;
        let slot = self.claim();
        self.grains[slot] = Grain {
            on: true,
            pos: src,
            step,
            age: 0,
            len: length,
            release: 0,
            fade_in,
            half: true,
        };
        self.current_len = length;
        self.newest = Some(slot);
        self.until_grain = 0;
    }
}

fn window_value(table: &[f32], age: u32, len: u32) -> f32 {
    let x = age as f32 / len as f32 * WINDOW_POINTS as f32;
    let i = (x as usize).min(WINDOW_POINTS - 1);
    let frac = x - i as f32;
    table[i] + (table[i + 1] - table[i]) * frac
}

impl Stretcher for Granular {
    fn prepare(&mut self, sample_rate: f64) {
        self.sample_rate = sample_rate;
        self.crossfade = crossfade_frames(sample_rate);
    }

    fn seek(&mut self, src: f64) {
        self.seek_to = Some(src);
    }

    fn reset(&mut self) {
        self.grains = [FREE; GRAINS];
        self.newest = None;
        self.until_grain = 0;
        self.seek_to = None;
        self.last = None;
        self.fresh = true;
        self.straight = None;
        self.random = 0x9E37_79B9_7F4A_7C15;
    }

    fn declicks(&self) -> bool {
        true
    }

    fn render(&mut self, s: &SampleBuffer, span: StretchSpan<'_>, l: &mut [f32], r: &mut [f32]) {
        let frames = l.len().min(r.len());
        if frames == 0 {
            return;
        }
        let (grain_ms, flux) = match span.mode {
            WarpMode::Tones { grain_size } => (grain_size, 0.0),
            WarpMode::Texture { grain_size, flux } => (grain_size, flux),
            _ => (DEFAULT_GRAIN_MS, 0.0),
        };
        let grain_ms = grain_ms.clamp(GRAIN_MS.0, GRAIN_MS.1);
        let base_len = f64::from(grain_ms) * 0.001 * self.sample_rate;
        let rate = (span.src_end - span.src_start) / frames as f64;
        let ratio = 2f64.powf(f64::from(span.transpose) / 12.0);
        let step = ratio * s.sample_rate / self.sample_rate;
        let band = Sinc::band_for(step);
        if is_native_rate(rate) && step == 1.0 {
            self.render_straight(s, &span, &mut l[..frames], &mut r[..frames]);
            return;
        }
        // Coming off the straight read: start over where the timeline is, with a crossfade.
        let was_straight = self.straight.is_some();
        self.leave_straight();
        let seek = if was_straight {
            self.seek_to.take().or(Some(span.src_start))
        } else {
            self.seek_to.take()
        };
        if let Some(src) = seek {
            self.begin_seek(s, src, base_len, step);
        }
        self.fresh = false;
        let crossfade = self.crossfade as f32;
        let windows = self.windows;
        // Texture blends towards a constant-power window as its grains stop being coherent.
        let power_share = if self.kind == Kind::Texture {
            flux.clamp(0.0, 100.0) / 100.0
        } else {
            0.0
        };
        for i in 0..frames {
            if self.until_grain == 0 {
                let now = span.src_start + rate * i as f64;
                self.start_grain(s, now, rate, step, base_len, flux);
            }
            self.until_grain -= 1;
            let (mut out_l, mut out_r, mut weight) = (0.0f32, 0.0f32, 0.0f32);
            for g in &mut self.grains {
                if !g.on {
                    continue;
                }
                let at = if g.half { g.age + g.len / 2 } else { g.age };
                if at >= g.len {
                    g.on = false;
                    continue;
                }
                let hann = window_value(&windows.hann, at, g.len);
                let mut gain = if power_share > 0.0 {
                    let power = window_value(&windows.power, at, g.len);
                    hann + (power - hann) * power_share
                } else {
                    hann
                };
                let (a, b) = self.sinc.read(s, g.pos, band);
                g.pos += g.step;
                g.age += 1;
                if g.fade_in > 0 {
                    gain *= 1.0 - g.fade_in as f32 / crossfade;
                    g.fade_in -= 1;
                }
                if g.release > 0 {
                    gain *= g.release as f32 / crossfade;
                    g.release -= 1;
                    if g.release == 0 {
                        g.on = false;
                    }
                } else {
                    weight += hann;
                }
                out_l += a * gain;
                out_r += b * gain;
            }
            // Grains of different lengths don't quite sum to one under their windows.
            if self.kind == Kind::Tones && weight > 0.25 {
                out_l /= weight;
                out_r /= weight;
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

/// Tones: grains cut on the pitch of what they hold.
pub struct Tones(Granular);

impl Tones {
    pub fn new() -> Self {
        Self(Granular::new(Kind::Tones))
    }

    /// The length of the current grain, in output frames. For tests.
    pub fn grain_frames(&self) -> u32 {
        self.0.grain_frames()
    }

    /// The pitch period the search last found, in source frames. For tests.
    pub fn found_period(&self) -> f64 {
        self.0.found_period()
    }
}

impl Default for Tones {
    fn default() -> Self {
        Self::new()
    }
}

/// Texture: grains that ignore pitch, scattered as far as Fluctuation says.
pub struct Texture(Granular);

impl Texture {
    pub fn new() -> Self {
        Self(Granular::new(Kind::Texture))
    }

    /// The length of the current grain, in output frames. For tests.
    pub fn grain_frames(&self) -> u32 {
        self.0.grain_frames()
    }
}

impl Default for Texture {
    fn default() -> Self {
        Self::new()
    }
}

macro_rules! forward_stretcher {
    ($t:ty) => {
        impl Stretcher for $t {
            fn prepare(&mut self, sample_rate: f64) {
                self.0.prepare(sample_rate);
            }
            fn seek(&mut self, src: f64) {
                self.0.seek(src);
            }
            fn reset(&mut self) {
                self.0.reset();
            }
            fn declicks(&self) -> bool {
                self.0.declicks()
            }
            fn render(
                &mut self,
                s: &SampleBuffer,
                span: StretchSpan<'_>,
                l: &mut [f32],
                r: &mut [f32],
            ) {
                self.0.render(s, span, l, r);
            }
            fn latency(&self) -> u32 {
                self.0.latency()
            }
        }
    };
}

forward_stretcher!(Tones);
forward_stretcher!(Texture);
