//! Groups, return tracks and sends: how the flat track list nests, and who feeds whom.
//!
//! A group's members follow it directly in [`Project::tracks`], each naming its group in
//! [`Track::group`]. Reads tolerate a file where that doesn't hold; edits keep it.

use crate::{Project, Track, TrackKind};
use rlive_core::param::mixer;
use rlive_core::TrackId;
use std::collections::HashSet;

/// A send's level when nothing has been set: silence.
pub const SEND_OFF_DB: f32 = mixer::VOLUME_MIN_DB as f32;
/// Deepest a chain of groups is followed, so a looped file can't hang a walk up it.
const MAX_GROUP_WALK: usize = 64;

impl Track {
    /// This track's level into a return track.
    pub fn send_level(&self, to: TrackId) -> f32 {
        self.sends
            .iter()
            .find(|s| s.to == to)
            .map_or(SEND_OFF_DB, |s| s.level_db)
    }

    /// Whether a send is on. A return track's sends can feed back, so they stay off until the
    /// user enables them; every other track's are on.
    pub fn send_enabled(&self, to: TrackId) -> bool {
        self.sends
            .iter()
            .find(|s| s.to == to)
            .map_or(self.kind != TrackKind::Return, |s| s.enabled)
    }

    pub fn is_group(&self) -> bool {
        self.kind == TrackKind::Group
    }
}

impl Project {
    /// The group a track is directly in, if there is one.
    pub fn parent_group(&self, id: TrackId) -> Option<TrackId> {
        let group = self.track(id)?.group?;
        self.track(group)
            .is_some_and(Track::is_group)
            .then_some(group)
    }

    /// The groups a track is inside, nearest first.
    pub fn ancestors(&self, id: TrackId) -> Vec<TrackId> {
        let mut out = Vec::new();
        let mut at = self.parent_group(id);
        while let Some(g) = at {
            if out.len() == MAX_GROUP_WALK || out.contains(&g) || g == id {
                break;
            }
            out.push(g);
            at = self.parent_group(g);
        }
        out
    }

    /// How many groups a track is inside.
    pub fn group_depth(&self, id: TrackId) -> usize {
        self.ancestors(id).len()
    }

    /// The tracks directly in a group, in track order.
    pub fn group_children(&self, group: TrackId) -> Vec<TrackId> {
        self.tracks
            .iter()
            .filter(|t| t.group == Some(group) && t.id != group)
            .map(|t| t.id)
            .collect()
    }

    /// Everything inside a group, at any depth, in track order.
    pub fn group_contents(&self, group: TrackId) -> Vec<TrackId> {
        let mut inside = HashSet::new();
        let mut frontier = vec![group];
        while let Some(g) = frontier.pop() {
            for child in self.group_children(g) {
                if inside.insert(child) {
                    frontier.push(child);
                }
            }
        }
        self.tracks
            .iter()
            .filter(|t| inside.contains(&t.id))
            .map(|t| t.id)
            .collect()
    }

    /// A track and, for a group, everything in it: the block that moves or goes together.
    pub fn track_block(&self, id: TrackId) -> Vec<TrackId> {
        let mut block = vec![id];
        block.extend(self.group_contents(id));
        block
    }

    /// Whether a group's contents include a soloed track, for its half-lit Solo button.
    pub fn holds_solo(&self, group: TrackId) -> bool {
        self.group_contents(group)
            .iter()
            .any(|id| self.track(*id).is_some_and(|t| t.solo))
    }

    /// Indices in [`Self::tracks`] of the tracks not hidden inside a folded group.
    pub fn visible_tracks(&self) -> Vec<usize> {
        (0..self.tracks.len())
            .filter(|&i| {
                self.ancestors(self.tracks[i].id)
                    .iter()
                    .all(|g| self.track(*g).is_some_and(|t| !t.ui.folded))
            })
            .collect()
    }

    /// A return track's position in [`Self::returns`], which is also its send's position.
    pub fn return_index(&self, id: TrackId) -> Option<usize> {
        self.returns.iter().position(|t| t.id == id)
    }

