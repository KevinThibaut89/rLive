//! Finding where notes and beats begin in a sample.
//!
//! Spectral flux finds candidate frames, then each is placed on its attack in the waveform. Runs
//! on a background thread; analysing a file is far faster than playing it.

use realfft::RealFftPlanner;

/// Bumped when detection changes enough that stored results should be recomputed.
pub const DETECTOR_VERSION: u32 = 1;

/// The analysis window at 44.1 or 48 kHz; other rates scale it.
const WINDOW_AT_48K: usize = 1024;
const HOP_AT_48K: usize = 256;
/// Two transients can't be closer than this, in seconds.
const MIN_GAP: f64 = 0.040;
/// How many frames either side of a candidate the threshold is measured over.
const MEDIAN_FRAMES: usize = 16;
/// A candidate must stand this far above the flux around it...
const THRESHOLD_RATIO: f32 = 1.6;
/// ...and this far up the sample's own range of flux.
const THRESHOLD_FLOOR: f32 = 0.07;
/// Frames quieter than this don't count, however spectrally they change.
const SILENCE_RMS: f32 = 0.000_5;
/// An attack starts where the level first climbs this far towards its peak.
const ATTACK_FRACTION: f32 = 0.15;
const ATTACK_WINDOW_SECS: f64 = 0.0008;

/// Seconds, ascending, at which transients start in the sample.
pub fn detect_transients(left: &[f32], right: Option<&[f32]>, sample_rate: f64) -> Vec<f64> {
    let frames = match right {
        Some(r) => left.len().min(r.len()),
        None => left.len(),
    };
    if frames == 0 || sample_rate <= 0.0 {
        return Vec::new();
    }
    let mono: Vec<f32> = match right {
        Some(r) => left[..frames]
            .iter()
            .zip(&r[..frames])
            .map(|(a, b)| 0.5 * (a + b))
            .collect(),
        None => left[..frames].to_vec(),
    };

    let scale = (sample_rate / 48_000.0).max(0.25);
    let window_len = ((WINDOW_AT_48K as f64 * scale) as usize).next_power_of_two();
    let hop = ((HOP_AT_48K as f64 * scale) as usize).max(16);
    if mono.len() < hop {
        return Vec::new();
    }
    let window: Vec<f32> = (0..window_len)
        .map(|i| 0.5 - 0.5 * ((i as f32 + 0.5) / window_len as f32 * std::f32::consts::TAU).cos())
        .collect();

    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(window_len);
    let mut input = fft.make_input_vec();
    let mut spectrum = fft.make_output_vec();
    let mut magnitudes = vec![0.0f32; spectrum.len()];
    let mut previous = vec![0.0f32; spectrum.len()];
    let frame_count = 1 + (mono.len() - 1) / hop;
    let mut flux = vec![0.0f32; frame_count];
    let mut levels = vec![0.0f32; frame_count];
    for frame in 0..frame_count {
        // Frame `frame` is centred on sample `frame * hop`, padded with silence at the edges.
        let centre = frame * hop;
        let start = centre as isize - (window_len / 2) as isize;
        let mut energy = 0.0;
        for (i, (out, w)) in input.iter_mut().zip(&window).enumerate() {
            let at = start + i as isize;
            let x = if (0..mono.len() as isize).contains(&at) {
                mono[at as usize]
            } else {
                0.0
            };
            energy += x * x;
            *out = x * w;
        }
        levels[frame] = (energy / window_len as f32).sqrt();
        if fft.process(&mut input, &mut spectrum).is_err() {
            return Vec::new();
        }
        for (m, c) in magnitudes.iter_mut().zip(&spectrum) {
            *m = (1.0 + 100.0 * c.norm()).ln();
        }
        if frame > 0 {
            flux[frame] = magnitudes
                .iter()
                .zip(&previous)
                .map(|(a, b)| (a - b).max(0.0))
                .sum();
        }
        std::mem::swap(&mut magnitudes, &mut previous);
    }

    let top = flux.iter().cloned().fold(0.0f32, f32::max);
    if top <= 0.0 {
        return Vec::new();
    }
    let floor = top * THRESHOLD_FLOOR;
    let min_gap_frames = (MIN_GAP * sample_rate) as usize;
    let mut picked: Vec<usize> = Vec::new();
    let mut around = Vec::with_capacity(2 * MEDIAN_FRAMES + 1);
    for frame in 1..frame_count.saturating_sub(1) {
        let here = flux[frame];
        if here < floor
            || levels[frame] < SILENCE_RMS
            || here < flux[frame - 1]
            || here < flux[frame + 1]
        {
            continue;
        }
        let from = frame.saturating_sub(MEDIAN_FRAMES);
        let to = (frame + MEDIAN_FRAMES + 1).min(frame_count);
        around.clear();
        around.extend_from_slice(&flux[from..to]);
        around.sort_by(f32::total_cmp);
        let median = around[around.len() / 2];
        if here < median * THRESHOLD_RATIO {
            continue;
        }
        let at = attack_start(&mono, frame * hop, window_len, sample_rate);
        match picked.last_mut() {
            // Too close to the last one: the first of a flurry is the attack.
            Some(last) if at.saturating_sub(*last) < min_gap_frames => {}
            _ => picked.push(at),
        }
    }
    picked.into_iter().map(|f| f as f64 / sample_rate).collect()
}

