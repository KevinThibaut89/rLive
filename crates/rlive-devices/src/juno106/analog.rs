//! Small analog building blocks shared by the Juno-106 model.

use std::f64::consts::PI;

/// xorshift32. Deterministic so that a given unit serial always yields the same machine.
#[derive(Clone, Copy)]
pub struct Rng(u32);

impl Rng {
    pub fn new(seed: u32) -> Self {
        // Scramble so that adjacent seeds give unrelated streams, and never zero.
        let mut z = seed.wrapping_mul(0x9E37_79B9).wrapping_add(0x7F4A_7C15);
        z = (z ^ (z >> 15)).wrapping_mul(0x85EB_CA6B);
        z = (z ^ (z >> 13)).wrapping_mul(0xC2B2_AE35);
        Self((z ^ (z >> 16)) | 1)
    }

    #[inline]
    pub fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    /// Uniform in `-1..1`.
    #[inline]
    pub fn uniform(&mut self) -> f32 {
        (self.next_u32() as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    /// Approximately unit-variance Gaussian (sum of uniforms), bounded to about ±3.5σ.
    pub fn gauss(&mut self) -> f32 {
        let s = self.uniform() + self.uniform() + self.uniform() + self.uniform();
        s * 0.866
    }
}

/// `tanh(x) / x`, by continued fraction. Used to linearise OTA stages around their operating point.
#[inline]
pub fn tanh_ratio(x: f32) -> f32 {
    let q = x * x;
    1.0 / (1.0 + q / (3.0 + q / (5.0 + q / 7.0)))
}

/// One-pole filter in topology-preserving form. `f64` state: several of the coupling capacitors
/// sit below 1 Hz, where `f32` runs out of precision at the oversampled rate.
#[derive(Clone, Copy, Default)]
pub struct OnePole {
    g: f64,
    s: f64,
}

impl OnePole {
    pub fn new(hz: f64, sr: f64) -> Self {
        let mut p = Self::default();
        p.set(hz, sr);
        p
    }

    pub fn set(&mut self, hz: f64, sr: f64) {
        let t = (PI * (hz / sr).min(0.49)).tan();
        self.g = t / (1.0 + t);
    }

    /// Start in steady state for a constant input, as a capacitor long since charged would be.
    pub fn precharge(&mut self, level: f32) {
        self.s = level as f64;
    }

    #[inline]
    pub fn lowpass(&mut self, x: f32) -> f32 {
        let v = (x as f64 - self.s) * self.g;
        let y = v + self.s;
        self.s = y + v;
        y as f32
    }

    #[inline]
    pub fn highpass(&mut self, x: f32) -> f32 {
        x - self.lowpass(x)
    }
}

/// Transposed direct-form II biquad, designed from an analog prototype.
#[derive(Clone, Copy, Default)]
pub struct Biquad {
    b: [f32; 3],
    a: [f32; 2],
    z: [f32; 2],
}

impl Biquad {
    /// Bilinear transform of `(b0 + b1·s + b2·s²) / (a0 + a1·s + a2·s²)`, `s` in rad/s.
    pub fn from_analog(b: [f64; 3], a: [f64; 3], sr: f64) -> Self {
        let k = 2.0 * sr;
        let k2 = k * k;
        let norm = a[0] + a[1] * k + a[2] * k2;
        Self {
            b: [
                ((b[0] + b[1] * k + b[2] * k2) / norm) as f32,
                ((2.0 * b[0] - 2.0 * b[2] * k2) / norm) as f32,
                ((b[0] - b[1] * k + b[2] * k2) / norm) as f32,
            ],
            a: [
                ((2.0 * a[0] - 2.0 * a[2] * k2) / norm) as f32,
                ((a[0] - a[1] * k + a[2] * k2) / norm) as f32,
            ],
            z: [0.0; 2],
        }
    }

    /// Unity-gain Sallen-Key section with resistors `r` and capacitors `c1` (feedback), `c2`
    /// (to ground). `tol` scales the corner to model component tolerance.
    pub fn sallen_key(r: f64, c1: f64, c2: f64, tol: f64, sr: f64) -> Self {
        let w = tol / (r * (c1 * c2).sqrt());
        let q = 0.5 * (c1 / c2).sqrt();
        // Pre-warp so the corner stays put at the running sample rate.
        let w = 2.0 * sr * (w / (2.0 * sr)).tan();
        Self::from_analog([w * w, 0.0, 0.0], [w * w, w / q, 1.0], sr)
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// Windowed-sinc decimator from the internal rate down to the host rate.
pub struct Decimator {
    coefs: Vec<f32>,
    buf: Vec<f32>,
    pos: usize,
    last: f32,
}

impl Decimator {
    pub fn new(factor: usize) -> Self {
        if factor <= 1 {
            return Self {
                coefs: Vec::new(),
                buf: Vec::new(),
                pos: 0,
                last: 0.0,
            };
        }
        let taps = 48 * factor;
        let beta = 9.0;
        let centre = (taps - 1) as f64 / 2.0;
        let cutoff = 0.49 / factor as f64;
        let mut coefs: Vec<f64> = (0..taps)
            .map(|i| {
                let n = i as f64 - centre;
                let sinc = if n.abs() < 1e-9 {
                    2.0 * cutoff
                } else {
                    (2.0 * PI * cutoff * n).sin() / (PI * n)
                };
                let w = (1.0 - (n / centre).powi(2)).max(0.0).sqrt();
                sinc * bessel_i0(beta * w) / bessel_i0(beta)
            })
            .collect();
        let sum: f64 = coefs.iter().sum();
        coefs.iter_mut().for_each(|c| *c /= sum);
        Self {
            coefs: coefs.into_iter().map(|c| c as f32).collect(),
            buf: vec![0.0; taps * 2],
            pos: 0,
            last: 0.0,
        }
    }

    #[inline]
    pub fn push(&mut self, x: f32) {
        if self.coefs.is_empty() {
            self.last = x;
            return;
        }
        // Written twice so `read` always sees one contiguous window.
        let n = self.coefs.len();
        self.buf[self.pos] = x;
        self.buf[self.pos + n] = x;
        self.pos = (self.pos + 1) % n;
    }

    pub fn read(&self) -> f32 {
        if self.coefs.is_empty() {
            return self.last;
        }
        let window = &self.buf[self.pos..self.pos + self.coefs.len()];
        window.iter().zip(&self.coefs).map(|(x, c)| x * c).sum()
    }
}

fn bessel_i0(x: f64) -> f64 {
    let q = x * x / 4.0;
    let mut term = 1.0;
    let mut sum = 1.0;
    for k in 1..40 {
        term *= q / (k * k) as f64;
        sum += term;
    }
    sum
}
