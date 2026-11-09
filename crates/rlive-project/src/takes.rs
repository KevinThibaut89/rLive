//! Take lanes and comping, without reference to the UI or the engine (manual ch. 21), and linked
//! tracks (p. 183).
//!
//! A lane is named by a `Option<usize>`: `None` is the main lane and `Some(i)` take lane `i`.

use crate::arrangement::place_on;
use crate::arrangement_edit::{delete_lane_span, split_lane};
use crate::{ArrangementClip, Project, TakeLane, Track};
use rlive_core::{ArrClipId, TrackId};

/// A stretch of take lane that a clip on the main lane was copied from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompSource {
    pub lane: usize,
    pub start: u64,
    pub end: u64,
    /// The main lane's clip.
    pub main: ArrClipId,
}

/// Which way Next and Previous Take go through the take lanes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TakeStep {
    /// The lane above.
    Next,
    /// The lane below.
    Previous,
}

/// Whether a clip on the main lane is the same material as a take lane clip: same content, same
/// properties and the same place in the file.
fn same_source(main: &ArrangementClip, take: &ArrangementClip) -> bool {
    main.offset as i64 - main.start as i64 == take.offset as i64 - take.start as i64
        && main.clip.content == take.clip.content
        && main.clip.envelopes == take.clip.envelopes
        && main.clip.looped == take.clip.looped
}

/// The clip's part inside `[start, end)`, with no fade left on an edge that was cut. Stays at its
/// place on the timeline.
fn cut_to(c: &ArrangementClip, start: u64, end: u64) -> Option<ArrangementClip> {
    let mut piece = c.clipped_to(start, end, 0)?;
    if c.start < start {
        piece.clip.fades.fade_in = 0;
        piece.clip.fades.in_tail = 0;
    }
    if c.end() > end {
        piece.clip.fades.fade_out = 0;
        piece.clip.fades.out_tail = 0;
    }
    Some(piece)
}

impl Track {
    /// The clips of a lane.
    pub fn lane(&self, lane: Option<usize>) -> Option<&Vec<ArrangementClip>> {
        match lane {
            None => Some(&self.arrangement),
            Some(i) => self.take_lanes.get(i).map(|l| &l.clips),
        }
    }

    /// The clips of a lane.
    pub fn lane_mut(&mut self, lane: Option<usize>) -> Option<&mut Vec<ArrangementClip>> {
        match lane {
            None => Some(&mut self.arrangement),
            Some(i) => self.take_lanes.get_mut(i).map(|l| &mut l.clips),
        }
    }

    /// The main lane and each take lane, for an edit of the whole timeline.
    pub fn lanes_mut(&mut self) -> impl Iterator<Item = &mut Vec<ArrangementClip>> {
        std::iter::once(&mut self.arrangement)
            .chain(self.take_lanes.iter_mut().map(|l| &mut l.clips))
    }

    /// Which lane a clip is on.
    pub fn lane_of(&self, id: ArrClipId) -> Option<Option<usize>> {
        if self.arrangement.iter().any(|c| c.id == id) {
            return Some(None);
        }
        self.take_lanes
            .iter()
            .position(|l| l.clips.iter().any(|c| c.id == id))
            .map(Some)
    }

    /// Adds a clip to a lane, overwriting what it overlaps there.
    pub fn place_on_lane(&mut self, lane: Option<usize>, new: ArrangementClip) -> bool {
        match self.lane_mut(lane) {
            Some(clips) => {
                place_on(clips, new);
                true
            }
            None => false,
        }
    }

    /// Splits a lane at `at`, returning the new second half.
    pub fn split_lane_at(&mut self, lane: Option<usize>, at: u64) -> Option<ArrClipId> {
        split_lane(self.lane_mut(lane)?, at)
    }

    /// Removes a lane's material in `[start, end)`, leaving a gap.
    pub fn delete_lane_span(&mut self, lane: Option<usize>, start: u64, end: u64) {
        if let Some(clips) = self.lane_mut(lane) {
            delete_lane_span(clips, start, end);
        }
    }

