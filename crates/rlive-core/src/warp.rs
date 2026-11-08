//! How audio clips are read against the timeline: warp maps and the stretching seam.
//!
//! The engine asks a [`Stretcher`] to render a source range into output frames. Only the
//! [`Passthrough`] exists so far, which ignores the range length and plays at native speed:
//! the real algorithms are milestones A9 and A10.

use crate::sample::SampleBuffer;
use serde::{Deserialize, Serialize};

/// Pins a frame of the source to a tick of the clip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WarpMarker {
    pub src_frame: f64,
    pub tick: f64,
}

impl WarpMarker {
    pub fn new(src_frame: f64, tick: f64) -> Self {
        Self { src_frame, tick }
    }
}

/// A piecewise-linear map between clip ticks and source frames, extended past its ends by its
/// first and last slope.
#[derive(Debug, Clone, PartialEq)]
pub struct WarpMap {
    markers: Vec<WarpMarker>,
}

impl WarpMap {
    /// Build a map from markers in any order. Markers that don't move forward in both ticks and
    /// frames are dropped; two must remain for the map to say anything, else this is `None`.
    pub fn new(markers: impl IntoIterator<Item = WarpMarker>) -> Option<Self> {
        let mut sorted: Vec<WarpMarker> = markers
            .into_iter()
            .filter(|m| m.src_frame.is_finite() && m.tick.is_finite())
            .collect();
        sorted.sort_by(|a, b| a.tick.total_cmp(&b.tick));
        let mut kept: Vec<WarpMarker> = Vec::with_capacity(sorted.len());
        for m in sorted {
            match kept.last() {
                Some(last) if m.tick <= last.tick || m.src_frame <= last.src_frame => {}
                _ => kept.push(m),
            }
        }
        (kept.len() >= 2).then_some(Self { markers: kept })
    }

    pub fn markers(&self) -> &[WarpMarker] {
        &self.markers
    }

    /// The pair of markers around the segment `key` is in, by an increasing key of the markers.
    fn segment(&self, key: impl Fn(&WarpMarker) -> bool) -> (WarpMarker, WarpMarker) {
        let i = self
            .markers
            .partition_point(key)
            .clamp(1, self.markers.len() - 1);
        (self.markers[i - 1], self.markers[i])
    }

    /// Source frame for a clip tick.
    pub fn tick_to_src(&self, tick: f64) -> f64 {
        let (a, b) = self.segment(|m| m.tick <= tick);
        a.src_frame + (tick - a.tick) * (b.src_frame - a.src_frame) / (b.tick - a.tick)
    }

    /// Clip tick for a source frame.
    pub fn src_to_tick(&self, src: f64) -> f64 {
        let (a, b) = self.segment(|m| m.src_frame <= src);
        a.tick + (src - a.src_frame) * (b.tick - a.tick) / (b.src_frame - a.src_frame)
    }

    /// Source frames per tick at a clip tick.
    pub fn slope_at(&self, tick: f64) -> f64 {
        let (a, b) = self.segment(|m| m.tick <= tick);
        (b.src_frame - a.src_frame) / (b.tick - a.tick)
    }
}

/// What Beats mode keeps intact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BeatsPreserve {
    #[default]
    Transients,
    /// Slices on a grid of this many ticks.
    Grid(u32),
}

/// How Beats mode fills the gap after a transient.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BeatsLoop {
    #[default]
    Off,
    Forward,
    BackAndForth,
}

/// A warp mode and the settings it runs with, as the engine sees them. The project keeps every
/// mode's settings in one place; the Builder picks the ones in force.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WarpMode {
    Beats {
        preserve: BeatsPreserve,
        beats_loop: BeatsLoop,
        transient_envelope: f32,
    },
    Tones {
        grain_size: f32,
    },
    Texture {
        grain_size: f32,
        flux: f32,
    },
    RePitch,
    Complex,
    ComplexPro {
        formants: f32,
        envelope: f32,
    },
}

impl WarpMode {
    pub fn kind(&self) -> StretchKind {
        match self {
            WarpMode::Beats { .. } => StretchKind::Beats,
            WarpMode::Tones { .. } => StretchKind::Tones,
            WarpMode::Texture { .. } => StretchKind::Texture,
            WarpMode::RePitch => StretchKind::RePitch,
            WarpMode::Complex => StretchKind::Complex,
            WarpMode::ComplexPro { .. } => StretchKind::ComplexPro,
        }
    }
}

