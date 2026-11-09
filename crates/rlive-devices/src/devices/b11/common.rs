//! What the four B11 synths share: band-limited waveforms, note-to-frequency, and scale helpers.

use crate::dsp::util::{lerp, midi_to_hz, sanitize};
use rlive_core::ScaleInfo;
use std::f32::consts::{FRAC_1_SQRT_2, PI, TAU};

/// Samples between control-rate updates of pitch, filters and matrix values.
pub const CONTROL_INTERVAL: usize = 16;

/// The one place a note number becomes a frequency. Tuning systems plug in here.
#[inline]
pub fn note_hz(note: f32) -> f32 {
    midi_to_hz(note)
}

/// The correction a band-limited unit step needs `d` samples from the edge (negative before
/// it): a cubic B-spline integrated once, four samples wide, so nothing needs a lookahead.
#[inline]
pub fn poly_blep(d: f32) -> f32 {
    if d.abs() >= 2.0 {
        return 0.0;
    }
    let t = -d.abs();
    let r = if t < -1.0 {
        (2.0 + t).powi(4) / 24.0
    } else {
        0.5 + t * (2.0 / 3.0) - t * t * t / 3.0 - t * t * t * t / 8.0
    };
    if d < 0.0 {
        r
    } else {
        -r
    }
}

/// The correction a band-limited slope change needs, for a corner `d` samples away.
#[inline]
pub fn poly_blamp(d: f32) -> f32 {
    if d.abs() >= 2.0 {
        return 0.0;
    }
    let t = -d.abs();
    if t < -1.0 {
        (2.0 + t).powi(5) / 120.0
    } else {
        let t2 = t * t;
        28.0 / 120.0 + t / 2.0 + t2 / 3.0 - t2 * t2 / 12.0 - t2 * t2 * t / 40.0
    }
}

/// How far `phase` is from an edge at `edge`, in samples, taking the nearest occurrence.
#[inline]
pub fn edge_distance(phase: f32, edge: f32, dt: f32) -> f32 {
    let mut x = phase - edge;
    x -= x.round();
    x / dt.max(1e-6)
}

/// A rising saw at `phase`, −1 to 1, band-limited.
#[inline]
pub fn saw(phase: f32, dt: f32) -> f32 {
    2.0 * phase - 1.0 - 2.0 * poly_blep(edge_distance(phase, 0.0, dt))
}

/// A pulse high for the first `width` of each cycle, without DC.
#[inline]
pub fn pulse(phase: f32, dt: f32, width: f32) -> f32 {
    let w = width.clamp(0.0, 1.0);
    let naive = if phase < w { 1.0 } else { -1.0 };
    naive + 2.0 * poly_blep(edge_distance(phase, 0.0, dt))
        - 2.0 * poly_blep(edge_distance(phase, w, dt))
        - (2.0 * w - 1.0)
}

/// A triangle rising for the first `rise` of the cycle: 0.5 is symmetric, near 0 or 1 a saw.
#[inline]
pub fn triangle(phase: f32, dt: f32, rise: f32) -> f32 {
    let margin = (2.0 * dt).clamp(0.001, 0.49);
    let w = rise.clamp(margin, 1.0 - margin);
    let naive = if phase < w {
        -1.0 + 2.0 * phase / w
    } else {
        1.0 - 2.0 * (phase - w) / (1.0 - w)
    };
    let corner = 2.0 / w + 2.0 / (1.0 - w);
    naive + dt * corner * poly_blamp(edge_distance(phase, 0.0, dt))
        - dt * corner * poly_blamp(edge_distance(phase, w, dt))
}

/// A sine with `amount` (0..1) of added 2nd and 3rd harmonics, kept below Nyquist.
#[inline]
pub fn rich_sine(phase: f32, dt: f32, amount: f32) -> f32 {
    let t = TAU * phase;
    let mut out = t.sin();
    if amount > 0.0 {
        if dt * 2.0 < 0.45 {
            out += amount * 0.5 * (2.0 * t).sin();
        }
        if dt * 3.0 < 0.45 {
            out += amount * 0.25 * (3.0 * t).sin();
        }
        out /= 1.0 + 0.6 * amount;
    }
    out
}

/// The odd harmonics of a hard-driven sine, added up to just below Nyquist. `bright` 0..1 is how
/// slowly they fall away.
pub fn saturated(phase: f32, dt: f32, bright: f32) -> f32 {
    let t = TAU * phase;
    let falloff = lerp(1.6, 0.8, bright.clamp(0.0, 1.0));
    let (s1, c1) = t.sin_cos();
    let step = 2.0 * (2.0 * c1 * c1 - 1.0);
    let (mut prev, mut cur) = ((-t).sin(), s1);
    let mut sum = 0.0;
    let mut norm = 0.0;
    let mut k = 1.0f32;
    while k * dt < 0.45 && k < 16.0 {
        let a = k.powf(-falloff);
        sum += a * cur;
        norm += a;
        let next = step * cur - prev;
        prev = cur;
        cur = next;
        k += 2.0;
    }
    if norm > 0.0 {
        sum / norm
    } else {
        s1
    }
}