    /// Whether anything of this kind of track can have take lanes.
    pub fn can_have_take_lanes(&self) -> bool {
        matches!(self.kind, crate::TrackKind::Audio | crate::TrackKind::Midi)
    }

    /// The take lane being auditioned, if it can be heard: not on a frozen track, which is
    /// playing its frozen material whatever the lanes hold.
    pub fn auditioned_lane(&self) -> Option<usize> {
        self.ui
            .audition_lane
            .filter(|i| *i < self.take_lanes.len() && !self.is_frozen())
    }

    /// What the track's timeline plays: the main lane, or the take lane being auditioned.
    pub fn playing_lane(&self) -> &[ArrangementClip] {
        match self.auditioned_lane() {
            Some(i) => &self.take_lanes[i].clips,
            None => &self.arrangement,
        }
    }

    // ------------------------------------------------------------- managing lanes

    /// A name for a new lane that no other has.
    fn fresh_take_name(&self) -> String {
        let mut n = self.take_lanes.len() + 1;
        loop {
            let name = format!("Take {n}");
            if self.take_lanes.iter().all(|l| l.name != name) {
                return name;
            }
            n += 1;
        }
    }

    /// Adds an empty take lane after `after`, or at the end. Returns its index.
    pub fn insert_take_lane(&mut self, after: Option<usize>) -> usize {
        let at = after.map_or(self.take_lanes.len(), |a| {
            (a + 1).min(self.take_lanes.len())
        });
        let lane = TakeLane {
            name: self.fresh_take_name(),
            color: None,
            clips: Vec::new(),
        };
        self.take_lanes.insert(at, lane);
        self.shift_audition(at, true);
        at
    }

    /// Copies a take lane to the one below it, with fresh clip ids.
    pub fn duplicate_take_lane(&mut self, i: usize) -> Option<usize> {
        let mut lane = self.take_lanes.get(i)?.clone();
        lane.name = format!("{} Copy", lane.name);
        for c in &mut lane.clips {
            c.id = ArrClipId::new();
        }
        self.take_lanes.insert(i + 1, lane);
        self.shift_audition(i + 1, true);
        if let Some(h) = self.ui.take_lane_heights.get(i).copied() {
            self.ui.take_lane_heights.insert(i + 1, h);
        }
        Some(i + 1)
    }

    /// Removes a take lane.
    pub fn delete_take_lane(&mut self, i: usize) -> bool {
        if i >= self.take_lanes.len() {
            return false;
        }
        self.take_lanes.remove(i);
        self.shift_audition(i, false);
        if i < self.ui.take_lane_heights.len() {
            self.ui.take_lane_heights.remove(i);
        }
        true
    }

    /// Removes every take lane.
    pub fn delete_all_take_lanes(&mut self) -> bool {
        let any = !self.take_lanes.is_empty();
        self.take_lanes.clear();
        self.ui.audition_lane = None;
        self.ui.take_lane_heights.clear();
        any
    }

    /// Removes the take lanes that have nothing in a comp on the main lane.
    pub fn delete_unused_take_lanes(&mut self) -> usize {
        let used: std::collections::HashSet<usize> =
            self.comp_sources().iter().map(|s| s.lane).collect();
        let mut removed = 0;
        for i in (0..self.take_lanes.len()).rev() {
            if !used.contains(&i) && self.delete_take_lane(i) {
                removed += 1;
            }
        }
        removed
    }

    /// Moves a take lane within the track, keeping the audition on the same lane.
    pub fn move_take_lane(&mut self, from: usize, to: usize) -> bool {
        if from >= self.take_lanes.len() || to >= self.take_lanes.len() || from == to {
            return false;
        }
        let lane = self.take_lanes.remove(from);
        self.take_lanes.insert(to, lane);
        if !self.ui.take_lane_heights.is_empty() {
            let last = from.max(to);
            if self.ui.take_lane_heights.len() <= last {
                self.ui.take_lane_heights.resize(last + 1, 0.0);
            }
            let h = self.ui.take_lane_heights.remove(from);
            self.ui.take_lane_heights.insert(to, h);
        }
        if let Some(a) = self.ui.audition_lane {
            self.ui.audition_lane = Some(if a == from {
                to
            } else if from < a && a <= to {
                a - 1
            } else if to <= a && a < from {
                a + 1
            } else {
                a
            });
        }
        true
    }