    /// The group a new or moved track belongs to when it is placed right after `prev`.
    pub fn group_after(&self, prev: Option<TrackId>) -> Option<TrackId> {
        let prev = self.track(prev?)?;
        if prev.is_group() {
            Some(prev.id)
        } else {
            prev.group
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TrackSend, TrackUi};

    fn track(p: &Project, name: &str, kind: TrackKind) -> Track {
        Track::new(name, kind, [1, 2, 3], p.scenes.len())
    }

    /// `[group a b [group c d] e]` as a flat list.
    fn nested() -> (Project, [TrackId; 6]) {
        let mut p = Project::empty();
        let (g, a, b, inner, c, e) = (
            track(&p, "g", TrackKind::Group),
            track(&p, "a", TrackKind::Midi),
            track(&p, "b", TrackKind::Audio),
            track(&p, "inner", TrackKind::Group),
            track(&p, "c", TrackKind::Midi),
            track(&p, "e", TrackKind::Midi),
        );
        let ids = [g.id, a.id, b.id, inner.id, c.id, e.id];
        p.tracks = vec![g, a, b, inner, c, e];
        for i in 1..4 {
            p.tracks[i].group = Some(ids[0]);
        }
        p.tracks[4].group = Some(ids[3]);
        (p, ids)
    }

    #[test]
    fn a_groups_contents_are_nested_and_in_order() {
        let (p, [g, a, b, inner, c, e]) = nested();
        assert_eq!(p.group_children(g), [a, b, inner]);
        assert_eq!(p.group_contents(g), [a, b, inner, c]);
        assert_eq!(p.ancestors(c), [inner, g]);
        assert_eq!(p.group_depth(e), 0);
        assert_eq!(p.track_block(inner), [inner, c]);
    }

    #[test]
    fn folding_hides_everything_inside() {
        let (mut p, [g, _, _, inner, ..]) = nested();
        assert_eq!(p.visible_tracks(), [0, 1, 2, 3, 4, 5]);
        p.track_mut(inner).unwrap().ui = TrackUi {
            folded: true,
            ..Default::default()
        };
        assert_eq!(p.visible_tracks(), [0, 1, 2, 3, 5]);
        p.track_mut(g).unwrap().ui.folded = true;
        assert_eq!(p.visible_tracks(), [0, 5]);
    }

    #[test]
    fn a_group_loop_in_a_file_cannot_hang_a_walk() {
        let (mut p, [g, _, _, inner, ..]) = nested();
        p.track_mut(g).unwrap().group = Some(inner);
        assert!(p.ancestors(g).len() <= MAX_GROUP_WALK);
        p.visible_tracks();
        p.group_contents(g);
    }

    #[test]
    fn a_nested_solo_lights_every_group_it_is_in() {
        let (mut p, [g, _, _, inner, c, e]) = nested();
        assert!(!p.holds_solo(g));
        p.track_mut(c).unwrap().solo = true;
        assert!(p.holds_solo(inner));
        assert!(p.holds_solo(g));
        p.track_mut(c).unwrap().solo = false;
        p.track_mut(e).unwrap().solo = true;
        assert!(!p.holds_solo(g), "a track outside doesn't count");
    }

    #[test]
    fn sends_default_off_for_returns_and_on_for_everything_else() {
        let p = Project::empty();
        let (clip, group, ret) = (
            track(&p, "c", TrackKind::Midi),
            track(&p, "g", TrackKind::Group),
            track(&p, "r", TrackKind::Return),
        );
        assert!(clip.send_enabled(ret.id));
        assert!(group.send_enabled(ret.id));
        assert!(!ret.send_enabled(ret.id));
        assert_eq!(clip.send_level(ret.id), SEND_OFF_DB);
        let mut on = ret.clone();
        on.sends.push(TrackSend {
            to: ret.id,
            level_db: -6.0,
            enabled: true,
        });
        assert!(on.send_enabled(ret.id));
        assert_eq!(on.send_level(ret.id), -6.0);
    }

    #[test]
    fn a_placed_track_joins_the_group_it_lands_in() {
        let (p, [g, a, _, inner, c, e]) = nested();
        assert_eq!(p.group_after(Some(g)), Some(g));
        assert_eq!(p.group_after(Some(a)), Some(g));
        assert_eq!(p.group_after(Some(inner)), Some(inner));
        assert_eq!(p.group_after(Some(c)), Some(inner));
        assert_eq!(p.group_after(Some(e)), None);
        assert_eq!(p.group_after(None), None);
    }
}
