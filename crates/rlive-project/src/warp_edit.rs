//! Editing a clip's warp markers and transients.
//!
//! Every edit is an [`WarpOp`], applied to a clip through [`apply`]. An op names positions, not
//! marker numbers, so the same op works on every clip of a multi-clip selection. Nothing here
//! knows about the Session or the engine; the caller records the undo step and sends the result.

use crate::{TransientEdits, Warp, WarpMarker};
use rlive_core::warp::{WarpMap, WarpMarker as MapMarker};
use rlive_core::PPQ;

/// Two markers closer than this in time count as one, in seconds.
const NEAR_SECONDS: f64 = 0.002;
/// Two markers closer than this on the clip count as one, in ticks.
const NEAR_TICKS: f64 = 1.0;
/// The tightest two markers can be squeezed together, in ticks.
const MIN_MARKER_GAP_TICKS: f64 = 1.0;
/// The tightest two markers can be squeezed together in the audio, in seconds.
const MIN_MARKER_GAP_SECONDS: f64 = 0.001;
/// How many bars a loop is assumed to be.
const LOOP_BARS: [u32; 5] = [1, 2, 4, 8, 16];
/// A marker on a bar line moves to a transient this close to it, in seconds, or stays put.
const BAR_SNAP_SECONDS: f64 = 0.06;

/// How Warp From... finds its tempo.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WarpFrom {
    /// A marker on each bar, on the sample's estimated tempo.
    Auto,
    /// A marker on each bar, on the Set's tempo.
    StartAtSetTempo,
    /// One marker, at the sample's estimated tempo.
    Straight,
    /// One marker, at the Set's tempo.
    SetTempo,
}

/// One edit to a clip's warping.
#[derive(Debug, Clone, PartialEq)]
pub enum WarpOp {
    /// Pins a moment of the audio to a place in the clip.
    Add {
        seconds: f64,
        tick: f64,
    },
    /// Moves a marker to a new place in the clip, stretching the audio either side.
    Move {
        tick: f64,
        to: f64,
    },
    /// A marker stays; the audio under it slides.
    Slide {
        tick: f64,
        to_seconds: f64,
    },
    Delete {
        tick: f64,
    },
    /// Moves a marker by a number of ticks, a nudge.
    Nudge {
        tick: f64,
        by: f64,
    },
    /// Turns a pseudo-marker into a marker, and perhaps the transients either side of it too.
    FromTransient {
        seconds: f64,
        adjacent: bool,
    },
    /// A marker on each transient between two moments of the audio, or at the edges if none.
    InsertInRange {
        from: f64,
        to: f64,
    },
    /// Double or halve the tempo: the same audio covers more or fewer beats.
    ScaleTempo {
        factor: f64,
    },
    /// Sets the tempo the clip plays at, as the BPM field does.
    SetBpm {
        bpm: f64,
    },
    /// Makes this moment of the audio the first beat of the clip.
    SetOneOneOne {
        seconds: f64,
    },
    /// Throws away everything right of this moment and warps it afresh.
    WarpFrom {
        seconds: f64,
        how: WarpFrom,
    },
    /// Fits a stretch of the audio to a whole number of bars and loops it.
    WarpAs {
        from: f64,
        to: f64,
        bars: u32,
    },
    /// Pulls transients to the grid.
    Quantize {
        /// In ticks.
        grid: f64,
        /// 0 to 1.
        amount: f64,
        /// Between these moments of the audio, or all of it.
        range: Option<(f64, f64)>,
    },
    AddTransient {
        seconds: f64,
    },
    DeleteTransient {
        seconds: f64,
    },
    MoveTransient {
        from: f64,
        to: f64,
    },
    ResetTransients,
}

/// What an op needs to know beyond the clip's own warp settings.
#[derive(Debug, Clone, Copy)]
pub struct WarpContext<'a> {
    /// Length of the audio file.
    pub seconds: f64,
    pub ticks_per_bar: u64,
    pub set_bpm: f64,
    /// The clip's transients in seconds, ascending, its own edits applied.
    pub transients: &'a [f64],
}

