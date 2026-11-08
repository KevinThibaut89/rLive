//! Monophonic pitch detection (the YIN difference function, computed with an FFT).

use super::fft::{Complex32, Fft};
use super::util::sanitize;

/// One pitch estimate.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Pitch {
    pub hz: f32,
    /// How sure the detector is, 0..1. Above about 0.8 is a clear pitch.
    pub confidence: f32,
}

/// Estimates a single pitch from a stream, a new estimate every `hop` samples.
#[derive(Clone, Debug)]
pub struct PitchDetector {
    sample_rate: f32,
    window: usize,
    tau_min: usize,
    tau_max: usize,
    hop: usize,
    threshold: f32,
    ring: Vec<f32>,
    write: usize,
    since_frame: usize,
    fft: Fft,
    a: Vec<f32>,
    b: Vec<f32>,
    fa: Vec<Complex32>,
    fb: Vec<Complex32>,
    corr: Vec<f32>,
    prefix: Vec<f64>,
    cmnd: Vec<f32>,
    latest: Option<Pitch>,
}

impl PitchDetector {
    /// Detects `min_hz..max_hz`, checking again every `hop` samples.
    pub fn new(sample_rate: f64, min_hz: f32, max_hz: f32, hop: usize) -> Self {
        let sr = sample_rate.max(1.0) as f32;
        let tau_max = ((sr / min_hz.max(1.0)).ceil() as usize).max(8);
        let tau_min = ((sr / max_hz.max(1.0)).floor() as usize).clamp(2, tau_max - 2);
        let window = tau_max;
        let buffer = window + tau_max + 2;
        let fft_size = (window + buffer).next_power_of_two();
        let fft = Fft::new(fft_size);
        let bins = fft.bins();
        Self {
            sample_rate: sr,
            window,
            tau_min,
            tau_max,
            hop: hop.max(1),
            threshold: 0.12,
            ring: vec![0.0; buffer],
            write: 0,
            since_frame: 0,
            a: vec![0.0; fft_size],
            b: vec![0.0; fft_size],
            fa: vec![Complex32::new(0.0, 0.0); bins],
            fb: vec![Complex32::new(0.0, 0.0); bins],
            corr: vec![0.0; fft_size],
            prefix: vec![0.0; buffer + 1],
            cmnd: vec![0.0; tau_max + 2],
            fft,
            latest: None,
        }
    }

    /// Samples of history an estimate is based on, and so its latency at worst.
    pub fn history_len(&self) -> usize {
        self.ring.len()
    }

    pub fn reset(&mut self) {
        self.ring.fill(0.0);
        self.write = 0;
        self.since_frame = 0;
        self.latest = None;
    }

    /// The most recent estimate; `None` for silence or noise.
    pub fn latest(&self) -> Option<Pitch> {
        self.latest
    }

    /// Takes in a block and returns the most recent estimate.
    pub fn process(&mut self, block: &[f32]) -> Option<Pitch> {
        for &x in block {
            self.ring[self.write] = sanitize(x);
            self.write = (self.write + 1) % self.ring.len();
            self.since_frame += 1;
            if self.since_frame >= self.hop {
                self.since_frame = 0;
                self.latest = self.estimate();
            }
        }
        self.latest
    }

    fn estimate(&mut self) -> Option<Pitch> {
        let total = self.ring.len();
        let window = self.window;
        let tau_max = self.tau_max;
        let len = window + tau_max + 2;
        // Oldest sample first.
        for i in 0..total {
            self.b[i] = self.ring[(self.write + i) % total];
        }
        self.b[len..].fill(0.0);
        self.prefix[0] = 0.0;
        for i in 0..total {
            let x = self.b[i] as f64;
            self.prefix[i + 1] = self.prefix[i] + x * x;
        }
        self.a[..window].copy_from_slice(&self.b[..window]);
        self.a[window..].fill(0.0);
        if self.prefix[total] < 1e-8 * total as f64 {
            return None;
        }

        self.fft.forward(&mut self.a, &mut self.fa);
        self.fft.forward(&mut self.b, &mut self.fb);
        for (a, b) in self.fa.iter_mut().zip(&self.fb) {
            *a = a.conj() * b;
        }
        self.fft.inverse(&mut self.fa, &mut self.corr);

        // r[tau] is now the sum of x[j] x[j + tau] over the window.
        let e0 = self.prefix[window];
        let mut running = 0.0f64;
        self.cmnd[0] = 1.0;
        for tau in 1..=tau_max {
            let e_tau = self.prefix[tau + window] - self.prefix[tau];
            let d = (e0 + e_tau - 2.0 * self.corr[tau] as f64).max(0.0);
            running += d;
            self.cmnd[tau] = if running > 0.0 {
                (d * tau as f64 / running) as f32
            } else {
                1.0
            };
        }

        // The first dip under the threshold, walked down to its bottom.
        let mut best = None;
        let mut tau = self.tau_min;
        while tau < tau_max {
            if self.cmnd[tau] < self.threshold {
                while tau + 1 < tau_max && self.cmnd[tau + 1] < self.cmnd[tau] {
                    tau += 1;
                }
                best = Some(tau);
                break;
            }
            tau += 1;
        }
        // Nothing clear: take the deepest dip, if there is a dip at all.
        let tau = match best {
            Some(tau) => tau,
            None => {
                let mut lowest = self.tau_min;
                for t in self.tau_min..tau_max {
                    if self.cmnd[t] < self.cmnd[lowest] {
                        lowest = t;
                    }
                }
                lowest
            }
        };
        if tau <= 1 || tau >= tau_max {
            return None;
        }
        let d = self.cmnd[tau];
        if d > 0.5 {
            return None;
        }
        let (l, c, r) = (self.cmnd[tau - 1], self.cmnd[tau], self.cmnd[tau + 1]);
        let denom = l - 2.0 * c + r;
        let shift = if denom.abs() > 1e-12 {
            (0.5 * (l - r) / denom).clamp(-1.0, 1.0)
        } else {
            0.0
        };
        Some(Pitch {
            hz: self.sample_rate / (tau as f32 + shift),
            confidence: (1.0 - d).clamp(0.0, 1.0),
        })
    }
}
