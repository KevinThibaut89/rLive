//! Tempo Leader clips: warped Arrangement clips the Set follows instead of the other way round.
//!
//! A leader's Warp Markers say what tempo the Set plays at while the clip plays, which plays it as
//! if it were unwarped. When leaders overlap the one on the bottom-most track wins. The result is
//! the tempo automation in force, which the Main track shows but can't edit until the leaders are
//! unfollowed.

use crate::envelope_edit;
use crate::{ArrangementClip, Breakpoint, ClipContent, Project};
use rlive_core::{ParamRange, PPQ};
use std::borrow::Cow;

#[derive(Debug, Clone, Copy, PartialEq)]
struct Segment {
    start: f64,
    end: f64,
    bpm: f64,
}

fn is_leader(c: &ArrangementClip) -> bool {
    c.clip.active
        && matches!(
            &c.clip.content,
            ClipContent::Audio { warp, .. }
                if warp.enabled && warp.tempo_leader && warp.markers.len() >= 2
        )
}

/// The stretches of the timeline a leader clip sets the tempo of, one per pair of its markers.
fn clip_segments(c: &ArrangementClip) -> Vec<Segment> {
    let ClipContent::Audio { warp, .. } = &c.clip.content else {
        return Vec::new();
    };
    let (start, end) = (c.start as f64, c.end() as f64);
    let cycle = if c.clip.looped {
        c.clip.length.max(1) as f64
    } else {
        f64::INFINITY
    };
    let last_pair = warp.markers.len() - 2;
    let mut out = Vec::new();
    let mut origin = start - c.offset as f64;
    while origin < end {
        for (i, pair) in warp.markers.windows(2).enumerate() {
            let (a, b) = (pair[0], pair[1]);
            let seconds = b.seconds - a.seconds;
            let ticks = b.tick - a.tick;
            if seconds <= 0.0 || ticks <= 0.0 {
                continue;
            }
            // The first and last segments reach past the markers, as the warp map does.
            let from = if i == 0 { f64::NEG_INFINITY } else { a.tick };
            let to = if i == last_pair {
                f64::INFINITY
            } else {
                b.tick
            };
            let (seg_start, seg_end) = (
                (origin + from.max(0.0)).max(start),
                (origin + to.min(cycle)).min(end),
            );
            if seg_end > seg_start {
                out.push(Segment {
                    start: seg_start,
                    end: seg_end,
                    bpm: ticks / PPQ as f64 * 60.0 / seconds,
                });
            }
        }
        if !c.clip.looped {
            break;
        }
        origin += cycle;
    }
    out
}

/// Lays `over` on top of what's already in `segments`.
fn overlay(segments: &mut Vec<Segment>, over: &[Segment]) {
    for new in over {
        let mut kept = Vec::with_capacity(segments.len() + 1);
        for s in segments.drain(..) {
            if s.end <= new.start || s.start >= new.end {
                kept.push(s);
                continue;
            }
            if s.start < new.start {
                kept.push(Segment {
                    end: new.start,
                    ..s
                });
            }
            if s.end > new.end {
                kept.push(Segment {
                    start: new.end,
                    ..s
                });
            }
        }
        kept.push(*new);
        kept.sort_by(|a, b| a.start.total_cmp(&b.start));
        *segments = kept;
    }
}

impl Project {
    /// Whether any clip of the Arrangement is leading the tempo.
    pub fn has_tempo_leader(&self) -> bool {
        self.tracks
            .iter()
            .any(|t| t.arrangement.iter().any(is_leader))
    }

    /// The tempo automation the leaders generate, with the Set's own tempo around them. Empty
    /// without leaders.
    pub fn leader_tempo_points(&self) -> Vec<Breakpoint> {
        let mut segments = Vec::new();
        for track in &self.tracks {
            for clip in track.arrangement.iter().filter(|c| is_leader(c)) {
                overlay(&mut segments, &clip_segments(clip));
            }
        }
        if segments.is_empty() {
            return Vec::new();
        }
        let automation = &self.arrangement.tempo;
        let range = ParamRange {
            min: automation.min,
            max: automation.max,
            skew: 1.0,
            quantized: false,
        };
        let base = |tick: f64| {
            envelope_edit::value_at(&automation.points, tick, range).unwrap_or(self.tempo)
        };
        let mut points = Vec::with_capacity(segments.len() * 2 + automation.points.len() + 2);
        let mut cursor = 0.0;
        for s in &segments {
            if s.start > cursor {
                points.push(Breakpoint::new(cursor, base(cursor)));
                points.extend(
                    automation
                        .points
                        .iter()
                        .filter(|p| p.time > cursor && p.time < s.start)
                        .copied(),
                );
                points.push(Breakpoint::new(s.start, base(s.start)));
            }
            points.push(Breakpoint::new(s.start, s.bpm));
            points.push(Breakpoint::new(s.end, s.bpm));
            cursor = s.end;
        }
        points.push(Breakpoint::new(cursor, base(cursor)));
        points.extend(
            automation
                .points
                .iter()
                .filter(|p| p.time > cursor)
                .copied(),
        );
        points
    }

