//! Finding the tempo and the beats of a sample, for Auto-Warp.
//!
//! Runs on a background thread. The flow is the classic one: an onset-strength curve from
//! band-wise spectral flux, a tempo from the curve's autocorrelation under a prior that doubts
//! very fast and very slow tempos, and the beats themselves by dynamic programming that rewards
//! onsets and punishes gaps away from the tempo (Ellis, 2007). Bars come from whichever beat
//! phase carries the most low-frequency attack.

use realfft::RealFftPlanner;
use rlive_core::warp::BeatGrid;

/// Bumped when tracking changes enough that stored results should be recomputed.
pub const TRACKER_VERSION: u32 = 1;

/// The analysis rate of the onset curve is close to this at any sample rate.
const HOP_SECONDS: f64 = 0.0058;
const WINDOW_SECONDS: f64 = 0.046;
const BANDS: usize = 6;
/// The slowest and fastest tempo the tracker will return.
const MIN_BPM: f64 = 60.0;
const MAX_BPM: f64 = 200.0;
/// Where the tempo prior is centred: the middle of what most music is, in BPM.
const PRIOR_BPM: f64 = 120.0;
/// How far from the prior's centre, in octaves, a tempo starts being doubted.
const PRIOR_OCTAVES: f64 = 1.0;
/// The octave of tempos that are reported when the rhythm can't choose between two of them.
const OCTAVE_BPM: (f64, f64) = (83.0, 166.0);
/// How much of a tempo's score its double or half needs to count as just as likely.
const OCTAVE_EVIDENCE: f32 = 0.4;
/// A short sample has too few beats to say anything useful.
const MIN_SECONDS: f64 = 3.0;
/// How strongly an interval away from the tempo is punished against an onset's worth of score.
const TIGHTNESS: f32 = 90.0;
/// Below this, the tempo is called unconfident and the caller can fall back to an estimate.
const MIN_CONFIDENCE: f32 = 0.05;

/// The tempo and beats of a sample. `None` if it is too short or too quiet to tell.
///
/// `beats_per_bar` only decides which beat is called the downbeat.
pub fn track_beats(
    left: &[f32],
    right: Option<&[f32]>,
    sample_rate: f64,
    beats_per_bar: usize,
) -> Option<BeatGrid> {
    if sample_rate <= 0.0 || (left.len() as f64) < MIN_SECONDS * sample_rate {
        return None;
    }
    let onsets = onset_curve(left, right, sample_rate)?;
    let hop_seconds = onsets.hop_seconds;
    let (lag, confidence) = estimate_tempo(&onsets.total, hop_seconds)?;
    if confidence < MIN_CONFIDENCE {
        return None;
    }
    let path = dynamic_beats(&onsets.total, lag)?;
    if path.len() < 2 {
        return None;
    }
    // A frame sees an attack when the attack reaches the middle of its window.
    let beats: Vec<f64> = path
        .iter()
        .map(|&i| i as f64 * hop_seconds + onsets.window_seconds * 0.5)
        .collect();
    // The tempo the beats actually keep, which is finer than the curve's hop.
    let bpm = fit_bpm(&beats).clamp(MIN_BPM, MAX_BPM);
    let downbeat = pick_downbeat(&path, &onsets.low, beats_per_bar.max(1));
    Some(BeatGrid {
        bpm,
        beats,
        downbeat,
        confidence,
    })
}

struct OnsetCurve {
    /// Summed over all bands.
    total: Vec<f32>,
    /// The low bands alone, which is where kicks are.
    low: Vec<f32>,
    hop_seconds: f64,
    window_seconds: f64,
}

fn onset_curve(left: &[f32], right: Option<&[f32]>, sample_rate: f64) -> Option<OnsetCurve> {
    let frames = match right {
        Some(r) => left.len().min(r.len()),
        None => left.len(),
    };
    let hop = ((HOP_SECONDS * sample_rate) as usize).max(32);
    let window_len = ((WINDOW_SECONDS * sample_rate) as usize)
        .next_power_of_two()
        .max(64);
    if frames < window_len + hop {
        return None;
    }
    let mono: Vec<f32> = match right {
        Some(r) => left[..frames]
            .iter()
            .zip(&r[..frames])
            .map(|(a, b)| 0.5 * (a + b))
            .collect(),
        None => left[..frames].to_vec(),
    };
    let window: Vec<f32> = (0..window_len)
        .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / window_len as f32).cos())
        .collect();
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(window_len);
    let mut input = fft.make_input_vec();
    let mut spectrum = fft.make_output_vec();
    let edges = band_edges(window_len, sample_rate);
    let count = (frames - window_len) / hop;
    let mut total = vec![0.0f32; count];
    let mut low = vec![0.0f32; count];
    let mut previous = [0.0f32; BANDS];
    let full_scale = window_len as f32 * 0.25;
    let scale = 40.0 / (full_scale * full_scale);
    for (f, (out_total, out_low)) in total.iter_mut().zip(low.iter_mut()).enumerate() {
        let start = f * hop;
        for (i, x) in input.iter_mut().enumerate() {
            *x = mono[start + i] * window[i];
        }
        let _ = fft.process(&mut input, &mut spectrum);
        let mut current = [0.0f32; BANDS];
        for b in 0..BANDS {
            let energy: f32 = spectrum[edges[b]..edges[b + 1]]
                .iter()
                .map(|c| c.norm_sqr())
                .sum();
            // Level-compressed, but gently: accents are what tell quarters from eighths.
            current[b] = (1.0 + energy * scale).ln();
        }
        let mut sum = 0.0f32;
        for b in 0..BANDS {
            let rise = (current[b] - previous[b]).max(0.0);
            sum += rise;
            if b < 2 {
                *out_low += rise;
            }
        }
        *out_total = sum;
        previous = current;
    }
    high_pass(&mut total);
    high_pass(&mut low);
    if total.iter().all(|v| *v <= 1e-6) {
        return None;
    }
    Some(OnsetCurve {
        total,
        low,
        hop_seconds: hop as f64 / sample_rate,
        window_seconds: window_len as f64 / sample_rate,
    })
}

