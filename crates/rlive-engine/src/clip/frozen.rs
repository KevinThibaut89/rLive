//! Playing a frozen clip.
//!
//! Freezing writes what a track sounded like to a file, frame for frame. Played back at the same
//! tempo and sample rate, the file is read straight off, so a frozen track sounds exactly as it
//! did (manual p. 992). Played anywhere else it goes through the warp engine like any audio.

use super::audio::{render_audio_env, AudioCursor, Quality};
use super::data::FrozenClip;
use super::BlockTime;
use rlive_core::warp::StretchBank;
use rlive_core::ClipRegion;

/// How far from one source frame per output frame still counts as the frozen tempo.
const UNITY: f64 = 1e-4;

/// The source frames one output frame moves through, at `local` in the clip.
fn rate_at(clip: &FrozenClip, local: f64, ticks: f64) -> Option<f64> {
    let map = clip.warped.map.as_ref()?;
    Some(map.tick_to_src(local + ticks) - map.tick_to_src(local))
}

/// Whether this block can be copied straight off the file.
fn plays_neutrally(clip: &FrozenClip, local: f64, time: &BlockTime) -> bool {
    let sample = &clip.warped.audio.sample;
    sample.sample_rate == time.sample_rate
        && time.ramp == 0.0
        && clip
            .warped
            .map
            .as_ref()
            .is_some_and(|m| m.markers().len() == 2)
        && rate_at(clip, local, time.ticks_per_sample).is_some_and(|r| (r - 1.0).abs() < UNITY)
}

/// Renders `[a, seg_end)` of a frozen clip into `l` and `r`.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_frozen(
    clip: &FrozenClip,
    region: ClipRegion,
    quality: Quality,
    origin: f64,
    a: f64,
    seg_end: f64,
    time: BlockTime,
    audio: &mut AudioCursor,
    l: &mut [f32],
    r: &mut [f32],
    stretch: &mut StretchBank,
) {
    let audio_clip = &clip.warped.audio;
    if !plays_neutrally(clip, (a - origin).max(0.0), &time) {
        render_audio_env(
            audio_clip,
            region,
            quality,
            Some(&clip.warped),
            None,
            origin,
            a,
            seg_end,
            time,
            audio,
            l,
            r,
            stretch,
        );
        return;
    }
    let (Some(map), sample) = (clip.warped.map.as_ref(), &audio_clip.sample) else {
        return;
    };
    // The warp path takes over from here if the tempo moves, and has to find its place then.
    audio.needs_seek = true;
    audio.next_local = seg_end - origin;
    let from = time.offset(a);
    let to = time.offset(seg_end).min(l.len()).min(r.len());
    for i in from..to {
        let local = if i == from {
            a - origin
        } else {
            time.tick_at(i) - origin
        };
        // A launch lands exactly on the clip's start, give or take rounding.
        let position = region.content_at(local.max(0.0));
        if position >= region.content_end() {
            continue;
        }
        let frame = map.tick_to_src(position).round();
        if frame < 0.0 {
            continue;
        }
        let frame = frame as usize;
        if let (Some(&sl), Some(&sr)) = (sample.left.get(frame), sample.right.get(frame)) {
            l[i] += sl;
            r[i] += sr;
        }
    }
}
