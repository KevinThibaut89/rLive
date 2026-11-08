//! Arrangement lanes: clips placed on the song timeline, played stateless from the tick.

use crate::clip::{chase, render_range, AudioCursor, BlockTime, ClipData, Voices, LANE_VOICE};
use rlive_core::warp::StretchBank;
use rlive_core::{MidiEvent, NoteSpace};
use std::sync::Arc;

/// A clip placed on the timeline over `[start, end)`, entering its content at `offset`.
#[derive(PartialEq)]
pub struct LaneClip {
    pub start: u64,
    pub end: u64,
    pub offset: u64,
    pub data: Arc<ClipData>,
}

impl LaneClip {
    /// Song tick of clip-local 0.
    fn origin(&self) -> f64 {
        self.start as f64 - self.offset as f64
    }

    /// Song tick where the clip stops sounding: its end, or earlier if it is a one-shot whose
    /// content runs out first.
    fn stop(&self) -> f64 {
        let end = self.end as f64;
        if self.data.looped() {
            end
        } else {
            end.min(self.origin() + self.data.length() as f64)
        }
    }
}

/// One track's timeline. `clips` must be sorted by start and must not overlap.
#[derive(Default)]
pub struct ArrangementLane {
    pub clips: Vec<LaneClip>,
}

/// Plays a track's [`ArrangementLane`].
pub(crate) struct LanePlayer {
    lane: Arc<ArrangementLane>,
    /// Index of the first clip not yet entered.
    next: usize,
    current: Option<usize>,
    voices: Voices,
    audio: AudioCursor,
    /// The playhead moved: find the clip again and chase notes on the next render.
    resync: bool,
}

impl LanePlayer {
    pub fn new(lane: Arc<ArrangementLane>) -> Self {
        Self {
            lane,
            next: 0,
            current: None,
            voices: Voices::new(NoteSpace::Arrangement),
            audio: AudioCursor::new(LANE_VOICE),
            resync: true,
        }
    }

    /// The clip under the playhead, if any.
    pub fn current_clip(&self) -> Option<&Arc<ClipData>> {
        self.current
            .and_then(|i| self.lane.clips.get(i))
            .map(|c| &c.data)
    }

    /// Release everything sounding at `offset` and chase afresh on the next render.
    pub fn release_and_resync(&mut self, offset: u32, ev: &mut Vec<MidiEvent>) {
        self.voices.release_all(offset, ev);
        self.current = None;
        self.resync = true;
    }

    /// Swap in a new lane at song tick `tick` and return the old one, to be freed off the audio
    /// thread. A clip under the playhead that is unchanged keeps playing undisturbed.
    pub fn swap(
        &mut self,
        lane: Arc<ArrangementLane>,
        tick: f64,
        ev: &mut Vec<MidiEvent>,
    ) -> Arc<ArrangementLane> {
        let old = std::mem::replace(&mut self.lane, lane);
        let idx = self.lane.clips.partition_point(|c| c.end as f64 <= tick);
        let under_playhead = self.lane.clips.get(idx).filter(|c| c.start as f64 <= tick);
        let playing = self.current.and_then(|i| old.clips.get(i));
        match (playing, under_playhead) {
            (Some(a), Some(b)) if a == b => {
                self.current = Some(idx);
                self.next = idx + 1;
            }
            (None, None) => self.next = idx,
            _ => self.release_and_resync(0, ev),
        }
        old
    }

    /// Render the lane over the song-tick range `[a, b)`.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        time: BlockTime,
        a: f64,
        b: f64,
        ev: &mut Vec<MidiEvent>,
        l: &mut [f32],
        r: &mut [f32],
        stretch: &mut StretchBank,
    ) {
        if b <= a {
            return;
        }
        let Self {
            lane,
            next,
            current,
            voices,
            audio,
            resync,
        } = self;
        let clips = &lane.clips;
        if std::mem::take(resync) {
            *current = None;
            *next = clips.partition_point(|c| c.end as f64 <= a);
        }

        let mut t = a;
        loop {
            let idx = match *current {
                Some(i) => i,
                None => {
                    let Some(clip) = clips.get(*next) else {
                        return;
                    };
                    let start = clip.start as f64;
                    if start >= b {
                        return;
                    }
                    let idx = *next;
                    *next += 1;
                    let enter = t.max(start);
                    if clip.stop() <= enter {
                        continue;
                    }
                    let origin = clip.origin();
                    chase(
                        &clip.data,
                        origin,
                        enter,
                        time.event_offset(enter),
                        voices,
                        ev,
                    );
                    audio.enter(clip.end as f64 - origin);
                    *current = Some(idx);
                    t = enter;
                    idx
                }
            };
            let Some(clip) = clips.get(idx) else {
                *current = None;
                return;
            };
            let stop = clip.stop();
            render_range(
                &clip.data,
                clip.origin(),
                t,
                b.min(stop),
                time,
                voices,
                audio,
                ev,
                l,
                r,
                stretch,
            );
            // An end that rounds to the block's last frame boundary belongs to the next block,
            // which releases it at frame 0.
            if stop >= b || time.offset(stop) >= time.frames {
                return;
            }
            voices.release_all(time.event_offset(stop), ev);
            *current = None;
            t = stop;
        }
    }
}