/// Bin edges of the analysis bands: below 150 Hz, 150-400, up to Nyquist in widening steps.
fn band_edges(window_len: usize, sample_rate: f64) -> [usize; BANDS + 1] {
    let bins = window_len / 2 + 1;
    let to_bin = |hz: f64| ((hz / sample_rate * window_len as f64) as usize).clamp(1, bins);
    let mut edges = [0usize; BANDS + 1];
    let hz = [
        0.0,
        150.0,
        400.0,
        1_200.0,
        3_500.0,
        8_000.0,
        sample_rate * 0.5,
    ];
    for (edge, h) in edges.iter_mut().zip(hz) {
        *edge = if h == 0.0 { 0 } else { to_bin(h) };
    }
    for i in 1..=BANDS {
        edges[i] = edges[i].max(edges[i - 1] + 1).min(bins);
    }
    edges
}

/// Subtracts a running mean, so a level change isn't an onset and only rises stay.
fn high_pass(curve: &mut [f32]) {
    const SPAN: usize = 32;
    let cumulative: Vec<f32> = curve
        .iter()
        .scan(0.0f32, |sum, v| {
            *sum += *v;
            Some(*sum)
        })
        .collect();
    let n = curve.len();
    for (i, v) in curve.iter_mut().enumerate() {
        let (a, b) = (i.saturating_sub(SPAN), (i + SPAN).min(n - 1));
        let width = (b - a) as f32;
        let mean = (cumulative[b] - cumulative[a]) / width.max(1.0);
        *v = (*v - mean).max(0.0);
    }
}

/// The autocorrelation of the onset curve at every lag up to `max`.
fn autocorrelate(curve: &[f32], max: usize) -> Vec<f32> {
    let n = (curve.len() + max + 1).next_power_of_two();
    let mut planner = RealFftPlanner::<f32>::new();
    let forward = planner.plan_fft_forward(n);
    let inverse = planner.plan_fft_inverse(n);
    let mut input = vec![0.0f32; n];
    input[..curve.len()].copy_from_slice(curve);
    let mut spectrum = forward.make_output_vec();
    let _ = forward.process(&mut input, &mut spectrum);
    for c in &mut spectrum {
        *c = rustfft_square(*c);
    }
    let mut output = vec![0.0f32; n];
    let _ = inverse.process(&mut spectrum, &mut output);
    output.truncate(max + 1);
    let zero = output[0].max(1e-9);
    for v in &mut output {
        *v /= zero;
    }
    output
}

fn rustfft_square(c: realfft::num_complex::Complex32) -> realfft::num_complex::Complex32 {
    realfft::num_complex::Complex32::new(c.norm_sqr(), 0.0)
}

