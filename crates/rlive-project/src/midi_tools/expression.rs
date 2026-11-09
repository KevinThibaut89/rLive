//! The Transformations that write per-note expression: Glissando and LFO (manual pp. 302-306).

use super::{Rng, ToolContext};
use crate::{Breakpoint, NoteData, NoteExpression};
use rlive_core::PPQ;

/// Glissando: bends each note up or down to the next one (manual p. 302).
#[derive(Clone, Debug, PartialEq)]
pub struct Glissando {
    /// Where in the note the bend starts, 0 to 1.
    pub start: f64,
    /// The shape of the bend, -1 to 1.
    pub curve: f64,
}

impl Default for Glissando {
    fn default() -> Self {
        Self {
            start: 0.5,
            curve: 0.0,
        }
    }
}

impl Glissando {
    pub(super) fn run(&self, input: &[NoteData], cx: &ToolContext) -> Vec<NoteData> {
        let mut out = input.to_vec();
        if out.len() < 2 {
            return out;
        }
        out.sort_by_key(|n| (n.start, n.key));
        for i in 0..out.len() - 1 {
            let (a_start, a_key) = (out[i].start, out[i].key);
            let later: Vec<&NoteData> = out[i + 1..].iter().filter(|n| n.start > a_start).collect();
            let Some(first) = later.first().map(|n| n.start) else {
                continue;
            };
            let target = later
                .iter()
                .take_while(|n| n.start == first)
                .min_by_key(|n| (i32::from(n.key) - i32::from(a_key)).abs())
                .map_or(a_key, |n| n.key);
            let bend = (f64::from(target) - f64::from(a_key)).clamp(-cx.bend_range, cx.bend_range);
            let len = out[i].length as f64;
            let t0 = (self.start.clamp(0.0, 1.0) * len).min(len - 1.0).max(0.0);
            let mut points = vec![
                Breakpoint {
                    time: t0,
                    value: 0.0,
                    curve: self.curve.clamp(-1.0, 1.0) as f32,
                },
                Breakpoint::new(len, bend),
            ];
            if t0 > 0.0 {
                points.insert(0, Breakpoint::new(0.0, 0.0));
            }
            out[i].expr.get_or_insert_with(Default::default).pitch = points;
        }
        out
    }
}

// ------------------------------------------------------------- LFO

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LfoTarget {
    Pitch,
    Slide,
    Pressure,
}

impl LfoTarget {
    pub const ALL: [LfoTarget; 3] = [Self::Pitch, Self::Slide, Self::Pressure];

    pub fn label(self) -> &'static str {
        match self {
            Self::Pitch => "Pitch Bend",
            Self::Slide => "Slide",
            Self::Pressure => "Pressure",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LfoShape {
    Sine,
    Square,
    Triangle,
    Random,
}

impl LfoShape {
    pub const ALL: [LfoShape; 4] = [Self::Sine, Self::Square, Self::Triangle, Self::Random];

    pub fn label(self) -> &'static str {
        match self {
            Self::Sine => "Sine",
            Self::Square => "Square",
            Self::Triangle => "Triangle",
            Self::Random => "Random",
        }
    }
}

/// LFO: writes a wave into one of a note's expression dimensions (manual pp. 304-306).
#[derive(Clone, Debug, PartialEq)]
pub struct Lfo {
    pub target: LfoTarget,
    /// Fade in, as a share of the oscillation.
    pub attack: f64,
    /// Fade out, as a share of the oscillation.
    pub decay: f64,
    pub shape_type: LfoShape,
    /// -1 to 1: bends the wave, in a way that suits its type.
    pub shape: f64,
    /// Changed by Reseed, for a new Random wave.
    pub reseed: u64,
    /// The period, in ticks: from a whole note (1) to 1/128.
    pub rate: u64,
    /// -1 to 1 of a period. Above zero delays the start; below zero moves the phase.
    pub time_shift: f64,
    /// Semitones for Pitch; for Slide and Pressure, -127 to 127.
    pub amount: f64,
    /// As Amount: the value the wave moves away from.
    pub base: f64,
}

impl Default for Lfo {
    fn default() -> Self {
        Self {
            target: LfoTarget::Pitch,
            attack: 0.0,
            decay: 0.0,
            shape_type: LfoShape::Sine,
            shape: 0.0,
            reseed: 0,
            rate: PPQ,
            time_shift: 0.0,
            amount: 1.0,
            base: 0.0,
        }
    }
}

/// Attack and Decay share the oscillation: moving one past what's left pushes the other down
/// (manual p. 304). `moved_attack` says which was just changed.
pub fn lfo_balance(attack: f64, decay: f64, moved_attack: bool) -> (f64, f64) {
    let (a, d) = (attack.clamp(0.0, 1.0), decay.clamp(0.0, 1.0));
    if a + d <= 1.0 {
        (a, d)
    } else if moved_attack {
        (a, 1.0 - a)
    } else {
        (1.0 - d, d)
    }
}

/// How finely a period is sampled.
const SAMPLES_PER_PERIOD: f64 = 32.0;
/// The most points one note gets.
const MAX_POINTS: f64 = 512.0;

impl Lfo {
    /// The wave at phase `x` (cycles from the start). -1 to 1.
    fn wave(&self, x: f64, randoms: &mut Vec<f64>, rng: &mut Rng) -> f64 {
        let frac = x.rem_euclid(1.0);
        let s = self.shape.clamp(-1.0, 1.0);
        match self.shape_type {
            LfoShape::Sine => (std::f64::consts::TAU * frac.powf(2f64.powf(s))).sin(),
            LfoShape::Square => {
                if frac < 0.5 + 0.45 * s {
                    1.0
                } else {
                    -1.0
                }
            }
            LfoShape::Triangle => {
                let apex = (0.5 + 0.45 * s).clamp(0.05, 0.95);
                if frac < apex {
                    -1.0 + 2.0 * frac / apex
                } else {
                    1.0 - 2.0 * (frac - apex) / (1.0 - apex)
                }
            }
            LfoShape::Random => {
                let cycle = x.floor().max(0.0) as usize;
                while randoms.len() <= cycle + 1 {
                    randoms.push(rng.signed());
                }
                let (a, b) = (randoms[cycle], randoms[cycle + 1]);
                // Shape runs from held steps to a straight line between them.
                let smooth = (s + 1.0) / 2.0;
                a + (b - a) * frac * smooth
            }
        }
    }