    /// What deleting lane `i` or inserting one there does to the audition.
    fn shift_audition(&mut self, i: usize, inserted: bool) {
        self.ui.audition_lane = match self.ui.audition_lane {
            Some(a) if inserted && a >= i => Some(a + 1),
            Some(a) if !inserted && a == i => None,
            Some(a) if !inserted && a > i => Some(a - 1),
            other => other,
        };
        let heights = &mut self.ui.take_lane_heights;
        if inserted && i < heights.len() {
            heights.insert(i, 0.0);
        }
    }

    // ------------------------------------------------------------- recording

    /// The take lane a recording that starts at `from` goes on: the first with nothing after
    /// that point, or a new one (manual p. 433).
    pub fn lane_for_take(&mut self, from: u64) -> usize {
        match self
            .take_lanes
            .iter()
            .position(|l| l.clips.iter().all(|c| c.end() <= from))
        {
            Some(i) => i,
            None => self.insert_take_lane(None),
        }
    }

    /// Puts a recorded clip on a take lane and an independent copy of it on the main lane, so
    /// the last recorded clip is always what plays. Returns the lane and the main lane's copy.
    pub fn record_take(&mut self, take: ArrangementClip) -> (usize, ArrClipId) {
        let i = self.lane_for_take(take.start);
        let mut copy = take.clone();
        copy.id = ArrClipId::new();
        let id = copy.id;
        place_on(&mut self.take_lanes[i].clips, take);
        self.place_clip(copy);
        self.settle_fades();
        (i, id)
    }

    // ------------------------------------------------------------- comping

    /// Copies what a take lane has in `[start, end)` to the main lane, over what's there. The
    /// copies are clips of their own. Returns them.
    pub fn comp_from_lane(&mut self, lane: usize, start: u64, end: u64) -> Vec<ArrClipId> {
        if end <= start {
            return Vec::new();
        }
        let Some(pieces) = self.take_lanes.get(lane).map(|l| {
            l.clips
                .iter()
                .filter_map(|c| cut_to(c, start, end))
                .collect::<Vec<_>>()
        }) else {
            return Vec::new();
        };
        let mut placed = Vec::with_capacity(pieces.len());
        for mut piece in pieces {
            piece.id = ArrClipId::new();
            placed.push(piece.id);
            self.place_clip(piece);
        }
        self.settle_fades();
        placed
    }

    /// Whether a take lane has anything in `[start, end)`.
    pub fn lane_has_material(&self, lane: usize, start: u64, end: u64) -> bool {
        self.take_lanes
            .get(lane)
            .is_some_and(|l| l.clips.iter().any(|c| c.start < end && c.end() > start))
    }

    /// The take lane that the main lane's clip at `[start, end)` came from.
    pub fn source_lane(&self, start: u64, end: u64) -> Option<usize> {
        let main = self
            .arrangement
            .iter()
            .find(|c| c.start < end && c.end() > start)?;
        self.comp_sources()
            .into_iter()
            .find(|s| s.main == main.id && s.start < end && s.end > start)
            .map(|s| s.lane)
    }

    /// Where the next or previous take is from `from`, which is a take lane or the main lane's
    /// source for the span. Empty lanes don't count.
    pub fn stepped_lane(
        &self,
        from: Option<usize>,
        start: u64,
        end: u64,
        step: TakeStep,
    ) -> Option<usize> {
        let n = self.take_lanes.len();
        let has = |i: usize| self.lane_has_material(i, start, end);
        let from = from.or_else(|| self.source_lane(start, end));
        match (step, from) {
            (TakeStep::Next, Some(f)) => (0..f.min(n)).rev().find(|i| has(*i)),
            (TakeStep::Next, None) => (0..n).rev().find(|i| has(*i)),
            (TakeStep::Previous, Some(f)) => (f + 1..n).find(|i| has(*i)),
            (TakeStep::Previous, None) => (0..n).find(|i| has(*i)),
        }
    }

