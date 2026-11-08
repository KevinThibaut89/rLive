//! Arrangement lanes: clips placed on the song timeline, played stateless from the tick.
//!
//! Clips sit end to end and never overlap on the timeline. A crossfade lets a clip keep sounding
//! past its end under the next one (its `tail`), and lets the next one start early under it (its
//! `lead`). Only neighbours can do this, so two voices are always enough: a clip that overlaps
//! the one before it takes the other voice, and keeps it for as long as it sounds.

use crate::clip::envelopes::enter_clip;
use crate::clip::{
    chase, render_range, AudioCursor, BlockTime, ClipData, Voices, LANE_CROSSFADE_VOICE, LANE_VOICE,
};
use rlive_core::fade::EdgeFades;
use rlive_core::warp::StretchBank;
use rlive_core::{MidiEvent, NoteSpace};
use std::sync::Arc;

/// The edges of a clip on the timeline, in ticks. The default is a plain clip with hard edges.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LaneFades {
    /// Length of the fade in, inside the clip's span.
    pub fade_in: u64,
    /// Length of the fade out, inside the clip's span.
    pub fade_out: u64,
    pub in_curve: f32,
    pub out_curve: f32,
    /// How long the clip sounds before `start`, under the clip ahead of it.
    pub lead: u64,
    /// How long the clip sounds after `end`, under the clip after it.
    pub tail: u64,
    /// Which of the lane's voices it plays on. The first voice plays everything that doesn't
    /// overlap, so a lane with no crossfades only ever uses that one.
    pub voice: u8,
}

/// A clip placed on the timeline over `[start, end)`, entering its content at `offset`.
#[derive(PartialEq)]
pub struct LaneClip {
    pub start: u64,
    pub end: u64,
    pub offset: u64,
    pub data: Arc<ClipData>,
    pub fades: LaneFades,
}

impl LaneClip {
    /// Song tick of clip-local 0.
    fn origin(&self) -> f64 {
        self.start as f64 - self.offset as f64
    }

    /// Song tick where the clip starts sounding.
    fn sound_start(&self) -> f64 {
        self.start.saturating_sub(self.fades.lead) as f64
    }

    /// The end of its span, including any crossfade under the next clip.
    fn sound_end(&self) -> f64 {
        (self.end + self.fades.tail) as f64
    }

    /// Song tick where the clip stops sounding: its end, or earlier if it is a one-shot whose
    /// content runs out first.
    fn stop(&self) -> f64 {
        let end = self.sound_end();
        if self.data.looped() {
            end
        } else {
            end.min(self.origin() + self.data.length() as f64)
        }
    }

    /// Clip-local positions of the fades.
    fn edge_fades(&self) -> EdgeFades {
        let f = self.fades;
        let origin = self.origin();
        let local = |tick: f64| tick - origin;
        if f.fade_in == 0 && f.fade_out == 0 && f.lead == 0 && f.tail == 0 {
            return EdgeFades::default();
        }
        EdgeFades {
            in_from: local(self.sound_start()),
            in_to: local((self.start + f.fade_in) as f64),
            in_curve: f.in_curve,
            out_from: local((self.end - f.fade_out.min(self.end)) as f64),
            out_to: local((self.end + f.tail) as f64),
            out_curve: f.out_curve,
        }
    }

    /// The same content at the same place, whatever its fades.
    fn same_content(&self, other: &LaneClip) -> bool {
        self.start == other.start
            && self.end == other.end
            && self.offset == other.offset
            && self.data == other.data
    }
}

/// One track's timeline. `clips` must be sorted by start. They may only overlap through the
/// `lead` and `tail` of a crossfade between neighbours.
#[derive(Default)]
pub struct ArrangementLane {
    pub clips: Vec<LaneClip>,
}

/// One of the lane's two voices, walking the clips assigned to it.
struct Slot {
    voice: u8,
    /// Index of the first clip not yet entered.
    next: usize,
    current: Option<usize>,
    audio: AudioCursor,
}

impl Slot {
    fn new(voice: u8, stretch_voice: usize) -> Self {
        Self {
            voice,
            next: 0,
            current: None,
            audio: AudioCursor::new(stretch_voice),
        }
    }
}

/// Plays a track's [`ArrangementLane`].
pub(crate) struct LanePlayer {
    lane: Arc<ArrangementLane>,
    slots: [Slot; 2],
    voices: Voices,
    /// The playhead moved: find the clip again and chase notes on the next render.
    resync: bool,
    /// MIDI Envelope Auto-Reset: each new clip sets controllers back first.
    pub(crate) auto_reset: bool,
    /// Whether a clip entered in the middle of a note plays that note.
    chase: bool,
}

