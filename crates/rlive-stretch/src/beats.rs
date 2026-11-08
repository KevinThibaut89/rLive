//! Beats: slices of the source, each starting when the timeline reaches it.
//!
//! The timeline says which part of the source should be playing. Inside a slice the audio plays at
//! its own speed, so the attacks in it keep their shape; the next slice starts the moment the
//! timeline reaches it. What happens when a slice runs out before that is the Transient Loop Mode.

use crate::crossfade_frames;
use crate::interp::Sinc;
use rlive_core::warp::{BeatsLoop, StretchSpan, Stretcher, WarpMode};
use rlive_core::SampleBuffer;

/// One sounding slice, plus at most a few fading out of the way.
const GRAINS: usize = 6;
/// A slice shorter than this in source frames has nothing to loop.
const MIN_LOOP_FRAMES: f64 = 64.0;
/// How far around the middle of a slice to look for a zero-crossing, in frames.
const ZERO_SEARCH: usize = 512;
/// The longest fade the Transient Envelope can make shorter than, in output frames.
const MIN_ENVELOPE_FRAMES: f32 = 32.0;

#[derive(Clone, Copy, PartialEq, Debug)]
enum State {
    Free,
    /// The slice the timeline is in.
    Main,
    /// Fading out of the way of what took over from it.
    Release,
}

#[derive(Clone, Copy, Debug)]
struct Grain {
    state: State,
    pos: f64,
    /// Source frames per output frame, signed by direction.
    step: f64,
    slice_start: f64,
    slice_end: f64,
    /// Where the slice loops back to; worked out the first time it's needed.
    loop_point: Option<f64>,
    age: u32,
    fade_in: u32,
    fade_in_left: u32,
    release_left: u32,
    /// The Transient Envelope's fade length in output frames; 0 for no fade.
    envelope: f32,
}

impl Grain {
    const FREE: Grain = Grain {
        state: State::Free,
        pos: 0.0,
        step: 1.0,
        slice_start: 0.0,
        slice_end: 0.0,
        loop_point: None,
        age: 0,
        fade_in: 0,
        fade_in_left: 0,
        release_left: 0,
        envelope: 0.0,
    };
}

/// Plays source slices against the timeline.
pub struct Beats {
    sinc: &'static Sinc,
    sample_rate: f64,
    crossfade: u32,
    grains: [Grain; GRAINS],
    /// Index in the slice list of the next boundary the timeline will reach.
    next: usize,
    seek_to: Option<f64>,
}

impl Beats {
    pub fn new() -> Self {
        Self {
            sinc: Sinc::shared(),
            sample_rate: 48_000.0,
            crossfade: crossfade_frames(48_000.0),
            grains: [Grain::FREE; GRAINS],
            next: 0,
            seek_to: None,
        }
    }

    /// A grain to start. Prefers a free one, then the release with the least left.
    fn claim(&mut self) -> usize {
        let free = self.grains.iter().position(|g| g.state == State::Free);
        free.unwrap_or_else(|| {
            (0..GRAINS)
                .filter(|&i| self.grains[i].state == State::Release)
                .min_by_key(|&i| self.grains[i].release_left)
                .unwrap_or(0)
        })
    }

    /// Lets the sounding slice go, so a new one can take over.
    fn release_main(&mut self) {
        let crossfade = self.crossfade;
        for g in &mut self.grains {
            if g.state == State::Main {
                g.state = State::Release;
                g.release_left = crossfade;
                // A slice that has run out reads nothing more of its own.
                if g.pos >= g.slice_end && g.step > 0.0 {
                    g.step = -g.step;
                }
            }
        }
    }

    /// Whether the sounding slice has anything left to sound.
    fn main_is_sounding(&self, n: usize) -> bool {
        self.grains
            .iter()
            .any(|g| g.state == State::Main && g.pos + 8.0 < n as f64)
    }

