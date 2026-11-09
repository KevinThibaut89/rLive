//! Ornament: flams and grace notes ahead of the selected notes (manual pp. 306-309).

use super::{step_key, with_start, ToolContext, ToolNotes};
use crate::NoteData;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrnamentKind {
    Flam,
    Grace,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GracePitch {
    High,
    Same,
    Low,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Ornament {
    pub kind: OrnamentKind,
    /// -1 to 1, of a grid step. Above zero replaces the note's start, below zero goes before it.
    pub flam_position: f64,
    /// The share of the note's velocity.
    pub flam_velocity: f64,
    pub grace_pitch: GracePitch,
    /// As Flam's position, for the grace notes together.
    pub grace_position: f64,
    pub grace_velocity: f64,
    /// The share of the note's chance.
    pub grace_chance: f64,
    pub grace_amount: u32,
}

impl Default for Ornament {
    fn default() -> Self {
        Self {
            kind: OrnamentKind::Flam,
            flam_position: -0.5,
            flam_velocity: 0.7,
            grace_pitch: GracePitch::Same,
            grace_position: -0.5,
            grace_velocity: 0.7,
            grace_chance: 1.0,
            grace_amount: 2,
        }
    }
}

/// Where a run of `span` ticks ahead of `start` can go: before anything of the same key already
/// tucked in front of it, so reapplying adds more ahead of what's there.
fn before_earlier(start: u64, key: u8, span: u64, taken: &[NoteData]) -> (u64, u64) {
    let mut at = start;
    while let Some(o) = taken
        .iter()
        .find(|o| o.key == key && o.start < at && o.start + o.length == at)
    {
        at = o.start;
    }
    let from = at.saturating_sub(span);
    (from, at - from)
}

impl Ornament {
    pub(super) fn run(
        &self,
        input: &[NoteData],
        others: &[NoteData],
        cx: &ToolContext,
    ) -> ToolNotes {
        let (position, amount) = match self.kind {
            OrnamentKind::Flam => (self.flam_position, 1),
            OrnamentKind::Grace => (self.grace_position, self.grace_amount.max(1)),
        };
        let span = (position.abs().clamp(0.0, 1.0) * cx.grid()).round() as u64;
        let mut out = ToolNotes::default();
        let mut added: Vec<NoteData> = others.to_vec();
        for n in input {
            let (from, span) = if position > 0.0 {
                (n.start, span.min(n.length.saturating_sub(1)))
            } else {
                before_earlier(n.start, n.key, span, &added)
            };
            let each = span / u64::from(amount);
            if each == 0 {
                out.selected.push(n.clone());
                continue;
            }
            let mut made = Vec::new();
            for i in 0..u64::from(amount) {
                let mut note = NoteData {
                    start: from + i * each,
                    length: each,
                    expr: None,
                    ..n.clone()
                };
                match self.kind {
                    OrnamentKind::Flam => {
                        note.velocity = (n.velocity * self.flam_velocity as f32).clamp(0.0, 1.0);
                    }
                    OrnamentKind::Grace => {
                        note.velocity = (n.velocity * self.grace_velocity as f32).clamp(0.0, 1.0);
                        note.probability =
                            (n.probability * self.grace_chance as f32).clamp(0.0, 1.0);
                        if i % 2 == 0 {
                            note.key = match self.grace_pitch {
                                GracePitch::High => step_key(n.key, 1, cx.scale),
                                GracePitch::Low => step_key(n.key, -1, cx.scale),
                                GracePitch::Same => n.key,
                            };
                        }
                    }
                }
                made.push(note);
            }
            added.extend(made.iter().cloned());
            out.unselected.extend(made);
            let kept = if position > 0.0 {
                with_start(n, n.start + span)
            } else {
                n.clone()
            };
            out.selected.push(kept);
        }
        out
    }
}
