//! Editing a note's expression (manual ch. 12) as pure functions from envelopes to envelopes.
//! Point times are ticks from the note's start, so moving a note moves its expression with it and
//! nothing here has to.

use crate::envelope_edit;
use crate::{Breakpoint, NoteData, NoteExpression};
use rlive_core::envelope::{curve_shape, value_at, Interp};
use rlive_core::{ParamRange, MPE_BEND_RANGE};

/// How far pitch can bend either way, in semitones.
pub const BEND_RANGE: f64 = MPE_BEND_RANGE as f64;

/// What Slide reads with no envelope: the controller's rest position.
const SLIDE_REST: f64 = 64.0 / 127.0;

/// The most ticks between the straight pieces a curve is played as.
const CURVE_STEP: f64 = 20.0;
/// The most straight pieces one curved segment is played as.
const MAX_CURVE_PIECES: usize = 128;

/// The envelope dimensions of a note.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dim {
    Pitch,
    Slide,
    Pressure,
}

impl Dim {
    pub const ALL: [Dim; 3] = [Self::Pitch, Self::Slide, Self::Pressure];

    pub fn label(self) -> &'static str {
        match self {
            Self::Pitch => "Pitch",
            Self::Slide => "Slide",
            Self::Pressure => "Pressure",
        }
    }

    pub fn range(self) -> ParamRange {
        let (min, max) = match self {
            Self::Pitch => (-BEND_RANGE, BEND_RANGE),
            Self::Slide | Self::Pressure => (0.0, 1.0),
        };
        ParamRange {
            min,
            max,
            skew: 1.0,
            quantized: false,
        }
    }

    /// What the dimension reads when a note has no envelope for it.
    pub fn rest(self) -> f64 {
        match self {
            Self::Pitch | Self::Pressure => 0.0,
            Self::Slide => SLIDE_REST,
        }
    }

    pub fn points(self, n: &NoteData) -> &[Breakpoint] {
        match (self, n.expr.as_deref()) {
            (Self::Pitch, Some(e)) => &e.pitch,
            (Self::Slide, Some(e)) => &e.slide,
            (Self::Pressure, Some(e)) => &e.pressure,
            (_, None) => &[],
        }
    }

    /// The value an envelope shows at `t`, or the rest value if it's empty.
    pub fn value_at(self, points: &[Breakpoint], t: f64) -> f64 {
        value_at(points, t, Interp::Smooth).unwrap_or_else(|| self.rest())
    }

    /// How a value reads in the editor.
    pub fn format(self, v: f64) -> String {
        match self {
            Self::Pitch => format!("{v:+.2} st"),
            Self::Slide | Self::Pressure => format!("{:.0}", v * 127.0),
        }
    }

    /// A typed value, in the units [`Self::format`] shows.
    pub fn from_typed(self, typed: f64) -> f64 {
        match self {
            Self::Pitch => typed,
            Self::Slide | Self::Pressure => typed / 127.0,
        }
    }
}

/// Puts an envelope on a note. A note left with no expression has none to carry.
pub fn set_points(n: &mut NoteData, dim: Dim, points: Vec<Breakpoint>) {
    let e = n.expr.get_or_insert_with(Default::default);
    match dim {
        Dim::Pitch => e.pitch = points,
        Dim::Slide => e.slide = points,
        Dim::Pressure => e.pressure = points,
    }
    if **e == NoteExpression::default() {
        n.expr = None;
    }
}

/// Moves points inside a note, which they can't leave: the drag stops at either end of it. What
/// they pass over goes, as in an automation lane (manual p. 334).
pub fn move_points(
    base: &[Breakpoint],
    moved: &[usize],
    dt: f64,
    dv: f64,
    dim: Dim,
    length: f64,
) -> Vec<Breakpoint> {
    let (Some(&first), Some(&last)) = (moved.first(), moved.last()) else {
        return base.to_vec();
    };
    let dt = dt.max(-base[first].time).min(length - base[last].time);
    let range = dim.range();
    let dknob = dv / (range.max - range.min);
    envelope_edit::move_points(base, moved, dt, dknob, range)
}

