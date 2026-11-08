//! Audio clip rendering.

use super::data::AudioClip;
use super::BlockTime;

/// Length of the anti-click ramps applied to audio at discontinuities, in frames.
pub(crate) const FADE_FRAMES: u32 = 64;

/// Read position of an audio clip.
pub(crate) struct AudioCursor {
    /// Next read position, in source frames.
    pub src: f64,
    /// Clip-local tick `src` corresponds to.
    pub next_local: f64,
    /// Frames of ramp-in still to apply.
    pub fade_in: u32,
    /// Clip-local tick the region ends at, which audio ramps out towards. Infinite (no ramp)
    /// for Session clips.
    pub region_end: f64,
}

impl AudioCursor {
    pub fn new() -> Self {
        Self {
            src: 0.0,
            next_local: 0.0,
            fade_in: 0,
            region_end: f64::INFINITY,
        }
    }

    /// Enter a new timeline region: the next render reseeds from the clip position.
    pub fn enter(&mut self, region_end: f64) {
        self.next_local = f64::NEG_INFINITY;
        self.region_end = region_end;
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_audio(
    clip: &AudioClip,
    origin: f64,
    a: f64,
    seg_end: f64,
    time: BlockTime,
    audio: &mut AudioCursor,
    l: &mut [f32],
    r: &mut [f32],
) {
    let sample = &*clip.sample;
    let len = clip.length as f64;
    let ticks_per_src_frame = rlive_core::time::ticks_per_sample(time.tempo, sample.sample_rate);
    let local_a = a - origin;
    // Continue from where the last render stopped, so a tempo change never moves the read
    // position; only a real discontinuity reseeds it from the clip position.
    if (local_a - audio.next_local).abs() > time.ticks_per_sample * 0.5 {
        let local = if clip.looped {
            local_a.rem_euclid(len)
        } else {
            local_a.max(0.0)
        };
        audio.src = local / ticks_per_src_frame;
        audio.fade_in = FADE_FRAMES;
    }
    audio.next_local = seg_end - origin;

    let from = time.offset(a);
    let to = time.offset(seg_end).min(l.len()).min(r.len());
    let ratio = sample.sample_rate / time.sample_rate;
    let loop_frames = len / ticks_per_src_frame;
    let n = sample.len();
    // A one-shot's content can run out before its region does.
    let fade_end = if clip.looped || audio.region_end.is_infinite() {
        audio.region_end
    } else {
        audio.region_end.min(len)
    };
    for i in from..to {
        if audio.src >= loop_frames {
            if clip.looped {
                audio.src -= loop_frames;
            } else {
                break;
            }
        }
        let pos = audio.src;
        let idx = pos as usize;
        let mut gain = clip.gain;
        if audio.fade_in > 0 {
            gain *= 1.0 - audio.fade_in as f32 / FADE_FRAMES as f32;
            audio.fade_in -= 1;
        }
        let local = local_a + (i - from) as f64 * time.ticks_per_sample;
        let frames_left = (fade_end - local) / time.ticks_per_sample;
        if frames_left < FADE_FRAMES as f64 {
            gain *= (frames_left / FADE_FRAMES as f64).max(0.0) as f32;
        }
        if idx + 1 < n {
            let frac = (pos - idx as f64) as f32;
            l[i] += (sample.left[idx] + (sample.left[idx + 1] - sample.left[idx]) * frac) * gain;
            r[i] += (sample.right[idx] + (sample.right[idx + 1] - sample.right[idx]) * frac) * gain;
        }
        audio.src += ratio;
    }
}
