//! Track automation: envelopes compiled for the audio thread.
//!
//! The UI compiles a track's envelopes into [`TrackRoutes`] and swaps them in whole with
//! `ParamCmd::SetRoutes` (or `GraphOp::SetRoutes`, when a device edit moves cells). The routes
//! are read-only here, so a control step only has to find each lane's place by binary search:
//! there is no cursor to fall behind at a loop or a seek, and nothing to allocate.

use crate::params::{CellIx, ParamBus};
use rlive_core::envelope::{value_at, EnvPoint, Interp};
use rlive_core::ParamRange;

/// A point of a compiled lane, in knob position, which is the space segments run straight in.
#[derive(Debug, Clone, Copy, PartialEq)]
struct LanePoint {
    time: f64,
    knob: f64,
    curve: f32,
}

impl EnvPoint for LanePoint {
    fn time(&self) -> f64 {
        self.time
    }
    fn value(&self) -> f64 {
        self.knob
    }
    fn curve(&self) -> f32 {
        self.curve
    }
}

/// One envelope, addressed to the cell it drives.
#[derive(Debug, Clone, PartialEq)]
pub struct AutomationLane {
    cell: CellIx,
    range: ParamRange,
    interp: Interp,
    points: Box<[LanePoint]>,
}

impl AutomationLane {
    /// A lane from points of `(tick, plain value, curve)` in any order. A lane with no point
    /// says nothing, so it comes back as `None`.
    pub fn new(
        cell: CellIx,
        range: ParamRange,
        points: impl IntoIterator<Item = (f64, f64, f32)>,
    ) -> Option<Self> {
        let mut points: Vec<LanePoint> = points
            .into_iter()
            .filter(|(time, value, _)| time.is_finite() && value.is_finite())
            .map(|(time, value, curve)| LanePoint {
                time,
                knob: range.to_normalized(value),
                curve,
            })
            .collect();
        if points.is_empty() {
            return None;
        }
        // Stable, so two points at one tick stay a step in the right direction.
        points.sort_by(|a, b| a.time.total_cmp(&b.time));
        Some(Self {
            cell,
            range,
            interp: Interp::of(range.quantized),
            points: points.into_boxed_slice(),
        })
    }

    pub fn cell(&self) -> CellIx {
        self.cell
    }

    /// The lane's value at `tick`, in plain units.
    pub fn plain_at(&self, tick: f64) -> f64 {
        let knob = value_at(&self.points, tick, self.interp).unwrap_or(0.0);
        self.range.from_normalized(knob)
    }
}

/// Everything that feeds a track's cells on the audio thread. Immutable once built: changing any
/// of it means building another and swapping it in. Modulation and Macro sources join it later.
#[derive(Debug, Default, PartialEq)]
pub struct TrackRoutes {
    /// Sorted by cell, one per cell.
    lanes: Box<[AutomationLane]>,
}

impl TrackRoutes {
    /// Routes for these lanes. If two address one cell the later one wins.
    pub fn new(mut lanes: Vec<AutomationLane>) -> Self {
        lanes.sort_by_key(|l| l.cell);
        lanes.reverse();
        lanes.dedup_by_key(|l| l.cell);
        lanes.reverse();
        Self {
            lanes: lanes.into_boxed_slice(),
        }
    }

    pub fn lanes(&self) -> &[AutomationLane] {
        &self.lanes
    }

    pub fn is_empty(&self) -> bool {
        self.lanes.is_empty()
    }

    /// Step 2 of the resolve order: write every lane's value for this tick.
    pub(crate) fn automate(&self, tick: f64, bus: &mut ParamBus) {
        for lane in self.lanes.iter() {
            bus.write_automation(lane.cell, lane.plain_at(tick));
        }
    }

    /// Hand the cells that `next` no longer automates back to their knobs.
    pub(crate) fn release_dropped(&self, next: &TrackRoutes, bus: &mut ParamBus) {
        let mut kept = next.lanes.iter().peekable();
        for lane in self.lanes.iter() {
            while kept.next_if(|l| l.cell < lane.cell).is_some() {}
            if kept.next_if(|l| l.cell == lane.cell).is_none() {
                bus.clear_automation(lane.cell);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RANGE: ParamRange = ParamRange {
        min: 0.0,
        max: 10.0,
        skew: 1.0,
        quantized: false,
    };

    fn lane(cell: u32, points: &[(f64, f64)]) -> AutomationLane {
        AutomationLane::new(
            CellIx(cell),
            RANGE,
            points.iter().map(|&(t, v)| (t, v, 0.0)),
        )
        .unwrap()
    }

    #[test]
    fn a_lane_without_points_is_no_lane() {
        assert!(AutomationLane::new(CellIx(0), RANGE, []).is_none());
        let nonsense = [(f64::NAN, 1.0, 0.0), (1.0, f64::INFINITY, 0.0)];
        assert!(AutomationLane::new(CellIx(0), RANGE, nonsense).is_none());
    }

    #[test]
    fn lanes_are_sorted_by_cell_and_points_by_time() {
        let routes = TrackRoutes::new(vec![
            lane(5, &[(10.0, 2.0), (0.0, 4.0)]),
            lane(1, &[(0.0, 1.0)]),
        ]);
        let cells: Vec<u32> = routes.lanes().iter().map(|l| l.cell().0).collect();
        assert_eq!(cells, [1, 5]);
        assert_eq!(routes.lanes()[1].plain_at(0.0), 4.0);
        assert!((routes.lanes()[1].plain_at(5.0) - 3.0).abs() < 1e-9);
    }

    #[test]
    fn the_later_of_two_lanes_on_a_cell_wins() {
        let routes = TrackRoutes::new(vec![lane(2, &[(0.0, 1.0)]), lane(2, &[(0.0, 9.0)])]);
        assert_eq!(routes.lanes().len(), 1);
        assert_eq!(routes.lanes()[0].plain_at(0.0), 9.0);
    }

    #[test]
    fn a_stepped_lane_holds() {
        let range = ParamRange {
            quantized: true,
            max: 3.0,
            ..RANGE
        };
        let lane =
            AutomationLane::new(CellIx(0), range, [(0.0, 0.0, 0.0), (10.0, 3.0, 0.0)]).unwrap();
        assert_eq!(lane.plain_at(9.0), 0.0);
        assert_eq!(lane.plain_at(10.0), 3.0);
    }
}