    /// Replaces the main lane's material in `[start, end)` with the next or previous take.
    /// Returns the lane it now comes from.
    pub fn step_take(
        &mut self,
        from: Option<usize>,
        start: u64,
        end: u64,
        step: TakeStep,
    ) -> Option<usize> {
        let lane = self.stepped_lane(from, start, end, step)?;
        self.delete_span(start, end);
        self.comp_from_lane(lane, start, end);
        Some(lane)
    }

    /// Every stretch of a take lane that is in a comp on the main lane. Only material that
    /// matches in position and properties counts (manual p. 436).
    pub fn comp_sources(&self) -> Vec<CompSource> {
        let mut out = Vec::new();
        for main in &self.arrangement {
            'lanes: for (lane, l) in self.take_lanes.iter().enumerate() {
                for take in &l.clips {
                    let (start, end) = (main.start.max(take.start), main.end().min(take.end()));
                    if start < end && same_source(main, take) {
                        out.push(CompSource {
                            lane,
                            start,
                            end,
                            main: main.id,
                        });
                        break 'lanes;
                    }
                }
            }
        }
        out
    }

    /// Moves the boundary between two touching clips on the main lane to `to`, as far as each
    /// has take material to show. Returns where it went.
    pub fn move_comp_split(&mut self, left: ArrClipId, right: ArrClipId, to: u64) -> Option<u64> {
        let sources = self.comp_sources();
        let extent = |id: ArrClipId| -> Option<(u64, u64)> {
            let s = sources.iter().find(|s| s.main == id)?;
            let take = self
                .take_lanes
                .get(s.lane)?
                .clips
                .iter()
                .find(|c| c.start < s.end && c.end() > s.start)?;
            Some((take.start, take.end()))
        };
        let (l_bound, r_bound) = (extent(left)?, extent(right)?);
        let (l, r) = (self.arr_clip(left)?, self.arr_clip(right)?);
        if l.end() != r.start {
            return None;
        }
        let low = (l.start + 1).max(r_bound.0);
        let high = (r.end().saturating_sub(1)).min(l_bound.1);
        if low > high {
            return None;
        }
        let at = to.clamp(low, high);
        if at == l.end() {
            return Some(at);
        }
        let main = self.arr_clip_mut(left)?;
        main.trim_right(at);
        let main = self.arr_clip_mut(right)?;
        main.trim_left(at);
        self.settle_fades();
        Some(at)
    }
}

impl Project {
    /// The instance of linked tracks `track` belongs to, itself included.
    pub fn linked_group(&self, track: TrackId) -> Option<&Vec<TrackId>> {
        self.arrangement
            .linked_tracks
            .iter()
            .find(|g| g.contains(&track))
    }

    /// `track` and whatever is linked with it.
    pub fn linked_with(&self, track: TrackId) -> Vec<TrackId> {
        self.linked_group(track)
            .cloned()
            .unwrap_or_else(|| vec![track])
    }

    /// Links these tracks as one instance, taking them out of any they were in. A track is in at
    /// most one instance, and an instance of fewer than two means nothing.
    pub fn link_tracks(&mut self, tracks: &[TrackId]) -> bool {
        let linkable: Vec<TrackId> = tracks
            .iter()
            .filter(|t| self.track(**t).is_some_and(Track::can_have_take_lanes))
            .copied()
            .collect();
        if linkable.len() < 2 {
            return false;
        }
        self.unlink_tracks(&linkable);
        self.arrangement.linked_tracks.push(linkable);
        true
    }

