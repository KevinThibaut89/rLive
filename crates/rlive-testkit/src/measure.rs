//! Measurements of rendered audio.

/// Root-mean-square level; zero for an empty buffer.
pub fn rms(buf: &[f32]) -> f32 {
    if buf.is_empty() {
        return 0.0;
    }
    (buf.iter().map(|s| s * s).sum::<f32>() / buf.len() as f32).sqrt()
}

/// Largest absolute sample value; zero for an empty buffer.
pub fn peak(buf: &[f32]) -> f32 {
    buf.iter().fold(0.0, |max: f32, s| max.max(s.abs()))
}

/// Index of the first sample whose magnitude exceeds `threshold`.
pub fn onset(buf: &[f32], threshold: f32) -> Option<usize> {
    buf.iter().position(|s| s.abs() > threshold)
}

/// The peak amplitude of the `hz` component of `buf`, by a Goertzel filter. Use a whole number
/// of cycles, or a long buffer, for accuracy.
pub fn tone_amplitude(buf: &[f32], hz: f64, sample_rate: f64) -> f64 {
    if buf.is_empty() {
        return 0.0;
    }
    let w = std::f64::consts::TAU * hz / sample_rate;
    let coeff = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for &x in buf {
        let s0 = x as f64 + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    let power = s1 * s1 + s2 * s2 - coeff * s1 * s2;
    2.0 * power.max(0.0).sqrt() / buf.len() as f64
}

/// The largest difference between neighbouring samples.
pub fn max_step(buf: &[f32]) -> f32 {
    buf.windows(2)
        .fold(0.0, |max: f32, w| max.max((w[1] - w[0]).abs()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::{dc, impulse, sine};

    #[test]
    fn tone_amplitude_reads_a_sine() {
        let s = sine(1_000.0, 48_000.0, 48_000);
        assert!((tone_amplitude(&s, 1_000.0, 48_000.0) - 1.0).abs() < 1e-3);
        assert!(tone_amplitude(&s, 3_000.0, 48_000.0) < 1e-3);
    }

    #[test]
    fn max_step_finds_the_biggest_jump() {
        assert_eq!(max_step(&[0.0, 0.5, 0.25]), 0.5);
        assert_eq!(max_step(&[]), 0.0);
    }

    #[test]
    fn rms_of_dc_and_sine() {
        assert_eq!(rms(&[]), 0.0);
        assert!((rms(&dc(0.5, 64)) - 0.5).abs() < 1e-6);
        let s = sine(1_000.0, 48_000.0, 48_000);
        assert!((rms(&s) - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-3);
    }

    #[test]
    fn peak_uses_magnitude() {
        assert_eq!(peak(&[]), 0.0);
        assert_eq!(peak(&[0.1, -0.7, 0.3]), 0.7);
    }

    #[test]
    fn onset_finds_the_first_sound() {
        assert_eq!(onset(&impulse(10, 4, -0.5), 0.1), Some(4));
        assert_eq!(onset(&dc(0.05, 10), 0.1), None);
    }
}
