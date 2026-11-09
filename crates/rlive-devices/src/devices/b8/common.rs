//! What the batch's devices share: layout shorthand, the note/frequency seam, the spectrogram
//! feed and a peak limiter.

use crate::dsp::spectrum::SpectrumFeed;
use crate::dsp::util::midi_to_hz;
use rlive_core::{Item, ParamId, ScaleInfo, ViewId, ViewWriter, SCOPE_VALUES};

pub fn knob(id: u32) -> Item {
    Item::Knob(ParamId(id))
}

pub fn toggle(id: u32) -> Item {
    Item::Toggle(ParamId(id))
}

pub fn menu(id: u32) -> Item {
    Item::Menu(ParamId(id))
}

/// The one place a pitch becomes a frequency. When tuning systems arrive (roadmap A14) this and
/// [`hz_to_note`] are where a system takes over.
pub fn note_to_hz(note: f32) -> f32 {
    midi_to_hz(note)
}

/// The fractional note a frequency sits at, in semitones of A440.
pub fn hz_to_note(hz: f32) -> f32 {
    69.0 + 12.0 * (hz.max(1e-3) / 440.0).log2()
}

/// The nearest note to `note` whose pitch class is in `scale`; chromatic when there is none.
pub fn quantize_to_scale(note: f32, scale: Option<ScaleInfo>) -> f32 {
    let nearest = note.round();
    let Some(scale) = scale else {
        return nearest;
    };
    if scale.mask & 0x0fff == 0 {
        return nearest;
    }
    let in_scale = |key: i32| scale.mask & (1 << (key - scale.root as i32).rem_euclid(12)) != 0;
    let base = note.round() as i32;
    let mut best = base;
    let mut distance = f32::MAX;
    for key in base - 12..=base + 12 {
        if in_scale(key) && (key as f32 - note).abs() < distance {
            distance = (key as f32 - note).abs();
            best = key;
        }
    }
    best as f32
}

/// Values a spectrogram's feed takes up in a view channel: the dry bands, the wet bands, and
/// the sample rate.
pub const SPECTROGRAM_VIEW_LEN: usize = 2 * SCOPE_VALUES + 1;

/// Feeds the spectrogram panel: dry and wet spectra, published side by side at one view id.
#[derive(Clone, Debug)]
pub struct Spectrogram {
    dry: SpectrumFeed,
    wet: SpectrumFeed,
    base: u32,
    sample_rate: f32,
}

impl Spectrogram {
    pub fn new(sample_rate: f64, view: ViewId) -> Self {
        Self {
            dry: SpectrumFeed::new(sample_rate, 2048, 512),
            wet: SpectrumFeed::new(sample_rate, 2048, 512),
            base: view.0,
            sample_rate: sample_rate as f32,
        }
    }

    pub fn reset(&mut self) {
        self.dry.reset();
        self.wet.reset();
    }

    /// Call with the signal going in, before it is processed.
    pub fn dry(&mut self, left: &[f32], right: &[f32], view: &ViewWriter<'_>) {
        self.dry
            .process_stereo(left, right, view, ViewId(self.base));
    }

    /// Call with the signal coming out.
    pub fn wet(&mut self, left: &[f32], right: &[f32], view: &ViewWriter<'_>) {
        self.wet
            .process_stereo(left, right, view, ViewId(self.base + SCOPE_VALUES as u32));
        view.publish(
            ViewId(self.base + 2 * SCOPE_VALUES as u32),
            &[self.sample_rate],
        );
    }
}

/// A sample-by-sample stereo peak limiter with no lookahead: the ceiling is never crossed,
/// and the gain comes back up over `release` seconds.
#[derive(Clone, Copy, Debug)]
pub struct PeakLimiter {
    gain: f32,
    coefficient: f32,
    ceiling: f32,
}

impl PeakLimiter {
    pub fn new(sample_rate: f32, ceiling: f32, release: f32) -> Self {
        Self {
            gain: 1.0,
            coefficient: 1.0 - (-1.0 / (release * sample_rate).max(1.0)).exp(),
            ceiling,
        }
    }

    /// How far the limiter has the signal pulled down right now, as a gain.
    pub fn gain(&self) -> f32 {
        self.gain
    }

    pub fn reset(&mut self) {
        self.gain = 1.0;
    }

    #[inline]
    pub fn process(&mut self, left: f32, right: f32) -> (f32, f32) {
        let peak = left.abs().max(right.abs());
        let wanted = if peak > self.ceiling {
            self.ceiling / peak
        } else {
            1.0
        };
        if wanted < self.gain {
            self.gain = wanted;
        } else {
            self.gain += (wanted - self.gain) * self.coefficient;
        }
        (left * self.gain, right * self.gain)
    }
}

/// A bipolar triangle, `phase` in `0..1`, starting at -1.
#[inline]
pub fn triangle(phase: f32) -> f32 {
    4.0 * (phase - 0.5).abs() - 1.0
}
