//! MIDI Tools (manual ch. 11): Transformations, which rework the notes they are given, and
//! Generators, which make new ones. Every tool is a pure function of its settings, the notes and
//! a [`ToolContext`], whose seed decides anything random, so the same input gives the same result.

mod arpeggiate;
mod chop;
mod connect;
mod euclidean;
mod expression;
mod ornament;
mod pitch_gen;
mod recombine;
mod rhythm;
mod stacks;
mod timing;

pub use arpeggiate::{ArpStyle, Arpeggiate};
pub use chop::Chop;
pub use connect::Connect;
pub use euclidean::{EuclidVoice, Euclidean};
pub use expression::{lfo_balance, Glissando, Lfo, LfoShape, LfoTarget};
pub use ornament::{GracePitch, Ornament, OrnamentKind};
pub use pitch_gen::{Seed, Shape, ShapePreset, SHAPE_POINTS};
pub use recombine::{Dimension, Recombine};
pub use rhythm::{pattern_count, Rhythm};
pub use stacks::{parse_bank, tonnetz, ChordBank, ChordDef, StackChord, Stacks};
pub use timing::{Articulation, QuantizeTool, Span, Strum, TimeWarp, VelocityShaper, WarpPoint};

use crate::clip_edit::{self, PitchScale};
use crate::note_edit::{self, Quantize};
use crate::{Breakpoint, NoteData, NoteExpression};
use rlive_core::envelope::split_curve;

// ------------------------------------------------------------- randomness

/// The tools' random numbers: the same seed always gives the same stream.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        // Xorshift has nothing to work with at 0, and close seeds would start out alike.
        Self(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x2545_f491_4f6c_dd1d | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// From 0 up to, but not including, 1.
    pub fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// From -1 to 1.
    pub fn signed(&mut self) -> f64 {
        self.unit() * 2.0 - 1.0
    }

    /// A whole number below `n`, which must not be 0.
    pub fn below(&mut self, n: usize) -> usize {
        (self.unit() * n as f64) as usize
    }

    pub fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
    }

    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            items.swap(i, self.below(i + 1));
        }
    }
}

// ------------------------------------------------------------- where a tool works

/// What a tool knows beyond its settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToolContext {
    /// The span it works over: a time selection, what the notes cover, or the loop.
    pub range: (u64, u64),
    /// Where Legato takes the last note: the end of the time selection, or of the loop.
    pub end: u64,
    /// The editor's grid, for tools that count in grid steps.
    pub grid: u64,
    /// The clip's scale, if it's on: pitch settings then count in degrees.
    pub scale: Option<PitchScale>,
    pub seed: u64,
    /// How far a pitch envelope can bend either way, in semitones.
    pub bend_range: f64,
}

impl ToolContext {
    pub(crate) fn grid(&self) -> f64 {
        self.grid.max(1) as f64
    }

    pub(crate) fn rng(&self) -> Rng {
        Rng::new(self.seed)
    }

    pub(crate) fn span(&self) -> u64 {
        self.range.1.saturating_sub(self.range.0)
    }
}

/// The notes a Transformation takes and the span it works over.
#[derive(Clone, Debug, PartialEq)]
pub struct Scope {
    pub picks: Vec<usize>,
    pub range: (u64, u64),
    pub end: u64,
}

/// What a Transformation applies to: the selected notes, or those starting in the time selection,
/// or those starting in the loop when nothing is selected (manual p. 295). `time_only` says the
/// time span has taken over from the notes in it.
pub fn transform_scope(
    notes: &[NoteData],
    selected: &[usize],
    time: Option<(u64, u64)>,
    time_only: bool,
    loop_range: (u64, u64),
) -> Scope {
    let time = time.filter(|(a, b)| b > a);
    let picks: Vec<usize> = selected
        .iter()
        .copied()
        .filter(|i| *i < notes.len())
        .collect();
    if !time_only {
        if let Some(span) = note_edit::span_of(notes, &picks) {
            // A time selection the notes have nothing to do with doesn't bound them.
            let around = time.filter(|t| t.0 <= span.0 && span.1 <= t.1);
            return Scope {
                picks,
                range: around.unwrap_or(span),
                end: around.map_or(loop_range.1, |t| t.1),
            };
        }
    }
    let (from, to) = time.unwrap_or(loop_range);
    let picks = (0..notes.len())
        .filter(|i| (from..to).contains(&notes[*i].start))
        .collect();
    Scope {
        picks,
        range: (from, to),
        end: to,
    }
}

