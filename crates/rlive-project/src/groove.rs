//! The groove pool and the project's tuning system.

use crate::defaults::is_default;
use crate::FileRef;
use rlive_core::GrooveId;
use serde::{Deserialize, Serialize};

/// A groove loaded into the project. Clips refer to it by id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Groove {
    pub id: GrooveId,
    pub name: String,
    #[serde(default, skip_serializing_if = "is_default")]
    pub file: Option<FileRef>,
    /// The grid the groove's timing is measured against, in ticks.
    #[serde(default = "sixteenth", skip_serializing_if = "is_sixteenth")]
    pub base: u32,
    /// Each 0..100.
    #[serde(default, skip_serializing_if = "is_default")]
    pub quantize: f32,
    #[serde(default, skip_serializing_if = "is_default")]
    pub timing: f32,
    #[serde(default, skip_serializing_if = "is_default")]
    pub random: f32,
    /// -100..100.
    #[serde(default, skip_serializing_if = "is_default")]
    pub velocity: f32,
}

fn sixteenth() -> u32 {
    rlive_core::PPQ as u32 / 4
}

fn is_sixteenth(v: &u32) -> bool {
    *v == sixteenth()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GroovePool {
    pub grooves: Vec<Groove>,
    /// Global Amount, 0..1.3.
    pub amount: f32,
}

impl Default for GroovePool {
    fn default() -> Self {
        Self {
            grooves: Vec::new(),
            amount: 1.0,
        }
    }
}

/// A tuning system loaded into the project, kept whole so the project doesn't need the file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tuning {
    pub name: String,
    #[serde(default, skip_serializing_if = "is_default")]
    pub description: String,
    /// Each scale degree's pitch in cents above the root; the last one is the pseudo-octave.
    pub cents: Vec<f64>,
    /// The note that sounds at `reference_hz`.
    pub reference_note: u8,
    pub reference_hz: f64,
    /// Notes outside this range are not tuned.
    pub lowest_note: u8,
    pub highest_note: u8,
}
