//! Tempo Follower: finds the tempo of audio arriving at an input, while it arrives.
//!
//! The signal becomes an onset envelope at about 170 Hz, a recursive autocorrelation of that keeps
//! a memory of the last few seconds, and a comb across the plausible tempos picks the beat. All of
//! it runs inside the audio callback's budget and allocates nothing after [`TempoFollower::new`].

/// How many onset frames a second, roughly. The hop is a whole number of samples of this.
const FRAMES_PER_SECOND: f64 = 172.0;
/// The tempos considered, in BPM.
const MIN_BPM: f64 = 60.0;
const MAX_BPM: f64 = 190.0;
const STEP_BPM: f64 = 0.1;
/// How quickly old onsets stop counting: the time constant of the autocorrelation, in seconds.
const MEMORY_SECONDS: f64 = 8.0;
/// How many multiples of the beat the comb looks at.
const COMB: usize = 4;
const COMB_WEIGHTS: [f32; COMB] = [1.0, 0.8, 0.6, 0.5];
/// How often an estimate is made, in onset frames.
const ESTIMATE_EVERY: usize = 32;
/// Half-width of the preference for tempos near the middle of the range, in octaves.
const PRIOR_OCTAVES: f64 = 0.9;
/// The tempo a pattern that fits two octaves is read nearest to. A steady click fits both.
const PRIOR_CENTRE_BPM: f64 = 125.0;
/// Below this much average onset activity the input counts as silent.
const SILENT: f32 = 1e-5;

#[derive(Debug, Clone)]
pub struct TempoFollower {
    hop: usize,
    acc: f32,
    acc_n: usize,
    previous: f32,
    highpass: (f32, f32),
    /// The last onset strengths, as a ring.
    env: Box<[f32]>,
    head: usize,
    /// Number of onset frames seen so far, stopping at the ring's length.
    frames: usize,
    corr: Box<[f32]>,
    energy: f32,
    decay: f32,
    /// Onset frames left before the next estimate.
    until_estimate: usize,
    /// The comb's score for each tempo, kept here to avoid allocating.
    scores: Box<[f32]>,
    /// The latest estimate, after smoothing.
    bpm: Option<f64>,
    env_rate: f64,
    min_lag: usize,
    max_lag: usize,
}

impl TempoFollower {
    pub fn new(sample_rate: f64) -> Self {
        let hop = (sample_rate / FRAMES_PER_SECOND).round().max(1.0) as usize;
        let env_rate = sample_rate / hop as f64;
        let max_lag = (env_rate * 60.0 / MIN_BPM * COMB as f64).ceil() as usize + 2;
        let min_lag = (env_rate * 60.0 / MAX_BPM).floor().max(1.0) as usize;
        let tempos = ((MAX_BPM - MIN_BPM) / STEP_BPM).round() as usize + 1;
        Self {
            hop,
            acc: 0.0,
            acc_n: 0,
            previous: 0.0,
            highpass: (0.0, 0.0),
            env: vec![0.0; max_lag + 1].into_boxed_slice(),
            head: 0,
            frames: 0,
            corr: vec![0.0; max_lag + 1].into_boxed_slice(),
            energy: 0.0,
            decay: (-1.0 / (env_rate * MEMORY_SECONDS)).exp() as f32,
            until_estimate: ESTIMATE_EVERY,
            scores: vec![0.0; tempos].into_boxed_slice(),
            bpm: None,
            env_rate,
            min_lag,
            max_lag,
        }
    }

    /// The tempo heard, once there's something to go on.
    pub fn tempo(&self) -> Option<f64> {
        self.bpm
    }

    /// Forget everything heard.
    pub fn reset(&mut self) {
        self.acc = 0.0;
        self.acc_n = 0;
        self.previous = 0.0;
        self.highpass = (0.0, 0.0);
        self.env.iter_mut().for_each(|v| *v = 0.0);
        self.corr.iter_mut().for_each(|v| *v = 0.0);
        self.head = 0;
        self.frames = 0;
        self.energy = 0.0;
        self.until_estimate = ESTIMATE_EVERY;
        self.bpm = None;
    }

    /// Hear one channel of input. Returns the tempo when an estimate was made this call.
    pub fn feed(&mut self, samples: &[f32]) -> Option<f64> {
        let mut out = None;
        // A gentle high-pass keeps low rumble from reading as level.
        let (mut x1, mut y1) = self.highpass;
        for &s in samples {
            let y = s - x1 + 0.995 * y1;
            x1 = s;
            y1 = y;
            self.acc += y * y;
            self.acc_n += 1;
            if self.acc_n == self.hop {
                self.push_frame();
                self.acc = 0.0;
                self.acc_n = 0;
                if self.until_estimate == 0 {
                    self.until_estimate = ESTIMATE_EVERY;
                    out = self.estimate().or(out);
                }
            }
        }
        self.highpass = (x1, y1);
        out
    }

    fn push_frame(&mut self) {
        let level = (self.acc / self.hop as f32 + 1e-10).ln();
        let onset = (level - self.previous).max(0.0);
        self.previous = level;
        // The envelope's own average would otherwise make every lag look alike.
        let value = onset.min(30.0);
        let len = self.env.len();
        self.head = (self.head + 1) % len;
        self.env[self.head] = value;
        self.frames = (self.frames + 1).min(len);
        self.energy = self.energy * self.decay + value * value;
        let lags = self.max_lag.min(self.frames.saturating_sub(1));
        for lag in 0..=lags {
            let other = self.env[(self.head + len - lag) % len];
            self.corr[lag] = self.corr[lag] * self.decay + value * other;
        }
        self.until_estimate = self.until_estimate.saturating_sub(1);
    }

