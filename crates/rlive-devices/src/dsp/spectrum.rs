//! Spectrum feed: measures a stream and publishes it for a scope to draw.

use super::fft::{Complex32, Fft};
use super::util::{gain_to_db, sanitize};
use rlive_core::{ViewId, ViewWriter, SCOPE_VALUES};
use std::f32::consts::TAU;

/// Lowest level shown on the scope, in decibels.
pub const SPECTRUM_FLOOR_DB: f32 = -96.0;

/// Publishes [`SCOPE_VALUES`] log-spaced bands from 20 Hz to Nyquist as `[0, 1]` magnitudes,
/// where 1 is a full-scale sine and 0 is [`SPECTRUM_FLOOR_DB`]. Does nothing while no UI is
/// reading.
#[derive(Clone, Debug)]
pub struct SpectrumFeed {
    fft: Fft,
    window: Vec<f32>,
    ring: Vec<f32>,
    write: usize,
    since_frame: usize,
    hop: usize,
    /// Fraction of the previous band value kept each frame.
    smoothing: f32,
    /// Fractional FFT bin at each band's edge; `SCOPE_VALUES + 1` of them.
    edges: Vec<f32>,
    amp_scale: f32,
    frame: Vec<f32>,
    spectrum: Vec<Complex32>,
    bands: [f32; SCOPE_VALUES],
}

impl SpectrumFeed {
    /// `fft_size` is a power of two. A hop of a quarter of it is a good default.
    pub fn new(sample_rate: f64, fft_size: usize, hop: usize) -> Self {
        let sr = sample_rate.max(1.0) as f32;
        let window: Vec<f32> = (0..fft_size)
            .map(|i| 0.5 - 0.5 * (TAU * i as f32 / fft_size as f32).cos())
            .collect();
        let sum: f32 = window.iter().sum();
        let fft = Fft::new(fft_size);
        let bins = fft.bins();
        let bin_hz = sr / fft_size as f32;
        let nyquist = sr * 0.5;
        let low = 20.0f32.min(nyquist * 0.5);
        let ratio = nyquist / low;
        let edges = (0..=SCOPE_VALUES)
            .map(|i| low * ratio.powf(i as f32 / SCOPE_VALUES as f32) / bin_hz)
            .collect();
        Self {
            fft,
            window,
            ring: vec![0.0; fft_size],
            write: 0,
            since_frame: 0,
            hop: hop.max(1),
            smoothing: 0.0,
            edges,
            amp_scale: 2.0 / sum,
            frame: vec![0.0; fft_size],
            spectrum: vec![Complex32::new(0.0, 0.0); bins],
            bands: [0.0; SCOPE_VALUES],
        }
    }

    /// How much of the previous frame each new one keeps, 0..1. Higher is smoother.
    pub fn set_smoothing(&mut self, smoothing: f32) {
        self.smoothing = smoothing.clamp(0.0, 0.999);
    }

    pub fn reset(&mut self) {
        self.ring.fill(0.0);
        self.write = 0;
        self.since_frame = 0;
        self.bands = [0.0; SCOPE_VALUES];
    }

    /// The bands as last measured.
    pub fn bands(&self) -> &[f32; SCOPE_VALUES] {
        &self.bands
    }

    /// Takes in a block and publishes at `view` whenever a frame is due.
    pub fn process(&mut self, block: &[f32], view: &ViewWriter<'_>, id: ViewId) {
        if !view.is_active() {
            return;
        }
        for &x in block {
            self.push(x, view, id);
        }
    }

    /// As [`process`](Self::process), for the mono sum of a stereo pair.
    pub fn process_stereo(
        &mut self,
        left: &[f32],
        right: &[f32],
        view: &ViewWriter<'_>,
        id: ViewId,
    ) {
        if !view.is_active() {
            return;
        }
        for (l, r) in left.iter().zip(right) {
            self.push(0.5 * (l + r), view, id);
        }
    }

    #[inline]
    fn push(&mut self, x: f32, view: &ViewWriter<'_>, id: ViewId) {
        let len = self.ring.len();
        self.ring[self.write] = sanitize(x);
        self.write = (self.write + 1) % len;
        self.since_frame += 1;
        if self.since_frame >= self.hop {
            self.since_frame = 0;
            self.measure();
            view.publish(id, &self.bands);
        }
    }

    fn measure(&mut self) {
        let len = self.ring.len();
        for i in 0..len {
            self.frame[i] = self.ring[(self.write + i) % len] * self.window[i];
        }
        self.fft.forward(&mut self.frame, &mut self.spectrum);
        let last = self.spectrum.len() - 1;
        for (band, value) in self.bands.iter_mut().enumerate() {
            let (lo, hi) = (self.edges[band], self.edges[band + 1]);
            let (first, end) = (lo.ceil() as usize, hi.floor() as usize);
            // Bands narrower than a bin read between the two bins either side.
            let mag = if end >= first {
                let (first, end) = (first.min(last), end.min(last));
                self.spectrum[first..=end]
                    .iter()
                    .fold(0.0f32, |m, c| m.max(c.norm_sqr()))
                    .sqrt()
            } else {
                let at = (0.5 * (lo + hi)).min(last as f32 - 1.0).max(0.0);
                let i = at.floor() as usize;
                let t = at - i as f32;
                let a = self.spectrum[i].norm();
                let b = self.spectrum[(i + 1).min(last)].norm();
                a + (b - a) * t
            };
            let db = gain_to_db(mag * self.amp_scale);
            let level = ((db - SPECTRUM_FLOOR_DB) / -SPECTRUM_FLOOR_DB).clamp(0.0, 1.0);
            *value = (*value * self.smoothing + level * (1.0 - self.smoothing)).clamp(0.0, 1.0);
        }
    }
}