/// Where the attack near `centre` starts, in the waveform itself.
fn attack_start(mono: &[f32], centre: usize, window: usize, sample_rate: f64) -> usize {
    let start = centre.saturating_sub(window);
    let end = (centre + window / 2).min(mono.len());
    if end <= start {
        return centre;
    }
    let step = ((ATTACK_WINDOW_SECS * sample_rate) as usize).max(4);
    let mut peak = 0.0f32;
    let mut levels = Vec::with_capacity((end - start) / step + 1);
    for chunk in mono[start..end].chunks(step) {
        let level = chunk.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        peak = peak.max(level);
        levels.push(level);
    }
    let peak_at = levels
        .iter()
        .position(|l| *l >= peak * 0.999)
        .unwrap_or_default();
    let target = peak * ATTACK_FRACTION;
    // Back from the peak to where the level last was this low.
    let mut i = peak_at;
    while i > 0 && levels[i - 1] >= target {
        i -= 1;
    }
    start + i * step
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48_000.0;

    fn silence(seconds: f64) -> Vec<f32> {
        vec![0.0; (seconds * SR) as usize]
    }

    struct Noise(u32);

    impl Noise {
        fn next(&mut self) -> f32 {
            self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (self.0 >> 9) as f32 / (1u32 << 22) as f32 * 2.0 - 1.0
        }
    }

    /// Adds a noise hit with a fast attack and an exponential decay.
    fn add_hit(buffer: &mut [f32], at: usize, level: f32, decay: f32, rng: &mut Noise) {
        for (i, s) in buffer[at..].iter_mut().enumerate() {
            let env = (-(i as f32) / (decay * SR as f32)).exp();
            if env < 1e-4 {
                break;
            }
            let attack = (i as f32 / 12.0).min(1.0);
            *s += rng.next() * level * env * attack;
        }
    }

    /// Adds a decaying tone, the shape of a pluck.
    fn add_pluck(buffer: &mut [f32], at: usize, hz: f32, level: f32) {
        for (i, s) in buffer[at..].iter_mut().enumerate() {
            let t = i as f32 / SR as f32;
            let env = (-t / 0.09).exp();
            if env < 1e-4 {
                break;
            }
            *s += level
                * env
                * ((t * hz * std::f32::consts::TAU).sin()
                    + 0.4 * (t * 2.0 * hz * std::f32::consts::TAU).sin());
        }
    }

    struct Score {
        precision: f64,
        recall: f64,
        worst_ms: f64,
    }

    fn score(found: &[f64], truth: &[usize]) -> Score {
        const TOLERANCE: f64 = 0.015;
        let mut used = vec![false; found.len()];
        let mut hits = 0;
        let mut worst: f64 = 0.0;
        for t in truth {
            let t = *t as f64 / SR;
            let nearest = found
                .iter()
                .enumerate()
                .filter(|(i, _)| !used[*i])
                .min_by(|a, b| (a.1 - t).abs().total_cmp(&(b.1 - t).abs()));
            if let Some((i, f)) = nearest {
                if (f - t).abs() < TOLERANCE {
                    used[i] = true;
                    hits += 1;
                    worst = worst.max((f - t).abs());
                }
            }
        }
        Score {
            precision: hits as f64 / found.len().max(1) as f64,
            recall: hits as f64 / truth.len().max(1) as f64,
            worst_ms: worst * 1000.0,
        }
    }

    fn assert_score(found: &[f64], truth: &[usize]) {
        let s = score(found, truth);
        assert!(
            s.precision >= 0.92 && s.recall >= 0.92,
            "precision {:.3} recall {:.3} ({} found, {} true)",
            s.precision,
            s.recall,
            found.len(),
            truth.len()
        );
        assert!(s.worst_ms < 12.0, "onset off by {:.2} ms", s.worst_ms);
    }

    #[test]
    fn finds_every_hit_of_a_noise_pattern() {
        let mut b = silence(6.0);
        let mut rng = Noise(7);
        let truth: Vec<usize> = (1..24).map(|i| (i as f64 * 0.235 * SR) as usize).collect();
        for (n, at) in truth.iter().enumerate() {
            let level = [0.9, 0.3, 0.6, 0.2][n % 4];
            add_hit(&mut b, *at, level, 0.04, &mut rng);
        }
        assert_score(&detect_transients(&b, None, SR), &truth);
    }

    #[test]
    fn finds_plucked_notes_of_different_levels() {
        let mut b = silence(8.0);
        let truth: Vec<usize> = (1..20).map(|i| (i as f64 * 0.37 * SR) as usize).collect();
        for (n, at) in truth.iter().enumerate() {
            let hz = [220.0, 330.0, 277.2, 440.0, 196.0][n % 5];
            let level = [0.5, 0.15, 0.3][n % 3];
            add_pluck(&mut b, *at, hz, level);
        }
        assert_score(&detect_transients(&b, None, SR), &truth);
    }

    #[test]
    fn works_at_other_sample_rates_and_in_stereo() {
        let rate = 44_100.0;
        let n = (3.0 * rate) as usize;
        let mut l = vec![0.0; n];
        let mut r = vec![0.0; n];
        let mut rng = Noise(3);
        let truth: Vec<f64> = (1..12).map(|i| i as f64 * 0.25).collect();
        for t in &truth {
            let at = (t * rate) as usize;
            for (i, (a, b)) in l[at..].iter_mut().zip(&mut r[at..]).enumerate().take(4000) {
                let env = (-(i as f32) / 1500.0).exp();
                let v = rng.next() * env * 0.5;
                *a += v;
                *b += v * 0.3;
            }
        }
        let found = detect_transients(&l, Some(&r), rate);
        assert_eq!(found.len(), truth.len(), "{found:?}");
        for (f, t) in found.iter().zip(&truth) {
            assert!((f - t).abs() < 0.008, "{f} {t}");
        }
    }

    #[test]
    fn a_steady_tone_has_no_transients_after_it_starts() {
        let b: Vec<f32> = (0..(3.0 * SR) as usize)
            .map(|i| (i as f32 / SR as f32 * 440.0 * std::f32::consts::TAU).sin() * 0.5)
            .collect();
        let found = detect_transients(&b, None, SR);
        assert!(found.iter().all(|t| *t < 0.1), "{found:?}");
    }

    #[test]
    fn silence_and_tiny_inputs_have_none() {
        assert!(detect_transients(&silence(1.0), None, SR).is_empty());
        assert!(detect_transients(&[], None, SR).is_empty());
        assert!(detect_transients(&[0.5; 10], None, SR).is_empty());
    }

    #[test]
    fn close_hits_collapse_to_one() {
        let mut b = silence(1.0);
        let mut rng = Noise(11);
        add_hit(&mut b, 12_000, 0.8, 0.02, &mut rng);
        add_hit(&mut b, 12_000 + 480, 0.8, 0.02, &mut rng);
        add_hit(&mut b, 30_000, 0.8, 0.02, &mut rng);
        let found = detect_transients(&b, None, SR);
        assert_eq!(found.len(), 2, "{found:?}");
    }
}