    #[allow(clippy::too_many_arguments)]
    fn start_grain(
        &mut self,
        pos: f64,
        slice_start: f64,
        slice_end: f64,
        rate: f64,
        step: f64,
        envelope: f32,
        fade_in: u32,
    ) {
        let i = self.claim();
        let slice_frames = ((slice_end - slice_start) / rate.max(1e-6)) as f32;
        self.grains[i] = Grain {
            state: State::Main,
            pos,
            step,
            slice_start,
            slice_end,
            loop_point: None,
            age: 0,
            fade_in,
            fade_in_left: fade_in,
            release_left: 0,
            envelope: if envelope >= 100.0 {
                0.0
            } else {
                (slice_frames * envelope / 100.0).max(MIN_ENVELOPE_FRAMES)
            },
        };
    }

    fn begin_seek(
        &mut self,
        s: &SampleBuffer,
        src: f64,
        slices: &[f64],
        rate: f64,
        step: f64,
        envelope: f32,
    ) {
        self.seek_to = None;
        self.next = slices.partition_point(|b| *b <= src);
        let slice_start = if self.next > 0 {
            slices[self.next - 1]
        } else {
            0.0
        };
        let slice_end = slices.get(self.next).copied().unwrap_or(s.len() as f64);
        let (had_main, sounding) = (
            self.grains.iter().any(|g| g.state == State::Main),
            self.main_is_sounding(s.len()),
        );
        // A previous reading that ran off the end of the file is gone already; see `RePitch`.
        let fade_in = if had_main && !sounding {
            0
        } else {
            self.crossfade
        };
        self.release_main();
        self.start_grain(src, slice_start, slice_end, rate, step, envelope, fade_in);
    }

    /// The zero-crossing nearest the middle of a slice, or the middle if there is none near.
    fn zero_crossing(s: &SampleBuffer, g: &Grain) -> f64 {
        let n = s.len();
        let middle = (g.slice_start + g.slice_end) * 0.5;
        let reach = ((g.slice_end - g.slice_start) * 0.25).clamp(1.0, ZERO_SEARCH as f64) as usize;
        let centre = (middle as usize).min(n.saturating_sub(2));
        let value = |i: usize| s.left[i] + s.right[i];
        for d in 0..reach {
            for at in [centre + d, centre.wrapping_sub(d)] {
                if at + 1 < n {
                    let (a, b) = (value(at), value(at + 1));
                    if a == 0.0 || (a > 0.0) != (b > 0.0) {
                        return at as f64 + 1.0;
                    }
                }
            }
        }
        middle
    }

    /// Makes the slice go on past its end, as its loop mode says.
    fn run_past_end(&mut self, i: usize, s: &SampleBuffer, mode: BeatsLoop) {
        let g = self.grains[i];
        let span = g.slice_end - g.slice_start;
        match mode {
            BeatsLoop::Off => self.grains[i].state = State::Free,
            _ if span < MIN_LOOP_FRAMES => self.grains[i].state = State::Free,
            BeatsLoop::Forward => {
                let point = g.loop_point.unwrap_or_else(|| Self::zero_crossing(s, &g));
                // What just played turns around and fades, covering the jump back.
                let t = self.claim_tail(i);
                if let Some(t) = t {
                    let mut tail = self.grains[i];
                    tail.state = State::Release;
                    tail.release_left = self.crossfade;
                    tail.step = -tail.step;
                    self.grains[t] = tail;
                }
                let g = &mut self.grains[i];
                g.loop_point = Some(point);
                g.pos = point + (g.pos - g.slice_end);
                g.fade_in = self.crossfade;
                g.fade_in_left = self.crossfade;
            }
            BeatsLoop::BackAndForth => {
                let point = g.loop_point.unwrap_or_else(|| Self::zero_crossing(s, &g));
                let g = &mut self.grains[i];
                g.loop_point = Some(point);
                g.pos = g.slice_end - (g.pos - g.slice_end);
                g.step = -g.step;
            }
        }
    }

    /// A grain to carry the tail of grain `keep`, if one can be spared.
    fn claim_tail(&self, keep: usize) -> Option<usize> {
        let free = self.grains.iter().position(|g| g.state == State::Free);
        free.or_else(|| {
            (0..GRAINS)
                .filter(|&i| i != keep && self.grains[i].state == State::Release)
                .min_by_key(|&i| self.grains[i].release_left)
        })
    }