/// The best beat length in onset frames, and how strongly the curve repeats at it.
fn estimate_tempo(curve: &[f32], hop_seconds: f64) -> Option<(f64, f32)> {
    let bpm_to_lag = |bpm: f64| 60.0 / bpm / hop_seconds;
    let lag_to_bpm = |lag: f64| 60.0 / (lag * hop_seconds);
    let (min_lag, max_lag) = (bpm_to_lag(MAX_BPM), bpm_to_lag(MIN_BPM));
    let max = (max_lag * 2.0).ceil() as usize;
    if curve.len() < max * 2 {
        return None;
    }
    let ac = autocorrelate(curve, max * 2 + 2);
    // A tempo's lag and its multiples should all repeat; a false double only has half of them.
    let score = |lag: f64| -> f32 {
        (1..=4)
            .map(|m| {
                let at = lag * m as f64;
                if at + 1.0 >= ac.len() as f64 {
                    0.0
                } else {
                    let whole = at as usize;
                    let frac = (at - whole as f64) as f32;
                    (ac[whole] + (ac[whole + 1] - ac[whole]) * frac) / m as f32
                }
            })
            .sum::<f32>()
    };
    let prior = |lag: f64| -> f32 {
        let octaves = (lag_to_bpm(lag) / PRIOR_BPM).log2().abs();
        (1.0 - (octaves - PRIOR_OCTAVES).max(0.0) * 0.6).max(0.2) as f32
    };
    let (start, end) = (min_lag.ceil() as usize, (max_lag.floor() as usize));
    let (mut best_lag, mut best_score) = (0usize, f32::MIN);
    for lag in start..=end {
        let value = score(lag as f64) * prior(lag as f64);
        if value > best_score {
            best_score = value;
            best_lag = lag;
        }
    }
    if best_lag == 0 {
        return None;
    }
    // Sub-frame peak of the raw score, so the DP starts close to the right beat length.
    let (a, b, c) = (
        score(best_lag as f64 - 1.0),
        score(best_lag as f64),
        score(best_lag as f64 + 1.0),
    );
    let divide = a - 2.0 * b + c;
    let offset = if divide.abs() > 1e-9 {
        f64::from((0.5 * (a - c) / divide).clamp(-0.5, 0.5))
    } else {
        0.0
    };
    let mut lag = best_lag as f64 + offset;
    let unfolded = lag;
    // A groove and its double or half are the same pattern at another speed, so nothing in the
    // audio tells them apart: report the one inside this octave, unless there's no evidence for
    // it at all. Live has the ×2 and ÷2 buttons for the ones this gets wrong.
    if lag_to_bpm(lag) < OCTAVE_BPM.0 && score(lag / 2.0) >= score(lag) * OCTAVE_EVIDENCE {
        lag /= 2.0;
    } else if lag_to_bpm(lag) >= OCTAVE_BPM.1 && score(lag * 2.0) >= score(lag) * OCTAVE_EVIDENCE {
        lag *= 2.0;
    }
    let confidence = (ac_at(&ac, unfolded) / ac[0].max(1e-9)).clamp(0.0, 1.0);
    Some((lag, confidence))
}

fn ac_at(ac: &[f32], lag: f64) -> f32 {
    let whole = lag as usize;
    if whole + 1 >= ac.len() {
        return 0.0;
    }
    let frac = (lag - whole as f64) as f32;
    ac[whole] + (ac[whole + 1] - ac[whole]) * frac
}

/// The beat path: each beat is rewarded for its onset and penalised for a gap away from `lag`.
#[allow(clippy::needless_range_loop)]
fn dynamic_beats(curve: &[f32], lag: f64) -> Option<Vec<usize>> {
    let n = curve.len();
    let lag_frames = lag.max(1.0);
    let window_left = (lag_frames * 2.0).ceil() as usize;
    let window_right = (lag_frames * 0.5).floor() as usize;
    if n < window_left + 2 {
        return None;
    }
    let scale = curve.iter().copied().fold(0.0f32, f32::max).max(1e-6);
    let mut score = vec![0.0f32; n];
    let mut previous = vec![-1i64; n];
    for t in 0..n {
        let onset = curve[t] / scale;
        let (mut best, mut from) = (f32::MIN, -1i64);
        let start = t.saturating_sub(window_left);
        let stop = t.saturating_sub(window_right);
        for tau in start..=stop {
            if tau >= t {
                break;
            }
            let ratio = ((t - tau) as f32 / lag_frames as f32).ln();
            let candidate = score[tau] - TIGHTNESS * 0.01 * ratio * ratio;
            if candidate > best {
                best = candidate;
                from = tau as i64;
            }
        }
        if best == f32::MIN || best < 0.0 {
            score[t] = onset;
            previous[t] = -1;
        } else {
            score[t] = onset + best;
            previous[t] = from;
        }
    }
    let tail = n.saturating_sub(lag_frames.ceil() as usize + 1);
    let end = (tail..n).max_by(|a, b| score[*a].total_cmp(&score[*b]))?;
    let mut beats = Vec::new();
    let mut at = end as i64;
    while at >= 0 {
        beats.push(at as usize);
        at = previous[at as usize];
    }
    beats.reverse();
    Some(beats)
}

fn fit_bpm(beats: &[f64]) -> f64 {
    let n = beats.len() as f64;
    let (sx, sy): (f64, f64) = beats
        .iter()
        .enumerate()
        .fold((0.0, 0.0), |(x, y), (i, b)| (x + i as f64, y + *b));
    let (sxx, sxy): (f64, f64) = beats
        .iter()
        .enumerate()
        .fold((0.0, 0.0), |(xx, xy), (i, b)| {
            let i = i as f64;
            (xx + i * i, xy + i * *b)
        });
    let denominator = n * sxx - sx * sx;
    if denominator.abs() < 1e-9 {
        return PRIOR_BPM;
    }
    let seconds_per_beat = (n * sxy - sx * sy) / denominator;
    if seconds_per_beat <= 1e-6 {
        PRIOR_BPM
    } else {
        60.0 / seconds_per_beat
    }
}

fn pick_downbeat(path: &[usize], low: &[f32], beats_per_bar: usize) -> usize {
    (0..beats_per_bar)
        .max_by(|a, b| {
            let sum = |phase: usize| -> f32 {
                path.iter()
                    .enumerate()
                    .filter(|(i, _)| i % beats_per_bar == phase)
                    .map(|(_, at)| low.get(*at).copied().unwrap_or(0.0))
                    .sum()
            };
            sum(*a).total_cmp(&sum(*b))
        })
        .unwrap_or(0)
}
