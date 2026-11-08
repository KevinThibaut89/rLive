//! Real FFT with preallocated scratch, so transforms are safe on the audio thread.

use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

pub use realfft::num_complex::Complex32;

/// A forward and inverse real FFT of one size. Planning allocates; transforming does not.
#[derive(Clone)]
pub struct Fft {
    size: usize,
    forward: Arc<dyn RealToComplex<f32>>,
    inverse: Arc<dyn ComplexToReal<f32>>,
    forward_scratch: Vec<Complex32>,
    inverse_scratch: Vec<Complex32>,
}

impl std::fmt::Debug for Fft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fft").field("size", &self.size).finish()
    }
}

impl Fft {
    pub fn new(size: usize) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let forward = planner.plan_fft_forward(size);
        let inverse = planner.plan_fft_inverse(size);
        Self {
            size,
            forward_scratch: forward.make_scratch_vec(),
            inverse_scratch: inverse.make_scratch_vec(),
            forward,
            inverse,
        }
    }

    pub fn size(&self) -> usize {
        self.size
    }

    /// How many complex bins a spectrum has: `size / 2 + 1`.
    pub fn bins(&self) -> usize {
        self.size / 2 + 1
    }

    /// `time` (`size` samples) to `spectrum` (`bins` values). `time` is left unspecified.
    #[inline]
    pub fn forward(&mut self, time: &mut [f32], spectrum: &mut [Complex32]) {
        let done = self
            .forward
            .process_with_scratch(time, spectrum, &mut self.forward_scratch);
        debug_assert!(done.is_ok());
    }

    /// `spectrum` (`bins` values) to `time` (`size` samples), scaled so a forward and inverse
    /// round trip returns the original. `spectrum` is left unspecified.
    #[inline]
    pub fn inverse(&mut self, spectrum: &mut [Complex32], time: &mut [f32]) {
        // Only exactly real DC and Nyquist bins pass realfft's check.
        spectrum[0].im = 0.0;
        if let Some(last) = spectrum.last_mut() {
            last.im = 0.0;
        }
        let done = self
            .inverse
            .process_with_scratch(spectrum, time, &mut self.inverse_scratch);
        debug_assert!(done.is_ok());
        let scale = 1.0 / self.size as f32;
        for s in time.iter_mut() {
            *s *= scale;
        }
    }
}