    /// Adds tracks to the instance `to` is in.
    pub fn add_to_link(&mut self, tracks: &[TrackId], to: TrackId) -> bool {
        let Some(mut group) = self.linked_group(to).cloned() else {
            let mut all = vec![to];
            all.extend_from_slice(tracks);
            return self.link_tracks(&all);
        };
        let new: Vec<TrackId> = tracks
            .iter()
            .filter(|t| {
                !group.contains(*t) && self.track(**t).is_some_and(Track::can_have_take_lanes)
            })
            .copied()
            .collect();
        if new.is_empty() {
            return false;
        }
        self.unlink_tracks(&new);
        group = self.linked_group(to).cloned().unwrap_or(group);
        group.extend(new);
        let g = &mut self.arrangement.linked_tracks;
        g.retain(|o| !o.contains(&to));
        g.push(group);
        true
    }

    /// Takes tracks out of their instances. One left with a single track is no instance.
    pub fn unlink_tracks(&mut self, tracks: &[TrackId]) -> bool {
        let groups = &mut self.arrangement.linked_tracks;
        let before = groups.iter().map(Vec::len).sum::<usize>();
        for g in groups.iter_mut() {
            g.retain(|t| !tracks.contains(t));
        }
        groups.retain(|g| g.len() > 1);
        groups.iter().map(Vec::len).sum::<usize>() != before
    }

    /// Forgets tracks that are gone.
    pub fn prune_links(&mut self) {
        let alive: Vec<TrackId> = self.tracks.iter().map(|t| t.id).collect();
        let groups = &mut self.arrangement.linked_tracks;
        for g in groups.iter_mut() {
            g.retain(|t| alive.contains(t));
        }
        groups.retain(|g| g.len() > 1);
    }

    /// Where the last clip on any take lane ends.
    pub fn takes_end(&self) -> u64 {
        self.tracks
            .iter()
            .flat_map(|t| t.take_lanes.iter())
            .filter_map(|l| l.clips.last())
            .map(|c| c.end())
            .max()
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Clip, ClipContent, TrackKind, Warp, PALETTE};
    use rlive_core::fade::DEFAULT_EDGE_FADE_MS;

    fn audio_clip(file: &str, length: u64) -> Clip {
        let mut content = ClipContent::audio(std::path::Path::new(file), 0.0);
        if let ClipContent::Audio { warp, .. } = &mut content {
            *warp = Warp::default();
        }
        Clip {
            name: file.into(),
            length,
            looped: false,
            content,
            ..Clip::default()
        }
    }

    fn track() -> Track {
        Track::new("t", TrackKind::Audio, PALETTE[0], 1)
    }

    /// A track with take lanes A and B, each one clip of 4000 ticks from a long file at offset 500.
    fn two_takes() -> Track {
        let mut t = track();
        for (i, f) in ["a.wav", "b.wav"].into_iter().enumerate() {
            t.insert_take_lane(None);
            let mut c = ArrangementClip::new(audio_clip(f, 8000), 0);
            c.length = 4000;
            c.offset = 500;
            t.take_lanes[i].clips.push(c);
        }
        t
    }

    fn spans(clips: &[ArrangementClip]) -> Vec<(u64, u64, u64)> {
        clips
            .iter()
            .map(|c| (c.start, c.length, c.offset))
            .collect()
    }

    fn file(c: &ArrangementClip) -> &std::path::Path {
        match &c.clip.content {
            ClipContent::Audio { file, .. } => &file.path,
            _ => panic!(),
        }
    }

    #[test]
    fn a_new_lane_gets_a_name_nothing_else_has() {
        let mut t = track();
        t.insert_take_lane(None);
        t.insert_take_lane(None);
        t.delete_take_lane(0);
        let i = t.insert_take_lane(None);
        assert_eq!(t.take_lanes[0].name, "Take 2");
        assert_eq!(t.take_lanes[i].name, "Take 3");
    }