    /// The next frame of grain `i`, added to `(l, r)`.
    #[inline]
    fn sound_grain(
        &mut self,
        i: usize,
        s: &SampleBuffer,
        mode: BeatsLoop,
        band: usize,
    ) -> (f32, f32) {
        let crossfade = self.crossfade as f32;
        let g = &mut self.grains[i];
        let mut gain = 1.0f32;
        if g.fade_in_left > 0 {
            gain *= 1.0 - g.fade_in_left as f32 / g.fade_in.max(1) as f32;
            g.fade_in_left -= 1;
        }
        if g.envelope > 0.0 {
            gain *= (1.0 - g.age as f32 / g.envelope).max(0.0);
        }
        if g.state == State::Release {
            if g.release_left == 0 {
                g.state = State::Free;
                return (0.0, 0.0);
            }
            gain *= g.release_left as f32 / crossfade;
            g.release_left -= 1;
        } else if g.step > 0.0 && mode == BeatsLoop::Off {
            // The slice stops at its end, which is rarely a zero-crossing.
            let frames_left = ((g.slice_end - g.pos) / g.step) as f32;
            if frames_left < crossfade {
                gain *= (frames_left / crossfade).max(0.0);
            }
        }
        let (l, r) = self.sinc.read(s, g.pos, band);
        g.pos += g.step;
        g.age += 1;
        let is_main = g.state == State::Main;
        let (forward_end, back_end) = (g.pos >= g.slice_end, g.pos <= g.slice_start);
        let loop_point = g.loop_point;
        let falling = g.step < 0.0;
        if is_main {
            if !falling && forward_end {
                self.run_past_end(i, s, mode);
            } else if falling && (back_end || loop_point.is_some_and(|p| self.grains[i].pos <= p)) {
                let g = &mut self.grains[i];
                let point = g.loop_point.unwrap_or(g.slice_start);
                g.pos = point + (point - g.pos);
                g.step = -g.step;
            }
        }
        (l * gain, r * gain)
    }
}

impl Default for Beats {
    fn default() -> Self {
        Self::new()
    }
}

impl Stretcher for Beats {
    fn prepare(&mut self, sample_rate: f64) {
        self.sample_rate = sample_rate;
        self.crossfade = crossfade_frames(sample_rate);
    }

    fn seek(&mut self, src: f64) {
        self.seek_to = Some(src);
    }

    fn reset(&mut self) {
        self.grains = [Grain::FREE; GRAINS];
        self.next = 0;
        self.seek_to = None;
    }

    fn declicks(&self) -> bool {
        true
    }

    fn render(&mut self, s: &SampleBuffer, span: StretchSpan<'_>, l: &mut [f32], r: &mut [f32]) {
        let frames = l.len().min(r.len());
        if frames == 0 {
            return;
        }
        let (mode, transient_envelope) = match span.mode {
            WarpMode::Beats {
                beats_loop,
                transient_envelope,
                ..
            } => (beats_loop, transient_envelope),
            _ => (BeatsLoop::Off, 100.0),
        };
        let rate = (span.src_end - span.src_start) / frames as f64;
        let step = s.sample_rate / self.sample_rate * 2f64.powf(span.transpose as f64 / 12.0);
        let band = Sinc::band_for(step);
        if let Some(src) = self.seek_to {
            self.begin_seek(s, src, span.slices, rate, step, transient_envelope);
        }
        let fade_in = self.crossfade;
        for i in 0..frames {
            let p = span.src_start + rate * i as f64;
            while let Some(&b) = span.slices.get(self.next) {
                if p < b {
                    break;
                }
                self.next += 1;
                let slice_end = span
                    .slices
                    .get(self.next)
                    .copied()
                    .unwrap_or(s.len() as f64);
                // Where the slice's own clock has got to inside this output frame.
                let since = ((p - b) / rate.max(1e-9)).clamp(0.0, 1.0);
                self.release_main();
                self.start_grain(
                    b + since * step,
                    b,
                    slice_end,
                    rate,
                    step,
                    transient_envelope,
                    fade_in,
                );
            }
            let (mut out_l, mut out_r) = (0.0, 0.0);
            for g in 0..GRAINS {
                if self.grains[g].state != State::Free {
                    let (a, b) = self.sound_grain(g, s, mode, band);
                    out_l += a;
                    out_r += b;
                }
            }
            l[i] = out_l;
            r[i] = out_r;
        }
    }

    fn latency(&self) -> u32 {
        0
    }
}