/// Which row of a [`StretchBank`] a clip plays through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StretchKind {
    /// Warp off.
    Off,
    Beats,
    Tones,
    Texture,
    RePitch,
    Complex,
    ComplexPro,
}

impl StretchKind {
    const COUNT: usize = 7;
}

/// The source range one render call covers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StretchSpan {
    pub src_start: f64,
    pub src_end: f64,
    /// Semitones.
    pub transpose: f32,
}

/// Reads a source range into output frames, stretching it to fit. One instance plays one clip
/// at a time. Runs on the audio thread: it must not allocate in any of these methods.
pub trait Stretcher: Send {
    /// Told the engine's sample rate when its track enters the engine, before any other call.
    fn prepare(&mut self, sample_rate: f64);
    /// Jump to source frame `src`: the next render doesn't continue from the last.
    fn seek(&mut self, src: f64);
    /// Write `l.len()` frames of `s` over `span` to `l` and `r`, which have the same length.
    fn render(&mut self, s: &SampleBuffer, span: StretchSpan, l: &mut [f32], r: &mut [f32]);
    /// How many output frames the output runs behind the source.
    fn latency(&self) -> u32;
}

/// Plays the source at its native speed, resampled to the engine rate: warp off. It reads from
/// `span.src_start` and ignores the rest of the span, so it never follows the tempo.
#[derive(Debug, Clone, Copy, Default)]
pub struct Passthrough {
    sample_rate: f64,
}

impl Passthrough {
    pub fn new(sample_rate: f64) -> Self {
        Self { sample_rate }
    }
}

impl Stretcher for Passthrough {
    fn prepare(&mut self, sample_rate: f64) {
        self.sample_rate = sample_rate;
    }

    fn seek(&mut self, _src: f64) {}

    fn render(&mut self, s: &SampleBuffer, span: StretchSpan, l: &mut [f32], r: &mut [f32]) {
        let n = s.len();
        let step = s.sample_rate / self.sample_rate;
        let mut pos = span.src_start;
        for (out_l, out_r) in l.iter_mut().zip(r.iter_mut()) {
            let idx = pos as usize;
            if idx + 1 < n {
                let frac = (pos - idx as f64) as f32;
                *out_l = s.left[idx] + (s.left[idx + 1] - s.left[idx]) * frac;
                *out_r = s.right[idx] + (s.right[idx + 1] - s.right[idx]) * frac;
            } else {
                *out_l = 0.0;
                *out_r = 0.0;
            }
            pos += step;
        }
    }

    fn latency(&self) -> u32 {
        0
    }
}

/// Voices kept per [`StretchKind`], so a crossfade can run two clips of one mode.
pub const VOICES_PER_MODE: usize = 2;

/// A track's stretchers, built up front so playing a clip never allocates.
pub struct StretchBank {
    voices: Vec<Box<dyn Stretcher>>,
    left: Vec<f32>,
    right: Vec<f32>,
}

impl StretchBank {
    /// A bank whose every voice is a passthrough. A9 and A10 replace the rows they implement.
    pub fn new(max_block: usize) -> Self {
        Self {
            voices: (0..StretchKind::COUNT * VOICES_PER_MODE)
                .map(|_| Box::new(Passthrough::default()) as Box<dyn Stretcher>)
                .collect(),
            left: vec![0.0; max_block],
            right: vec![0.0; max_block],
        }
    }

    pub fn prepare(&mut self, sample_rate: f64) {
        for voice in &mut self.voices {
            voice.prepare(sample_rate);
        }
    }

