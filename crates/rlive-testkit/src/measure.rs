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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::{dc, impulse, sine};

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
