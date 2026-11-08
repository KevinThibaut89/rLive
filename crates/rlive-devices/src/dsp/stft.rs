//! Short-time Fourier transform with overlap-add resynthesis.

use super::fft::{Complex32, Fft};
use super::util::sanitize;
use std::f32::consts::TAU;

/// Streams audio through a spectral process, at a fixed latency of `size` samples.
///
/// Analysis and synthesis both use a square-root periodic Hann window, which reconstructs
/// perfectly when the frames overlap-add at a hop that divides the size.
#[derive(Clone, Debug)]
pub struct Stft {
    size: usize,
    hop: usize,
    fft: Fft,
    window: Vec<f32>,
    /// Corrects for how much the windows overlap.
    scale: f32,
    input: Vec<f32>,
    overlap: Vec<f32>,
    write: usize,
    read: usize,
    since_frame: usize,
    frame: Vec<f32>,
    spectrum: Vec<Complex32>,
}

impl Stft {
    /// `hop` must divide `size` and be at most `size / 2`.
    ///
    /// # Panics
    /// Panics if the hop doesn't fit the size. Both are programmer's choices made at build time.
    pub fn new(size: usize, hop: usize) -> Self {
        assert!(
            size >= 4 && hop >= 1 && hop * 2 <= size && size.is_multiple_of(hop),
            "STFT hop must divide the frame size and be at most half of it"
        );
        let window: Vec<f32> = (0..size)
            .map(|i| (0.5 - 0.5 * (TAU * i as f32 / size as f32).cos()).sqrt())
            .collect();
        // With these windows the overlap sum is the same everywhere; sum it at one sample.
        let overlap_sum: f32 = (0..size).step_by(hop).map(|k| window[k] * window[k]).sum();
        let fft = Fft::new(size);
        let bins = fft.bins();
        Self {
            size,
            hop,
            fft,
            window,
            scale: 1.0 / overlap_sum,
            input: vec![0.0; size],
            overlap: vec![0.0; size],
            write: 0,
            read: 0,
            since_frame: 0,
            frame: vec![0.0; size],
            spectrum: vec![Complex32::new(0.0, 0.0); bins],
        }
    }

    pub fn size(&self) -> usize {
        self.size
    }

    pub fn hop(&self) -> usize {
        self.hop
    }

    /// How many bins the process callback sees.
    pub fn bins(&self) -> usize {
        self.spectrum.len()
    }

    /// Delay through the transform, in samples.
    pub fn latency(&self) -> u32 {
        self.size as u32
    }

    pub fn reset(&mut self) {
        self.input.fill(0.0);
        self.overlap.fill(0.0);
        self.write = 0;
        self.read = 0;
        self.since_frame = 0;
    }

    /// Runs `input` through, writing `output`, and calling `process` on each frame's spectrum.
    /// A block longer than the hop can call it more than once.
    pub fn process(
        &mut self,
        input: &[f32],
        output: &mut [f32],
        mut process: impl FnMut(&mut [Complex32]),
    ) {
        for (x, y) in input.iter().zip(output.iter_mut()) {
            *y = sanitize(std::mem::take(&mut self.overlap[self.read]));
            self.read = (self.read + 1) % self.size;
            self.input[self.write] = sanitize(*x);
            self.write = (self.write + 1) % self.size;
            self.since_frame += 1;
            if self.since_frame == self.hop {
                self.since_frame = 0;
                self.run_frame(&mut process);
            }
        }
    }

    fn run_frame(&mut self, process: &mut impl FnMut(&mut [Complex32])) {
        for i in 0..self.size {
            let at = (self.write + i) % self.size;
            self.frame[i] = self.input[at] * self.window[i];
        }
        self.fft.forward(&mut self.frame, &mut self.spectrum);
        process(&mut self.spectrum);
        self.fft.inverse(&mut self.spectrum, &mut self.frame);
        for i in 0..self.size {
            let at = (self.read + i) % self.size;
            self.overlap[at] += self.frame[i] * self.window[i] * self.scale;
        }
    }
}
