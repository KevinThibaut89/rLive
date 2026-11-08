//! Audio clip rendering.

use super::data::{AudioClip, WarpedClip};
use super::BlockTime;
use rlive_core::warp::{StretchBank, StretchKind, StretchSpan, WarpMap, WarpMode};

/// Length of the anti-click ramps applied to audio at discontinuities, in frames.
pub(crate) const FADE_FRAMES: u32 = 64;

/// Which voice of a [`StretchBank`] row a cursor plays through. Each cursor owns one, so the
/// lane and a launched clip can both sound in a block.
pub(crate) const LANE_VOICE: usize = 0;
pub(crate) const SESSION_VOICE: usize = 1;

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
    /// The stretch voice this cursor plays through.
    voice: usize,
    /// The voice doesn't know where the next render starts.
    needs_seek: bool,
    /// The voice has rendered for this cursor, so it may be holding sound worth fading out.
    in_progress: bool,
}

impl AudioCursor {
    pub fn new(voice: usize) -> Self {
        Self {
            src: 0.0,
            next_local: 0.0,
            fade_in: 0,
            region_end: f64::INFINITY,
            voice,
            needs_seek: true,
            in_progress: false,
        }
    }

    /// Enter a new timeline region: the next render reseeds from the clip position.
    pub fn enter(&mut self, region_end: f64) {
        self.next_local = f64::NEG_INFINITY;
        self.region_end = region_end;
        self.in_progress = false;
    }
}

/// Render `[a, seg_end)` of an audio clip through its stretcher. `warp` is `None` for warp off.
///
/// Warp off plays the source at its native speed, so what the timeline says about the source only
/// matters at discontinuities and loop ends. A warped clip whose mode has a real stretcher goes
/// through [`render_following`] instead, where the timeline says where the source is at every
/// moment. The rest of the warp modes still read at native speed, as they did before those
/// stretchers existed.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_audio(
    clip: &AudioClip,
    warp: Option<&WarpedClip>,
    origin: f64,
    a: f64,
    seg_end: f64,
    time: BlockTime,
    audio: &mut AudioCursor,
    l: &mut [f32],
    r: &mut [f32],
    stretch: &mut StretchBank,
) {
    if let Some((w, map)) = warp.and_then(|w| w.map.as_ref().map(|m| (w, m))) {
        if w.mode.kind().follows_timeline() {
            render_following(clip, w, map, origin, a, seg_end, time, audio, l, r, stretch);
            return;
        }
    }
    let sample = &*clip.sample;
    let len = clip.length as f64;
    let ticks_per_src_frame = rlive_core::time::ticks_per_sample(time.tempo, sample.sample_rate);
    let map = warp.and_then(|w| w.map.as_ref());
    let tick_to_src = |local: f64| match map {
        Some(m) => m.tick_to_src(local),
        None => local / ticks_per_src_frame,
    };
    let local_a = a - origin;
    // Continue from where the last render stopped, so a tempo change never moves the read
    // position; only a real discontinuity reseeds it from the clip position.
    if (local_a - audio.next_local).abs() > time.ticks_per_sample * 0.5 {
        let local = if clip.looped {
            local_a.rem_euclid(len)
        } else {
            local_a.max(0.0)
        };
        audio.src = tick_to_src(local);
        audio.fade_in = FADE_FRAMES;
        audio.needs_seek = true;
    }
    audio.next_local = seg_end - origin;

    let from = time.offset(a);
    let to = time.offset(seg_end).min(l.len()).min(r.len());
    let ratio = sample.sample_rate / time.sample_rate;
    let loop_start = map.map_or(0.0, |m| m.tick_to_src(0.0));
    let loop_end = tick_to_src(len);
    // A one-shot's content can run out before its region does.
    let fade_end = if clip.looped || audio.region_end.is_infinite() {
        audio.region_end
    } else {
        audio.region_end.min(len)
    };
    // A warped clip with too few markers has no timeline to follow and plays like warp off, as a
    // mode with a real stretcher would otherwise be rendered through the wrong kind of span.
    let (kind, transpose, mode) = match warp {
        Some(w) if !w.mode.kind().follows_timeline() => (w.mode.kind(), w.transpose, w.mode),
        _ => (StretchKind::Off, 0.0, WarpMode::Complex),
    };
    let (voice, voice_l, voice_r) = stretch.voice(kind, audio.voice);
    if std::mem::take(&mut audio.needs_seek) {
        voice.seek(audio.src);
    }

    let mut i = from;
    while i < to {
        if audio.src >= loop_end {
            if !clip.looped {
                break;
            }
            audio.src -= loop_end - loop_start;
            voice.seek(audio.src);
        }
        // Advance one frame at a time so wraps land where the single-speed read always did.
        let mut src = audio.src + ratio;
        let mut count = 1;
        while i + count < to && src < loop_end {
            src += ratio;
            count += 1;
        }
        voice.render(
            sample,
            StretchSpan {
                src_start: audio.src,
                src_end: src,
                transpose,
                mode,
                slices: &[],
            },
            &mut voice_l[i..i + count],
            &mut voice_r[i..i + count],
        );
        for f in i..i + count {
            let mut gain = clip.gain;
            if audio.fade_in > 0 {
                gain *= 1.0 - audio.fade_in as f32 / FADE_FRAMES as f32;
                audio.fade_in -= 1;
            }
            let local = local_a + (f - from) as f64 * time.ticks_per_sample;
            let frames_left = (fade_end - local) / time.ticks_per_sample;
            if frames_left < FADE_FRAMES as f64 {
                gain *= (frames_left / FADE_FRAMES as f64).max(0.0) as f32;
            }
            l[f] += voice_l[f] * gain;
            r[f] += voice_r[f] * gain;
        }
        audio.src = src;
        i += count;
    }
}