    #[test]
    fn a_comp_copies_exactly_the_selection_as_independent_clips() {
        let mut t = two_takes();
        let placed = t.comp_from_lane(1, 1000, 3000);
        assert_eq!(placed.len(), 1);
        assert_eq!(spans(&t.arrangement), vec![(1000, 2000, 1500)]);
        assert_ne!(t.arrangement[0].id, t.take_lanes[1].clips[0].id);
        assert_eq!(
            t.take_lanes[1].clips[0].length, 4000,
            "the take stays whole"
        );
        t.arrangement[0].clip.name = "edited".into();
        assert_eq!(
            t.take_lanes[1].clips[0].clip.name, "b.wav",
            "and independent"
        );
    }

    #[test]
    fn a_later_comp_replaces_only_its_own_span() {
        let mut t = two_takes();
        t.comp_from_lane(0, 0, 4000);
        t.comp_from_lane(1, 1000, 2000);
        assert_eq!(
            spans(&t.arrangement),
            vec![(0, 1000, 500), (1000, 1000, 1500), (2000, 2000, 2500)]
        );
        assert_eq!(file(&t.arrangement[1]), std::path::Path::new("b.wav"));
    }

    #[test]
    fn comp_splits_crossfade_with_the_default_fades_on() {
        let mut t = two_takes();
        t.comp_from_lane(0, 0, 4000);
        t.comp_from_lane(1, 1000, 2000);
        // A 4 ms default at 120 BPM.
        let edge = crate::arrangement_edit::ms_to_ticks(DEFAULT_EDGE_FADE_MS, 120.0);
        let f = t.effective_fades(Some(edge));
        assert!(edge > 2, "enough ticks to halve: {edge}");
        assert_eq!(f[0].fade_out, edge / 2, "out, before the split");
        assert_eq!(f[0].tail, edge / 2, "and past it, into its handle");
        assert_eq!(f[1].fade_in, edge / 2);
        assert_eq!(f[1].lead, edge / 2);
    }

    #[test]
    fn a_cut_edge_loses_the_fades_a_take_had_there() {
        let mut t = two_takes();
        t.take_lanes[0].clips[0].clip.fades.fade_in = 200;
        t.take_lanes[0].clips[0].clip.fades.fade_out = 300;
        t.comp_from_lane(0, 100, 3900);
        assert_eq!(t.arrangement[0].clip.fades.fade_in, 0);
        assert_eq!(t.arrangement[0].clip.fades.fade_out, 0);
        t.comp_from_lane(0, 0, 4000);
        assert_eq!(t.arrangement[0].clip.fades.fade_in, 200, "kept when whole");
    }

    #[test]
    fn next_and_previous_step_through_lanes_and_skip_empty_ones() {
        let mut t = two_takes();
        t.insert_take_lane(Some(0)); // an empty lane between the two
        t.comp_from_lane(2, 0, 4000); // the main lane comes from lane 2
        assert_eq!(t.source_lane(0, 4000), Some(2));
        let up = t.step_take(None, 0, 4000, TakeStep::Next);
        assert_eq!(up, Some(0), "lane 1 has nothing here");
        assert_eq!(file(&t.arrangement[0]), std::path::Path::new("a.wav"));
        assert_eq!(
            t.step_take(None, 0, 4000, TakeStep::Next),
            None,
            "top reached"
        );
        let down = t.step_take(None, 0, 4000, TakeStep::Previous);
        assert_eq!(down, Some(2));
        assert_eq!(file(&t.arrangement[0]), std::path::Path::new("b.wav"));
    }

    #[test]
    fn stepping_from_a_take_lane_starts_at_that_lane() {
        let mut t = two_takes();
        t.step_take(Some(1), 500, 1500, TakeStep::Next).unwrap();
        assert_eq!(spans(&t.arrangement), vec![(500, 1000, 1000)]);
        assert_eq!(file(&t.arrangement[0]), std::path::Path::new("a.wav"));
    }

    #[test]
    fn stepping_replaces_exactly_the_span() {
        let mut t = two_takes();
        t.comp_from_lane(0, 0, 4000);
        t.step_take(None, 1000, 2000, TakeStep::Previous).unwrap();
        assert_eq!(
            spans(&t.arrangement),
            vec![(0, 1000, 500), (1000, 1000, 1500), (2000, 2000, 2500)]
        );
    }