/// Applies `op` and returns whether anything changed. `length` is the clip's loop length, in
/// ticks, which a tempo change or Warp As also sets.
pub fn apply(warp: &mut Warp, op: &WarpOp, cx: &WarpContext<'_>, length: &mut u64) -> bool {
    match *op {
        WarpOp::Add { seconds, tick } => add(warp, seconds, tick),
        WarpOp::Move { tick, to } => shift_marker(warp, tick, to),
        WarpOp::Slide { tick, to_seconds } => slide(warp, tick, to_seconds),
        WarpOp::Delete { tick } => delete(warp, tick),
        WarpOp::Nudge { tick, by } => shift_marker(warp, tick, tick + by),
        WarpOp::FromTransient { seconds, adjacent } => {
            from_transient(warp, seconds, adjacent, cx.transients)
        }
        WarpOp::InsertInRange { from, to } => insert_in_range(warp, from, to, cx.transients),
        WarpOp::ScaleTempo { factor } => scale(warp, factor, length),
        WarpOp::SetBpm { bpm } => match clip_bpm(warp) {
            Some(current) if current > 0.0 && bpm > 0.0 => scale(warp, bpm / current, length),
            _ => false,
        },
        WarpOp::SetOneOneOne { seconds } => one_one_one(warp, seconds, cx.seconds),
        WarpOp::WarpFrom { seconds, how } => warp_from(warp, seconds, how, cx),
        WarpOp::WarpAs { from, to, bars } => warp_as(warp, from, to, bars, cx, length),
        WarpOp::Quantize {
            grid,
            amount,
            range,
        } => quantize(warp, grid, amount, range, cx.transients),
        WarpOp::AddTransient { seconds } => {
            warp.transients.added.push(seconds);
            warp.transients.added.sort_by(f64::total_cmp);
            true
        }
        WarpOp::DeleteTransient { seconds } => delete_transient(&mut warp.transients, seconds),
        WarpOp::MoveTransient { from, to } => {
            let moved = delete_transient(&mut warp.transients, from);
            if moved {
                warp.transients.added.push(to);
                warp.transients.added.sort_by(f64::total_cmp);
            }
            moved
        }
        WarpOp::ResetTransients => {
            let had = warp.transients != TransientEdits::default();
            warp.transients = TransientEdits::default();
            had
        }
    }
}

/// The map a clip's markers make, between seconds in the file and ticks in the clip. `None` until
/// there are two usable markers.
pub fn seconds_map(warp: &Warp) -> Option<WarpMap> {
    WarpMap::new(
        warp.markers
            .iter()
            .map(|m| MapMarker::new(m.seconds, m.tick)),
    )
}

/// The clip's tempo, averaged between its outer markers, in BPM.
pub fn clip_bpm(warp: &Warp) -> Option<f64> {
    let (first, last) = (warp.markers.first()?, warp.markers.last()?);
    let seconds = last.seconds - first.seconds;
    (seconds > 0.0 && last.tick > first.tick)
        .then(|| (last.tick - first.tick) / PPQ as f64 * 60.0 / seconds)
}

/// The transients a clip has: what was found in the file, its own edits applied, ascending.
pub fn resolve_transients(found: &[f64], edits: &TransientEdits) -> Vec<f64> {
    let mut out: Vec<f64> = found
        .iter()
        .copied()
        .filter(|t| !edits.removed.iter().any(|r| near(*r, *t)))
        .collect();
    out.extend_from_slice(&edits.added);
    out.sort_by(f64::total_cmp);
    out.dedup_by(|a, b| near(*a, *b));
    out
}

/// The loop length, in bars, that makes a file of `seconds` play closest to `set_bpm`.
pub fn suggest_bars(seconds: f64, ticks_per_bar: u64, set_bpm: f64) -> u32 {
    let beats_per_bar = ticks_per_bar as f64 / PPQ as f64;
    let tempo_of = |bars: u32| bars as f64 * beats_per_bar * 60.0 / seconds.max(1e-6);
    LOOP_BARS
        .iter()
        .copied()
        .min_by(|a, b| {
            let off = |bars| (tempo_of(bars) / set_bpm.max(1.0)).log2().abs();
            off(*a).total_cmp(&off(*b))
        })
        .unwrap_or(1)
}

/// The markers of a well-cut loop: the start and end of the file, on `bars` whole bars.
pub fn loop_markers(seconds: f64, bars: u32, ticks_per_bar: u64) -> Vec<WarpMarker> {
    vec![
        WarpMarker {
            seconds: 0.0,
            tick: 0.0,
        },
        WarpMarker {
            seconds,
            tick: (bars as u64 * ticks_per_bar) as f64,
        },
    ]
}

