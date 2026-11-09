//! Track automation: envelopes compiled for the audio thread.
//!
//! The UI compiles a track's envelopes into [`TrackRoutes`] and swaps them in whole with
//! `ParamCmd::SetRoutes` (or `GraphOp::SetRoutes`, when a device edit moves cells). The routes
//! are read-only here, so a control step only has to find each lane's place by binary search:
//! there is no cursor to fall behind at a loop or a seek, and nothing to allocate.
//!
//! Clip envelopes that drive parameters are compiled into the routes as well, one [`ClipRoutes`]
//! per clip, since they address cells and so have to change when cells do. Which of them counts
//! at a step depends only on which clip is under the playhead.

use crate::clip::{ClipCurve, EnvCurve};
use crate::modulation::{ModBus, ModRoute};
use crate::params::{CellIx, ParamBus};
use rlive_core::param::{rack, ModMode};
use rlive_core::ParamRange;

/// One envelope, addressed to the cell it drives.
#[derive(Debug, Clone, PartialEq)]
pub struct AutomationLane {
    cell: CellIx,
    curve: EnvCurve,
}

impl AutomationLane {
    /// A lane from points of `(tick, plain value, curve)` in any order. A lane with no point
    /// says nothing, so it comes back as `None`.
    pub fn new(
        cell: CellIx,
        range: ParamRange,
        points: impl IntoIterator<Item = (f64, f64, f32)>,
    ) -> Option<Self> {
        Some(Self {
            cell,
            curve: EnvCurve::new(range, points)?,
        })
    }

    pub fn cell(&self) -> CellIx {
        self.cell
    }

    /// The lane's value at `tick`, in plain units.
    pub fn plain_at(&self, tick: f64) -> f64 {
        self.curve.plain_at(tick)
    }
}

/// What a clip's envelope does to the parameter it drives.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ClipEnvAction {
    /// Sets its absolute value, in plain units (only a Session clip's envelopes do).
    Automate,
    /// Moves it, the way the parameter modulates.
    Modulate(ModMode),
}

/// One of a clip's envelopes for a parameter of its track.
#[derive(Debug, Clone, PartialEq)]
pub struct ClipLane {
    cell: CellIx,
    curve: ClipCurve,
    action: ClipEnvAction,
}

impl ClipLane {
    pub fn new(cell: CellIx, curve: ClipCurve, action: ClipEnvAction) -> Self {
        Self {
            cell,
            curve,
            action,
        }
    }

    pub fn cell(&self) -> CellIx {
        self.cell
    }
}

/// Every envelope of one clip that drives its track's parameters.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ClipRoutes {
    lanes: Box<[ClipLane]>,
}

impl ClipRoutes {
    pub fn new(lanes: Vec<ClipLane>) -> Self {
        Self {
            lanes: lanes.into_boxed_slice(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.lanes.is_empty()
    }

    pub fn lanes(&self) -> &[ClipLane] {
        &self.lanes
    }

    /// Steps 2 and 3 of the resolve order, for a clip `local` ticks past its start.
    pub(crate) fn run(&self, local: f64, bus: &mut ParamBus) {
        for lane in self.lanes.iter() {
            let value = lane.curve.at(local);
            match lane.action {
                ClipEnvAction::Automate => bus.write_clip_automation(lane.cell, value),
                ClipEnvAction::Modulate(ModMode::Scale) => {
                    bus.modulate_clip(lane.cell, 0.0, value.clamp(0.0, 1.0) as f32)
                }
                ClipEnvAction::Modulate(_) => {
                    bus.modulate_clip(lane.cell, value.clamp(-1.0, 1.0) as f32, 1.0)
                }
            }
        }
    }
}

/// One parameter a Macro drives: where the macro's knob is, what it owns, and the range it sweeps
/// in knob position. `max` below `min` inverts the sweep.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MacroRoute {
    source: CellIx,
    target: CellIx,
    range: ParamRange,
    min: f64,
    max: f64,
}

impl MacroRoute {
    /// A route over a target's plain values `min..=max`.
    pub fn new(source: CellIx, target: CellIx, range: ParamRange, min: f64, max: f64) -> Self {
        Self {
            source,
            target,
            range,
            min: range.to_normalized(min),
            max: range.to_normalized(max),
        }
    }

    /// The target's plain value for this macro value (0..127).
    pub fn target_value(&self, macro_value: f64) -> f64 {
        let x = (macro_value / rack::SCALE).clamp(0.0, 1.0);
        self.range
            .from_normalized(self.min + (self.max - self.min) * x)
    }