/// Equal-power gains for `pan` in −1..1, with both channels at unity in the centre.
#[inline]
pub fn pan_gains(pan: f32) -> (f32, f32) {
    let angle = (pan.clamp(-1.0, 1.0) + 1.0) * PI * 0.25;
    (angle.cos() / FRAC_1_SQRT_2, angle.sin() / FRAC_1_SQRT_2)
}

/// A one-pole DC blocker for what asymmetric shaping leaves behind.
#[derive(Clone, Copy, Debug, Default)]
pub struct DcBlock {
    x1: f32,
    y1: f32,
}

impl DcBlock {
    #[inline]
    pub fn process(&mut self, x: f32, coeff: f32) -> f32 {
        let y = x - self.x1 + coeff * self.y1;
        self.x1 = x;
        self.y1 = sanitize(y);
        self.y1
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// The coefficient for a [`DcBlock`] at about 5 Hz.
pub fn dc_coeff(sample_rate: f32) -> f32 {
    1.0 - TAU * 5.0 / sample_rate.max(1.0)
}

/// A major scale on `root`, what scale-aware devices use when there's no scale to follow.
pub const fn major_scale(root: u8) -> ScaleInfo {
    ScaleInfo {
        root: root % 12,
        mask: 0b1010_1011_0101,
    }
}

fn degrees_in(scale: ScaleInfo) -> u32 {
    (scale.mask & 0x0fff).count_ones().max(1)
}

/// Is this note in the scale?
#[inline]
pub fn in_scale(note: i32, scale: ScaleInfo) -> bool {
    let pc = (note - scale.root as i32).rem_euclid(12) as u32;
    scale.mask & (1 << pc) != 0
}

/// The note in `scale` nearest to `note`, preferring downwards on a tie.
pub fn nearest_in_scale(note: f32, scale: ScaleInfo) -> f32 {
    if scale.mask & 0x0fff == 0 {
        return note;
    }
    let base = note.round() as i32;
    for offset in 0..12 {
        let down = base - offset;
        let up = base + offset;
        if in_scale(down, scale) {
            return if in_scale(up, scale) && (up as f32 - note).abs() < (down as f32 - note).abs() {
                up as f32
            } else {
                down as f32
            };
        }
        if in_scale(up, scale) {
            return up as f32;
        }
    }
    note
}

/// `note` moved by `degrees` steps along `scale`, from the scale note nearest to it.
pub fn transpose_degrees(note: i32, degrees: i32, scale: ScaleInfo) -> i32 {
    if degrees == 0 {
        return nearest_in_scale(note as f32, scale) as i32;
    }
    let mut at = nearest_in_scale(note as f32, scale) as i32;
    let step = degrees.signum();
    // At most an octave's worth of semitones per scale degree, so this always ends.
    let span = 12 * degrees.unsigned_abs() * 12 / degrees_in(scale);
    let mut left = degrees.abs();
    for _ in 0..=span {
        if left == 0 {
            break;
        }
        at += step;
        if in_scale(at, scale) {
            left -= 1;
        }
    }
    at
}

/// A gentle saturator, odd and with a slope of 1 at the origin.
#[inline]
pub fn soft_clip(x: f32) -> f32 {
    x / (1.0 + x * x).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_blep_step_is_continuous_and_odd() {
        // S(0) = 0.5, so the correction is 0.5 on both sides of the edge, with opposite signs.
        assert!((poly_blep(-1e-4) - 0.5).abs() < 1e-3);
        assert!((poly_blep(1e-4) + 0.5).abs() < 1e-3);
        assert_eq!(poly_blep(2.5), 0.0);
        assert!((poly_blep(-1.0) - 1.0 / 24.0).abs() < 1e-5);
        assert!((poly_blep(1.0) + 1.0 / 24.0).abs() < 1e-5);
    }

    #[test]
    fn the_blamp_is_even_and_meets_zero() {
        assert!((poly_blamp(0.0) - 7.0 / 30.0).abs() < 1e-5);
        assert_eq!(poly_blamp(2.0), 0.0);
        assert!((poly_blamp(-0.7) - poly_blamp(0.7)).abs() < 1e-6);
        assert!((poly_blamp(1.0) - 1.0 / 120.0).abs() < 1e-5);
    }

    #[test]
    fn scale_degrees_walk_the_scale() {
        let c_major = major_scale(0);
        assert_eq!(transpose_degrees(60, 1, c_major), 62);
        assert_eq!(transpose_degrees(60, 2, c_major), 64);
        assert_eq!(transpose_degrees(60, 7, c_major), 72);
        assert_eq!(transpose_degrees(60, -1, c_major), 59);
        assert_eq!(transpose_degrees(61, 0, c_major), 60);
    }

    #[test]
    fn the_nearest_scale_note_is_used() {
        let c_major = major_scale(0);
        assert_eq!(nearest_in_scale(66.0, c_major), 65.0);
        assert_eq!(nearest_in_scale(66.4, c_major), 67.0);
    }

    #[test]
    fn the_pan_law_is_unity_in_the_centre() {
        let (l, r) = pan_gains(0.0);
        assert!((l - 1.0).abs() < 1e-5 && (r - 1.0).abs() < 1e-5);
        let (l, r) = pan_gains(-1.0);
        assert!(l > 1.3 && r.abs() < 1e-5);
    }
}
