//! The Arrangement: clips placed on a linear timeline, one lane per track.

use crate::defaults::is_default;
use crate::{Breakpoint, Clip, Project, Track};
use rlive_core::{ArrClipId, TimeSignature, TrackId, PPQ};
use serde::{Deserialize, Serialize};

/// A clip placed on a track's timeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArrangementClip {
    pub id: ArrClipId,
    /// Timeline position in ticks.
    pub start: u64,
    /// Length on the timeline in ticks; may exceed `clip.length` when looped.
    pub length: u64,
    /// Position inside the clip's content that plays at `start`.
    pub offset: u64,
    pub clip: Clip,
}

impl ArrangementClip {
    /// Places all of `clip` at `start`.
    pub fn new(clip: Clip, start: u64) -> Self {
        Self {
            id: ArrClipId::new(),
            start,
            length: clip.length.max(1),
            offset: 0,
            clip,
        }
    }

    pub fn end(&self) -> u64 {
        self.start + self.length
    }

    /// Moves the left edge to `to`, keeping the audible content where it is.
    pub fn trim_left(&mut self, to: u64) {
        let to = to.min(self.end().saturating_sub(1));
        if to >= self.start {
            let d = to - self.start;
            self.start = to;
            self.length -= d;
            self.offset += d;
            return;
        }
        let mut d = self.start - to;
        if self.clip.looped && self.clip.length > 0 {
            // Looped content extends indefinitely in both directions.
            self.offset +=
                d.saturating_sub(self.offset).div_ceil(self.clip.length) * self.clip.length;
        } else {
            d = d.min(self.offset);
        }
        self.start -= d;
        self.length += d;
        self.offset -= d;
    }

    /// Moves the right edge to `to`. Unlooped clips stop at the end of their content.
    pub fn trim_right(&mut self, to: u64) {
        let mut length = to.saturating_sub(self.start).max(1);
        if !self.clip.looped {
            length = length.min(self.clip.length.saturating_sub(self.offset).max(1));
        }
        self.length = length;
    }
}

/// A named position on the timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Locator {
    pub tick: u64,
    pub name: String,
    /// The user's info text.
    #[serde(default, skip_serializing_if = "is_default")]
    pub info: String,
}

/// A change of time signature on the timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignatureMarker {
    pub tick: u64,
    pub signature: TimeSignature,
}

/// The Arrangement's tempo envelope, in BPM.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TempoAutomation {
    pub points: Vec<Breakpoint>,
    /// The range the envelope is drawn in, which also bounds a mapped MIDI control.
    pub min: f64,
    pub max: f64,
}

impl Default for TempoAutomation {
    fn default() -> Self {
        Self {
            points: Vec::new(),
            min: 60.0,
            max: 200.0,
        }
    }
}

/// A lane of alternate takes under a track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TakeLane {
    pub name: String,
    #[serde(default, skip_serializing_if = "is_default")]
    pub color: Option<[u8; 3]>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub clips: Vec<ArrangementClip>,
}

/// Timeline settings shared by all tracks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Arrangement {
    pub loop_enabled: bool,
    pub loop_start: u64,
    pub loop_end: u64,
    #[serde(default, skip_serializing_if = "is_default")]
    pub punch_in: bool,
    #[serde(default, skip_serializing_if = "is_default")]
    pub punch_out: bool,
    /// Where playback starts when none has been set by the insert marker.
    #[serde(default, skip_serializing_if = "is_default")]
    pub song_start: Option<u64>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub locators: Vec<Locator>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub signatures: Vec<SignatureMarker>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub tempo: TempoAutomation,
    /// Groups of tracks whose clips edit together. A track is in at most one.
    #[serde(default, skip_serializing_if = "is_default")]
    pub linked_tracks: Vec<Vec<TrackId>>,
}

impl Default for Arrangement {
    fn default() -> Self {
        Self {
            loop_enabled: false,
            loop_start: 0,
            loop_end: 16 * PPQ,
            punch_in: false,
            punch_out: false,
            song_start: None,
            locators: Vec::new(),
            signatures: Vec::new(),
            tempo: TempoAutomation::default(),
            linked_tracks: Vec::new(),
        }
    }
}