/// The transient nearest `seconds`, within `reach`.
pub fn nearest_transient(transients: &[f64], seconds: f64, reach: f64) -> Option<f64> {
    transients
        .iter()
        .copied()
        .min_by(|a, b| (a - seconds).abs().total_cmp(&(b - seconds).abs()))
        .filter(|t| (t - seconds).abs() <= reach)
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < NEAR_SECONDS
}

fn index_of(warp: &Warp, tick: f64) -> Option<usize> {
    warp.markers
        .iter()
        .position(|m| (m.tick - tick).abs() <= NEAR_TICKS)
}

fn sort(markers: &mut [WarpMarker]) {
    markers.sort_by(|a, b| a.tick.total_cmp(&b.tick));
}

fn add(warp: &mut Warp, seconds: f64, tick: f64) -> bool {
    let i = warp.markers.partition_point(|m| m.tick < tick);
    let fits_left = i == 0
        || (seconds > warp.markers[i - 1].seconds + NEAR_SECONDS
            && tick > warp.markers[i - 1].tick + NEAR_TICKS);
    let fits_right = i == warp.markers.len()
        || (seconds < warp.markers[i].seconds - NEAR_SECONDS
            && tick < warp.markers[i].tick - NEAR_TICKS);
    if !(fits_left && fits_right) || seconds < 0.0 {
        return false;
    }
    warp.markers.insert(i, WarpMarker { seconds, tick });
    true
}

fn shift_marker(warp: &mut Warp, tick: f64, to: f64) -> bool {
    let Some(i) = index_of(warp, tick) else {
        return false;
    };
    let low = i.checked_sub(1).map_or(f64::NEG_INFINITY, |p| {
        warp.markers[p].tick + MIN_MARKER_GAP_TICKS
    });
    let high = warp
        .markers
        .get(i + 1)
        .map_or(f64::INFINITY, |n| n.tick - MIN_MARKER_GAP_TICKS);
    let clamped = to.clamp(low, high.max(low));
    if clamped == warp.markers[i].tick {
        return false;
    }
    warp.markers[i].tick = clamped;
    true
}

fn slide(warp: &mut Warp, tick: f64, to_seconds: f64) -> bool {
    let Some(i) = index_of(warp, tick) else {
        return false;
    };
    let low = i
        .checked_sub(1)
        .map_or(0.0, |p| warp.markers[p].seconds + MIN_MARKER_GAP_SECONDS);
    let high = warp
        .markers
        .get(i + 1)
        .map_or(f64::INFINITY, |n| n.seconds - MIN_MARKER_GAP_SECONDS);
    let clamped = to_seconds.clamp(low, high.max(low));
    if clamped == warp.markers[i].seconds {
        return false;
    }
    warp.markers[i].seconds = clamped;
    true
}

fn delete(warp: &mut Warp, tick: f64) -> bool {
    // A clip keeps two markers: with fewer it would have no tempo of its own left.
    if warp.markers.len() <= 2 {
        return false;
    }
    match index_of(warp, tick) {
        Some(i) => {
            warp.markers.remove(i);
            true
        }
        None => false,
    }
}

fn from_transient(warp: &mut Warp, seconds: f64, adjacent: bool, transients: &[f64]) -> bool {
    let Some(map) = seconds_map(warp) else {
        return false;
    };
    let mut changed = add(warp, seconds, map.src_to_tick(seconds));
    if adjacent {
        let at = transients.partition_point(|t| *t < seconds - NEAR_SECONDS);
        let right = at + usize::from(transients.get(at).is_some_and(|t| near(*t, seconds)));
        let neighbours = at.checked_sub(1).into_iter().chain(Some(right));
        for i in neighbours {
            if let Some(&t) = transients.get(i) {
                changed |= add(warp, t, map.src_to_tick(t));
            }
        }
    }
    changed
}