/// Where a Generator puts its notes: the time selection, or the loop (manual p. 295).
pub fn generate_range(time: Option<(u64, u64)>, loop_range: (u64, u64)) -> (u64, u64) {
    time.filter(|(a, b)| b > a).unwrap_or(loop_range)
}

// ------------------------------------------------------------- the tools

/// A tool's notes: those that stay selected, and any it adds beside them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolNotes {
    pub selected: Vec<NoteData>,
    pub unselected: Vec<NoteData>,
}

impl From<Vec<NoteData>> for ToolNotes {
    fn from(selected: Vec<NoteData>) -> Self {
        Self {
            selected,
            unselected: Vec::new(),
        }
    }
}

/// A clip's notes after a tool, and which of them are selected.
#[derive(Clone, Debug, PartialEq)]
pub struct Applied {
    pub notes: Vec<NoteData>,
    pub selected: Vec<usize>,
}

/// A Transformation and its settings.
#[derive(Clone, Debug, PartialEq)]
pub enum Transform {
    Arpeggiate(Arpeggiate),
    Chop(Chop),
    Connect(Connect),
    Glissando(Glissando),
    Lfo(Lfo),
    Ornament(Ornament),
    Quantize(QuantizeTool),
    Recombine(Recombine),
    Span(Span),
    Strum(Strum),
    TimeWarp(TimeWarp),
    VelocityShaper(VelocityShaper),
}

impl Transform {
    /// Every Transformation, at its default settings.
    pub fn all() -> Vec<Self> {
        vec![
            Self::Arpeggiate(Arpeggiate::default()),
            Self::Chop(Chop::default()),
            Self::Connect(Connect::default()),
            Self::Glissando(Glissando::default()),
            Self::Lfo(Lfo::default()),
            Self::Ornament(Ornament::default()),
            Self::Quantize(QuantizeTool::default()),
            Self::Recombine(Recombine::default()),
            Self::Span(Span::default()),
            Self::Strum(Strum::default()),
            Self::TimeWarp(TimeWarp::default()),
            Self::VelocityShaper(VelocityShaper::default()),
        ]
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Arpeggiate(_) => "Arpeggiate",
            Self::Chop(_) => "Chop",
            Self::Connect(_) => "Connect",
            Self::Glissando(_) => "Glissando",
            Self::Lfo(_) => "LFO",
            Self::Ornament(_) => "Ornament",
            Self::Quantize(_) => "Quantize",
            Self::Recombine(_) => "Recombine",
            Self::Span(_) => "Span",
            Self::Strum(_) => "Strum",
            Self::TimeWarp(_) => "Time Warp",
            Self::VelocityShaper(_) => "Velocity Shaper",
        }
    }

    /// This tool at its defaults, which is what Reset goes back to.
    pub fn defaults(&self) -> Self {
        Self::all()
            .into_iter()
            .find(|t| t.name() == self.name())
            .unwrap_or_else(|| self.clone())
    }

    fn run(&self, input: &[NoteData], others: &[NoteData], cx: &ToolContext) -> ToolNotes {
        match self {
            Self::Arpeggiate(t) => t.run(input, cx).into(),
            Self::Chop(t) => t.run(input, cx).into(),
            Self::Connect(t) => t.run(input, cx).into(),
            Self::Glissando(t) => t.run(input, cx).into(),
            Self::Lfo(t) => t.run(input, cx).into(),
            Self::Ornament(t) => t.run(input, others, cx),
            Self::Quantize(t) => t.run(input, cx).into(),
            Self::Recombine(t) => t.run(input, cx).into(),
            Self::Span(t) => t.run(input, cx).into(),
            Self::Strum(t) => t.run(input, cx).into(),
            Self::TimeWarp(t) => t.run(input, cx).into(),
            Self::VelocityShaper(t) => t.run(input, cx).into(),
        }
    }
}