    #[test]
    fn sources_follow_position_and_properties() {
        let mut t = two_takes();
        t.comp_from_lane(0, 1000, 3000);
        let s = t.comp_sources();
        assert_eq!(s.len(), 1);
        assert_eq!((s[0].lane, s[0].start, s[0].end), (0, 1000, 3000));
        if let ClipContent::Audio { gain_db, .. } = &mut t.arrangement[0].clip.content {
            *gain_db = -6.0;
        }
        assert!(t.comp_sources().is_empty(), "edited: no longer the same");
    }

    #[test]
    fn delete_unused_keeps_what_a_comp_is_using() {
        let mut t = two_takes();
        t.comp_from_lane(1, 1000, 3000);
        assert_eq!(t.delete_unused_take_lanes(), 1);
        assert_eq!(t.take_lanes.len(), 1);
        assert_eq!(
            file(&t.take_lanes[0].clips[0]),
            std::path::Path::new("b.wav")
        );
    }

    #[test]
    fn the_audition_stays_on_its_lane_as_lanes_come_and_go() {
        let mut t = two_takes();
        t.ui.audition_lane = Some(1);
        t.insert_take_lane(Some(0));
        assert_eq!(t.ui.audition_lane, Some(2), "shifted down by the insert");
        t.delete_take_lane(1);
        assert_eq!(t.ui.audition_lane, Some(1));
        t.move_take_lane(1, 0);
        assert_eq!(t.ui.audition_lane, Some(0));
        t.delete_take_lane(0);
        assert_eq!(t.ui.audition_lane, None, "its own lane went");
    }

    #[test]
    fn the_audition_replaces_what_plays_only_while_it_lasts() {
        let mut t = two_takes();
        t.comp_from_lane(0, 0, 1000);
        assert_eq!(t.playing_lane().len(), t.arrangement.len());
        t.ui.audition_lane = Some(1);
        assert_eq!(file(&t.playing_lane()[0]), std::path::Path::new("b.wav"));
        t.freeze = Some(Default::default());
        assert_eq!(t.auditioned_lane(), None, "a frozen track plays its freeze");
        assert_eq!(t.playing_lane().len(), t.arrangement.len());
    }

    #[test]
    fn duplicating_a_lane_gives_every_clip_a_new_id() {
        let mut t = two_takes();
        let i = t.duplicate_take_lane(0).unwrap();
        assert_eq!(i, 1);
        assert_eq!(t.take_lanes.len(), 3);
        assert_ne!(t.take_lanes[0].clips[0].id, t.take_lanes[1].clips[0].id);
        assert_eq!(
            t.take_lanes[0].clips[0].clip.content,
            t.take_lanes[1].clips[0].clip.content
        );
    }

    #[test]
    fn a_take_only_gets_a_new_lane_when_it_would_overlap() {
        let mut t = track();
        let make = |at: u64| {
            let mut c = ArrangementClip::new(audio_clip("x.wav", 960), at);
            c.length = 960;
            c
        };
        let (a, _) = t.record_take(make(0));
        assert_eq!(a, 0);
        // No clip after this punch-in on Take 1: it's reused.
        let (b, _) = t.record_take(make(1920));
        assert_eq!(b, 0);
        assert_eq!(t.take_lanes.len(), 1);
        // Over the first clip: a new lane.
        let (c, _) = t.record_take(make(0));
        assert_eq!(c, 1);
        assert_eq!(t.take_lanes.len(), 2);
    }

    #[test]
    fn the_last_recorded_take_is_copied_to_the_main_lane() {
        let mut t = track();
        let c = ArrangementClip::new(audio_clip("x.wav", 960), 0);
        let (_, main) = t.record_take(c.clone());
        assert_eq!(t.arrangement[0].id, main);
        assert_ne!(main, c.id);
        assert_eq!(t.arrangement.len(), 1);
        assert_eq!(t.take_lanes[0].clips.len(), 1);
    }