fn insert_in_range(warp: &mut Warp, from: f64, to: f64, transients: &[f64]) -> bool {
    let Some(map) = seconds_map(warp) else {
        return false;
    };
    let (from, to) = (from.min(to), from.max(to));
    let inside: Vec<f64> = transients
        .iter()
        .copied()
        .filter(|t| *t >= from && *t <= to)
        .collect();
    let points = if inside.is_empty() {
        vec![from, to]
    } else {
        inside
    };
    let mut changed = false;
    for s in points {
        changed |= add(warp, s, map.src_to_tick(s));
    }
    changed
}

fn scale(warp: &mut Warp, factor: f64, length: &mut u64) -> bool {
    let Some(first) = warp.markers.first().map(|m| m.tick) else {
        return false;
    };
    if factor <= 0.0 || (factor - 1.0).abs() < 1e-9 {
        return false;
    }
    for m in &mut warp.markers {
        m.tick = first + (m.tick - first) * factor;
    }
    *length = ((*length as f64 * factor).round() as u64).max(1);
    true
}

fn one_one_one(warp: &mut Warp, seconds: f64, file_seconds: f64) -> bool {
    let Some(map) = seconds_map(warp) else {
        return false;
    };
    let offset = map.src_to_tick(seconds);
    let mut kept = vec![WarpMarker { seconds, tick: 0.0 }];
    kept.extend(
        warp.markers
            .iter()
            .filter(|m| m.seconds > seconds + NEAR_SECONDS)
            .map(|m| WarpMarker {
                seconds: m.seconds,
                tick: m.tick - offset,
            }),
    );
    if kept.len() < 2 {
        kept.push(WarpMarker {
            seconds: file_seconds,
            tick: map.src_to_tick(file_seconds) - offset,
        });
    }
    let unchanged = kept == warp.markers;
    warp.markers = kept;
    !unchanged
}

/// The tempo of everything from `start` on, as a whole number of bars of the file.
fn estimate_bpm(start: f64, cx: &WarpContext<'_>) -> f64 {
    let remaining = (cx.seconds - start).max(1e-3);
    let bars = suggest_bars(remaining, cx.ticks_per_bar, cx.set_bpm);
    bars as f64 * cx.ticks_per_bar as f64 / PPQ as f64 * 60.0 / remaining
}

fn warp_from(warp: &mut Warp, seconds: f64, how: WarpFrom, cx: &WarpContext<'_>) -> bool {
    let tick = seconds_map(warp).map_or(0.0, |m| m.src_to_tick(seconds));
    let bpm = match how {
        WarpFrom::Auto | WarpFrom::Straight => estimate_bpm(seconds, cx),
        WarpFrom::StartAtSetTempo | WarpFrom::SetTempo => cx.set_bpm,
    };
    let seconds_per_bar = cx.ticks_per_bar as f64 / PPQ as f64 * 60.0 / bpm.max(1.0);
    let mut kept: Vec<WarpMarker> = warp
        .markers
        .iter()
        .copied()
        .filter(|m| m.seconds < seconds - NEAR_SECONDS)
        .collect();
    kept.push(WarpMarker { seconds, tick });
    if matches!(how, WarpFrom::Auto | WarpFrom::StartAtSetTempo) {
        let mut bar = 1u64;
        loop {
            let nominal = seconds + bar as f64 * seconds_per_bar;
            if nominal >= cx.seconds - NEAR_SECONDS {
                break;
            }
            let on = nearest_transient(cx.transients, nominal, BAR_SNAP_SECONDS).unwrap_or(nominal);
            kept.push(WarpMarker {
                seconds: on,
                tick: tick + (bar * cx.ticks_per_bar) as f64,
            });
            bar += 1;
        }
    }
    let end_tick = tick + (cx.seconds - seconds) * bpm / 60.0 * PPQ as f64;
    if end_tick > tick {
        kept.push(WarpMarker {
            seconds: cx.seconds,
            tick: end_tick,
        });
    }
    if kept.len() < 2 {
        return false;
    }
    let unchanged = kept == warp.markers;
    warp.markers = kept;
    !unchanged
}

fn warp_as(
    warp: &mut Warp,
    from: f64,
    to: f64,
    bars: u32,
    cx: &WarpContext<'_>,
    length: &mut u64,
) -> bool {
    if to <= from || bars == 0 {
        return false;
    }
    warp.markers = vec![
        WarpMarker {
            seconds: from,
            tick: 0.0,
        },
        WarpMarker {
            seconds: to,
            tick: (bars as u64 * cx.ticks_per_bar) as f64,
        },
    ];
    *length = bars as u64 * cx.ticks_per_bar;
    true
}

