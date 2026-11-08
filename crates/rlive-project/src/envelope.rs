//! Breakpoint envelopes: track automation, clip envelopes and per-note expression.

use crate::defaults::is_default;
use rlive_core::envelope::{self, EnvPoint, Interp};
use rlive_core::param::EnvTarget;
use rlive_core::ParamRange;
use serde::{Deserialize, Serialize};

/// How far a recorded pass may stray from what the user did, as a share of the knob's range.
const THIN_TOLERANCE: f64 = 0.002;

/// A point on an envelope.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Breakpoint {
    /// Position in ticks. Two points at the same position make a step.
    pub time: f64,
    /// In the parameter's plain units. Segments run straight in knob position.
    pub value: f64,
    /// Shape of the segment to the next point: 0 is straight, above 0 it starts slow and below 0
    /// it starts fast. The exact shape is in [`rlive_core::envelope`].
    #[serde(default, skip_serializing_if = "is_default")]
    pub curve: f32,
}

impl Breakpoint {
    pub fn new(time: f64, value: f64) -> Self {
        Self {
            time,
            value,
            curve: 0.0,
        }
    }
}

impl EnvPoint for Breakpoint {
    fn time(&self) -> f64 {
        self.time
    }
    fn value(&self) -> f64 {
        self.value
    }
    fn curve(&self) -> f32 {
        self.curve
    }
}

/// `points` read in knob position, which is the space segments run straight in.
fn as_knob(points: &[Breakpoint], range: ParamRange) -> Vec<Breakpoint> {
    points
        .iter()
        .map(|p| Breakpoint {
            value: range.to_normalized(p.value),
            ..*p
        })
        .collect()
}

/// Drops the points of a recorded stroke that make no audible difference, leaving straight lines
/// between the rest. A stepped parameter only keeps the points where its value changes.
pub fn thin_recorded(stroke: &[Breakpoint], range: ParamRange) -> Vec<Breakpoint> {
    if stroke.len() < 3 {
        return stroke.to_vec();
    }
    if range.quantized {
        let mut kept = vec![stroke[0]];
        for p in &stroke[1..] {
            if p.value != kept[kept.len() - 1].value {
                kept.push(*p);
            }
        }
        return kept;
    }
    let knob = as_knob(stroke, range);
    let mut keep = vec![false; knob.len()];
    keep[0] = true;
    keep[knob.len() - 1] = true;
    // Douglas-Peucker on the vertical error, with a stack so a long stroke can't overflow.
    let mut spans = vec![(0, knob.len() - 1)];
    while let Some((from, to)) = spans.pop() {
        let (a, b) = (knob[from], knob[to]);
        let mut worst = (0.0, from);
        for (i, p) in knob.iter().enumerate().take(to).skip(from + 1) {
            let x = if b.time > a.time {
                (p.time - a.time) / (b.time - a.time)
            } else {
                1.0
            };
            let error = (p.value - envelope::segment_value(a.value, b.value, x, 0.0)).abs();
            if error > worst.0 {
                worst = (error, i);
            }
        }
        if worst.0 > THIN_TOLERANCE {
            keep[worst.1] = true;
            spans.push((from, worst.1));
            spans.push((worst.1, to));
        }
    }
    stroke
        .iter()
        .zip(&keep)
        .filter(|(_, keep)| **keep)
        .map(|(p, _)| *p)
        .collect()
}

/// An automation envelope: `points` sorted by time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub target: EnvTarget,
    #[serde(default, skip_serializing_if = "is_default")]
    pub points: Vec<Breakpoint>,
}

impl Envelope {
    /// The envelope's value at `tick`, in plain units.
    pub fn value_at(&self, tick: f64, range: ParamRange) -> Option<f64> {
        let interp = Interp::of(range.quantized);
        let knob =
            envelope::value_at_mapped(&self.points, tick, interp, |v| range.to_normalized(v))?;
        Some(range.from_normalized(knob))
    }

