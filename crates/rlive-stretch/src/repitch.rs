//! Re-Pitch: the source is read at whatever rate the timeline asks for.

use crate::crossfade_frames;
use crate::interp::Sinc;
use rlive_core::warp::{StretchSpan, Stretcher};
use rlive_core::SampleBuffer;

/// Reads the source at the rate of the span, so pitch follows speed like a turntable. Transpose
/// has no effect: the rate is the transposition.
pub struct RePitch {
    sinc: &'static Sinc,
    crossfade: u32,
    /// Where the previous render was heading, for the crossfade out of it.
    tail_pos: f64,
    tail_rate: f64,
    tail_left: u32,
    /// Where the source was last being read, if it is being read at all.
    last: Option<(f64, f64)>,
    fade_in_left: u32,
    pending_seek: bool,
    fresh: bool,
}

impl RePitch {
    pub fn new() -> Self {
        Self {
            sinc: Sinc::shared(),
            crossfade: crossfade_frames(48_000.0),
            tail_pos: 0.0,
            tail_rate: 1.0,
            tail_left: 0,
            last: None,
            fade_in_left: 0,
            pending_seek: false,
            fresh: true,
        }
    }

    fn begin_seek(&mut self, n: usize) {
        self.pending_seek = false;
        match self.last {
            // Mid-file: the old reading has somewhere to go, so hand over to the new one.
            Some((pos, rate)) if pos + 8.0 < n as f64 => {
                self.tail_pos = pos;
                self.tail_rate = rate;
                self.tail_left = self.crossfade;
                self.fade_in_left = self.crossfade;
            }
            // The old reading ran off the end of the file: its signal is gone, as at the end
            // of a well-cut loop, so the new one starts at once.
            Some(_) => {
                self.tail_left = 0;
                self.fade_in_left = 0;
            }
            None => {
                self.tail_left = 0;
                self.fade_in_left = if self.fresh { self.crossfade } else { 0 };
            }
        }
    }
}

impl Default for RePitch {
    fn default() -> Self {
        Self::new()
    }
}

impl Stretcher for RePitch {
    fn prepare(&mut self, sample_rate: f64) {
        self.crossfade = crossfade_frames(sample_rate);
    }

    fn seek(&mut self, _src: f64) {
        self.pending_seek = true;
    }

    fn reset(&mut self) {
        self.last = None;
        self.tail_left = 0;
        self.fade_in_left = 0;
        self.pending_seek = false;
        self.fresh = true;
    }

    fn declicks(&self) -> bool {
        true
    }

    fn render(&mut self, s: &SampleBuffer, span: StretchSpan<'_>, l: &mut [f32], r: &mut [f32]) {
        let frames = l.len().min(r.len());
        if frames == 0 {
            return;
        }
        if self.pending_seek {
            self.begin_seek(s.len());
        }
        self.fresh = false;
        let rate = (span.src_end - span.src_start) / frames as f64;
        let band = Sinc::band_for(rate);
        let tail_band = Sinc::band_for(self.tail_rate);
        let crossfade = self.crossfade as f32;
        for i in 0..frames {
            let pos = span.src_start + rate * i as f64;
            let (mut out_l, mut out_r) = self.sinc.read(s, pos, band);
            if self.fade_in_left > 0 {
                let gain = 1.0 - self.fade_in_left as f32 / crossfade;
                out_l *= gain;
                out_r *= gain;
                self.fade_in_left -= 1;
            }
            if self.tail_left > 0 {
                let gain = self.tail_left as f32 / crossfade;
                let (tail_l, tail_r) = self.sinc.read(s, self.tail_pos, tail_band);
                out_l += tail_l * gain;
                out_r += tail_r * gain;
                self.tail_pos += self.tail_rate;
                self.tail_left -= 1;
            }
            l[i] = out_l;
            r[i] = out_r;
        }
        self.last = Some((span.src_end, rate));
    }

    fn latency(&self) -> u32 {
        0
    }
}
