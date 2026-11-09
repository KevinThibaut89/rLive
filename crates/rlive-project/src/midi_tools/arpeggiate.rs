//! Arpeggiate: chops held notes into a pattern, as the Arpeggiator does (manual p. 297).

use super::{overlapping_runs, piece, step_key, Rng, ToolContext};
use crate::NoteData;
use rlive_core::PPQ;

/// The Arpeggiator's 18 styles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArpStyle {
    Up,
    Down,
    UpDown,
    DownUp,
    UpAndDown,
    DownAndUp,
    Converge,
    Diverge,
    ConAndDiverge,
    PinkyUp,
    PinkyUpDown,
    ThumbUp,
    ThumbUpDown,
    PlayOrder,
    ChordTrigger,
    Random,
    RandomOther,
    RandomOnce,
}

impl ArpStyle {
    pub const ALL: [ArpStyle; 18] = [
        Self::Up,
        Self::Down,
        Self::UpDown,
        Self::DownUp,
        Self::UpAndDown,
        Self::DownAndUp,
        Self::Converge,
        Self::Diverge,
        Self::ConAndDiverge,
        Self::PinkyUp,
        Self::PinkyUpDown,
        Self::ThumbUp,
        Self::ThumbUpDown,
        Self::PlayOrder,
        Self::ChordTrigger,
        Self::Random,
        Self::RandomOther,
        Self::RandomOnce,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Up => "Up",
            Self::Down => "Down",
            Self::UpDown => "UpDown",
            Self::DownUp => "DownUp",
            Self::UpAndDown => "Up & Down",
            Self::DownAndUp => "Down & Up",
            Self::Converge => "Converge",
            Self::Diverge => "Diverge",
            Self::ConAndDiverge => "Con & Diverge",
            Self::PinkyUp => "Pinky Up",
            Self::PinkyUpDown => "Pinky UpDown",
            Self::ThumbUp => "Thumb Up",
            Self::ThumbUpDown => "Thumb UpDown",
            Self::PlayOrder => "Play Order",
            Self::ChordTrigger => "Chord Trigger",
            Self::Random => "Random",
            Self::RandomOther => "Random Other",
            Self::RandomOnce => "Random Once",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Arpeggiate {
    pub style: ArpStyle,
    /// Between transposed passes, in semitones or scale degrees.
    pub distance: i32,
    /// How many transposed passes follow the first.
    pub steps: u32,
    /// Ticks from one note to the next.
    pub rate: u64,
    /// Note length as a share of the rate.
    pub gate: f64,
}

impl Default for Arpeggiate {
    fn default() -> Self {
        Self {
            style: ArpStyle::Up,
            distance: 12,
            steps: 0,
            rate: PPQ / 4,
            gate: 1.0,
        }
    }
}

fn converge(n: usize) -> Vec<usize> {
    let (mut low, mut high) = (0, n);
    let mut out = Vec::with_capacity(n);
    while low < high {
        out.push(low);
        low += 1;
        if low < high {
            high -= 1;
            out.push(high);
        }
    }
    out
}

/// One pass of the pattern, as positions in the chord, lowest first. A chord trigger plays
/// every position, so it is handled by the caller.
fn pass(style: ArpStyle, n: usize, played: &[usize], rng: &mut Rng) -> Vec<usize> {
    let top = n - 1;
    let up: Vec<usize> = (0..n).collect();
    let down: Vec<usize> = (0..n).rev().collect();
    let inner_down = (1..top).rev();
    let inner_up = 1..top;
    match style {
        ArpStyle::Up | ArpStyle::ChordTrigger => up,
        ArpStyle::Down => down,
        ArpStyle::UpDown => up.into_iter().chain(inner_down).collect(),
        ArpStyle::DownUp => down.into_iter().chain(inner_up).collect(),
        ArpStyle::UpAndDown => up.into_iter().chain(down).collect(),
        ArpStyle::DownAndUp => down.into_iter().chain(up).collect(),
        ArpStyle::Converge => converge(n),
        ArpStyle::Diverge => converge(n).into_iter().rev().collect(),
        ArpStyle::ConAndDiverge => {
            let c = converge(n);
            let back: Vec<usize> = c.iter().rev().skip(1).copied().collect();
            c.into_iter().chain(back).collect()
        }
        ArpStyle::PinkyUp if n > 1 => (0..top).flat_map(|i| [i, top]).collect(),
        ArpStyle::PinkyUpDown if n > 1 => {
            (0..top).chain(inner_down).flat_map(|i| [i, top]).collect()
        }
        ArpStyle::ThumbUp if n > 1 => (1..n).flat_map(|i| [0, i]).collect(),
        ArpStyle::ThumbUpDown if n > 1 => (1..n).chain(inner_down).flat_map(|i| [0, i]).collect(),
        ArpStyle::PinkyUp | ArpStyle::PinkyUpDown | ArpStyle::ThumbUp | ArpStyle::ThumbUpDown => {
            vec![0]
        }
        ArpStyle::PlayOrder => played.to_vec(),
        ArpStyle::Random => (0..n).map(|_| rng.below(n)).collect(),
        ArpStyle::RandomOther | ArpStyle::RandomOnce => {
            let mut order = up;
            rng.shuffle(&mut order);
            order
        }
    }
}

impl Arpeggiate {
    pub(super) fn run(&self, input: &[NoteData], cx: &ToolContext) -> Vec<NoteData> {
        let mut rng = cx.rng();
        let rate = self.rate.max(1);
        let gate = ((rate as f64 * self.gate).round() as u64).max(1);
        let mut out = Vec::new();
        for run in overlapping_runs(input) {
            let mut chord = run.clone();
            chord.sort_by_key(|n| n.key);
            chord.dedup_by_key(|n| n.key);
            let played: Vec<usize> = run
                .iter()
                .filter_map(|n| chord.iter().position(|c| c.key == n.key))
                .fold(Vec::new(), |mut seen, at| {
                    if !seen.contains(&at) {
                        seen.push(at);
                    }
                    seen
                });
            let from = run.iter().map(|n| n.start).min().unwrap_or(0);
            let to = run.iter().map(|n| n.start + n.length).max().unwrap_or(0);
            let mut order = pass(self.style, chord.len(), &played, &mut rng);
            let (mut at, mut cycle, mut i) = (from, 0u32, 0);
            while at < to {
                if i == order.len() {
                    i = 0;
                    cycle += 1;
                    if matches!(self.style, ArpStyle::Random | ArpStyle::RandomOther) {
                        order = pass(self.style, chord.len(), &played, &mut rng);
                    }
                }
                let up = self.distance * (cycle % (self.steps + 1)) as i32;
                let make = |src: &NoteData| NoteData {
                    key: step_key(src.key, up, cx.scale),
                    ..piece(src, at, gate)
                };
                if self.style == ArpStyle::ChordTrigger {
                    out.extend(chord.iter().map(make));
                } else {
                    out.push(make(&chord[order[i]]));
                }
                i += 1;
                at += rate;
            }
        }
        out
    }
}