/// While the tempo is ramping, a span is at most this long, so the read position stays close to
/// the curve between its exact ends.
const RAMP_CHUNK: usize = 32;
/// How many source frames apart the read head and the timeline can be and still count as one
/// continuous reading.
const CONTINUOUS_FRAMES: f64 = 2.0;

/// Renders a warped clip whose source position comes from the timeline, through its map.
///
/// The output frames are cut into stretches, each playing at one rate: a stretch ends at a warp
/// marker and at the end of the loop, and its span is the exact source range between the map's
/// positions at each end. Nothing accumulates from block to block, so however the tempo moves
/// the clip stays where the song position says.
#[allow(clippy::too_many_arguments)]
fn render_following(
    clip: &AudioClip,
    warp: &WarpedClip,
    map: &WarpMap,
    origin: f64,
    a: f64,
    seg_end: f64,
    time: BlockTime,
    audio: &mut AudioCursor,
    l: &mut [f32],
    r: &mut [f32],
    stretch: &mut StretchBank,
) {
    let sample = &*clip.sample;
    let len = clip.length as f64;
    let local_a = a - origin;
    let jumped = (local_a - audio.next_local).abs() > time.ticks_per_sample * 0.5;
    audio.next_local = seg_end - origin;

    let from = time.offset(a);
    let to = time.offset(seg_end).min(l.len()).min(r.len());
    if from >= to {
        return;
    }
    let fade_end = if clip.looped || audio.region_end.is_infinite() {
        audio.region_end
    } else {
        audio.region_end.min(len)
    };
    let (voice, voice_l, voice_r) = stretch.voice(warp.mode.kind(), audio.voice);
    if !audio.in_progress {
        voice.reset();
    }

    // The local tick frame `f` plays, measured from the block so a mid-block start (a launch) is
    // exact.
    let local_at = |f: usize| {
        if f == from {
            local_a
        } else {
            time.tick_at(f) - origin
        }
    };
    let markers = map.markers();
    let mut f = from;
    while f < to {
        let local = local_at(f);
        let (position, cycle_start) = if clip.looped {
            let cycle = (local / len).floor();
            (local - cycle * len, cycle * len)
        } else {
            (local.max(0.0), 0.0)
        };
        let want = map.tick_to_src(position);
        if audio.needs_seek || jumped && f == from || (want - audio.src).abs() > CONTINUOUS_FRAMES {
            voice.seek(want);
            audio.needs_seek = false;
        }

        // How far this stretch can run at one rate.
        let next_marker = markers.partition_point(|m| m.tick <= position);
        let mut boundary = markers.get(next_marker).map_or(f64::INFINITY, |m| m.tick);
        if clip.looped && boundary > len {
            boundary = len;
        }
        let limit = if time.ramp != 0.0 {
            (f + RAMP_CHUNK).min(to)
        } else {
            to
        };
        let end = if boundary.is_finite() {
            // The first frame that is at or past the boundary.
            let ticks = boundary + cycle_start + origin - time.t0;
            let frame = time.run().frames_to(ticks).ceil();
            (frame.max(f as f64 + 1.0) as usize).min(limit)
        } else {
            limit
        };
        let count = end - f;
        let end_position = local_at(end) - cycle_start;
        let src_end = map.tick_to_src(end_position);
        voice.render(
            sample,
            StretchSpan {
                src_start: want,
                src_end,
                transpose: warp.transpose,
                mode: warp.mode,
                slices: &warp.slices,
            },
            &mut voice_l[f..end],
            &mut voice_r[f..end],
        );
        let tps = time.ticks_per_sample;
        for i in f..end {
            let mut gain = clip.gain;
            if !voice.declicks() && audio.fade_in > 0 {
                gain *= 1.0 - audio.fade_in as f32 / FADE_FRAMES as f32;
                audio.fade_in -= 1;
            }
            let frames_left = (fade_end - (local_a + (i - from) as f64 * tps)) / tps;
            if frames_left < FADE_FRAMES as f64 {
                gain *= (frames_left / FADE_FRAMES as f64).max(0.0) as f32;
            }
            l[i] += voice_l[i] * gain;
            r[i] += voice_r[i] * gain;
        }
        audio.src = src_end;
        f += count;
    }
    audio.in_progress = true;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clip::data::WarpedClip;
    use rlive_core::warp::{WarpMarker, WarpMode};
    use rlive_core::SampleBuffer;
    use std::sync::Arc;

    const SR: f64 = 48_000.0;

    fn sample(rate: f64) -> Arc<SampleBuffer> {
        let n = 30_000;
        let left: Vec<f32> = (0..n)
            .map(|i| ((i as f32 * 0.0137).sin() + (i as f32 * 0.073).cos()) * 0.4)
            .collect();
        let right: Vec<f32> = left.iter().rev().copied().collect();
        Arc::new(SampleBuffer {
            name: "s".into(),
            left,
            right,
            sample_rate: rate,
        })
    }

    fn time(t0: f64, frames: usize, tempo: f64) -> BlockTime {
        let tps = rlive_core::time::ticks_per_sample(tempo, SR);
        BlockTime {
            t0,
            ticks_per_sample: tps,
            ramp: 0.0,
            end: t0 + frames as f64 * tps,
            frames,
            sample_rate: SR,
            tempo,
        }
    }

    /// The warp-off read as the engine had it before stretchers existed, kept to pin it down.
    #[allow(clippy::too_many_arguments)]
    fn old_render(
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
        let ticks_per_src_frame =
            rlive_core::time::ticks_per_sample(time.tempo, sample.sample_rate);
        let local_a = a - origin;
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
                l[i] +=
                    (sample.left[idx] + (sample.left[idx + 1] - sample.left[idx]) * frac) * gain;
                r[i] +=
                    (sample.right[idx] + (sample.right[idx + 1] - sample.right[idx]) * frac) * gain;
            }
            audio.src += ratio;
        }
    }

    /// Plays `blocks` through both readers and asserts every sample matches bit for bit. Each
    /// block is `(frames, tempo, seek_to)`; `sub` shortens a segment so it starts and stops
    /// inside the block, as a launch does.
    fn assert_matches_old(clip: AudioClip, region_end: f64, blocks: &[(usize, f64, Option<f64>)]) {
        let max = blocks.iter().map(|b| b.0).max().unwrap_or(0).max(4096);
        let mut bank = StretchBank::new(max);
        bank.prepare(SR);
        let mut new_cursor = AudioCursor::new(0);
        new_cursor.enter(region_end);
        let mut old_cursor = AudioCursor::new(0);
        old_cursor.enter(region_end);

        let mut t = 0.0;
        for &(frames, tempo, seek) in blocks {
            if let Some(to) = seek {
                t = to;
            }
            let time = time(t, frames, tempo);
            let (a, seg_end) = (t, time.end);
            let mut new_l = vec![0.0f32; frames];
            let mut new_r = vec![0.0f32; frames];
            let mut old_l = vec![0.0f32; frames];
            let mut old_r = vec![0.0f32; frames];
            render_audio(
                &clip,
                None,
                0.0,
                a,
                seg_end,
                time,
                &mut new_cursor,
                &mut new_l,
                &mut new_r,
                &mut bank,
            );
            old_render(
                &clip,
                0.0,
                a,
                seg_end,
                time,
                &mut old_cursor,
                &mut old_l,
                &mut old_r,
            );
            let bits = |v: &[f32]| v.iter().map(|s| s.to_bits()).collect::<Vec<_>>();
            assert_eq!(bits(&new_l), bits(&old_l), "left at t={t}");
            assert_eq!(bits(&new_r), bits(&old_r), "right at t={t}");
            assert_eq!(new_cursor.src.to_bits(), old_cursor.src.to_bits());
            assert_eq!(new_cursor.fade_in, old_cursor.fade_in);
            t = seg_end;
        }
    }

    fn clip(rate: f64, length: u64, looped: bool, gain: f32) -> AudioClip {
        AudioClip {
            sample: sample(rate),
            length,
            looped,
            gain,
            scale: None,
        }
    }

    #[test]
    fn passthrough_matches_warp_off_at_the_native_rate() {
        assert_matches_old(
            clip(SR, 1920, true, 0.7),
            f64::INFINITY,
            &[(512, 120.0, None), (64, 120.0, None), (777, 120.0, None)],
        );
    }

    #[test]
    fn passthrough_matches_warp_off_when_resampling_and_looping() {
        for rate in [44_100.0, 96_000.0, 8_000.0, 12_345.6] {
            for tempo in [120.0, 97.3, 141.0] {
                let clip = clip(rate, 960, true, 0.35);
                let mut blocks = vec![(300, tempo, None); 40];
                blocks.push((512, tempo, Some(777.0)));
                assert_matches_old(clip, f64::INFINITY, &blocks);
            }
        }
    }

    #[test]
    fn passthrough_matches_warp_off_for_a_one_shot_running_out_and_tempo_changes() {
        for rate in [44_100.0, 22_050.0] {
            let clip = clip(rate, 1500, false, 0.5);
            let mut blocks: Vec<(usize, f64, Option<f64>)> = (0..80)
                .map(|i| (128 + i * 3, 90.0 + i as f64, None))
                .collect();
            blocks.insert(40, (256, 133.0, Some(50.0)));
            assert_matches_old(clip, f64::INFINITY, &blocks);
        }
    }

    #[test]
    fn passthrough_matches_warp_off_fading_towards_the_region_end() {
        let clip = clip(44_100.0, 8000, true, 0.9);
        assert_matches_old(clip, 2500.0, &vec![(256, 120.0, None); 30]);
    }

    #[test]
    fn a_warped_clip_seeks_through_its_map() {
        // Warp on, passthrough read: position after a jump comes from the markers.
        let audio = clip(SR, 3840, true, 1.0);
        let warped = WarpedClip {
            map: WarpMap::new([WarpMarker::new(0.0, 0.0), WarpMarker::new(24_000.0, 3840.0)]),
            mode: WarpMode::Complex,
            transpose: 0.0,
            slices: Arc::from(Vec::new()),
            audio,
        };
        let mut bank = StretchBank::new(256);
        bank.prepare(SR);
        let mut cursor = AudioCursor::new(0);
        let time = time(1920.0, 64, 120.0);
        let (mut l, mut r) = (vec![0.0; 64], vec![0.0; 64]);
        render_audio(
            &warped.audio,
            Some(&warped),
            0.0,
            1920.0,
            time.end,
            time,
            &mut cursor,
            &mut l,
            &mut r,
            &mut bank,
        );
        // Tick 1920 is frame 12_000 by the map (3840 ticks = 24 000 frames), then 64 frames on
        // at the native rate: the seam until a stretcher follows the span.
        assert_eq!(cursor.src, 12_000.0 + 64.0);
        assert!(l.iter().any(|s| *s != 0.0));
    }
}