    #[test]
    fn moving_a_split_trims_both_sides_within_their_takes() {
        let mut t = two_takes();
        t.comp_from_lane(0, 0, 2000);
        t.comp_from_lane(1, 2000, 4000);
        let (a, b) = (t.arrangement[0].id, t.arrangement[1].id);
        assert_eq!(t.move_comp_split(a, b, 1500), Some(1500));
        assert_eq!(
            spans(&t.arrangement),
            vec![(0, 1500, 500), (1500, 2500, 2000)]
        );
        assert_eq!(t.move_comp_split(a, b, 9000), Some(3999));
        assert_eq!(t.arrangement[1].length, 1);
    }

    #[test]
    fn linking_makes_one_instance_and_a_track_is_in_at_most_one() {
        let mut p = Project::empty();
        for name in ["a", "b", "c", "d"] {
            p.tracks
                .push(Track::new(name, TrackKind::Audio, PALETTE[0], 1));
        }
        let id = |i: usize| p.tracks[i].id;
        let (a, b, c, d) = (id(0), id(1), id(2), id(3));
        assert!(p.link_tracks(&[a, b]));
        assert!(p.link_tracks(&[c, d]));
        assert_eq!(p.arrangement.linked_tracks.len(), 2);
        // A mix of linked and unlinked makes a new instance from the two it took from.
        assert!(p.link_tracks(&[b, c]));
        assert_eq!(
            p.arrangement.linked_tracks,
            vec![vec![b, c]],
            "a and d are alone now"
        );
        assert_eq!(p.linked_with(a), vec![a]);
        assert_eq!(p.linked_with(b), vec![b, c]);
        assert!(p.add_to_link(&[a], b));
        assert_eq!(p.linked_with(c).len(), 3);
        assert!(p.unlink_tracks(&[a, b]));
        assert!(
            p.arrangement.linked_tracks.is_empty(),
            "one left isn't an instance"
        );
    }

    #[test]
    fn group_and_return_tracks_cannot_be_linked() {
        let mut p = Project::empty();
        p.tracks
            .push(Track::new("a", TrackKind::Audio, PALETTE[0], 1));
        p.tracks
            .push(Track::new("g", TrackKind::Group, PALETTE[0], 1));
        let (a, g) = (p.tracks[0].id, p.tracks[1].id);
        assert!(!p.link_tracks(&[a, g]), "only one linkable");
    }

    #[test]
    fn links_to_a_deleted_track_go_with_it() {
        let mut p = Project::empty();
        for name in ["a", "b", "c"] {
            p.tracks
                .push(Track::new(name, TrackKind::Midi, PALETTE[0], 1));
        }
        let ids: Vec<_> = p.tracks.iter().map(|t| t.id).collect();
        p.link_tracks(&ids);
        p.tracks.remove(0);
        p.prune_links();
        assert_eq!(p.linked_with(ids[1]).len(), 2);
        p.tracks.remove(0);
        p.prune_links();
        assert!(p.arrangement.linked_tracks.is_empty());
    }

    #[test]
    fn the_time_commands_move_take_lanes_with_the_rest() {
        use crate::arrangement_edit::flat_range;
        let mut p = Project::empty();
        let mut t = track();
        t.insert_take_lane(None);
        t.take_lanes[0].clips.push(ArrangementClip {
            length: 960,
            ..ArrangementClip::new(audio_clip("x.wav", 960), 1920)
        });
        p.tracks.push(t);
        p.insert_time(960, 480, &flat_range);
        assert_eq!(p.tracks[0].take_lanes[0].clips[0].start, 2400);
        let copied = p.copy_time(2400, 3360, &flat_range);
        assert_eq!(copied.takes.len(), 1);
        p.delete_time(0, 480, &flat_range);
        assert_eq!(p.tracks[0].take_lanes[0].clips[0].start, 1920);
        p.paste_time(0, &copied, &flat_range);
        assert_eq!(
            p.tracks[0].take_lanes[0]
                .clips
                .iter()
                .map(|c| c.start)
                .collect::<Vec<_>>(),
            vec![0, 2880]
        );
    }
}