    /// The tempo automation that plays: the leaders' if there are any, the Set's otherwise.
    pub fn tempo_points_in_force(&self) -> Cow<'_, [Breakpoint]> {
        if self.has_tempo_leader() {
            Cow::Owned(self.leader_tempo_points())
        } else {
            Cow::Borrowed(&self.arrangement.tempo.points)
        }
    }

    /// Makes the leaders followers and keeps the automation they generated, now editable.
    /// Returns whether there was anything to unfollow.
    pub fn unfollow_tempo_automation(&mut self) -> bool {
        if !self.has_tempo_leader() {
            return false;
        }
        let points = self.leader_tempo_points();
        for track in &mut self.tracks {
            for clip in &mut track.arrangement {
                if let ClipContent::Audio { warp, .. } = &mut clip.clip.content {
                    warp.tempo_leader = false;
                }
            }
        }
        self.arrangement.tempo.points = points;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Clip, Track, TrackKind, Warp, WarpMarker};

    const BAR: u64 = 4 * PPQ;

    /// A clip of `bars` bars whose file plays at `bpm`.
    fn clip_at(start: u64, bars: u64, bpm: f64, looped: bool) -> ArrangementClip {
        let seconds = bars as f64 * 4.0 * 60.0 / bpm;
        let mut content = ClipContent::audio("a.wav", 0.0);
        if let ClipContent::Audio { warp, .. } = &mut content {
            *warp = Warp {
                enabled: true,
                tempo_leader: true,
                markers: vec![
                    WarpMarker {
                        seconds: 0.0,
                        tick: 0.0,
                    },
                    WarpMarker {
                        seconds,
                        tick: (bars * BAR) as f64,
                    },
                ],
                ..Warp::default()
            };
        }
        ArrangementClip::new(
            Clip {
                length: bars * BAR,
                looped,
                content,
                ..Clip::default()
            },
            start,
        )
    }

    fn project(lanes: Vec<Vec<ArrangementClip>>) -> Project {
        let mut p = Project::new_default();
        p.tempo = 120.0;
        p.tracks.clear();
        for (i, clips) in lanes.into_iter().enumerate() {
            let mut t = Track::new(&format!("{i}"), TrackKind::Audio, [0; 3], 0);
            t.arrangement = clips;
            p.tracks.push(t);
        }
        p
    }

    fn at(points: &[Breakpoint], tick: f64) -> f64 {
        let i = points.partition_point(|p| p.time <= tick);
        points[i - 1].value
    }

    #[test]
    fn a_leader_sets_the_tempo_while_it_plays_and_the_set_comes_back_after() {
        let p = project(vec![vec![clip_at(BAR, 2, 90.0, false)]]);
        let points = p.tempo_points_in_force();
        assert_eq!(at(&points, 0.0), 120.0);
        assert!((at(&points, 1.5 * BAR as f64) - 90.0).abs() < 1e-9);
        assert_eq!(at(&points, 3.5 * BAR as f64), 120.0);
    }

    #[test]
    fn the_bottom_most_track_wins() {
        let top = clip_at(0, 4, 90.0, false);
        let bottom = clip_at(BAR, 1, 140.0, false);
        let p = project(vec![vec![top], vec![bottom]]);
        let points = p.tempo_points_in_force();
        assert!((at(&points, 0.5 * BAR as f64) - 90.0).abs() < 1e-9);
        assert!((at(&points, 1.5 * BAR as f64) - 140.0).abs() < 1e-9);
        assert!((at(&points, 2.5 * BAR as f64) - 90.0).abs() < 1e-9);
    }

    #[test]
    fn marker_tempos_change_inside_a_clip() {
        let mut c = clip_at(0, 2, 100.0, false);
        if let ClipContent::Audio { warp, .. } = &mut c.clip.content {
            warp.markers = vec![
                WarpMarker {
                    seconds: 0.0,
                    tick: 0.0,
                },
                WarpMarker {
                    seconds: 2.0,
                    tick: BAR as f64,
                },
                WarpMarker {
                    seconds: 3.0,
                    tick: 2.0 * BAR as f64,
                },
            ];
        }
        let p = project(vec![vec![c]]);
        let points = p.tempo_points_in_force();
        assert!((at(&points, 0.5 * BAR as f64) - 120.0).abs() < 1e-9);
        assert!((at(&points, 1.5 * BAR as f64) - 240.0).abs() < 1e-9);
    }

    #[test]
    fn a_looped_leader_repeats_its_markers() {
        let p = project(vec![vec![{
            let mut c = clip_at(0, 1, 80.0, true);
            c.length = 3 * BAR;
            c
        }]]);
        let points = p.tempo_points_in_force();
        for bar in [0.5, 1.5, 2.5] {
            assert!((at(&points, bar * BAR as f64) - 80.0).abs() < 1e-9, "{bar}");
        }
    }

    #[test]
    fn a_follower_leads_nothing_and_a_deactivated_leader_neither() {
        let mut follower = clip_at(0, 2, 90.0, false);
        if let ClipContent::Audio { warp, .. } = &mut follower.clip.content {
            warp.tempo_leader = false;
        }
        assert!(!project(vec![vec![follower]]).has_tempo_leader());
        let mut off = clip_at(0, 2, 90.0, false);
        off.clip.active = false;
        assert!(!project(vec![vec![off]]).has_tempo_leader());
    }

    #[test]
    fn unfollowing_keeps_the_automation_and_frees_the_clips() {
        let mut p = project(vec![vec![clip_at(BAR, 2, 90.0, false)]]);
        let generated = p.leader_tempo_points();
        assert!(p.unfollow_tempo_automation());
        assert_eq!(p.arrangement.tempo.points, generated);
        assert!(!p.has_tempo_leader());
        assert!(!p.unfollow_tempo_automation());
    }
}