/// Moves one segment, or the part of the envelope in a selection when it's inside one: points are
/// put on the selection's edges and it goes as one (manual p. 334).
pub fn move_segment(
    base: &[Breakpoint],
    segment: usize,
    span: Option<(f64, f64)>,
    dt: f64,
    dv: f64,
    dim: Dim,
    length: f64,
) -> Vec<Breakpoint> {
    let range = dim.range();
    let dknob = dv / (range.max - range.min);
    if let Some((a, b)) = span {
        let dt = dt.max(-a).min(length - b);
        return envelope_edit::move_span(base, a, b, dt, dknob, range);
    }
    move_points(base, &[segment, segment + 1], dt, dv, dim, length)
}

/// Adds a breakpoint, keeping the shape of a curve it lands on.
pub fn insert(points: &[Breakpoint], t: f64, value: f64, dim: Dim) -> (Vec<Breakpoint>, usize) {
    let mut out = points.to_vec();
    let at = envelope_edit::insert(&mut out, t, value, dim.range());
    (out, at)
}

/// Removes breakpoint `i`.
pub fn delete(points: &[Breakpoint], i: usize) -> Vec<Breakpoint> {
    let mut out = points.to_vec();
    if i < out.len() {
        out.remove(i);
    }
    out
}

/// Gives a point an exact value; the others in `group` move by the same amount (manual p. 333).
pub fn set_value(
    base: &[Breakpoint],
    anchor: usize,
    group: &[usize],
    value: f64,
    dim: Dim,
) -> Vec<Breakpoint> {
    envelope_edit::set_value_relative(base, anchor, group, value, dim.range())
}

/// Multiplies every value, as dragging a non-Pitch envelope does (manual p. 334). The shape is
/// kept and values stay in range.
pub fn scale_values(base: &[Breakpoint], factor: f64, dim: Dim) -> Vec<Breakpoint> {
    let range = dim.range();
    base.iter()
        .map(|p| Breakpoint {
            value: range.clamp(p.value * factor.max(0.0)),
            ..*p
        })
        .collect()
}

/// Moves every value by the same amount, as dragging after Select All does (manual p. 335).
pub fn offset_values(base: &[Breakpoint], dv: f64, dim: Dim) -> Vec<Breakpoint> {
    let range = dim.range();
    base.iter()
        .map(|p| Breakpoint {
            value: range.clamp(p.value + dv),
            ..*p
        })
        .collect()
}

/// Clears Pitch, Slide and Pressure of the chosen notes (manual p. 335).
pub fn clear_envelopes(notes: &mut [NoteData], selected: &[usize]) {
    for i in crate::clip_edit::chosen(notes, selected) {
        notes[i].expr = None;
    }
}

/// `v` to the nearest semitone (manual p. 335).
pub fn snap_semitone(v: f64) -> f64 {
    v.round()
}

/// `t` moved to the nearest of `targets` if one is within `reach`.
pub fn snap_to(t: f64, targets: impl IntoIterator<Item = f64>, reach: f64) -> f64 {
    let mut best = t;
    let mut distance = reach;
    for target in targets {
        let d = (target - t).abs();
        if d <= distance {
            distance = d;
            best = target;
        }
    }
    best
}

/// `t` (ticks into a note starting at `note_start`) on the grid line nearest it, when one is within
/// `reach` ticks; a breakpoint made close to a line lands on it (manual p. 334).
pub fn snap_to_grid(t: f64, note_start: u64, grid: u64, reach: f64) -> f64 {
    let grid = grid.max(1) as f64;
    let at = t + note_start as f64;
    let line = (at / grid).round() * grid;
    if (line - at).abs() <= reach {
        (line - note_start as f64).max(0.0)
    } else {
        t
    }
}

