//! Clip data, stateless clip rendering and the per-track clip launcher.

mod audio;
mod data;
#[cfg(test)]
mod envelope_tests;
pub(crate) mod envelopes;
#[cfg(test)]
mod following_tests;
mod frozen;
mod launcher;
mod midi;

pub(crate) use audio::{AudioCursor, LANE_CROSSFADE_VOICE, LANE_VOICE};
pub use data::{
    AudioClip, ClipData, ExprPoint, FrozenClip, MidiClip, Note, NoteExpression, ProgramChange,
    WarpedClip,
};
pub use envelopes::{ClipCurve, ClipEnvelopes, EnvCurve, EnvTiming, EnvelopedClip};
pub use launcher::{velocity_gain, SlotLaunch};
pub(crate) use launcher::{
    ArmedFollow, ClipPlayer, ClipUpdate, LaunchEnv, Pending, Playing, Repeat,
};
pub(crate) use midi::{chase, Voices};

use audio::{render_audio_env, Quality};
use envelopes::render_controllers;
use frozen::render_frozen;
use midi::render_midi;
use rlive_core::time::TempoRun;
use rlive_core::warp::StretchBank;
use rlive_core::{ClipRegion, MidiEvent};

/// Block geometry shared by everything rendered in one block.
#[derive(Clone, Copy)]
pub(crate) struct BlockTime {
    pub t0: f64,
    pub ticks_per_sample: f64,
    /// How fast `ticks_per_sample` grows within the block, per tick. Zero when the tempo holds.
    pub ramp: f64,
    /// Tick at the end of the block. Exactly `loop_end` when the block runs up to a loop end,
    /// so float error can never pull a note on the boundary into the block.
    pub end: f64,
    pub frames: usize,
    pub sample_rate: f64,
    pub tempo: f64,
}

impl BlockTime {
    pub fn t1(&self) -> f64 {
        self.end
    }

    /// Frame offset of `tick` inside the block, clamped to `0..=frames`.
    pub fn offset(&self, tick: f64) -> usize {
        let off = self.run().frames_to(tick - self.t0).round();
        off.clamp(0.0, self.frames as f64) as usize
    }

    /// The song tick `offset` frames into the block.
    pub fn tick_at(&self, offset: usize) -> f64 {
        self.t0 + self.run().advance(offset as f64)
    }

    pub(crate) fn run(&self) -> TempoRun {
        TempoRun {
            ticks_per_sample: self.ticks_per_sample,
            ramp: self.ramp,
            until: f64::INFINITY,
        }
    }

    pub fn event_offset(&self, tick: f64) -> u32 {
        self.offset(tick).min(self.frames.saturating_sub(1)) as u32
    }
}

/// Render clip content for the song-tick range `[a, b)`. MIDI clips push events, audio clips
/// write into `l`/`r` (block-sized buffers). `origin` is the song tick the clip's run began on,
/// and `region` is how that run goes through its content.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_range(
    clip: &ClipData,
    region: ClipRegion,
    origin: f64,
    a: f64,
    b: f64,
    time: BlockTime,
    voices: &mut Voices,
    audio: &mut AudioCursor,
    ev: &mut Vec<MidiEvent>,
    l: &mut [f32],
    r: &mut [f32],
    stretch: &mut StretchBank,
) {
    let len = clip.length() as f64;
    if b <= a || len <= 0.0 {
        return;
    }
    let seg_end = if region.looped {
        b
    } else {
        b.min(origin + region.play_length())
    };
    if seg_end <= a {
        return;
    }
    let envelopes = clip.envelopes();
    let quality = Quality {
        hi_q: clip.hi_q(),
        edge_fade: clip.edge_fade(),
    };
    match clip.content() {
        ClipData::Midi(clip) => {
            render_midi(clip, region, origin, a, seg_end, time, voices, ev);
            if let Some(envelopes) = envelopes {
                render_controllers(envelopes, origin, a, seg_end, time, ev);
            }
        }
        ClipData::Audio(audio_clip) => render_audio_env(
            audio_clip, region, quality, None, envelopes, origin, a, seg_end, time, audio, l, r,
            stretch,
        ),
        ClipData::Warped(warped) => render_audio_env(
            &warped.audio,
            region,
            quality,
            Some(warped),
            envelopes,
            origin,
            a,
            seg_end,
            time,
            audio,
            l,
            r,
            stretch,
        ),
        ClipData::Frozen(clip) => render_frozen(
            clip, region, quality, origin, a, seg_end, time, audio, l, r, stretch,
        ),
        ClipData::Enveloped(_) => {}
    }
}

#[cfg(test)]
mod region_tests;