    /// How strongly the comb responds at this lag, from the autocorrelation by interpolating.
    fn corr_at(&self, lag: f64) -> f32 {
        let i = lag.floor() as usize;
        if i + 1 >= self.corr.len() {
            return 0.0;
        }
        let frac = (lag - i as f64) as f32;
        self.corr[i] * (1.0 - frac) + self.corr[i + 1] * frac
    }

    fn estimate(&mut self) -> Option<f64> {
        if self.frames < self.env.len() / 2 || self.energy < SILENT {
            return None;
        }
        let mean: f32 = self.corr[self.min_lag..self.max_lag].iter().sum::<f32>()
            / (self.max_lag - self.min_lag) as f32;
        let mut best = (f32::MIN, 0usize);
        for i in 0..self.scores.len() {
            let bpm = MIN_BPM + i as f64 * STEP_BPM;
            let lag = self.env_rate * 60.0 / bpm;
            let mut score = 0.0;
            for (k, w) in COMB_WEIGHTS.iter().enumerate() {
                score += w * (self.corr_at(lag * (k + 1) as f64) - mean);
            }
            // A tempo near the middle of the range is the likelier reading of a pattern that
            // fits two tempos an octave apart.
            let octaves = (bpm / PRIOR_CENTRE_BPM).log2() / PRIOR_OCTAVES;
            score *= (-0.5 * octaves * octaves).exp() as f32;
            self.scores[i] = score;
            if score > best.0 {
                best = (score, i);
            }
        }
        if best.0 <= 0.0 {
            return None;
        }
        let i = best.1;
        // Between the two best tempos, by fitting a parabola to the scores.
        let refined = if i > 0 && i + 1 < self.scores.len() {
            let (a, b, c) = (self.scores[i - 1], self.scores[i], self.scores[i + 1]);
            let bend = a - 2.0 * b + c;
            if bend < 0.0 {
                i as f64 + f64::from(((a - c) / (2.0 * bend)).clamp(-0.5, 0.5))
            } else {
                i as f64
            }
        } else {
            i as f64
        };
        let bpm = MIN_BPM + refined * STEP_BPM;
        // A tempo moves smoothly; a jump (the source changed song) is taken in one go.
        let next = match self.bpm {
            Some(old) if (bpm - old).abs() < old * 0.06 => old + (bpm - old) * 0.35,
            _ => bpm,
        };
        self.bpm = Some(next);
        Some(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `seconds` of a click at `bpm`, each a short burst of a sine.
    fn clicks(bpm: f64, seconds: f64, sr: f64) -> Vec<f32> {
        let n = (seconds * sr) as usize;
        let period = sr * 60.0 / bpm;
        let mut out = vec![0.0f32; n];
        let mut at = 0.0f64;
        while (at as usize) < n {
            for i in 0..(0.012 * sr) as usize {
                if let Some(s) = out.get_mut(at as usize + i) {
                    let t = i as f32 / sr as f32;
                    *s += (t * 2500.0 * std::f32::consts::TAU).sin() * (1.0 - t / 0.012) * 0.7;
                }
            }
            at += period;
        }
        out
    }

    fn listen(f: &mut TempoFollower, audio: &[f32]) -> Option<f64> {
        let mut last = None;
        for block in audio.chunks(256) {
            last = f.feed(block).or(last);
        }
        last.or(f.tempo())
    }

    #[test]
    fn a_click_gives_its_tempo() {
        for bpm in [90.0, 107.5, 127.0, 140.0, 172.0] {
            let mut f = TempoFollower::new(48_000.0);
            let got = listen(&mut f, &clicks(bpm, 12.0, 48_000.0));
            let got = got.expect("an estimate");
            assert!((got - bpm).abs() < 0.6, "{bpm}: heard {got}");
        }
    }

    #[test]
    fn silence_says_nothing() {
        let mut f = TempoFollower::new(48_000.0);
        let got = listen(&mut f, &vec![0.0; 48_000 * 8]);
        assert_eq!(got, None);
    }

    #[test]
    fn it_follows_when_the_click_changes() {
        let sr = 48_000.0;
        let mut f = TempoFollower::new(sr);
        listen(&mut f, &clicks(120.0, 10.0, sr));
        // A small drift tracks along smoothly.
        let got = listen(&mut f, &clicks(123.0, 20.0, sr)).unwrap();
        assert!((got - 123.0).abs() < 1.0, "heard {got}");
    }

    #[test]
    fn a_mix_of_noise_and_click_still_gives_the_click() {
        let sr = 48_000.0;
        let mut audio = clicks(120.0, 14.0, sr);
        let mut seed = 1u32;
        for s in &mut audio {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            *s += (seed as f32 / u32::MAX as f32 - 0.5) * 0.02;
        }
        let mut f = TempoFollower::new(sr);
        let got = listen(&mut f, &audio).unwrap();
        assert!((got - 120.0).abs() < 1.0, "heard {got}");
    }

    #[test]
    fn nothing_is_allocated_after_building() {
        let mut f = TempoFollower::new(44_100.0);
        let env = f.env.as_ptr();
        listen(&mut f, &clicks(100.0, 12.0, 44_100.0));
        assert_eq!(f.env.as_ptr(), env);
    }
}