/// Runs a Transformation on the `picks` of a clip's notes. They are replaced by what it makes.
pub fn apply_transform(
    tool: &Transform,
    notes: &[NoteData],
    picks: &[usize],
    cx: &ToolContext,
) -> Applied {
    let mut others: Vec<NoteData> = Vec::with_capacity(notes.len());
    let mut input: Vec<NoteData> = Vec::with_capacity(picks.len());
    for (i, n) in notes.iter().enumerate() {
        if picks.contains(&i) {
            input.push(n.clone());
        } else {
            others.push(n.clone());
        }
    }
    let made = tool.run(&input, &others, cx);
    others.extend(made.unselected);
    let first = others.len();
    others.extend(made.selected);
    let selected = (first..others.len()).collect();
    Applied {
        notes: others,
        selected,
    }
}

/// A Generator and its settings.
#[derive(Clone, Debug, PartialEq)]
pub enum Generator {
    Rhythm(Rhythm),
    Seed(Seed),
    Shape(Shape),
    Stacks(Stacks),
    Euclidean(Euclidean),
}

impl Generator {
    /// Every Generator, at its default settings.
    pub fn all() -> Vec<Self> {
        vec![
            Self::Rhythm(Rhythm::default()),
            Self::Seed(Seed::default()),
            Self::Shape(Shape::default()),
            Self::Stacks(Stacks::default()),
            Self::Euclidean(Euclidean::default()),
        ]
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Rhythm(_) => "Rhythm",
            Self::Seed(_) => "Seed",
            Self::Shape(_) => "Shape",
            Self::Stacks(_) => "Stacks",
            Self::Euclidean(_) => "Euclidean",
        }
    }

    /// This tool at its defaults. A custom chord bank is a choice of content, so Stacks keeps it.
    pub fn defaults(&self) -> Self {
        if let Self::Stacks(s) = self {
            return Self::Stacks(Stacks::with_bank(s.bank.clone()));
        }
        Self::all()
            .into_iter()
            .find(|t| t.name() == self.name())
            .unwrap_or_else(|| self.clone())
    }

    /// The notes it makes.
    pub fn run(&self, cx: &ToolContext) -> Vec<NoteData> {
        match self {
            Self::Rhythm(t) => t.run(cx),
            Self::Seed(t) => t.run(cx),
            Self::Shape(t) => t.run(cx),
            Self::Stacks(t) => t.run(cx),
            Self::Euclidean(t) => t.run(cx),
        }
    }
}

fn collide(a: &NoteData, b: &NoteData) -> bool {
    a.key == b.key && a.start < b.start + b.length && b.start < a.start + a.length
}

/// Runs a Generator over a clip's notes. A note it lands on goes; the rest stay (manual p. 295).
pub fn apply_generator(tool: &Generator, notes: &[NoteData], cx: &ToolContext) -> Applied {
    let made = tool.run(cx);
    let mut out: Vec<NoteData> = notes
        .iter()
        .filter(|n| !made.iter().any(|m| collide(n, m)))
        .cloned()
        .collect();
    let first = out.len();
    out.extend(made);
    let selected = (first..out.len()).collect();
    Applied {
        notes: out,
        selected,
    }
}

// ------------------------------------------------------------- what the tools share

/// `key` moved by `by` semitones, or scale degrees with a scale.
pub(crate) fn step_key(key: u8, by: i32, scale: Option<PitchScale>) -> u8 {
    clip_edit::moved(key, by, scale)
}

/// The nearest key in the scale, going down on a tie; `key` itself without a scale.
pub(crate) fn in_scale(key: i32, scale: Option<PitchScale>) -> u8 {
    let key = key.clamp(0, 127);
    let Some(s) = scale else {
        return key as u8;
    };
    (0..12)
        .flat_map(|d| [key - d, key + d])
        .find(|k| (0..=127).contains(k) && clip_edit::in_scale(*k, s))
        .unwrap_or(key) as u8
}

