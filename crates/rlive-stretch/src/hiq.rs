//! Hi-Q interpolation: reading a sample at its own speed through the windowed-sinc kernels, so
//! matching its rate to the engine's folds far less back down (manual p. 220).

use crate::interp::Sinc;
use rlive_core::warp::{StretchSpan, Stretcher};
use rlive_core::SampleBuffer;

/// Plays a source at its native speed, like the passthrough it stands in for when a clip has
/// Hi-Q on.
pub struct HiQ {
    sample_rate: f64,
    sinc: &'static Sinc,
}

impl HiQ {
    pub fn new() -> Self {
        Self {
            sample_rate: 48_000.0,
            sinc: Sinc::shared(),
        }
    }
}

impl Default for HiQ {
    fn default() -> Self {
        Self::new()
    }
}

impl Stretcher for HiQ {
    fn prepare(&mut self, sample_rate: f64) {
        self.sample_rate = sample_rate;
    }

    fn seek(&mut self, _src: f64) {}

    fn render(&mut self, s: &SampleBuffer, span: StretchSpan<'_>, l: &mut [f32], r: &mut [f32]) {
        let step = s.sample_rate / self.sample_rate;
        let band = Sinc::band_for(step);
        let mut pos = span.src_start;
        for (out_l, out_r) in l.iter_mut().zip(r.iter_mut()) {
            (*out_l, *out_r) = self.sinc.read(s, pos, band);
            pos += step;
        }
    }

    fn latency(&self) -> u32 {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rlive_core::warp::{Passthrough, WarpMode};

    fn tone(hz: f64, rate: f64, frames: usize) -> SampleBuffer {
        let left: Vec<f32> = (0..frames)
            .map(|i| (i as f64 / rate * hz * std::f64::consts::TAU).sin() as f32 * 0.5)
            .collect();
        SampleBuffer {
            name: "t".into(),
            right: left.clone(),
            left,
            sample_rate: rate,
        }
    }

    fn render(stretcher: &mut dyn Stretcher, s: &SampleBuffer, frames: usize) -> Vec<f32> {
        let (mut l, mut r) = (vec![0.0; frames], vec![0.0; frames]);
        let span = StretchSpan {
            src_start: 64.0,
            src_end: 0.0,
            transpose: 0.0,
            mode: WarpMode::Complex,
            slices: &[],
        };
        stretcher.render(s, span, &mut l, &mut r);
        l
    }

    /// The level at `hz` of `x`, sampled at `rate`, by one bin of a Hann-windowed DFT.
    fn level(x: &[f32], hz: f64, rate: f64) -> f64 {
        let (mut re, mut im, mut sum) = (0.0, 0.0, 0.0);
        for (i, s) in x.iter().enumerate() {
            let w = 0.5 - 0.5 * (i as f64 / x.len() as f64 * std::f64::consts::TAU).cos();
            let t = i as f64 / rate * hz * std::f64::consts::TAU;
            re += *s as f64 * w * t.cos();
            im += *s as f64 * w * t.sin();
            sum += w;
        }
        (re * re + im * im).sqrt() / sum * 2.0
    }

    #[test]
    fn a_source_above_the_engine_rate_folds_far_less_than_with_straight_lines() {
        // 44 kHz in a 96 kHz file is past the engine's Nyquist: it can only come back as 4 kHz.
        let s = tone(44_000.0, 96_000.0, 96_000);
        let mut linear = Passthrough::new(48_000.0);
        let mut hiq = HiQ::new();
        hiq.prepare(48_000.0);
        let plain = level(&render(&mut linear, &s, 16_384), 4_000.0, 48_000.0);
        let better = level(&render(&mut hiq, &s, 16_384), 4_000.0, 48_000.0);
        assert!(better < plain * 0.03, "{better} against {plain}");
    }

    #[test]
    fn it_keeps_what_is_in_band_when_resampling() {
        let s = tone(1_000.0, 44_100.0, 44_100);
        let mut hiq = HiQ::new();
        hiq.prepare(48_000.0);
        let out = level(&render(&mut hiq, &s, 16_384), 1_000.0, 48_000.0);
        assert!((out - 0.5).abs() < 0.01, "{out}");
    }
}
