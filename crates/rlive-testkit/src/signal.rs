//! Simple test signals.

pub fn silence(frames: usize) -> Vec<f32> {
    vec![0.0; frames]
}

pub fn dc(value: f32, frames: usize) -> Vec<f32> {
    vec![value; frames]
}

/// One sample of `amplitude` at frame `at`, otherwise silence.
pub fn impulse(frames: usize, at: usize, amplitude: f32) -> Vec<f32> {
    let mut buf = vec![0.0; frames];
    buf[at] = amplitude;
    buf
}

/// A full-scale sine at `hz`, starting at phase zero.
pub fn sine(hz: f32, sample_rate: f64, frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|i| (i as f32 / sample_rate as f32 * hz * std::f32::consts::TAU).sin())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signals_have_the_expected_shape() {
        assert_eq!(silence(3), [0.0, 0.0, 0.0]);
        assert_eq!(dc(0.5, 2), [0.5, 0.5]);
        assert_eq!(impulse(3, 1, 0.8), [0.0, 0.8, 0.0]);
        let s = sine(12_000.0, 48_000.0, 4);
        assert!(s[0].abs() < 1e-6 && (s[1] - 1.0).abs() < 1e-3 && (s[3] + 1.0).abs() < 1e-3);
    }
}