    pub fn target(&self) -> CellIx {
        self.target
    }
}

/// Everything that feeds a track's cells on the audio thread. Immutable once built: changing any
/// of it means building another and swapping it in.
#[derive(Debug, Default, PartialEq)]
pub struct TrackRoutes {
    /// Sorted by cell, one per cell.
    lanes: Box<[AutomationLane]>,
    /// In the order they must run: a macro comes before anything it maps a nested macro onto.
    macros: Box<[MacroRoute]>,
    /// The envelopes of each Session clip, sorted by slot.
    session_clips: Box<[(u32, ClipRoutes)]>,
    /// The envelopes of each Arrangement clip, sorted by where it starts.
    lane_clips: Box<[(u64, ClipRoutes)]>,
    /// What modulators on this or any other track do to this track's cells, one per cell.
    mods: Box<[ModRoute]>,
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
            macros: Box::default(),
            session_clips: Box::default(),
            lane_clips: Box::default(),
            mods: Box::default(),
        }
    }

    /// These routes with what modulators do to the track's cells.
    pub fn with_mods(mut self, mods: Vec<ModRoute>) -> Self {
        self.mods = mods.into_boxed_slice();
        self
    }

    pub fn mods(&self) -> &[ModRoute] {
        &self.mods
    }

    /// These routes with the envelopes of the track's clips, by Session slot and by the tick an
    /// Arrangement clip starts on.
    pub fn with_clips(
        mut self,
        mut session: Vec<(u32, ClipRoutes)>,
        mut lane: Vec<(u64, ClipRoutes)>,
    ) -> Self {
        session.retain(|(_, c)| !c.is_empty());
        lane.retain(|(_, c)| !c.is_empty());
        session.sort_by_key(|(slot, _)| *slot);
        lane.sort_by_key(|(start, _)| *start);
        self.session_clips = session.into_boxed_slice();
        self.lane_clips = lane.into_boxed_slice();
        self
    }

    /// The envelopes of the clip in a Session slot.
    pub(crate) fn session_clip(&self, slot: u32) -> Option<&ClipRoutes> {
        let i = self
            .session_clips
            .binary_search_by_key(&slot, |(s, _)| *s)
            .ok()?;
        Some(&self.session_clips[i].1)
    }

    /// The envelopes of the Arrangement clip starting on this tick.
    pub(crate) fn lane_clip(&self, start: u64) -> Option<&ClipRoutes> {
        let i = self
            .lane_clips
            .binary_search_by_key(&start, |(s, _)| *s)
            .ok()?;
        Some(&self.lane_clips[i].1)
    }

    /// Whether these clip envelopes are the same as `other`'s.
    pub(crate) fn same_clips(&self, other: &TrackRoutes) -> bool {
        self.session_clips == other.session_clips && self.lane_clips == other.lane_clips
    }

    /// Whether any clip of the track has envelopes for its parameters.
    pub fn has_clip_routes(&self) -> bool {
        !self.session_clips.is_empty() || !self.lane_clips.is_empty()
    }

    /// Whether a lane of this track automates a cell.
    pub(crate) fn has_lane(&self, cell: CellIx) -> bool {
        self.lanes.binary_search_by_key(&cell, |l| l.cell).is_ok()
    }

    /// These routes with Macro mappings as well, outermost racks first.
    pub fn with_macros(mut self, macros: Vec<MacroRoute>) -> Self {
        self.macros = macros.into_boxed_slice();
        self
    }

    pub fn macros(&self) -> &[MacroRoute] {
        &self.macros
    }

    pub fn lanes(&self) -> &[AutomationLane] {
        &self.lanes
    }

    pub fn is_empty(&self) -> bool {
        self.lanes.is_empty()
            && self.macros.is_empty()
            && self.session_clips.is_empty()
            && self.lane_clips.is_empty()
            && self.mods.is_empty()
    }

    /// Step 2 of the resolve order: write every lane's value for this tick.
    pub(crate) fn automate(&self, tick: f64, bus: &mut ParamBus) {
        for lane in self.lanes.iter() {
            bus.write_automation(lane.cell, lane.plain_at(tick));
        }
    }

    /// Step 1 of the resolve order: every mapping takes its macro's value. A macro can be mapped
    /// onto a macro of a rack nested in it, so outer racks go first.
    pub(crate) fn run_macros(&self, bus: &mut ParamBus) {
        for route in self.macros.iter() {
            let value = bus.current(route.source);
            bus.write_macro(route.target, route.target_value(value));
        }
    }

    /// Step 3 of the resolve order for modulators, and step 1 for a Remote Control. `at` is how
    /// far into the block this control step is, in `0.0..=1.0`.
    pub(crate) fn run_mods(&self, mods: &ModBus, at: f32, bus: &mut ParamBus) {
        for route in self.mods.iter() {
            let Some(value) = mods.value(route.value(), at) else {
                bus.modulate_by(route.target(), 0.0);
                bus.release_macro(route.target());
                continue;
            };
            if mods.flag(route.mode()) {
                bus.modulate_by(route.target(), 0.0);
                if let Some(spec) = bus.spec(route.target()) {
                    let norm = f64::from(value).clamp(0.0, 1.0);
                    bus.write_macro(route.target(), spec.range.from_normalized(norm));
                }
            } else {
                bus.release_macro(route.target());
                bus.modulate_by(route.target(), value);
            }
        }
    }

    /// Hand the cells that `next` no longer automates back to their knobs.
    pub(crate) fn release_dropped(&self, next: &TrackRoutes, bus: &mut ParamBus) {
        for route in self.mods.iter() {
            if !next.mods.iter().any(|m| m.target() == route.target()) {
                bus.modulate_by(route.target(), 0.0);
                if !next.macros.iter().any(|m| m.target == route.target()) {
                    bus.release_macro(route.target());
                }
            }
        }
        for route in self.macros.iter() {
            if !next.macros.iter().any(|m| m.target == route.target) {
                bus.release_macro(route.target);
            }
        }
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