fn quantize(
    warp: &mut Warp,
    grid: f64,
    amount: f64,
    range: Option<(f64, f64)>,
    transients: &[f64],
) -> bool {
    let Some(map) = seconds_map(warp) else {
        return false;
    };
    if grid <= 0.0 {
        return false;
    }
    let (from, to) = range.map_or((f64::NEG_INFINITY, f64::INFINITY), |(a, b)| {
        (a.min(b), a.max(b))
    });
    let amount = amount.clamp(0.0, 1.0);
    let wanted: Vec<WarpMarker> = transients
        .iter()
        .copied()
        .filter(|t| *t >= from && *t <= to)
        .map(|seconds| {
            let tick = map.src_to_tick(seconds);
            let line = (tick / grid).round() * grid;
            WarpMarker {
                seconds,
                tick: tick + (line - tick) * amount,
            }
        })
        .collect();
    if wanted.is_empty() {
        return false;
    }
    // A transient that already has a marker moves it instead of adding another.
    let mut markers: Vec<WarpMarker> = warp
        .markers
        .iter()
        .copied()
        .filter(|m| !wanted.iter().any(|w| near(w.seconds, m.seconds)))
        .collect();
    markers.extend(wanted);
    sort(&mut markers);
    let mut kept: Vec<WarpMarker> = Vec::with_capacity(markers.len());
    for m in markers {
        match kept.last() {
            Some(last)
                if m.tick < last.tick + MIN_MARKER_GAP_TICKS
                    || m.seconds < last.seconds + MIN_MARKER_GAP_SECONDS => {}
            _ => kept.push(m),
        }
    }
    if kept.len() < 2 || kept == warp.markers {
        return false;
    }
    warp.markers = kept;
    true
}

