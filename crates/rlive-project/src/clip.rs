use rlive_core::TimeSignature;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoteData {
    pub start: u64,
    pub length: u64,
    pub key: u8,
    pub velocity: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClipContent {
    Midi { notes: Vec<NoteData> },
    Audio { path: PathBuf, gain_db: f32 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub name: String,
    pub color: [u8; 3],
    /// Loop length in ticks.
    pub length: u64,
    pub looped: bool,
    pub content: ClipContent,
}

impl Clip {
    pub fn new_midi(name: &str, color: [u8; 3], bars: u64, sig: TimeSignature) -> Self {
        Self {
            name: name.into(),
            color,
            length: bars * sig.ticks_per_bar(),
            looped: true,
            content: ClipContent::Midi { notes: Vec::new() },
        }
    }

    pub fn notes(&self) -> &[NoteData] {
        match &self.content {
            ClipContent::Midi { notes } => notes,
            ClipContent::Audio { .. } => &[],
        }
    }

    pub fn notes_mut(&mut self) -> Option<&mut Vec<NoteData>> {
        match &mut self.content {
            ClipContent::Midi { notes } => Some(notes),
            ClipContent::Audio { .. } => None,
        }
    }

    pub fn is_midi(&self) -> bool {
        matches!(self.content, ClipContent::Midi { .. })
    }
}