    fn write(&self, n: &NoteData, cx: &ToolContext) -> Vec<Breakpoint> {
        let period = self.rate.max(1) as f64;
        let len = n.length as f64;
        let shift = self.time_shift.clamp(-1.0, 1.0);
        let delay = (shift.max(0.0) * period).min(len);
        let phase = (-shift).max(0.0);
        let length = (len - delay).max(1.0);
        let steps = (length / period * SAMPLES_PER_PERIOD)
            .ceil()
            .clamp(1.0, MAX_POINTS) as usize;
        let (attack, decay) = lfo_balance(self.attack, self.decay, true);
        let env = |x: f64| {
            let up = if attack > 0.0 {
                (x / attack).min(1.0)
            } else {
                1.0
            };
            let down = if decay > 0.0 {
                ((1.0 - x) / decay).min(1.0)
            } else {
                1.0
            };
            up.min(down).max(0.0)
        };
        let mut rng = Rng::new(cx.seed ^ self.reseed.wrapping_mul(0x51_7c_c1_b7));
        let mut randoms = Vec::new();
        let (scale, low, high, unipolar) = match self.target {
            LfoTarget::Pitch => (1.0, -cx.bend_range, cx.bend_range, false),
            LfoTarget::Slide | LfoTarget::Pressure => (1.0 / 127.0, 0.0, 1.0, true),
        };
        let mut points = Vec::with_capacity(steps + 3);
        if delay > 0.0 {
            points.push(Breakpoint::new(0.0, (self.base * scale).clamp(low, high)));
        }
        for i in 0..=steps {
            let x = i as f64 / steps as f64;
            let t = delay + x * length;
            let raw = self.wave(phase + (t - delay) / period, &mut randoms, &mut rng);
            let swing = if unipolar { (raw + 1.0) / 2.0 } else { raw };
            let value = (self.base + self.amount * env(x) * swing) * scale;
            points.push(Breakpoint::new(t.min(len), value.clamp(low, high)));
        }
        points
    }

    /// The curve over two periods for the display: position 0 to 1 and value -1 to 1.
    pub fn preview(&self) -> Vec<(f64, f64)> {
        let shown = Self {
            target: LfoTarget::Pitch,
            amount: 1.0,
            base: 0.0,
            ..self.clone()
        };
        let length = self.rate.max(1) * 2;
        let cx = ToolContext {
            range: (0, length),
            end: length,
            grid: 1,
            scale: None,
            seed: 1,
            bend_range: 1.0,
        };
        shown
            .write(&NoteData::new(0, length, 60, 1.0), &cx)
            .iter()
            .map(|p| (p.time / length as f64, p.value))
            .collect()
    }

    pub(super) fn run(&self, input: &[NoteData], cx: &ToolContext) -> Vec<NoteData> {
        let mut out = input.to_vec();
        for n in &mut out {
            let points = self.write(n, cx);
            let e: &mut NoteExpression = n.expr.get_or_insert_with(Default::default);
            match self.target {
                LfoTarget::Pitch => e.pitch = points,
                LfoTarget::Slide => e.slide = points,
                LfoTarget::Pressure => e.pressure = points,
            }
        }
        out
    }
}
