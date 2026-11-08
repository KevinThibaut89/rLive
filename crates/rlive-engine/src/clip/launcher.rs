//! The per-track clip launcher.

use super::audio::AudioCursor;
use super::data::ClipData;
use super::midi::Voices;
use super::{render_range, BlockTime};
use rlive_core::MidiEvent;
use std::sync::Arc;

pub(crate) struct Playing {
    pub slot: usize,
    pub clip: Arc<ClipData>,
    /// Song tick at which clip position 0 happened.
    pub start_tick: f64,
    pub audio: AudioCursor,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Pending {
    Launch { slot: usize, at: f64 },
    Stop { at: f64 },
}

impl Pending {
    pub fn at(&self) -> f64 {
        match *self {
            Pending::Launch { at, .. } | Pending::Stop { at } => at,
        }
    }

    pub fn set_at(&mut self, tick: f64) {
        match self {
            Pending::Launch { at, .. } | Pending::Stop { at } => *at = tick,
        }
    }
}

/// Plays one clip at a time and handles quantized launches.
pub(crate) struct ClipPlayer {
    pub playing: Option<Playing>,
    pub pending: Option<Pending>,
    voices: Voices,
}

impl ClipPlayer {
    pub fn new() -> Self {
        Self {
            playing: None,
            pending: None,
            voices: Voices::new(),
        }
    }

    /// Emit note-offs for every sounding note at `offset`.
    pub fn release_all(&mut self, offset: u32, events: &mut Vec<MidiEvent>) {
        self.voices.release_all(offset, events);
    }

    /// Render one block: MIDI clips push events, audio clips write into `left`/`right`.
    /// Clips that stop playing are pushed to `retired` so they are freed off the audio thread.
    pub fn render(
        &mut self,
        time: BlockTime,
        events: &mut Vec<MidiEvent>,
        left: &mut [f32],
        right: &mut [f32],
        retired: &mut dyn FnMut(Arc<ClipData>),
        slots: &[Option<Arc<ClipData>>],
    ) {
        let t1 = time.t1();
        let mut seg_start = time.t0;

        if let Some(p) = self.pending {
            if p.at() < t1 {
                let at = p.at().max(time.t0);
                self.render_segment(time, seg_start, at, events, left, right, retired);
                let off = time.event_offset(at);
                self.release_all(off, events);
                if let Some(old) = self.playing.take() {
                    retired(old.clip);
                }
                if let Pending::Launch { slot, .. } = p {
                    if let Some(Some(clip)) = slots.get(slot) {
                        self.playing = Some(Playing {
                            slot,
                            clip: clip.clone(),
                            start_tick: at,
                            audio: AudioCursor::new(),
                        });
                    }
                }
                self.pending = None;
                seg_start = at;
            }
        }
        self.render_segment(time, seg_start, t1, events, left, right, retired);
    }

    #[allow(clippy::too_many_arguments)]
    fn render_segment(
        &mut self,
        time: BlockTime,
        a: f64,
        b: f64,
        events: &mut Vec<MidiEvent>,
        left: &mut [f32],
        right: &mut [f32],
        retired: &mut dyn FnMut(Arc<ClipData>),
    ) {
        if b <= a {
            return;
        }
        let Some(playing) = self.playing.as_mut() else {
            return;
        };
        let len = playing.clip.length() as f64;
        if len <= 0.0 {
            return;
        }
        render_range(
            &playing.clip,
            playing.start_tick,
            a,
            b,
            time,
            &mut self.voices,
            &mut playing.audio,
            events,
            left,
            right,
        );

        let end = playing.start_tick + len;
        if !playing.clip.looped() && b >= end {
            // A one-shot clip reached its end inside this segment.
            let off = time.event_offset(end);
            let old = self.playing.take();
            self.release_all(off, events);
            if let Some(old) = old {
                retired(old.clip);
            }
        }
    }

    /// Position of the playhead within the playing clip, in ticks.
    pub fn position(&self, tick: f64) -> f64 {
        match &self.playing {
            Some(p) => {
                let len = p.clip.length() as f64;
                if len <= 0.0 {
                    0.0
                } else {
                    (tick - p.start_tick).max(0.0) % len
                }
            }
            None => 0.0,
        }
    }
}
