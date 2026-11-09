//! The Transformations about when and how long notes are: Quantize, Span, Strum, Time Warp, and
//! Velocity Shaper, which shapes velocity over time (manual pp. 309-321).

use super::{run_quantize, snap_tick, with_start, ToolContext};
use crate::note_edit::{self, Quantize};
use crate::{Breakpoint, NoteData};
use rlive_core::envelope::{curve_shape, value_at, Interp};
use rlive_core::PPQ;

// ------------------------------------------------------------- Quantize

/// Quantize: the settings the Edit menu's Quantize command also uses (manual p. 309).
#[derive(Clone, Debug, PartialEq)]
pub struct QuantizeTool {
    /// Follow the current grid rather than `settings.grid`.
    pub use_grid: bool,
    pub settings: Quantize,
}

impl Default for QuantizeTool {
    fn default() -> Self {
        Self {
            use_grid: false,
            settings: Quantize {
                grid: PPQ / 4,
                amount: 1.0,
                starts: true,
                ends: false,
            },
        }
    }
}

impl QuantizeTool {
    pub(super) fn run(&self, input: &[NoteData], cx: &ToolContext) -> Vec<NoteData> {
        let q = Quantize {
            grid: if self.use_grid {
                cx.grid
            } else {
                self.settings.grid
            },
            ..self.settings
        };
        run_quantize(input, q)
    }
}

// ------------------------------------------------------------- Span

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Articulation {
    Legato,
    Tenuto,
    Staccato,
}

/// Span: sets note lengths by articulation (manual pp. 313-315).
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub articulation: Articulation,
    /// Added to each length: -1 to 1 of a grid step.
    pub offset: f64,
    /// Random change of length: -1 to 1. Above zero shortens, below zero lengthens.
    pub variation: f64,
}

impl Default for Span {
    fn default() -> Self {
        Self {
            articulation: Articulation::Tenuto,
            offset: 0.0,
            variation: 0.0,
        }
    }
}

impl Span {
    pub(super) fn run(&self, input: &[NoteData], cx: &ToolContext) -> Vec<NoteData> {
        let mut out = input.to_vec();
        out.sort_by_key(|n| (n.start, n.key));
        let starts: Vec<u64> = {
            let mut s: Vec<u64> = out.iter().map(|n| n.start).collect();
            s.dedup();
            s
        };
        let shortest = starts.windows(2).map(|w| w[1] - w[0]).min();
        let mut rng = cx.rng();
        for n in &mut out {
            let base = match self.articulation {
                Articulation::Tenuto => n.length,
                Articulation::Staccato => shortest.map_or(n.length, |d| (d / 2).max(1)),
                Articulation::Legato => {
                    let next = starts.iter().find(|s| **s > n.start).copied();
                    next.unwrap_or(cx.end.max(n.start + 1)) - n.start
                }
            };
            let mut length = base as f64 + self.offset.clamp(-1.0, 1.0) * cx.grid();
            let v = self.variation.clamp(-1.0, 1.0);
            if v != 0.0 {
                length *= 1.0 - v * rng.unit();
            }
            n.length = (length.round() as u64).max(1);
        }
        out
    }
}

// ------------------------------------------------------------- Strum

/// Strum: spreads the notes of a chord in time (manual pp. 315-317).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Strum {
    /// How far the lowest note moves: -1 to 1 of a grid step.
    pub low: f64,
    /// How far the highest note moves.
    pub high: f64,
    /// -1 to 1. Above zero the notes are furthest apart at the start.
    pub tension: f64,
}

impl Strum {
    pub(super) fn run(&self, input: &[NoteData], cx: &ToolContext) -> Vec<NoteData> {
        // Chords are notes starting at about the same time; the first says when.
        let reach = (cx.grid.max(1) / 4).max(1);
        let mut sorted = input.to_vec();
        sorted.sort_by_key(|n| n.start);
        let mut chords: Vec<Vec<NoteData>> = Vec::new();
        for n in sorted {
            match chords.last_mut() {
                Some(c) if n.start - c[0].start < reach => c.push(n),
                _ => chords.push(vec![n]),
            }
        }
        let mut out = Vec::with_capacity(input.len());
        for mut chord in chords {
            chord.sort_by_key(|n| n.key);
            let origin = chord[0].start as f64;
            let top = (chord.len() - 1).max(1) as f64;
            for (i, n) in chord.iter().enumerate() {
                let x = curve_shape(i as f64 / top, -self.tension.clamp(-1.0, 1.0) as f32);
                let moved = (self.low + (self.high - self.low) * x) * cx.grid();
                let start = (origin + moved).round().max(0.0) as u64;
                out.push(with_start(n, start));
            }
        }
        out
    }
}

// ------------------------------------------------------------- Time Warp

/// A breakpoint of Time Warp's speed curve.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WarpPoint {
    pub on: bool,
    /// Where on the range, 0 to 1.
    pub time: f64,
    /// How fast the notes run there: 2 is twice as fast, so everything is closer together.
    pub speed: f64,
}

