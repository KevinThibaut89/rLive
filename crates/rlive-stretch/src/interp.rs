//! Windowed-sinc reading of a sample at a fractional position.

use rlive_core::SampleBuffer;
use std::sync::OnceLock;

const TAPS: usize = 16;
const HALF: i64 = TAPS as i64 / 2;
const PHASES: usize = 256;
const KAISER_BETA: f64 = 9.0;

/// The fastest read rate each table is for. The first passes everything; the others low-pass so
/// reading faster than the source can't fold high frequencies back down.
const BAND_RATES: [f64; 7] = [1.0, 1.15, 1.35, 1.6, 2.0, 2.8, 4.0];

/// A kernel table for each band: `PHASES + 1` rows of `TAPS` coefficients each.
pub(crate) struct Sinc {
    bands: Vec<Vec<f32>>,
}

/// The modified Bessel function I0 by its series.
fn bessel_i0(x: f64) -> f64 {
    let quarter_square = x * x / 4.0;
    let mut term = 1.0;
    let mut sum = 1.0;
    for k in 1..64 {
        term *= quarter_square / (k * k) as f64;
        sum += term;
        if term < 1e-14 * sum {
            break;
        }
    }
    sum
}

fn build_band(rate: f64) -> Vec<f32> {
    let cutoff = (1.0 / rate).min(1.0);
    let denom = bessel_i0(KAISER_BETA);
    let mut table = Vec::with_capacity((PHASES + 1) * TAPS);
    for phase in 0..=PHASES {
        let frac = phase as f64 / PHASES as f64;
        let row_start = table.len();
        for tap in 0..TAPS {
            let offset = tap as f64 - (HALF - 1) as f64 - frac;
            let x = offset * cutoff;
            let sinc = if x.abs() < 1e-12 {
                1.0
            } else {
                (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x)
            };
            let ratio = (offset / HALF as f64).clamp(-1.0, 1.0);
            let window = bessel_i0(KAISER_BETA * (1.0 - ratio * ratio).sqrt()) / denom;
            table.push(sinc * window);
        }
        // Every row sums to one, so DC passes through every fraction unchanged.
        let sum: f64 = table[row_start..].iter().sum();
        for c in &mut table[row_start..] {
            *c /= sum;
        }
    }
    table.into_iter().map(|c| c as f32).collect()
}

impl Sinc {
    /// The kernels, built the first time anything asks; a stretcher asks when it is created.
    pub(crate) fn shared() -> &'static Sinc {
        static SINC: OnceLock<Sinc> = OnceLock::new();
        SINC.get_or_init(|| Sinc {
            bands: BAND_RATES.iter().map(|r| build_band(*r)).collect(),
        })
    }

    /// The band to read at `rate` source frames per output frame.
    pub(crate) fn band_for(rate: f64) -> usize {
        let rate = rate.abs();
        BAND_RATES
            .iter()
            .position(|b| rate <= *b * 1.000_001)
            .unwrap_or(BAND_RATES.len() - 1)
    }

    /// The stereo value of `s` at `pos`. Everything outside the sample reads as silence.
    #[inline]
    pub(crate) fn read(&self, s: &SampleBuffer, pos: f64, band: usize) -> (f32, f32) {
        let n = s.len() as i64;
        if pos <= -(HALF as f64) || pos >= n as f64 + HALF as f64 || n == 0 {
            return (0.0, 0.0);
        }
        let whole = pos.floor();
        let index = whole as i64;
        let phase = ((pos - whole) * PHASES as f64) as f32;
        let row = (phase as usize).min(PHASES - 1);
        let blend = phase - row as f32;
        let table = &self.bands[band];
        let near = &table[row * TAPS..(row + 1) * TAPS];
        let far = &table[(row + 1) * TAPS..(row + 2) * TAPS];
        let (mut l, mut r) = (0.0, 0.0);
        let first = index - (HALF - 1);
        if first >= 0 && first + (TAPS as i64) <= n {
            let left = &s.left[first as usize..first as usize + TAPS];
            let right = &s.right[first as usize..first as usize + TAPS];
            for i in 0..TAPS {
                let c = near[i] + (far[i] - near[i]) * blend;
                l += left[i] * c;
                r += right[i] * c;
            }
        } else {
            for (i, (a, b)) in near.iter().zip(far).enumerate() {
                let at = first + i as i64;
                if (0..n).contains(&at) {
                    let c = a + (b - a) * blend;
                    l += s.left[at as usize] * c;
                    r += s.right[at as usize] * c;
                }
            }
        }
        (l, r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(hz: f64, rate: f64, frames: usize) -> SampleBuffer {
        let left: Vec<f32> = (0..frames)
            .map(|i| (i as f64 / rate * hz * std::f64::consts::TAU).sin() as f32)
            .collect();
        SampleBuffer {
            name: "t".into(),
            right: left.clone(),
            left,
            sample_rate: rate,
        }
    }

    #[test]
    fn whole_positions_read_the_samples_exactly() {
        let s = tone(997.0, 48_000.0, 200);
        let sinc = Sinc::shared();
        for i in 20..180 {
            let (l, _) = sinc.read(&s, i as f64, 0);
            assert!((l - s.left[i]).abs() < 1e-5, "{i}");
        }
    }

    #[test]
    fn fractional_positions_stay_on_the_waveform() {
        let s = tone(1000.0, 48_000.0, 400);
        let sinc = Sinc::shared();
        for i in 100..300 {
            let pos = i as f64 + 0.37;
            let want = (pos / 48_000.0 * 1000.0 * std::f64::consts::TAU).sin() as f32;
            assert!((sinc.read(&s, pos, 0).0 - want).abs() < 2e-3, "{i}");
        }
    }

    #[test]
    fn faster_reads_use_narrower_bands() {
        assert_eq!(Sinc::band_for(0.5), 0);
        assert_eq!(Sinc::band_for(1.0), 0);
        assert_eq!(Sinc::band_for(1.1), 1);
        assert_eq!(Sinc::band_for(2.0), 4);
        assert_eq!(Sinc::band_for(9.0), BAND_RATES.len() - 1);
    }

    #[test]
    fn reading_outside_the_sample_is_silent() {
        let s = tone(1000.0, 48_000.0, 64);
        let sinc = Sinc::shared();
        assert_eq!(sinc.read(&s, -100.0, 0), (0.0, 0.0));
        assert_eq!(sinc.read(&s, 500.0, 0), (0.0, 0.0));
    }
}