fn delete_transient(edits: &mut TransientEdits, seconds: f64) -> bool {
    match edits.added.iter().position(|t| near(*t, seconds)) {
        Some(i) => {
            edits.added.remove(i);
        }
        None => edits.removed.push(seconds),
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const BAR: u64 = 3840;

    fn m(seconds: f64, tick: f64) -> WarpMarker {
        WarpMarker { seconds, tick }
    }

    fn warp_of(markers: Vec<WarpMarker>) -> Warp {
        Warp {
            enabled: true,
            markers,
            ..Warp::default()
        }
    }

    fn cx(seconds: f64, transients: &[f64]) -> WarpContext<'_> {
        WarpContext {
            seconds,
            ticks_per_bar: BAR,
            set_bpm: 120.0,
            transients,
        }
    }

    fn run(warp: &mut Warp, op: WarpOp, transients: &[f64]) -> bool {
        let mut length = BAR;
        apply(warp, &op, &cx(2.0, transients), &mut length)
    }

    #[test]
    fn a_marker_is_added_in_order_and_refuses_to_cross_its_neighbours() {
        let mut w = warp_of(vec![m(0.0, 0.0), m(2.0, 3840.0)]);
        assert!(run(
            &mut w,
            WarpOp::Add {
                seconds: 1.0,
                tick: 1920.0
            },
            &[]
        ));
        assert_eq!(w.markers[1], m(1.0, 1920.0));
        assert!(!run(
            &mut w,
            WarpOp::Add {
                seconds: 0.5,
                tick: 3000.0
            },
            &[]
        ));
        assert!(
            !run(
                &mut w,
                WarpOp::Add {
                    seconds: 1.0005,
                    tick: 2000.0
                },
                &[]
            ),
            "too close in the audio"
        );
        assert_eq!(w.markers.len(), 3);
    }

    #[test]
    fn moving_a_marker_stops_short_of_its_neighbours() {
        let mut w = warp_of(vec![m(0.0, 0.0), m(1.0, 960.0), m(2.0, 3840.0)]);
        assert!(run(
            &mut w,
            WarpOp::Move {
                tick: 960.0,
                to: 2000.0
            },
            &[]
        ));
        assert_eq!(w.markers[1], m(1.0, 2000.0));
        run(
            &mut w,
            WarpOp::Move {
                tick: 2000.0,
                to: 99_999.0,
            },
            &[],
        );
        assert_eq!(w.markers[1].tick, 3839.0);
        assert!(!run(
            &mut w,
            WarpOp::Move {
                tick: 1234.0,
                to: 1.0
            },
            &[]
        ));
    }

    #[test]
    fn nudging_moves_a_marker_by_the_step() {
        let mut w = warp_of(vec![m(0.0, 0.0), m(1.0, 960.0), m(2.0, 3840.0)]);
        run(
            &mut w,
            WarpOp::Nudge {
                tick: 960.0,
                by: 240.0,
            },
            &[],
        );
        assert_eq!(w.markers[1].tick, 1200.0);
        run(
            &mut w,
            WarpOp::Nudge {
                tick: 1200.0,
                by: -480.0,
            },
            &[],
        );
        assert_eq!(w.markers[1].tick, 720.0);
    }

    #[test]
    fn sliding_keeps_the_marker_and_moves_the_audio_under_it() {
        let mut w = warp_of(vec![m(0.0, 0.0), m(1.0, 960.0), m(2.0, 3840.0)]);
        assert!(run(
            &mut w,
            WarpOp::Slide {
                tick: 960.0,
                to_seconds: 1.25
            },
            &[]
        ));
        assert_eq!(w.markers[1], m(1.25, 960.0));
        run(
            &mut w,
            WarpOp::Slide {
                tick: 960.0,
                to_seconds: 5.0,
            },
            &[],
        );
        assert!(
            (w.markers[1].seconds - 1.999).abs() < 1e-9,
            "stays before the next"
        );
    }

    #[test]
    fn a_clip_keeps_two_markers() {
        let mut w = warp_of(vec![m(0.0, 0.0), m(1.0, 960.0), m(2.0, 3840.0)]);
        assert!(run(&mut w, WarpOp::Delete { tick: 960.0 }, &[]));
        assert_eq!(w.markers.len(), 2);
        assert!(!run(&mut w, WarpOp::Delete { tick: 0.0 }, &[]));
    }

    #[test]
    fn a_pseudo_marker_becomes_a_marker_with_its_neighbours_on_request() {
        let transients = [0.2, 0.5, 0.75, 1.5];
        let mut w = warp_of(vec![m(0.0, 0.0), m(2.0, 3840.0)]);
        assert!(run(
            &mut w,
            WarpOp::FromTransient {
                seconds: 0.5,
                adjacent: false
            },
            &transients
        ));
        assert_eq!(w.markers, [m(0.0, 0.0), m(0.5, 960.0), m(2.0, 3840.0)]);

        let mut w = warp_of(vec![m(0.0, 0.0), m(2.0, 3840.0)]);
        run(
            &mut w,
            WarpOp::FromTransient {
                seconds: 0.5,
                adjacent: true,
            },
            &transients,
        );
        let at: Vec<f64> = w.markers.iter().map(|m| m.seconds).collect();
        assert_eq!(at, [0.0, 0.2, 0.5, 0.75, 2.0]);
    }

    #[test]
    fn markers_cover_the_transients_of_a_selection_or_its_edges() {
        let transients = [0.2, 0.5, 0.75, 1.5];
        let mut w = warp_of(vec![m(0.0, 0.0), m(2.0, 3840.0)]);
        run(
            &mut w,
            WarpOp::InsertInRange { from: 0.3, to: 0.8 },
            &transients,
        );
        let at: Vec<f64> = w.markers.iter().map(|m| m.seconds).collect();
        assert_eq!(at, [0.0, 0.5, 0.75, 2.0]);

        let mut w = warp_of(vec![m(0.0, 0.0), m(2.0, 3840.0)]);
        run(
            &mut w,
            WarpOp::InsertInRange { from: 0.9, to: 1.2 },
            &transients,
        );
        let at: Vec<f64> = w.markers.iter().map(|m| m.seconds).collect();
        assert_eq!(at, [0.0, 0.9, 1.2, 2.0]);
    }

    #[test]
    fn the_even_loop_assumption_picks_the_length_nearest_the_set_tempo() {
        assert_eq!(suggest_bars(2.0, BAR, 120.0), 1);
        assert_eq!(suggest_bars(4.0, BAR, 120.0), 2);
        assert_eq!(suggest_bars(16.0, BAR, 120.0), 8);
        assert_eq!(suggest_bars(32.0, BAR, 120.0), 16);
        assert_eq!(suggest_bars(1.0, BAR, 120.0), 1);
        assert_eq!(suggest_bars(120.0, BAR, 120.0), 16, "16 at most");
        let m = loop_markers(4.0, 2, BAR);
        assert_eq!(clip_bpm(&warp_of(m)), Some(120.0));
    }

    #[test]
    fn scaling_the_tempo_stretches_the_markers_and_the_loop() {
        let mut w = warp_of(vec![m(0.0, 0.0), m(1.0, 1920.0), m(2.0, 3840.0)]);
        assert_eq!(clip_bpm(&w), Some(120.0));
        let mut length = BAR;
        assert!(apply(
            &mut w,
            &WarpOp::ScaleTempo { factor: 2.0 },
            &cx(2.0, &[]),
            &mut length
        ));
        assert_eq!(w.markers[2].tick, 7680.0);
        assert_eq!(clip_bpm(&w), Some(240.0));
        assert_eq!(length, 2 * BAR);
        apply(
            &mut w,
            &WarpOp::ScaleTempo { factor: 0.5 },
            &cx(2.0, &[]),
            &mut length,
        );
        assert_eq!(clip_bpm(&w), Some(120.0));
        assert_eq!(length, BAR);
    }

    #[test]
    fn typing_a_bpm_scales_the_clip_to_it() {
        let mut w = warp_of(vec![m(0.0, 0.0), m(2.0, 3840.0)]);
        let mut length = BAR;
        apply(
            &mut w,
            &WarpOp::SetBpm { bpm: 90.0 },
            &cx(2.0, &[]),
            &mut length,
        );
        assert!((clip_bpm(&w).unwrap() - 90.0).abs() < 1e-9);
    }

    #[test]
    fn set_one_one_one_makes_a_moment_the_first_beat_and_keeps_the_tempo() {
        let mut w = warp_of(vec![m(0.0, 0.0), m(1.0, 1920.0), m(2.0, 3840.0)]);
        assert!(run(&mut w, WarpOp::SetOneOneOne { seconds: 0.5 }, &[]));
        assert_eq!(w.markers[0], m(0.5, 0.0));
        assert_eq!(w.markers[1], m(1.0, 960.0));
        assert_eq!(w.markers.last().unwrap().tick, 2880.0);
    }

    #[test]
    fn warp_from_here_keeps_what_is_left_of_it() {
        let mut w = warp_of(vec![
            m(0.0, 0.0),
            m(0.5, 960.0),
            m(1.0, 1920.0),
            m(2.0, 3000.0),
        ]);
        let c = cx(2.0, &[]);
        let mut length = BAR;
        assert!(apply(
            &mut w,
            &WarpOp::WarpFrom {
                seconds: 1.0,
                how: WarpFrom::SetTempo
            },
            &c,
            &mut length
        ));
        assert_eq!(
            &w.markers[..3],
            &[m(0.0, 0.0), m(0.5, 960.0), m(1.0, 1920.0)]
        );
        assert_eq!(w.markers.len(), 4, "start and end");
        assert_eq!(w.markers[3], m(2.0, 3840.0));
    }

    #[test]
    fn warp_from_here_straight_uses_the_estimated_tempo() {
        // Four seconds at a Set tempo of 120 is two bars at 120.
        let mut w = warp_of(vec![m(0.0, 0.0), m(4.0, 1000.0)]);
        let mut length = BAR;
        let c = WarpContext {
            seconds: 4.0,
            ticks_per_bar: BAR,
            set_bpm: 120.0,
            transients: &[],
        };
        apply(
            &mut w,
            &WarpOp::WarpFrom {
                seconds: 0.0,
                how: WarpFrom::Straight,
            },
            &c,
            &mut length,
        );
        assert_eq!(w.markers.len(), 2);
        assert!((w.markers[1].tick - 7680.0).abs() < 1e-6);
    }

    #[test]
    fn warp_from_here_snaps_bar_markers_to_transients_nearby() {
        let transients = [2.03, 4.02];
        let mut w = warp_of(vec![m(0.0, 0.0), m(8.0, 15_360.0)]);
        let mut length = 4 * BAR;
        let c = WarpContext {
            seconds: 8.0,
            ticks_per_bar: BAR,
            set_bpm: 120.0,
            transients: &transients,
        };
        apply(
            &mut w,
            &WarpOp::WarpFrom {
                seconds: 0.0,
                how: WarpFrom::Auto,
            },
            &c,
            &mut length,
        );
        assert_eq!(w.markers[1], m(2.03, 3840.0));
        assert_eq!(w.markers[2], m(4.02, 7680.0));
        assert_eq!(w.markers[3], m(6.0, 11_520.0), "no transient: on the bar");
        assert_eq!(w.markers.last().unwrap(), &m(8.0, 15_360.0));
    }

    #[test]
    fn warp_as_fits_a_range_to_whole_bars() {
        let mut w = warp_of(vec![m(0.0, 0.0), m(4.0, 1000.0)]);
        let mut length = 1;
        assert!(apply(
            &mut w,
            &WarpOp::WarpAs {
                from: 1.0,
                to: 3.0,
                bars: 2
            },
            &cx(4.0, &[]),
            &mut length
        ));
        assert_eq!(w.markers, [m(1.0, 0.0), m(3.0, 7680.0)]);
        assert_eq!(length, 2 * BAR);
    }

    #[test]
    fn quantize_pulls_transients_to_the_grid_by_the_amount() {
        let transients = [0.26, 0.74];
        let mut w = warp_of(vec![m(0.0, 0.0), m(2.0, 3840.0)]);
        // 1/4 grid is 960 ticks; 0.26 s is tick 499.2, nearest line 480... at this tempo 1 s is
        // 1920 ticks, so the nearest beat line is 480.
        run(
            &mut w,
            WarpOp::Quantize {
                grid: 480.0,
                amount: 1.0,
                range: None,
            },
            &transients,
        );
        assert_eq!(w.markers[1], m(0.26, 480.0));
        assert_eq!(w.markers[2], m(0.74, 1440.0));

        let mut w = warp_of(vec![m(0.0, 0.0), m(2.0, 3840.0)]);
        run(
            &mut w,
            WarpOp::Quantize {
                grid: 480.0,
                amount: 0.5,
                range: None,
            },
            &transients,
        );
        assert!((w.markers[1].tick - (0.26 * 1920.0 + 480.0) / 2.0).abs() < 1e-9);
    }

    #[test]
    fn quantize_takes_triplet_grids_and_ranges_and_moves_existing_markers() {
        let transients = [0.26, 0.74];
        let grid = 320.0;
        let mut w = warp_of(vec![m(0.0, 0.0), m(0.26, 500.0), m(2.0, 3840.0)]);
        run(
            &mut w,
            WarpOp::Quantize {
                grid,
                amount: 1.0,
                range: Some((0.1, 0.5)),
            },
            &transients,
        );
        assert_eq!(w.markers.len(), 3, "the marker already on it moved");
        assert_eq!(w.markers[1], m(0.26, 640.0));
    }

    #[test]
    fn transient_edits_apply_to_what_was_found() {
        let found = [0.1, 0.5, 1.0];
        let mut edits = TransientEdits::default();
        let mut w = warp_of(vec![m(0.0, 0.0), m(2.0, 3840.0)]);
        run(&mut w, WarpOp::AddTransient { seconds: 0.7 }, &found);
        run(&mut w, WarpOp::DeleteTransient { seconds: 0.5 }, &found);
        edits.clone_from(&w.transients);
        assert_eq!(resolve_transients(&found, &edits), vec![0.1, 0.7, 1.0]);

        run(&mut w, WarpOp::MoveTransient { from: 0.7, to: 0.8 }, &found);
        assert_eq!(
            resolve_transients(&found, &w.transients),
            vec![0.1, 0.8, 1.0]
        );

        assert!(run(&mut w, WarpOp::ResetTransients, &found));
        assert_eq!(resolve_transients(&found, &w.transients), found);
        assert!(
            !run(&mut w, WarpOp::ResetTransients, &found),
            "nothing left"
        );
    }

    #[test]
    fn a_warp_round_trips_in_a_project_file_with_its_edits() {
        let mut w = warp_of(vec![m(0.5, 0.0), m(2.0, 3840.0)]);
        w.transients.added.push(1.25);
        let text = ron::to_string(&w).unwrap();
        let back: Warp = ron::from_str(&text).unwrap();
        assert_eq!(back, w);
    }
}
