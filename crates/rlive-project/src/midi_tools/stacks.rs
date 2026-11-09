//! Stacks: chords and chord progressions from a bank of shapes (manual pp. 326-327).

use super::{in_scale, ToolContext};
use crate::clip_edit::{self, PitchScale};
use crate::NoteData;
use serde::{Deserialize, Serialize};

/// A named set of intervals above a root.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChordDef {
    pub name: String,
    pub intervals: Vec<i32>,
}

/// The chords a Stacks pad can choose from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChordBank {
    #[serde(default)]
    pub name: String,
    pub chords: Vec<ChordDef>,
}

impl Default for ChordBank {
    fn default() -> Self {
        let chord = |name: &str, intervals: &[i32]| ChordDef {
            name: name.to_string(),
            intervals: intervals.to_vec(),
        };
        Self {
            name: "Default".into(),
            chords: vec![
                chord("Major", &[0, 4, 7]),
                chord("Minor", &[0, 3, 7]),
                chord("Sus2", &[0, 2, 7]),
                chord("Sus4", &[0, 5, 7]),
                chord("Diminished", &[0, 3, 6]),
                chord("Augmented", &[0, 4, 8]),
                chord("Major 7", &[0, 4, 7, 11]),
                chord("Minor 7", &[0, 3, 7, 10]),
                chord("Dominant 7", &[0, 4, 7, 10]),
                chord("Half Dim 7", &[0, 3, 6, 10]),
                chord("Add 9", &[0, 4, 7, 14]),
                chord("Minor 9", &[0, 3, 7, 10, 14]),
            ],
        }
    }
}

/// Reads a `.stacks` chord bank from its JSON text.
pub fn parse_bank(text: &str) -> Result<ChordBank, String> {
    let bank: ChordBank = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if bank.chords.is_empty() || bank.chords.iter().any(|c| c.intervals.is_empty()) {
        return Err("A chord bank needs chords, and every chord needs intervals".into());
    }
    Ok(bank)
}

/// One chord of a progression.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StackChord {
    /// Which chord of the bank.
    pub chord: usize,
    pub root: u8,
    /// Above zero, that many notes moved up; below zero, the same an octave lower.
    pub inversion: i32,
    /// Its length, in eighths of its slot.
    pub duration: u32,
    /// How far into its slot it starts, in eighths.
    pub offset: u32,
}

impl Default for StackChord {
    fn default() -> Self {
        Self {
            chord: 0,
            root: 60,
            inversion: 0,
            duration: 8,
            offset: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stacks {
    pub bank: ChordBank,
    pub chords: Vec<StackChord>,
    /// The chord the controls show.
    pub current: usize,
}

impl Default for Stacks {
    fn default() -> Self {
        Self::with_bank(ChordBank::default())
    }
}

/// The notes of a chord above its root, with an inversion.
fn voiced(intervals: &[i32], inversion: i32) -> Vec<i32> {
    let mut notes = intervals.to_vec();
    notes.sort_unstable();
    let n = notes.len() as i32;
    if n == 0 {
        return notes;
    }
    let moved = inversion.abs().rem_euclid(n) as usize;
    for note in notes.iter_mut().take(moved) {
        *note += 12;
    }
    if inversion < 0 {
        for note in &mut notes {
            *note -= 12;
        }
    }
    notes.sort_unstable();
    notes
}

impl Stacks {
    pub fn with_bank(bank: ChordBank) -> Self {
        Self {
            bank,
            chords: vec![StackChord::default()],
            current: 0,
        }
    }

    /// A chord after the selected one, with the same settings.
    pub fn add_chord(&mut self) {
        let copy = self.chords.get(self.current).copied().unwrap_or_default();
        self.chords.insert(self.current + 1, copy);
        self.current += 1;
    }

    /// Takes away the selected chord, leaving at least one.
    pub fn remove_chord(&mut self) {
        if self.chords.len() > 1 {
            self.chords.remove(self.current);
            self.current = self.current.min(self.chords.len() - 1);
        }
    }

    /// A chord's inversions reach as far as it has notes above the root.
    pub fn most_inversions(&self, chord: usize) -> i32 {
        self.bank
            .chords
            .get(chord)
            .map_or(0, |c| c.intervals.len() as i32 - 1)
    }

    /// With a scale on, a root outside it moves to the scale's own root (manual p. 327).
    pub fn follow_scale(&mut self, scale: Option<PitchScale>) {
        let Some(s) = scale else {
            return;
        };
        for c in &mut self.chords {
            if !clip_edit::in_scale(i32::from(c.root), s) {
                let octave = c.root / 12 * 12;
                c.root = (octave + s.0).min(127);
            }
        }
    }

    /// The keys that can be a root: all of them, or the notes of the scale.
    pub fn roots(scale: Option<PitchScale>) -> Vec<u8> {
        (0..=127u8)
            .filter(|k| scale.is_none_or(|s| clip_edit::in_scale(i32::from(*k), s)))
            .collect()
    }

    pub(super) fn run(&self, cx: &ToolContext) -> Vec<NoteData> {
        let count = self.chords.len() as u64;
        let span = cx.span();
        if count == 0 || span == 0 {
            return Vec::new();
        }
        let slot = span / count;
        let mut out = Vec::new();
        for (i, c) in self.chords.iter().enumerate() {
            let Some(def) = self.bank.chords.get(c.chord) else {
                continue;
            };
            let start = cx.range.0 + i as u64 * slot + slot * u64::from(c.offset.min(7)) / 8;
            let length = (slot * u64::from(c.duration.clamp(1, 8)) / 8).max(1);
            let mut keys: Vec<u8> = voiced(&def.intervals, c.inversion)
                .into_iter()
                .map(|step| in_scale(i32::from(c.root) + step, cx.scale))
                .collect();
            keys.sort_unstable();
            keys.dedup();
            out.extend(
                keys.into_iter()
                    .map(|key| NoteData::new(start, length, key, 0.8)),
            );
        }
        out
    }
}

/// Where an interval sits on the Tonnetz, from a root at (0, 0): fifths across, major thirds up.
/// Used to draw a chord's pad.
pub fn tonnetz(interval: i32) -> (i32, i32) {
    let target = interval.rem_euclid(12);
    let mut best = (0, 0);
    let mut cost = i32::MAX;
    for a in -3i32..=3 {
        for b in -3i32..=3 {
            if (7 * a + 4 * b).rem_euclid(12) == target && a.abs() + b.abs() < cost {
                cost = a.abs() + b.abs();
                best = (a, b);
            }
        }
    }
    best
}