/// Time Warp: moves notes along a speed curve (manual pp. 317-319).
#[derive(Clone, Debug, PartialEq)]
pub struct TimeWarp {
    pub points: [WarpPoint; 3],
    pub quantize: bool,
    pub preserve_range: bool,
    pub include_note_end: bool,
}

impl Default for TimeWarp {
    fn default() -> Self {
        Self {
            points: [
                WarpPoint {
                    on: true,
                    time: 0.5,
                    speed: 1.0,
                },
                WarpPoint {
                    on: false,
                    time: 0.25,
                    speed: 1.0,
                },
                WarpPoint {
                    on: false,
                    time: 0.75,
                    speed: 1.0,
                },
            ],
            quantize: false,
            preserve_range: true,
            include_note_end: false,
        }
    }
}

/// How many steps the integral of the speed curve is taken in.
const WARP_STEPS: usize = 512;

impl TimeWarp {
    /// The speed curve's breakpoints in use, in order of time.
    fn curve(&self) -> Vec<Breakpoint> {
        let mut pts: Vec<Breakpoint> = self
            .points
            .iter()
            .filter(|p| p.on)
            .map(|p| Breakpoint::new(p.time.clamp(0.0, 1.0), p.speed.clamp(0.1, 10.0)))
            .collect();
        pts.sort_by(|a, b| a.time.total_cmp(&b.time));
        pts
    }

    /// Where each of `WARP_STEPS + 1` even positions along the range lands, as a share of it.
    fn table(&self) -> Vec<f64> {
        let curve = self.curve();
        let speed = |x: f64| value_at(&curve, x, Interp::Smooth).unwrap_or(1.0);
        let dx = 1.0 / WARP_STEPS as f64;
        let mut table = Vec::with_capacity(WARP_STEPS + 1);
        let mut at = 0.0;
        table.push(0.0);
        for i in 0..WARP_STEPS {
            let (a, b) = (i as f64 * dx, (i + 1) as f64 * dx);
            at += dx * 0.5 * (1.0 / speed(a) + 1.0 / speed(b));
            table.push(at);
        }
        if self.preserve_range && at > 0.0 {
            for v in &mut table {
                *v /= at;
            }
        }
        table
    }

    pub(super) fn run(&self, input: &[NoteData], cx: &ToolContext) -> Vec<NoteData> {
        let span = cx.span();
        if span == 0 {
            return input.to_vec();
        }
        let table = self.table();
        let (from, len) = (cx.range.0 as f64, span as f64);
        let warp = |t: f64| {
            let x = ((t - from) / len).clamp(0.0, 1.0) * WARP_STEPS as f64;
            let i = (x.floor() as usize).min(WARP_STEPS - 1);
            let v = table[i] + (table[i + 1] - table[i]) * (x - i as f64);
            from + v * len
        };
        let mut out = input.to_vec();
        for n in &mut out {
            let start = warp(n.start as f64);
            let tick = if self.quantize {
                snap_tick(start, cx.grid)
            } else {
                start.round().max(0.0) as u64
            };
            if self.include_note_end {
                let end = warp((n.start + n.length) as f64);
                let length = ((end - start).round() as u64).max(1);
                note_edit::stretch_expression(n, length as f64 / n.length as f64);
                n.length = length;
            }
            n.start = tick;
        }
        out
    }
}

// ------------------------------------------------------------- Velocity Shaper

/// Velocity Shaper: shapes velocities with an envelope over the span (manual pp. 319-321).
#[derive(Clone, Debug, PartialEq)]
pub struct VelocityShaper {
    /// Position and value of each breakpoint, all 0 to 1, in order.
    pub envelope: Vec<(f64, f64)>,
    /// Velocities are kept between these, 0 to 1.
    pub min: f32,
    pub max: f32,
    /// How many times the shape runs across the notes.
    pub loops: u32,
    /// Steps the shape moves to the right, `division` ticks each.
    pub rotate: i32,
    /// `None` is the grid.
    pub division: Option<u64>,
}

impl Default for VelocityShaper {
    fn default() -> Self {
        Self {
            envelope: vec![(0.0, 0.0), (1.0, 1.0)],
            min: 0.2,
            max: 1.0,
            loops: 1,
            rotate: 0,
            division: None,
        }
    }
}

impl VelocityShaper {
    pub(super) fn run(&self, input: &[NoteData], cx: &ToolContext) -> Vec<NoteData> {
        let span = cx.span().max(1) as f64;
        let curve: Vec<Breakpoint> = self
            .envelope
            .iter()
            .map(|&(x, y)| Breakpoint::new(x, y))
            .collect();
        let step = self.division.unwrap_or(cx.grid.max(1)) as f64;
        let shift = f64::from(self.rotate) * step / span;
        let loops = f64::from(self.loops.max(1));
        let (lo, hi) = (self.min.min(self.max), self.min.max(self.max));
        let mut out = input.to_vec();
        for n in &mut out {
            let x = (n.start.saturating_sub(cx.range.0) as f64 / span - shift).rem_euclid(1.0);
            let y = value_at(&curve, (x * loops).rem_euclid(1.0), Interp::Smooth).unwrap_or(1.0);
            n.velocity = (lo + (hi - lo) * y as f32).clamp(0.0, 1.0);
        }
        out
    }
}