/// The keys from `lo` to `hi` that a tool may use.
pub(crate) fn keys_between(lo: u8, hi: u8, scale: Option<PitchScale>) -> Vec<u8> {
    let (lo, hi) = (lo.min(hi), lo.max(hi));
    (lo..=hi)
        .filter(|k| scale.is_none_or(|s| clip_edit::in_scale(i32::from(*k), s)))
        .collect()
}

fn slice_points(points: &[Breakpoint], from: f64, to: f64) -> Vec<Breakpoint> {
    if points.is_empty() {
        return Vec::new();
    }
    let at = |t: f64| rlive_core::envelope::value_at(points, t, Default::default()).unwrap_or(0.0);
    let curve_at = |t: f64| {
        let i = points.partition_point(|p| p.time <= t);
        if i == 0 || i >= points.len() {
            return 0.0;
        }
        let (a, b) = (points[i - 1], points[i]);
        let x = (t - a.time) / (b.time - a.time);
        split_curve(a.curve, x).1
    };
    let mut out = Vec::new();
    if !points.iter().any(|p| p.time == from) {
        out.push(Breakpoint {
            time: 0.0,
            value: at(from),
            curve: curve_at(from),
        });
    }
    out.extend(
        points
            .iter()
            .filter(|p| p.time >= from && p.time < to)
            .map(|p| Breakpoint {
                time: p.time - from,
                ..*p
            }),
    );
    out.push(Breakpoint::new(to - from, at(to)));
    out
}

fn slice_expression(expr: &NoteExpression, from: f64, to: f64) -> NoteExpression {
    NoteExpression {
        pitch: slice_points(&expr.pitch, from, to),
        slide: slice_points(&expr.slide, from, to),
        pressure: slice_points(&expr.pressure, from, to),
    }
}

/// A part of `n` from `start` for `length` ticks, keeping the expression that falls in it.
pub(crate) fn piece(n: &NoteData, start: u64, length: u64) -> NoteData {
    let length = length.max(1);
    let from = start.saturating_sub(n.start) as f64;
    let to = from + length as f64;
    NoteData {
        start,
        length,
        expr: n
            .expr
            .as_deref()
            .map(|e| Box::new(slice_expression(e, from, to))),
        ..n.clone()
    }
}

/// `n` with its start at `new_start` and its end where it is (or just past the start). Expression
/// stays where it was in time.
pub(crate) fn with_start(n: &NoteData, new_start: u64) -> NoteData {
    let end = n.start + n.length;
    let length = end.saturating_sub(new_start).max(1);
    let by = new_start as f64 - n.start as f64;
    let shift = |points: &[Breakpoint]| -> Vec<Breakpoint> {
        points
            .iter()
            .filter(|p| p.time - by >= 0.0)
            .map(|p| Breakpoint {
                time: p.time - by,
                ..*p
            })
            .collect()
    };
    NoteData {
        start: new_start,
        length,
        expr: n.expr.as_deref().map(|e| {
            Box::new(NoteExpression {
                pitch: shift(&e.pitch),
                slide: shift(&e.slide),
                pressure: shift(&e.pressure),
            })
        }),
        ..n.clone()
    }
}

/// Runs of notes that sound together: each is a chord to tools that treat them as one. The
/// notes in a run are in order of start, then key.
pub(crate) fn overlapping_runs(input: &[NoteData]) -> Vec<Vec<NoteData>> {
    let mut sorted = input.to_vec();
    sorted.sort_by_key(|n| (n.start, n.key));
    let mut runs: Vec<Vec<NoteData>> = Vec::new();
    let mut end = 0;
    for n in sorted {
        match runs.last_mut() {
            Some(run) if n.start < end => run.push(n.clone()),
            _ => runs.push(vec![n.clone()]),
        }
        end = end.max(n.start + n.length);
    }
    runs
}

/// A simple quantize, shared by the tool that does nothing else and by Time Warp.
pub(crate) fn snap_tick(t: f64, grid: u64) -> u64 {
    let grid = grid.max(1) as f64;
    ((t / grid).round() * grid).max(0.0) as u64
}

pub(crate) fn run_quantize(input: &[NoteData], q: Quantize) -> Vec<NoteData> {
    let mut out = input.to_vec();
    note_edit::quantize(&mut out, &[], q);
    out
}

#[cfg(test)]
mod tests;
