//! What's selected in the Arrangement: clips, or a span of time across lanes.
//!
//! A time selection with no length is the insert marker on those lanes. The automation lanes
//! read the same selection, so a span dragged across clip lanes and envelopes is one selection.

use crate::session::{ChainRef, Session};
use crate::ui::automation::LaneKey;
use rlive_core::{ArrClipId, TrackId};

/// A time selection, or with `start == end` an insert marker, on some lanes.
#[derive(Debug, Clone, PartialEq)]
pub struct TimeSel {
    pub start: f64,
    pub end: f64,
    pub lanes: Vec<LaneKey>,
}

impl TimeSel {
    pub fn point(tick: f64, lane: LaneKey) -> Self {
        Self {
            start: tick,
            end: tick,
            lanes: vec![lane],
        }
    }

    pub fn is_point(&self) -> bool {
        self.start == self.end
    }

    /// The whole ticks of the span.
    pub fn ticks(&self) -> (u64, u64) {
        (self.start.max(0.0) as u64, self.end.max(0.0) as u64)
    }

    /// The tracks the selection is on.
    pub fn tracks(&self) -> Vec<TrackId> {
        let mut out = Vec::new();
        for lane in &self.lanes {
            if let ChainRef::Track(id) = lane.chain {
                if !out.contains(&id) {
                    out.push(id);
                }
            }
        }
        out
    }
}

/// Extends a selection to `tick`, and to the lanes between it and `lane`, as Shift-click does.
pub fn extended(sel: &TimeSel, tick: f64, lane: LaneKey, order: &[LaneKey]) -> TimeSel {
    let anchor = if (tick - sel.start).abs() < (tick - sel.end).abs() {
        sel.end
    } else {
        sel.start
    };
    let (start, end) = if tick < anchor {
        (tick, anchor)
    } else {
        (anchor, tick)
    };
    let lanes = lanes_between(sel.lanes.first().copied().unwrap_or(lane), lane, order);
    TimeSel { start, end, lanes }
}

/// The lanes from `a` to `b` inclusive, top to bottom.
pub fn lanes_between(a: LaneKey, b: LaneKey, order: &[LaneKey]) -> Vec<LaneKey> {
    let (Some(i), Some(j)) = (
        order.iter().position(|k| *k == a),
        order.iter().position(|k| *k == b),
    ) else {
        return vec![b];
    };
    order[i.min(j)..=i.max(j)].to_vec()
}

/// Moves the end of a selection further by `delta`, the way Shift and the arrow keys do. A point
/// grows from the side it is pushed; a span shrinks to nothing and goes no further.
pub fn grown(sel: &TimeSel, delta: f64) -> TimeSel {
    let mut out = sel.clone();
    if sel.is_point() {
        if delta < 0.0 {
            out.start = (sel.start + delta).max(0.0);
        } else {
            out.end += delta;
        }
    } else {
        out.end = (sel.end + delta).max(sel.start);
    }
    out
}

/// The span the clips selected cover.
pub fn clips_span(session: &Session, items: &[(TrackId, ArrClipId)]) -> Option<(u64, u64)> {
    let mut span: Option<(u64, u64)> = None;
    for (t, id) in items {
        let Some(c) = session.project.track(*t).and_then(|t| t.arr_clip(*id)) else {
            continue;
        };
        span = Some(match span {
            Some((a, b)) => (a.min(c.start), b.max(c.end())),
            None => (c.start, c.end()),
        });
    }
    span
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(id: u64) -> LaneKey {
        LaneKey {
            chain: ChainRef::Track(TrackId(id)),
            slot: None,
            take: None,
        }
    }

    #[test]
    fn a_point_has_no_length() {
        assert!(TimeSel::point(100.0, key(1)).is_point());
    }

    #[test]
    fn shift_click_extends_past_either_end() {
        let order = [key(1), key(2)];
        let s = TimeSel {
            start: 100.0,
            end: 200.0,
            lanes: vec![key(1)],
        };
        assert_eq!(extended(&s, 300.0, key(1), &order).end, 300.0);
        assert_eq!(extended(&s, 300.0, key(1), &order).start, 100.0);
        assert_eq!(extended(&s, 50.0, key(1), &order).start, 50.0);
        assert_eq!(extended(&s, 50.0, key(1), &order).end, 200.0);
    }

    #[test]
    fn shift_click_on_another_track_spans_the_tracks_between() {
        let order = [key(1), key(2), key(3)];
        let s = TimeSel::point(100.0, key(1));
        let e = extended(&s, 200.0, key(3), &order);
        assert_eq!(e.lanes, vec![key(1), key(2), key(3)]);
        assert_eq!((e.start, e.end), (100.0, 200.0));
    }

    #[test]
    fn the_tracks_of_a_selection_come_once_each_in_order() {
        let s = TimeSel {
            start: 0.0,
            end: 1.0,
            lanes: vec![
                key(2),
                LaneKey {
                    chain: ChainRef::Track(TrackId(2)),
                    slot: Some(0),
                    take: None,
                },
                key(1),
            ],
        };
        assert_eq!(s.tracks(), vec![TrackId(2), TrackId(1)]);
    }

    #[test]
    fn arrows_grow_a_point_either_way_and_shrink_a_span_to_nothing() {
        let p = TimeSel::point(100.0, key(1));
        assert_eq!(grown(&p, 10.0).end, 110.0);
        assert_eq!(grown(&p, -10.0).start, 90.0);
        let s = TimeSel {
            start: 100.0,
            end: 200.0,
            lanes: vec![key(1)],
        };
        assert_eq!(grown(&s, 50.0).end, 250.0);
        assert_eq!(grown(&s, -500.0).end, 100.0, "no further than its start");
    }
}