    /// A voice with a block-sized buffer to render into.
    pub fn voice(
        &mut self,
        kind: StretchKind,
        slot: usize,
    ) -> (&mut dyn Stretcher, &mut [f32], &mut [f32]) {
        let index = kind as usize * VOICES_PER_MODE + slot.min(VOICES_PER_MODE - 1);
        (&mut *self.voices[index], &mut self.left, &mut self.right)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    fn map() -> WarpMap {
        WarpMap::new([
            WarpMarker::new(0.0, 0.0),
            WarpMarker::new(4800.0, 960.0),
            WarpMarker::new(14_400.0, 1920.0),
        ])
        .unwrap()
    }

    #[test]
    fn a_warp_map_interpolates_between_markers() {
        let m = map();
        close(m.tick_to_src(480.0), 2400.0);
        close(m.tick_to_src(1440.0), 9600.0);
        close(m.slope_at(100.0), 5.0);
        close(m.slope_at(1000.0), 10.0);
    }

    #[test]
    fn a_warp_map_inverts() {
        let m = map();
        for i in 0..500 {
            let tick = i as f64 * 7.3 - 200.0;
            close(m.src_to_tick(m.tick_to_src(tick)), tick);
        }
    }

    #[test]
    fn a_warp_map_extends_past_its_ends_at_the_end_slopes() {
        let m = map();
        close(m.tick_to_src(-96.0), -480.0);
        close(m.tick_to_src(2400.0), 14_400.0 + 4800.0);
        close(m.src_to_tick(-480.0), -96.0);
    }

    #[test]
    fn a_warp_map_needs_two_forward_markers() {
        assert!(WarpMap::new([]).is_none());
        assert!(WarpMap::new([WarpMarker::new(0.0, 0.0)]).is_none());
        assert!(WarpMap::new([WarpMarker::new(0.0, 0.0), WarpMarker::new(0.0, 960.0)]).is_none());
        // Out of order and with a marker that goes back in the source.
        let m = WarpMap::new([
            WarpMarker::new(1000.0, 1920.0),
            WarpMarker::new(0.0, 0.0),
            WarpMarker::new(500.0, 960.0),
            WarpMarker::new(200.0, 1500.0),
        ])
        .unwrap();
        assert_eq!(m.markers().len(), 3);
    }

    fn ramp_sample(rate: f64) -> SampleBuffer {
        SampleBuffer {
            name: "ramp".into(),
            left: (0..2000).map(|i| i as f32 * 0.001).collect(),
            right: (0..2000).map(|i| -(i as f32) * 0.001).collect(),
            sample_rate: rate,
        }
    }

    #[test]
    fn passthrough_reads_at_the_native_rate_and_ignores_the_span_length() {
        let s = ramp_sample(24_000.0);
        let mut p = Passthrough::new(48_000.0);
        let (mut l, mut r) = ([9.0; 8], [9.0; 8]);
        let span = StretchSpan {
            src_start: 100.0,
            src_end: 999.0,
            transpose: 7.0,
        };
        p.render(&s, span, &mut l, &mut r);
        // Half speed, linear interpolation.
        for (i, (a, b)) in l.iter().zip(r.iter()).enumerate() {
            let want = (100.0 + i as f32 * 0.5) * 0.001;
            assert!((a - want).abs() < 1e-5, "{a} {want}");
            assert!((b + want).abs() < 1e-5);
        }
        assert_eq!(p.latency(), 0);
    }

    #[test]
    fn passthrough_writes_silence_past_the_sample() {
        let s = ramp_sample(48_000.0);
        let mut p = Passthrough::new(48_000.0);
        let (mut l, mut r) = ([9.0; 4], [9.0; 4]);
        let span = StretchSpan {
            src_start: 1998.0,
            src_end: 2002.0,
            transpose: 0.0,
        };
        p.render(&s, span, &mut l, &mut r);
        assert!(l[0] > 1.9);
        assert_eq!(&l[1..], &[0.0, 0.0, 0.0]);
        assert_eq!(&r[1..], &[0.0, 0.0, 0.0]);
    }

    #[test]
    fn every_mode_has_two_voices_and_a_mode_kind_row() {
        let mut bank = StretchBank::new(16);
        bank.prepare(48_000.0);
        let modes = [
            StretchKind::Off,
            StretchKind::Beats,
            StretchKind::Tones,
            StretchKind::Texture,
            StretchKind::RePitch,
            StretchKind::Complex,
            StretchKind::ComplexPro,
        ];
        for kind in modes {
            for slot in 0..VOICES_PER_MODE {
                let (voice, l, r) = bank.voice(kind, slot);
                assert_eq!(voice.latency(), 0);
                assert_eq!((l.len(), r.len()), (16, 16));
            }
        }
        assert_eq!(WarpMode::Complex.kind(), StretchKind::Complex);
        assert_eq!(
            WarpMode::Tones { grain_size: 30.0 }.kind(),
            StretchKind::Tones
        );
    }
}