    /// Replaces the stretch of the envelope a recorded `stroke` covers with that stroke. Outside
    /// it, the envelope keeps the shape it had, even where the cut falls inside a curved segment.
    /// A new envelope holds `before` up to the stroke, and the stroke's end after it.
    pub fn overwrite(&mut self, stroke: &[Breakpoint], before: f64, range: ParamRange) {
        let (Some(first), Some(last)) = (stroke.first().copied(), stroke.last().copied()) else {
            return;
        };
        let thinned = thin_recorded(stroke, range);
        if self.points.is_empty() {
            if first.time > 0.0 {
                self.points.push(Breakpoint::new(0.0, before));
                if before != first.value {
                    self.points.push(Breakpoint::new(first.time, before));
                }
            }
            self.points.extend(thinned);
            return;
        }
        let interp = Interp::of(range.quantized);
        let knob = as_knob(&self.points, range);
        let head = self.points.partition_point(|p| p.time < first.time);
        let tail = self.points.partition_point(|p| p.time <= last.time);
        let mut merged = Vec::with_capacity(self.points.len() + thinned.len() + 2);
        merged.extend_from_slice(&self.points[..head]);

        // Arrive at the stroke the way the old envelope did.
        let arrive = if head == 0 {
            // Before its first point an envelope reads that point, which it must go on doing.
            (first.time > 0.0).then(|| self.points[0].value)
        } else if interp == Interp::Stepped {
            None
        } else {
            let a = knob[head - 1];
            Some(knob.get(head).map_or(self.points[head - 1].value, |b| {
                let x = (first.time - a.time) / (b.time - a.time);
                merged[head - 1].curve = envelope::split_curve(a.curve, x).0;
                range.from_normalized(envelope::segment_value(a.value, b.value, x, a.curve))
            }))
        };
        if let Some(value) = arrive.filter(|v| *v != first.value) {
            merged.push(Breakpoint::new(first.time, value));
        }
        merged.extend(thinned);

        // Leave it where the old envelope would have been by then.
        let leave = envelope::value_at(&knob, last.time, interp).map(|v| range.from_normalized(v));
        if let Some(value) = leave.filter(|v| *v != last.value) {
            let mut resume = Breakpoint::new(last.time, value);
            if let (true, Some(a), Some(b)) =
                (tail > 0, knob.get(tail.wrapping_sub(1)), knob.get(tail))
            {
                let x = (last.time - a.time) / (b.time - a.time);
                resume.curve = envelope::split_curve(a.curve, x).1;
            }
            merged.push(resume);
        }
        merged.extend_from_slice(&self.points[tail..]);
        self.points = merged;
    }
}

/// How a clip envelope applies to its target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum EnvMode {
    /// Offsets the target, on top of what automation sets. The only mode in the Arrangement.
    #[default]
    Modulation,
    /// Sets the target's absolute value while the clip plays.
    Automation,
}

/// A clip envelope. With `loop_region` it is unlinked: it loops by itself instead of following
/// the clip's loop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipEnvelope {
    pub envelope: Envelope,
    #[serde(default, skip_serializing_if = "is_default")]
    pub mode: EnvMode,
    #[serde(default, skip_serializing_if = "is_default")]
    pub loop_region: Option<EnvLoop>,
}

