//! The clip content the engine plays.

use super::envelopes::{ClipEnvelopes, EnvelopedClip};
use rlive_core::envelope::EnvPoint;
use rlive_core::warp::{WarpMap, WarpMode};
use rlive_core::{ClipRegion, SampleBuffer, ScaleInfo};
use std::sync::Arc;

/// A point on an expression envelope, `time` ticks after the note starts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExprPoint {
    pub time: f64,
    pub value: f32,
    /// Shape of the segment leaving this point, as [`rlive_core::envelope`] defines it.
    pub curve: f32,
}

impl EnvPoint for ExprPoint {
    fn time(&self) -> f64 {
        self.time
    }
    fn value(&self) -> f64 {
        f64::from(self.value)
    }
    fn curve(&self) -> f32 {
        self.curve
    }
}

/// A note's expression envelopes, each sorted by time and possibly empty.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NoteExpression {
    /// Semitones from the note's key.
    pub pitch: Vec<ExprPoint>,
    /// 0..1.
    pub slide: Vec<ExprPoint>,
    /// 0..1.
    pub pressure: Vec<ExprPoint>,
}

/// A note inside a MIDI clip. Positions are in ticks relative to the clip start.
#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub start: u64,
    pub length: u64,
    pub key: u8,
    pub velocity: f32,
    /// Velocity of the note-off, 0..1.
    pub release: f32,
    /// The chance the note plays each time it comes round, 0..1.
    pub probability: f32,
    /// Each playback adds a random amount up to this to the velocity, downward if negative.
    pub vel_range: f32,
    /// The probability group the note is in, if any.
    pub group: Option<PlayGroup>,
    pub expr: Option<Box<NoteExpression>>,
}

/// A note's place in a probability group, which all its members decide together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayGroup {
    pub id: u32,
    /// Play One: only the member the group picks sounds.
    pub one: bool,
    /// This note's place in the group and the size of the group, filled in by [`MidiClip::new`].
    pub member: u16,
    pub size: u16,
}

impl Default for Note {
    fn default() -> Self {
        Self {
            start: 0,
            length: 0,
            key: 60,
            velocity: 1.0,
            release: 0.5,
            probability: 1.0,
            vel_range: 0.0,
            group: None,
            expr: None,
        }
    }
}

/// Says who each group's members are, so playback can pick one without looking at the others.
fn number_groups(notes: &mut [Note]) {
    let mut sizes = std::collections::HashMap::<u32, u16>::new();
    for g in notes.iter_mut().filter_map(|n| n.group.as_mut()) {
        let count = sizes.entry(g.id).or_insert(0);
        g.member = *count;
        *count += 1;
    }
    for g in notes.iter_mut().filter_map(|n| n.group.as_mut()) {
        g.size = sizes[&g.id];
    }
}

/// The bank and program messages a MIDI clip sends as it starts. Each is optional.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProgramChange {
    pub bank: Option<u8>,
    pub sub_bank: Option<u8>,
    pub program: Option<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MidiClip {
    /// Loop length in ticks.
    pub length: u64,
    /// Notes sorted by start.
    pub notes: Vec<Note>,
    pub looped: bool,
    /// Longest note in `notes` (at least 1), the lookback needed to find held notes.
    pub(super) max_len: u64,
    /// The clip's scale, for devices with Use Current Scale.
    pub scale: Option<ScaleInfo>,
    /// What it sends when it starts.
    pub program: Option<ProgramChange>,
    /// Where in the engine's groove bank its groove is, if it has one.
    pub groove: Option<u32>,
}

impl MidiClip {
    /// Build a clip, sorting `notes` by start and key.
    pub fn new(length: u64, mut notes: Vec<Note>, looped: bool) -> Self {
        notes.sort_by_key(|n| (n.start, n.key));
        number_groups(&mut notes);
        let max_len = notes.iter().map(|n| n.length.max(1)).max().unwrap_or(1);
        Self {
            length,
            notes,
            looped,
            max_len,
            scale: None,
            program: None,
            groove: None,
        }
    }

    /// The clip played through a groove in the engine's bank.
    pub fn with_groove(mut self, groove: Option<u32>) -> Self {
        self.groove = groove;
        self
    }

    /// The clip with a scale for scale-aware devices.
    pub fn with_scale(mut self, scale: Option<ScaleInfo>) -> Self {
        self.scale = scale;
        self
    }