impl LanePlayer {
    pub fn new(lane: Arc<ArrangementLane>) -> Self {
        Self {
            lane,
            slots: [Slot::new(0, LANE_VOICE), Slot::new(1, LANE_CROSSFADE_VOICE)],
            voices: Voices::new(NoteSpace::Arrangement),
            resync: true,
            auto_reset: false,
            chase: true,
        }
    }

    /// The clip placed over song tick `tick`, with what's needed to find it again: its start,
    /// and the tick of its local 0.
    pub fn clip_at(&self, tick: f64) -> Option<(u64, f64)> {
        let clips = &self.lane.clips;
        let c = clips.get(clips.partition_point(|c| c.end as f64 <= tick))?;
        (c.start as f64 <= tick).then(|| (c.start, c.origin()))
    }

    pub fn set_chase(&mut self, on: bool) {
        self.chase = on;
    }

    /// The clip under the playhead, if any.
    pub fn current_clip(&self) -> Option<&Arc<ClipData>> {
        self.slots
            .iter()
            .find_map(|s| s.current)
            .and_then(|i| self.lane.clips.get(i))
            .map(|c| &c.data)
    }

    /// Release everything sounding at `offset` and chase afresh on the next render.
    pub fn release_and_resync(&mut self, offset: u32, ev: &mut Vec<MidiEvent>) {
        self.voices.release_all(offset, ev);
        for slot in &mut self.slots {
            slot.current = None;
        }
        self.resync = true;
    }

    /// Swap in a new lane at song tick `tick` and return the old one, to be freed off the audio
    /// thread. A clip under the playhead that is unchanged keeps playing undisturbed, and takes
    /// up any change to its fades.
    pub fn swap(
        &mut self,
        lane: Arc<ArrangementLane>,
        tick: f64,
        ev: &mut Vec<MidiEvent>,
    ) -> Arc<ArrangementLane> {
        let old = std::mem::replace(&mut self.lane, lane);
        let clips = &self.lane.clips;
        let idx = clips.partition_point(|c| c.sound_end() <= tick);
        let mut in_step = true;
        for slot in &mut self.slots {
            // Only neighbours overlap, so this slot's clip can only be one of these two.
            let under_playhead = [idx, idx + 1].into_iter().find_map(|i| {
                clips
                    .get(i)
                    .filter(|c| c.fades.voice == slot.voice && c.sound_start() <= tick)
                    .map(|c| (i, c))
            });
            let playing = slot.current.and_then(|i| old.clips.get(i));
            match (playing, under_playhead) {
                (Some(a), Some((i, b))) if a.same_content(b) => {
                    slot.current = Some(i);
                    slot.next = i + 1;
                    slot.audio.fades = b.edge_fades();
                    slot.audio.region_end = b.sound_end() - b.origin();
                }
                (None, None) => slot.next = idx,
                _ => in_step = false,
            }
        }
        if !in_step {
            self.release_and_resync(0, ev);
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
            slots,
            voices,
            resync,
            auto_reset,
            chase,
        } = self;
        let clips = &lane.clips;
        let seek = std::mem::take(resync);
        for slot in slots {
            render_slot(
                slot,
                clips,
                seek,
                *chase,
                *auto_reset,
                time,
                a,
                b,
                voices,
                ev,
                l,
                r,
                stretch,
            );
        }
    }
}

/// Renders the clips of one voice over `[a, b)`. They never overlap each other.
#[allow(clippy::too_many_arguments)]
fn render_slot(
    slot: &mut Slot,
    clips: &[LaneClip],
    seek: bool,
    chase_notes: bool,
    auto_reset: bool,
    time: BlockTime,
    a: f64,
    b: f64,
    voices: &mut Voices,
    ev: &mut Vec<MidiEvent>,
    l: &mut [f32],
    r: &mut [f32],
    stretch: &mut StretchBank,
) {
    let Slot {
        voice,
        next,
        current,
        audio,
    } = slot;
    let first_voice = *voice == 0;
    if seek {
        *current = None;
        *next = clips.partition_point(|c| c.sound_end() <= a);
    }

    let mut t = a;
    loop {
        let idx = match *current {
            Some(i) => i,
            None => {
                while clips.get(*next).is_some_and(|c| c.fades.voice != *voice) {
                    *next += 1;
                }
                let Some(clip) = clips.get(*next) else {
                    return;
                };
                let start = clip.sound_start();
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
                if chase_notes && first_voice {
                    chase(
                        &clip.data,
                        origin,
                        enter,
                        time.event_offset(enter),
                        voices,
                        ev,
                    );
                }
                enter_clip(
                    &clip.data,
                    enter - origin,
                    time.event_offset(enter),
                    auto_reset,
                    ev,
                );
                audio.enter(clip.sound_end() - origin);
                audio.fades = clip.edge_fades();
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
        // Only the first voice ever plays MIDI, so the other mustn't release what it holds.
        if first_voice {
            voices.release_all(time.event_offset(stop), ev);
        }
        *current = None;
        t = stop;
    }
}
