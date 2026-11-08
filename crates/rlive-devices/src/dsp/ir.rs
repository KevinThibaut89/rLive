//! Impulse responses, generated from code. Nothing here is sampled from any existing space or
//! product: same spec and seed, same response.

use super::util::{sanitize, Rng};
use std::f32::consts::TAU;

/// The character of a generated impulse response.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum IrKind {
    #[default]
    Room,
    Hall,
    Plate,
    /// A bouncing, dispersive tank.
    Spring,
    /// A short speaker-and-cabinet response, for amp sounds.
    Cabinet,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct IrSpec {
    pub kind: IrKind,
    /// Seconds for the tail to fall 60 dB. `Cabinet` responses ignore it.
    pub decay: f32,
    /// How much faster the highs die away, 0..1.
    pub damping: f32,
    /// A gap before anything is heard, in seconds.
    pub predelay: f32,
    /// A different seed is a different response of the same character.
    pub seed: u32,
}

impl Default for IrSpec {
    fn default() -> Self {
        Self {
            kind: IrKind::Room,
            decay: 1.0,
            damping: 0.5,
            predelay: 0.0,
            seed: 1,
        }
    }
}

/// Stereo samples of a generated response, normalised to a peak of 1.
#[derive(Clone, Debug, Default)]
pub struct StereoIr {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

impl StereoIr {
    pub fn len(&self) -> usize {
        self.left.len().min(self.right.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Runs a one-pole low-pass whose cutoff falls over time, so highs decay faster than lows.
fn damp(signal: &mut [f32], sr: f32, start_hz: f32, end_hz: f32) {
    let n = signal.len().max(1) as f32;
    let ratio = (end_hz / start_hz).max(1e-3);
    let mut y = 0.0f32;
    for (i, s) in signal.iter_mut().enumerate() {
        let hz = start_hz * ratio.powf(i as f32 / n);
        let a = 1.0 - (-TAU * hz / sr).exp();
        y += (*s - y) * a;
        *s = y;
    }
}

fn normalize(ir: &mut StereoIr) {
    let peak = ir
        .left
        .iter()
        .chain(&ir.right)
        .fold(0.0f32, |m, s| m.max(s.abs()));
    if peak > 0.0 {
        for s in ir.left.iter_mut().chain(ir.right.iter_mut()) {
            *s = sanitize(*s / peak);
        }
    }
}

fn noise_tail(rng: &mut Rng, frames: usize, sr: f32, decay: f32) -> Vec<f32> {
    let per_sample = (-6.907_755 / (decay.max(0.01) * sr)).exp();
    let mut envelope = 1.0f32;
    (0..frames)
        .map(|_| {
            let s = rng.bipolar() * envelope;
            envelope *= per_sample;
            s
        })
        .collect()
}

/// Sparse first reflections of a box-shaped room, per ear.
fn add_reflections(left: &mut [f32], right: &mut [f32], rng: &mut Rng, sr: f32, room: f32) {
    const SPEED_OF_SOUND: f32 = 343.0;
    let frames = left.len();
    if frames < 8 {
        return;
    }
    for order in 1..=4 {
        for wall in 0..4 {
            let along = room * (order as f32 + 0.3 * rng.bipolar() + wall as f32 * 0.17);
            let across = room * (0.4 + 0.6 * rng.unipolar());
            let ear = 0.09 * if wall % 2 == 0 { 1.0 } else { -1.0 };
            let gain = 0.6f32.powi(order) * if wall % 2 == 0 { 1.0 } else { -0.9 };
            let place = |buf: &mut [f32], distance: f32| {
                let at = ((distance / SPEED_OF_SOUND) * sr) as usize;
                if at < frames {
                    buf[at] += gain / (1.0 + distance * 0.2);
                }
            };
            place(left, (along * along + (across - ear).powi(2)).sqrt());
            place(right, (along * along + (across + ear).powi(2)).sqrt());
        }
    }
}

fn hall_or_room(spec: &IrSpec, sr: f32, room: f32, onset: f32) -> StereoIr {
    let frames = ((spec.decay.max(0.05) * 1.4 + spec.predelay.max(0.0)) * sr) as usize + 1;
    let mut rng = Rng::new(spec.seed);
    let mut left = noise_tail(&mut rng, frames, sr, spec.decay);
    let mut right = noise_tail(&mut rng, frames, sr, spec.decay);
    // The tail takes a moment to build.
    let build = (onset * sr).max(1.0);
    for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
        let t = (i as f32 / build).min(1.0);
        let g = t * t;
        *l *= g * 0.5;
        *r *= g * 0.5;
    }
    let start = 18_000.0 - spec.damping.clamp(0.0, 1.0) * 14_000.0;
    let end = start * (1.0 - 0.9 * spec.damping.clamp(0.0, 1.0)).max(0.05);
    damp(&mut left, sr, start, end);
    damp(&mut right, sr, start, end);
    add_reflections(&mut left, &mut right, &mut rng, sr, room);
    finish(left, right, spec.predelay, sr)
}

fn finish(mut left: Vec<f32>, mut right: Vec<f32>, predelay: f32, sr: f32) -> StereoIr {
    let gap = (predelay.max(0.0) * sr) as usize;
    if gap > 0 {
        left.splice(0..0, std::iter::repeat_n(0.0, gap));
        right.splice(0..0, std::iter::repeat_n(0.0, gap));
    }
    let mut ir = StereoIr { left, right };
    normalize(&mut ir);
    ir
}

fn plate(spec: &IrSpec, sr: f32) -> StereoIr {
    let frames = ((spec.decay.max(0.05) * 1.3 + spec.predelay.max(0.0)) * sr) as usize + 1;
    let mut rng = Rng::new(spec.seed);
    let mut left = noise_tail(&mut rng, frames, sr, spec.decay);
    let mut right = noise_tail(&mut rng, frames, sr, spec.decay);
    // Plates are dense and bright straight away, and thin at the bottom.
    let mut lowcut = [0.0f32; 2];
    for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
        let t = (i as f32 / (0.002 * sr)).min(1.0);
        lowcut[0] += (*l - lowcut[0]) * 0.02;
        lowcut[1] += (*r - lowcut[1]) * 0.02;
        *l = (*l - lowcut[0]) * t;
        *r = (*r - lowcut[1]) * t;
    }
    let d = spec.damping.clamp(0.0, 1.0);
    damp(
        &mut left,
        sr,
        19_000.0 - 7_000.0 * d,
        16_000.0 - 12_000.0 * d,
    );
    damp(
        &mut right,
        sr,
        19_000.0 - 7_000.0 * d,
        16_000.0 - 12_000.0 * d,
    );
    finish(left, right, spec.predelay, sr)
}

/// Echoes of a swept chirp, each one longer and duller than the last.
fn spring(spec: &IrSpec, sr: f32) -> StereoIr {
    let frames = ((spec.decay.max(0.05) * 1.2 + spec.predelay.max(0.0)) * sr) as usize + 1;
    let mut rng = Rng::new(spec.seed);
    let mut left = vec![0.0f32; frames];
    let mut right = vec![0.0f32; frames];
    let per_sample = (-6.907_755 / (spec.decay.max(0.05) * sr)).exp();
    let echo_gap = 0.035 + 0.01 * rng.unipolar();
    let mut echo = 0usize;
    loop {
        let start = (echo as f32 * echo_gap * sr) as usize;
        if start >= frames {
            break;
        }
        let spread = 0.015 + 0.012 * echo as f32;
        let span = (spread * sr) as usize;
        let (detune_l, detune_r) = (1.0 + 0.05 * rng.bipolar(), 1.0 + 0.05 * rng.bipolar());
        for n in 0..span.min(frames - start) {
            let t = n as f32 / span as f32;
            let at = start + n;
            let env = (std::f32::consts::PI * t).sin();
            let decay = per_sample.powi(at as i32);
            // A falling sweep: springs delay high frequencies less.
            let phase = TAU * spread * (4_500.0 * t - 2_000.0 * t * t);
            left[at] += (phase * detune_l).sin() * env * decay;
            right[at] += (phase * detune_r).sin() * env * decay;
        }
        echo += 1;
    }
    let d = spec.damping.clamp(0.0, 1.0);
    damp(&mut left, sr, 9_000.0 - 3_000.0 * d, 7_000.0 - 5_000.0 * d);
    damp(&mut right, sr, 9_000.0 - 3_000.0 * d, 7_000.0 - 5_000.0 * d);
    finish(left, right, spec.predelay, sr)
}

/// A few resonances with a falling high end, like a closed box with a cone in it.
fn cabinet(spec: &IrSpec, sr: f32) -> StereoIr {
    let frames = ((0.04 + spec.predelay.max(0.0)) * sr) as usize + 1;
    let mut rng = Rng::new(spec.seed);
    let modes: [(f32, f32, f32); 6] = [
        (110.0, 0.006, 0.7),
        (420.0 * (1.0 + 0.1 * rng.bipolar()), 0.006, 0.5),
        (1_400.0 * (1.0 + 0.1 * rng.bipolar()), 0.003, 0.8),
        (2_800.0 * (1.0 + 0.1 * rng.bipolar()), 0.002, 0.6),
        (4_200.0 * (1.0 + 0.1 * rng.bipolar()), 0.001, 0.3),
        (6_200.0 * (1.0 + 0.1 * rng.bipolar()), 0.0005, 0.1),
    ];
    let mut left = vec![0.0f32; frames];
    for (hz, tau, gain) in modes {
        let per_sample = (-1.0 / (tau * sr)).exp();
        let mut env = gain;
        for (i, s) in left.iter_mut().enumerate() {
            *s += env * (TAU * hz * i as f32 / sr).sin();
            env *= per_sample;
        }
    }
    let d = spec.damping.clamp(0.0, 1.0);
    damp(&mut left, sr, 8_000.0 - 4_000.0 * d, 6_000.0 - 3_000.0 * d);
    let right = left.clone();
    finish(left, right, spec.predelay, sr)
}

/// Generates a response. Allocates; do this away from the audio thread.
pub fn generate_ir(spec: &IrSpec, sample_rate: f64) -> StereoIr {
    let sr = sample_rate.max(1.0) as f32;
    match spec.kind {
        IrKind::Room => hall_or_room(spec, sr, 4.0, 0.006),
        IrKind::Hall => hall_or_room(spec, sr, 12.0, 0.035),
        IrKind::Plate => plate(spec, sr),
        IrKind::Spring => spring(spec, sr),
        IrKind::Cabinet => cabinet(spec, sr),
    }
}