impl Track {
    /// Adds a clip to the lane, overwriting whatever it overlaps: neighbours
    /// are trimmed, split around it, or removed.
    pub fn place_clip(&mut self, new: ArrangementClip) {
        let (start, end) = (new.start, new.end());
        let mut lane = Vec::with_capacity(self.arrangement.len() + 2);
        for c in self.arrangement.drain(..) {
            if c.end() <= start || c.start >= end {
                lane.push(c);
                continue;
            }
            if c.start < start {
                let mut left = c.clone();
                left.trim_right(start);
                lane.push(left);
            }
            if c.end() > end {
                let mut right = c;
                if right.start < start {
                    right.id = ArrClipId::new();
                }
                right.trim_left(end);
                lane.push(right);
            }
        }
        lane.push(new);
        lane.sort_by_key(|c| c.start);
        self.arrangement = lane;
    }

    pub fn arr_clip(&self, id: ArrClipId) -> Option<&ArrangementClip> {
        self.arrangement.iter().find(|c| c.id == id)
    }

    pub fn arr_clip_mut(&mut self, id: ArrClipId) -> Option<&mut ArrangementClip> {
        self.arrangement.iter_mut().find(|c| c.id == id)
    }

    pub fn remove_arr_clip(&mut self, id: ArrClipId) -> Option<ArrangementClip> {
        let i = self.arrangement.iter().position(|c| c.id == id)?;
        Some(self.arrangement.remove(i))
    }
}

impl Project {
    /// Where the last clip on any lane ends.
    pub fn arrangement_end(&self) -> u64 {
        self.tracks
            .iter()
            .filter_map(|t| t.arrangement.last())
            .map(|c| c.end())
            .max()
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TrackKind, PALETTE};
    use rlive_core::TimeSignature;

    fn clip(looped: bool) -> Clip {
        let mut c = Clip::new_midi("c", PALETTE[0], 1, TimeSignature::default());
        c.looped = looped;
        c
    }

    fn placed(start: u64, length: u64) -> ArrangementClip {
        ArrangementClip {
            length,
            ..ArrangementClip::new(clip(true), start)
        }
    }

    fn track() -> Track {
        Track::new("t", TrackKind::Midi, PALETTE[0], 1)
    }

    fn spans(t: &Track) -> Vec<(u64, u64)> {
        t.arrangement.iter().map(|c| (c.start, c.end())).collect()
    }

    #[test]
    fn place_trims_a_partial_overlap() {
        let mut t = track();
        t.place_clip(placed(0, 100));
        t.place_clip(placed(50, 100));
        assert_eq!(spans(&t), vec![(0, 50), (50, 150)]);
    }

    #[test]
    fn place_in_the_middle_splits_with_a_fresh_id_and_offset() {
        let mut t = track();
        let original = placed(0, 300);
        let id = original.id;
        t.place_clip(original);
        t.place_clip(placed(100, 100));
        assert_eq!(spans(&t), vec![(0, 100), (100, 200), (200, 300)]);
        assert_eq!(t.arrangement[0].id, id);
        assert_ne!(t.arrangement[2].id, id);
        assert_eq!(t.arrangement[2].offset, 200);
    }

    #[test]
    fn place_removes_a_fully_covered_clip() {
        let mut t = track();
        t.place_clip(placed(10, 10));
        t.place_clip(placed(0, 100));
        assert_eq!(spans(&t), vec![(0, 100)]);
    }

    #[test]
    fn left_trim_keeps_content_in_place() {
        let mut c = placed(100, 100);
        c.trim_left(150);
        assert_eq!((c.start, c.length, c.offset), (150, 50, 50));
        c.trim_left(120);
        assert_eq!((c.start, c.length, c.offset), (120, 80, 20));
    }

    #[test]
    fn unlooped_clips_cannot_extend_past_their_content() {
        let mut c = ArrangementClip::new(clip(false), 100);
        let len = c.clip.length;
        c.trim_right(100 + len * 3);
        assert_eq!(c.length, len);
        c.trim_left(0);
        assert_eq!(c.start, 100);
    }

    #[test]
    fn looped_clips_extend_left_past_zero_offset() {
        let mut c = ArrangementClip::new(clip(true), 10_000);
        let len = c.clip.length;
        c.trim_left(10_000 - 100);
        assert_eq!(c.start, 10_000 - 100);
        assert_eq!(c.length, len + 100);
        assert_eq!(c.offset, len - 100);
    }
}