    /// The clip with bank and program messages to send when it starts.
    pub fn with_program(mut self, program: Option<ProgramChange>) -> Self {
        self.program = program;
        self
    }
}

/// An audio clip played back without time-stretching ("warp off").
#[derive(Debug, Clone)]
pub struct AudioClip {
    pub sample: Arc<SampleBuffer>,
    /// Loop length in ticks.
    pub length: u64,
    pub looped: bool,
    pub gain: f32,
    /// The clip's scale, for devices with Use Current Scale.
    pub scale: Option<ScaleInfo>,
}

impl PartialEq for AudioClip {
    /// Samples compare by identity, so comparing clips never walks the audio data.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.sample, &other.sample)
            && self.length == other.length
            && self.looped == other.looped
            && self.gain == other.gain
            && self.scale == other.scale
    }
}

/// An audio clip with Warp on. Plays through its mode's stretcher.
#[derive(Debug, Clone, PartialEq)]
pub struct WarpedClip {
    pub audio: AudioClip,
    /// `None` until the clip has two usable markers: it plays at one speed, like warp off.
    pub map: Option<WarpMap>,
    pub mode: WarpMode,
    /// Semitones.
    pub transpose: f32,
    /// Where Beats slices the source, in source frames, ascending. Empty for other modes.
    pub slices: Arc<[f64]>,
}

/// A clip on a frozen track: the rendered audio in place of what the clip holds. At the tempo it
/// was frozen at it is read straight off the file, one sample per frame, so playing it back is
/// neutral (manual p. 992); at any other tempo it goes through its warp mode.
#[derive(Debug, Clone, PartialEq)]
pub struct FrozenClip {
    pub warped: WarpedClip,
    /// How the file plays: from its start, looping over whatever part of it is the loop.
    pub region: ClipRegion,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClipData {
    Midi(MidiClip),
    Audio(AudioClip),
    Warped(WarpedClip),
    Frozen(Box<FrozenClip>),
    /// Any of the above, with envelopes on its own content or anything else that sets it apart
    /// from plain playback: markers away from their defaults, an edge fade, Hi-Q.
    Enveloped(Box<EnvelopedClip>),
}

impl ClipData {
    /// What the clip plays, envelopes or no envelopes.
    pub fn content(&self) -> &ClipData {
        match self {
            ClipData::Enveloped(c) => &c.clip,
            _ => self,
        }
    }

    /// The envelopes on the clip's own content.
    pub fn envelopes(&self) -> Option<&ClipEnvelopes> {
        match self {
            ClipData::Enveloped(c) => Some(&c.envelopes),
            _ => None,
        }
    }

    pub fn length(&self) -> u64 {
        match self.content() {
            ClipData::Midi(c) => c.length,
            ClipData::Audio(c) => c.length,
            ClipData::Warped(c) => c.audio.length,
            ClipData::Frozen(c) => c.warped.audio.length,
            ClipData::Enveloped(_) => 0,
        }
    }

    /// Where this clip's markers put playback.
    pub fn region(&self) -> ClipRegion {
        match self {
            ClipData::Enveloped(c) => c
                .region
                .unwrap_or_else(|| ClipRegion::whole(c.clip.length() as f64, c.clip.looped())),
            ClipData::Frozen(c) => c.region,
            _ => ClipRegion::whole(self.length() as f64, self.looped()),
        }
    }

    /// Whether this clip's cut edges get a short fade.
    pub fn edge_fade(&self) -> bool {
        matches!(self, ClipData::Enveloped(c) if c.edge_fade)
    }

    /// Whether this clip resamples with Hi-Q interpolation.
    pub fn hi_q(&self) -> bool {
        matches!(self, ClipData::Enveloped(c) if c.hi_q) || matches!(self, ClipData::Frozen(_))
    }

    /// The scale devices should follow while this clip plays.
    pub fn scale(&self) -> Option<ScaleInfo> {
        match self.content() {
            ClipData::Midi(c) => c.scale,
            ClipData::Audio(c) => c.scale,
            ClipData::Warped(c) => c.audio.scale,
            ClipData::Frozen(_) | ClipData::Enveloped(_) => None,
        }
    }

    pub fn looped(&self) -> bool {
        match self.content() {
            ClipData::Midi(c) => c.looped,
            ClipData::Audio(c) => c.looped,
            ClipData::Warped(c) => c.audio.looped,
            ClipData::Frozen(c) => c.warped.audio.looped,
            ClipData::Enveloped(_) => false,
        }
    }
}