/// The loop of an unlinked clip envelope, in ticks of the clip's content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvLoop {
    pub start: u64,
    pub length: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINEAR: ParamRange = ParamRange {
        min: 0.0,
        max: 1.0,
        skew: 1.0,
        quantized: false,
    };
    const TOGGLE: ParamRange = ParamRange {
        min: 0.0,
        max: 1.0,
        skew: 1.0,
        quantized: true,
    };

    fn target() -> EnvTarget {
        EnvTarget::MidiCtrl(1)
    }

    fn envelope(points: &[(f64, f64)]) -> Envelope {
        Envelope {
            target: target(),
            points: points.iter().map(|&(t, v)| Breakpoint::new(t, v)).collect(),
        }
    }

    fn points(e: &Envelope) -> Vec<(f64, f64)> {
        e.points.iter().map(|p| (p.time, p.value)).collect()
    }

    #[test]
    fn thinning_a_ramp_leaves_its_ends() {
        let stroke: Vec<_> = (0..=100)
            .map(|i| Breakpoint::new(i as f64, i as f64 / 100.0))
            .collect();
        assert_eq!(
            thin_recorded(&stroke, LINEAR),
            [stroke[0], stroke[stroke.len() - 1]]
        );
    }

    #[test]
    fn thinning_keeps_the_corners() {
        // Up, then down: nothing near the peak can go.
        let stroke: Vec<_> = (0..=100)
            .map(|i| Breakpoint::new(i as f64, 1.0 - (i as f64 - 50.0).abs() / 50.0))
            .collect();
        let kept = thin_recorded(&stroke, LINEAR);
        assert_eq!(kept.len(), 3, "{kept:?}");
        assert_eq!(kept[1].time, 50.0);
    }

    #[test]
    fn thinning_stays_within_a_fraction_of_the_range() {
        let stroke: Vec<_> = (0..=500)
            .map(|i| {
                let t = i as f64;
                Breakpoint::new(t, 0.5 + 0.4 * (t * 0.05).sin())
            })
            .collect();
        let kept = thin_recorded(&stroke, LINEAR);
        assert!(kept.len() < stroke.len() / 2, "{}", kept.len());
        let e = Envelope {
            target: target(),
            points: kept,
        };
        for p in &stroke {
            let v = e.value_at(p.time, LINEAR).unwrap();
            assert!(
                (v - p.value).abs() <= THIN_TOLERANCE * 1.5,
                "{v} {}",
                p.value
            );
        }
    }

    #[test]
    fn thinning_a_stepped_parameter_keeps_only_changes() {
        let stroke: Vec<_> = [0.0, 0.0, 1.0, 1.0, 1.0, 0.0, 0.0]
            .iter()
            .enumerate()
            .map(|(i, v)| Breakpoint::new(i as f64, *v))
            .collect();
        let kept = thin_recorded(&stroke, TOGGLE);
        let times: Vec<f64> = kept.iter().map(|p| p.time).collect();
        assert_eq!(times, [0.0, 2.0, 5.0]);
    }

    #[test]
    fn a_new_envelope_holds_the_old_value_up_to_the_stroke() {
        let mut e = envelope(&[]);
        e.overwrite(
            &[Breakpoint::new(100.0, 0.8), Breakpoint::new(200.0, 0.8)],
            0.2,
            LINEAR,
        );
        assert_eq!(
            points(&e),
            [(0.0, 0.2), (100.0, 0.2), (100.0, 0.8), (200.0, 0.8)]
        );
    }

    #[test]
    fn a_stroke_from_the_very_start_needs_nothing_before_it() {
        let mut e = envelope(&[]);
        e.overwrite(
            &[Breakpoint::new(0.0, 0.8), Breakpoint::new(200.0, 0.9)],
            0.2,
            LINEAR,
        );
        assert_eq!(points(&e), [(0.0, 0.8), (200.0, 0.9)]);
    }

    #[test]
    fn overwriting_cuts_in_and_out_of_what_was_there() {
        let mut e = envelope(&[(0.0, 0.0), (400.0, 1.0)]);
        e.overwrite(
            &[Breakpoint::new(100.0, 0.9), Breakpoint::new(300.0, 0.9)],
            0.0,
            LINEAR,
        );
        assert_eq!(
            points(&e),
            [
                (0.0, 0.0),
                (100.0, 0.25),
                (100.0, 0.9),
                (300.0, 0.9),
                (300.0, 0.75),
                (400.0, 1.0)
            ]
        );
        assert_eq!(e.value_at(50.0, LINEAR), Some(0.125));
        assert_eq!(e.value_at(350.0, LINEAR), Some(0.875));
    }

    #[test]
    fn overwriting_replaces_points_inside_the_stroke() {
        let mut e = envelope(&[(0.0, 0.0), (150.0, 1.0), (200.0, 0.0), (400.0, 1.0)]);
        e.overwrite(
            &[Breakpoint::new(100.0, 0.6), Breakpoint::new(300.0, 0.6)],
            0.0,
            LINEAR,
        );
        let times: Vec<f64> = e.points.iter().map(|p| p.time).collect();
        assert_eq!(times, [0.0, 100.0, 100.0, 300.0, 300.0, 400.0]);
    }

    #[test]
    fn cutting_into_a_curved_segment_leaves_the_rest_unchanged() {
        let mut e = envelope(&[(0.0, 0.0), (400.0, 1.0)]);
        e.points[0].curve = 0.7;
        let original = e.clone();
        e.overwrite(
            &[Breakpoint::new(100.0, 0.9), Breakpoint::new(300.0, 0.9)],
            0.0,
            LINEAR,
        );
        for t in [0.0, 25.0, 50.0, 99.0, 301.0, 350.0, 399.0, 400.0] {
            let (a, b) = (e.value_at(t, LINEAR), original.value_at(t, LINEAR));
            assert!(
                (a.unwrap() - b.unwrap()).abs() < 1e-5,
                "at {t}: {a:?} {b:?}"
            );
        }
    }

    #[test]
    fn a_later_pass_replaces_what_an_earlier_one_recorded() {
        let mut e = envelope(&[(0.0, 0.5), (400.0, 0.5)]);
        let stroke = [Breakpoint::new(100.0, 0.9), Breakpoint::new(200.0, 0.9)];
        e.overwrite(&stroke, 0.0, LINEAR);
        let again = [Breakpoint::new(100.0, 0.1), Breakpoint::new(200.0, 0.1)];
        e.overwrite(&again, 0.0, LINEAR);
        assert_eq!(e.value_at(150.0, LINEAR), Some(0.1));
        assert_eq!(e.value_at(50.0, LINEAR), Some(0.5));
        assert_eq!(e.value_at(300.0, LINEAR), Some(0.5));
    }

    #[test]
    fn a_stepped_envelope_needs_no_point_to_arrive_at_the_stroke() {
        let mut e = envelope(&[(0.0, 0.0), (400.0, 1.0)]);
        e.overwrite(
            &[Breakpoint::new(100.0, 1.0), Breakpoint::new(200.0, 0.0)],
            0.0,
            TOGGLE,
        );
        assert_eq!(e.value_at(50.0, TOGGLE), Some(0.0));
        assert_eq!(e.value_at(150.0, TOGGLE), Some(1.0));
        assert_eq!(e.value_at(250.0, TOGGLE), Some(0.0));
        assert_eq!(e.value_at(400.0, TOGGLE), Some(1.0));
    }

    #[test]
    fn a_skewed_parameter_runs_straight_in_knob_position() {
        let range = ParamRange {
            min: 20.0,
            max: 20_000.0,
            skew: 3.0,
            quantized: false,
        };
        let e = envelope(&[(0.0, 20.0), (100.0, 20_000.0)]);
        let v = e.value_at(50.0, range).unwrap();
        assert!(
            (range.to_normalized(v) - 0.5).abs() < 1e-9,
            "halfway on the knob, not in Hz: {v}"
        );
    }
}
