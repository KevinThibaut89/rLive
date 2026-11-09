//! The breakpoint envelope Shaper and Shaper MIDI share.
//!
//! A shape is a device's own saved state: plain text the device writes and reads itself. It is
//! handed to the audio thread already parsed, through the sample-player state slot.

use crate::dsp::envelope::slope_curve;
use std::sync::Arc;

/// Text format version.
const V1: &str = "v1";
/// A hard limit on how many breakpoints a shape can have.
pub const MAX_POINTS: usize = 64;

/// One breakpoint. Time is 0 to 1 across the envelope, value -1 to 1. `curve` bends the segment
/// that starts here, -1 to 1, with 0 a straight line.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub time: f32,
    pub value: f32,
    pub curve: f32,
}

/// A whole envelope, sorted by time.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Shape {
    pub points: Vec<Point>,
    /// The sustain breakpoint, on Shaper MIDI.
    pub sustain: Option<usize>,
}

/// The six presets a Shaper offers (manual p. 847). The manual doesn't say what they are, so
/// these are ours.
pub const PRESETS: [&str; 6] = ["Saw", "Ramp", "Triangle", "Pulse", "Hump", "Steps"];

impl Shape {
    /// A flat line at zero.
    pub fn flat() -> Self {
        Self {
            points: vec![
                Point {
                    time: 0.0,
                    value: 0.0,
                    curve: 0.0,
                },
                Point {
                    time: 1.0,
                    value: 0.0,
                    curve: 0.0,
                },
            ],
            sustain: None,
        }
    }

    /// One of [`PRESETS`], by position.
    pub fn preset(n: usize) -> Self {
        let p = |time, value| Point {
            time,
            value,
            curve: 0.0,
        };
        let points = match n {
            0 => vec![p(0.0, 1.0), p(1.0, -1.0)],
            1 => vec![p(0.0, -1.0), p(1.0, 1.0)],
            2 => vec![p(0.0, -1.0), p(0.5, 1.0), p(1.0, -1.0)],
            3 => vec![p(0.0, 1.0), p(0.5, 1.0), p(0.5, -1.0), p(1.0, -1.0)],
            4 => vec![
                Point {
                    time: 0.0,
                    value: -1.0,
                    curve: 0.6,
                },
                p(0.5, 1.0),
                Point {
                    time: 0.5,
                    value: 1.0,
                    curve: -0.6,
                },
                p(1.0, -1.0),
            ],
            _ => vec![
                p(0.0, -1.0),
                p(0.25, -1.0),
                p(0.25, -0.33),
                p(0.5, -0.33),
                p(0.5, 0.33),
                p(0.75, 0.33),
                p(0.75, 1.0),
                p(1.0, 1.0),
            ],
        };
        Self {
            points,
            sustain: None,
        }
    }

    /// Puts the points in order and within bounds, keeping the sustain point on the same one.
    pub fn normalized(mut self) -> Self {
        let sustain_at = self.sustain.and_then(|i| self.points.get(i).copied());
        for p in &mut self.points {
            p.time = p.time.clamp(0.0, 1.0);
            p.value = p.value.clamp(-1.0, 1.0);
            p.curve = p.curve.clamp(-1.0, 1.0);
        }
        self.points.sort_by(|a, b| {
            a.time
                .partial_cmp(&b.time)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        self.points.truncate(MAX_POINTS);
        self.sustain = sustain_at.and_then(|s| {
            self.points
                .iter()
                .position(|p| p.time == s.time && p.value == s.value)
        });
        self
    }

    /// Saved form.
    pub fn to_text(&self) -> String {
        let mut out = String::from(V1);
        out.push('|');
        for (i, p) in self.points.iter().enumerate() {
            if i > 0 {
                out.push(';');
            }
            out.push_str(&format!("{},{},{}", p.time, p.value, p.curve));
        }
        if let Some(s) = self.sustain {
            out.push_str(&format!("|{s}"));
        }
        out
    }

    /// Reads a saved form. `None` if it doesn't look like one.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split('|');
        if parts.next()? != V1 {
            return None;
        }
        let points = parts
            .next()?
            .split(';')
            .filter(|s| !s.is_empty())
            .map(|s| {
                let mut f = s.split(',').map(|x| x.parse::<f32>());
                let time = f.next()?.ok()?;
                let value = f.next()?.ok()?;
                let curve = f.next().and_then(Result::ok).unwrap_or(0.0);
                Some(Point { time, value, curve })
            })
            .collect::<Option<Vec<_>>>()?;
        let sustain = parts.next().and_then(|s| s.parse().ok());
        Some(Self { points, sustain }.normalized())
    }

    /// The envelope's value at `time`, 0 to 1.
    #[inline]
    pub fn value_at(&self, time: f32) -> f32 {
        let pts = &self.points;
        let Some(first) = pts.first() else {
            return 0.0;
        };
        let last = *pts.last().unwrap_or(first);
        if time <= first.time {
            return first.value;
        }
        if time >= last.time {
            return last.value;
        }
        let i = pts.partition_point(|p| p.time <= time);
        let a = pts[i - 1];
        let b = pts[i];
        if b.time <= a.time {
            return b.value;
        }
        let t = (time - a.time) / (b.time - a.time);
        a.value + (b.value - a.value) * slope_curve(t, a.curve)
    }
}

/// A shape as the audio thread keeps it.
#[derive(Clone, Debug)]
pub struct ParsedShape(pub Arc<Shape>);

impl ParsedShape {
    pub fn parse(text: Option<&str>) -> Self {
        Self(Arc::new(
            text.and_then(Shape::parse).unwrap_or_else(Shape::flat),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_round_trips() {
        let mut s = Shape::preset(2);
        s.sustain = Some(1);
        assert_eq!(Shape::parse(&s.to_text()), Some(s));
        assert_eq!(Shape::parse("garbage"), None);
    }

    #[test]
    fn it_interpolates_and_holds_past_the_ends() {
        let s = Shape::preset(2);
        assert_eq!(s.value_at(-0.5), -1.0);
        assert_eq!(s.value_at(0.25), 0.0);
        assert_eq!(s.value_at(0.5), 1.0);
        assert_eq!(s.value_at(2.0), -1.0);
    }

    #[test]
    fn sorting_keeps_the_sustain_point() {
        let p = |time, value| Point {
            time,
            value,
            curve: 0.0,
        };
        let s = Shape {
            points: vec![p(0.8, 0.5), p(0.2, -0.5)],
            sustain: Some(0),
        }
        .normalized();
        assert_eq!(s.sustain, Some(1));
        assert_eq!(s.points[1].time, 0.8);
    }

    #[test]
    fn a_step_in_time_jumps_at_once() {
        let s = Shape::preset(3);
        assert_eq!(s.value_at(0.499), 1.0);
        assert_eq!(s.value_at(0.501), -1.0);
    }

    #[test]
    fn every_preset_is_within_bounds() {
        for n in 0..PRESETS.len() {
            let s = Shape::preset(n).normalized();
            assert!(s.points.iter().all(|p| p.value.abs() <= 1.0));
            assert_eq!(s.points.first().unwrap().time, 0.0);
            assert_eq!(s.points.last().unwrap().time, 1.0);
        }
    }
}