/// How high and how low the chosen notes reach, bends included, among those sounding between
/// `from` and `to` (manual p. 335). `None` with nothing there.
pub fn pitch_extent(notes: &[NoteData], picks: &[usize], from: u64, to: u64) -> Option<(f64, f64)> {
    let mut extent: Option<(f64, f64)> = None;
    for i in crate::clip_edit::chosen(notes, picks) {
        let n = &notes[i];
        if n.start >= to || n.start + n.length <= from {
            continue;
        }
        let key = f64::from(n.key);
        let (mut lo, mut hi) = (key, key);
        for p in Dim::Pitch.points(n) {
            let sounding = n.start as f64 + p.time;
            if sounding >= from as f64 && sounding <= to as f64 {
                lo = lo.min(key + p.value);
                hi = hi.max(key + p.value);
            }
        }
        extent = Some(extent.map_or((lo, hi), |(a, b)| (a.min(lo), b.max(hi))));
    }
    extent
}

/// An envelope with its curves replaced by short straight pieces, which is how it reaches the
/// engine.
pub fn flatten(points: &[Breakpoint]) -> Vec<Breakpoint> {
    let mut out = Vec::with_capacity(points.len());
    for (i, p) in points.iter().enumerate() {
        out.push(Breakpoint { curve: 0.0, ..*p });
        let Some(next) = points.get(i + 1) else {
            continue;
        };
        let span = next.time - p.time;
        if p.curve == 0.0 || span <= 0.0 {
            continue;
        }
        let pieces = ((span / CURVE_STEP).ceil() as usize).clamp(2, MAX_CURVE_PIECES);
        for n in 1..pieces {
            let x = n as f64 / pieces as f64;
            let v = p.value + (next.value - p.value) * curve_shape(x, p.curve);
            out.push(Breakpoint::new(p.time + span * x, v));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pts(list: &[(f64, f64)]) -> Vec<Breakpoint> {
        list.iter().map(|&(t, v)| Breakpoint::new(t, v)).collect()
    }

    fn pairs(points: &[Breakpoint]) -> Vec<(f64, f64)> {
        points.iter().map(|p| (p.time, p.value)).collect()
    }

    fn note() -> NoteData {
        NoteData::new(0, 960, 60, 0.8)
    }

    #[test]
    fn a_note_cant_lose_its_points_off_either_end() {
        let base = pts(&[(100.0, 0.2), (500.0, 0.6)]);
        let moved = move_points(&base, &[1], 900.0, 0.0, Dim::Slide, 960.0);
        assert_eq!(moved[1].time, 960.0);
        let moved = move_points(&base, &[0], -900.0, 0.0, Dim::Slide, 960.0);
        assert_eq!(moved[0].time, 0.0);
    }

    #[test]
    fn dragging_over_a_neighbour_removes_it() {
        let base = pts(&[(0.0, 0.0), (200.0, 0.5), (400.0, 1.0)]);
        let moved = move_points(&base, &[0], 300.0, 0.0, Dim::Slide, 960.0);
        assert_eq!(pairs(&moved), vec![(300.0, 0.0), (400.0, 1.0)]);
        assert_eq!(base.len(), 3, "a drag recomputes from where it began");
    }

    #[test]
    fn values_stay_within_each_dimensions_range() {
        let base = pts(&[(0.0, 40.0)]);
        let moved = move_points(&base, &[0], 0.0, 100.0, Dim::Pitch, 960.0);
        assert_eq!(moved[0].value, BEND_RANGE);
        let base = pts(&[(0.0, 0.9)]);
        let moved = move_points(&base, &[0], 0.0, 0.5, Dim::Slide, 960.0);
        assert_eq!(moved[0].value, 1.0);
    }

    #[test]
    fn a_selected_segment_inserts_points_at_the_selection_and_moves_as_one() {
        let base = pts(&[(0.0, 0.0), (960.0, 0.0)]);
        let moved = move_segment(&base, 0, Some((240.0, 720.0)), 0.0, 0.5, Dim::Slide, 960.0);
        assert_eq!(
            pairs(&moved),
            vec![(0.0, 0.0), (240.0, 0.5), (720.0, 0.5), (960.0, 0.0)]
        );
    }

    #[test]
    fn a_new_point_keeps_a_curves_shape() {
        let mut base = pts(&[(0.0, 0.0), (960.0, 1.0)]);
        base[0].curve = 0.6;
        let before = Dim::Slide.value_at(&base, 300.0);
        let (with, at) = insert(&base, 480.0, Dim::Slide.value_at(&base, 480.0), Dim::Slide);
        assert_eq!(at, 1);
        assert!((Dim::Slide.value_at(&with, 300.0) - before).abs() < 1e-6);
    }

    #[test]
    fn edit_value_moves_the_group_relatively() {
        let base = pts(&[(0.0, 1.0), (10.0, 2.0)]);
        let set = set_value(&base, 0, &[0, 1], 3.0, Dim::Pitch);
        assert_eq!(set[0].value, 3.0);
        assert!((set[1].value - 4.0).abs() < 1e-9);
    }

    #[test]
    fn scaling_keeps_the_shape_and_stays_in_range() {
        let base = pts(&[(0.0, 0.2), (10.0, 0.8)]);
        let up = scale_values(&base, 2.0, Dim::Pressure);
        assert_eq!(pairs(&up), vec![(0.0, 0.4), (10.0, 1.0)]);
    }

    #[test]
    fn select_all_offsets_every_value_equally() {
        let base = pts(&[(0.0, 0.2), (10.0, 0.8)]);
        let up = offset_values(&base, 0.1, Dim::Slide);
        assert!((up[0].value - 0.3).abs() < 1e-9 && (up[1].value - 0.9).abs() < 1e-9);
    }

    #[test]
    fn a_note_left_with_nothing_carries_nothing() {
        let mut n = note();
        set_points(&mut n, Dim::Pitch, pts(&[(0.0, 1.0)]));
        assert!(n.expr.is_some());
        set_points(&mut n, Dim::Pitch, Vec::new());
        assert!(n.expr.is_none());
    }

    #[test]
    fn clear_all_goes_by_the_selection() {
        let mut notes = vec![note(), note()];
        for n in &mut notes {
            set_points(n, Dim::Slide, pts(&[(0.0, 0.5)]));
        }
        clear_envelopes(&mut notes, &[1]);
        assert!(notes[0].expr.is_some() && notes[1].expr.is_none());
    }

    #[test]
    fn semitone_and_grid_snapping_round_to_the_nearest_step() {
        assert_eq!(snap_semitone(2.6), 3.0);
        assert_eq!(snap_to(102.0, [0.0, 100.0, 200.0], 5.0), 100.0);
        assert_eq!(snap_to(150.0, [0.0, 100.0, 200.0], 5.0), 150.0);
        assert_eq!(snap_to_grid(235.0, 0, 240, 10.0), 240.0);
        assert_eq!(snap_to_grid(100.0, 0, 240, 10.0), 100.0);
        // Grid lines are the clip's, wherever the note starts.
        assert_eq!(snap_to_grid(10.0, 230, 240, 15.0), 10.0);
    }

    #[test]
    fn zoom_fits_the_bends_in_the_selection() {
        let mut n = note();
        set_points(&mut n, Dim::Pitch, pts(&[(100.0, 7.0), (800.0, -5.0)]));
        let notes = vec![n];
        assert_eq!(pitch_extent(&notes, &[], 0, 960), Some((55.0, 67.0)));
        assert_eq!(pitch_extent(&notes, &[], 0, 500), Some((60.0, 67.0)));
        assert_eq!(pitch_extent(&notes, &[], 2000, 3000), None);
    }

    #[test]
    fn flattening_a_curve_traces_it_and_leaves_lines_alone() {
        let mut base = pts(&[(0.0, 0.0), (960.0, 1.0), (1000.0, 1.0)]);
        base[0].curve = 0.7;
        let flat = flatten(&base);
        assert!(flat.len() > 10);
        for p in &flat {
            let want = value_at(&base, p.time, Interp::Smooth).unwrap();
            assert!((p.value - want).abs() < 1e-9);
            assert_eq!(p.curve, 0.0);
        }
        let straight = pts(&[(0.0, 0.0), (960.0, 1.0)]);
        assert_eq!(flatten(&straight), straight);
    }
}
