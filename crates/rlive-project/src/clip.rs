use crate::defaults::{is_default, is_one, is_true, one, yes};
use crate::{Breakpoint, ClipEnvelope, FileRef};
use rlive_core::{ClipRegion, GrooveId, Quantize, TimeSignature};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub use rlive_core::launch::{FollowAction, FollowActions, LaunchMode};
pub use rlive_core::warp::{BeatsLoop, BeatsPreserve};

/// Release velocity of a note that never recorded one (MIDI's default of 64, rounded).
const DEFAULT_RELEASE: f32 = 0.5;

fn default_release() -> f32 {
    DEFAULT_RELEASE
}

fn is_default_release(v: &f32) -> bool {
    *v == DEFAULT_RELEASE
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoteData {
    pub start: u64,
    pub length: u64,
    pub key: u8,
    pub velocity: f32,
    /// Release velocity, 0..1.
    #[serde(
        default = "default_release",
        skip_serializing_if = "is_default_release"
    )]
    pub release: f32,
    /// Chance the note plays, 0..1.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub probability: f32,
    /// Velocity deviation, in velocity units: each playback adds a random amount up to this,
    /// downward if it is negative.
    #[serde(default, skip_serializing_if = "is_default")]
    pub vel_range: f32,
    /// A deactivated note is kept but does not play.
    #[serde(default, skip_serializing_if = "is_default")]
    pub muted: bool,
    /// Per-note expression. Most notes have none, so it costs one pointer.
    #[serde(default, skip_serializing_if = "is_default")]
    pub expr: Option<Box<NoteExpression>>,
}

impl NoteData {
    pub fn new(start: u64, length: u64, key: u8, velocity: f32) -> Self {
        Self {
            start,
            length,
            key,
            velocity,
            release: DEFAULT_RELEASE,
            probability: 1.0,
            vel_range: 0.0,
            muted: false,
            expr: None,
        }
    }
}

/// A note's MPE dimensions. Point times are in ticks from the start of the note.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NoteExpression {
    /// Bend in semitones from the note's key.
    #[serde(default, skip_serializing_if = "is_default")]
    pub pitch: Vec<Breakpoint>,
    /// Slide (CC74), 0..1.
    #[serde(default, skip_serializing_if = "is_default")]
    pub slide: Vec<Breakpoint>,
    /// Pressure, 0..1.
    #[serde(default, skip_serializing_if = "is_default")]
    pub pressure: Vec<Breakpoint>,
}

/// Bank and program messages a MIDI clip sends when launched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MidiProgram {
    pub bank: u8,
    pub sub_bank: u8,
    pub program: u8,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClipContent {
    Midi {
        notes: Vec<NoteData>,
        #[serde(default, skip_serializing_if = "is_default")]
        program: Option<MidiProgram>,
    },
    Audio {
        file: FileRef,
        gain_db: f32,
        /// Transposition in semitones.
        #[serde(default, skip_serializing_if = "is_default")]
        transpose: i32,
        #[serde(default, skip_serializing_if = "is_default")]
        detune_cents: f32,
        #[serde(default, skip_serializing_if = "is_default")]
        reverse: bool,
        /// Keep the sample in memory instead of streaming it.
        #[serde(default, skip_serializing_if = "is_default")]
        ram: bool,
        /// High-quality interpolation.
        #[serde(default = "yes", skip_serializing_if = "is_true")]
        hi_q: bool,
        /// Short anti-click fades at the clip's edges in the Session.
        #[serde(default = "yes", skip_serializing_if = "is_true")]
        edge_fade: bool,
        #[serde(default, skip_serializing_if = "is_default")]
        warp: Warp,
    },
}

impl ClipContent {
    /// Audio content with every setting at its default, unwarped.
    pub fn audio(path: impl Into<PathBuf>, gain_db: f32) -> Self {
        Self::Audio {
            file: FileRef::new(path),
            gain_db,
            transpose: 0,
            detune_cents: 0.0,
            reverse: false,
            ram: false,
            hi_q: true,
            edge_fade: true,
            warp: Warp::default(),
        }
    }

    pub fn midi(notes: Vec<NoteData>) -> Self {
        Self::Midi {
            notes,
            program: None,
        }
    }
}

/// How an audio clip follows the tempo.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Warp {
    #[serde(default, skip_serializing_if = "is_default")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "is_default")]
    pub mode: WarpMode,
    /// Sorted by position. Without markers the clip plays at one speed.
    #[serde(default, skip_serializing_if = "is_default")]
    pub markers: Vec<WarpMarker>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub params: WarpParams,
    /// The user's changes to the transients found in the file.
    #[serde(default, skip_serializing_if = "is_default")]
    pub transients: TransientEdits,
}

/// Changes a clip makes to the transients analysed from its file, in seconds.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TransientEdits {
    #[serde(default, skip_serializing_if = "is_default")]
    pub added: Vec<f64>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub removed: Vec<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum WarpMode {
    #[default]
    Beats,
    Tones,
    Texture,
    RePitch,
    Complex,
    ComplexPro,
}

/// Pins a moment of the audio file to a position in the clip.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WarpMarker {
    /// Seconds into the file.
    pub seconds: f64,
    /// Ticks into the clip.
    pub tick: f64,
}

/// The settings of each warp mode. All are kept so switching modes doesn't lose them.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WarpParams {
    pub preserve: BeatsPreserve,
    pub beats_loop: BeatsLoop,
    /// Beats: how quickly each slice fades out, 0..100.
    pub transient_envelope: f32,
    /// Tones and Texture.
    pub grain_size: f32,
    /// Texture: randomness of the grains, 0..100.
    pub flux: f32,
    /// Complex Pro: formant preservation, 0..100.
    pub formants: f32,
    /// Complex Pro: spectral envelope detail.
    pub envelope: f32,
}

impl Default for WarpParams {
    fn default() -> Self {
        Self {
            preserve: BeatsPreserve::default(),
            beats_loop: BeatsLoop::default(),
            transient_envelope: 100.0,
            grain_size: 30.0,
            flux: 0.0,
            formants: 100.0,
            envelope: 128.0,
        }
    }
}

/// What a Session clip does when launched. Ignored in the Arrangement.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LaunchSettings {
    #[serde(default, skip_serializing_if = "is_default")]
    pub mode: LaunchMode,
    /// `None` follows the global quantization.
    #[serde(default, skip_serializing_if = "is_default")]
    pub quantize: Option<Quantize>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub legato: bool,
    /// How much MIDI velocity affects the clip's volume, 0..1.
    #[serde(default, skip_serializing_if = "is_default")]
    pub velocity_amount: f32,
    #[serde(default, skip_serializing_if = "is_default")]
    pub follow: FollowActions,
}

/// A clip's scale. Audio clips only pass it on to scale-aware devices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipScale {
    pub enabled: bool,
    /// Root note, 0 (C) to 11.
    pub root: u8,
    pub name: String,
}

impl Default for ClipScale {
    fn default() -> Self {
        Self {
            enabled: false,
            root: 0,
            name: "Major".into(),
        }
    }
}

/// Fades at the edges of an audio clip, in ticks. Curves shape them as [`Breakpoint`] curves do.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Fades {
    pub fade_in: u64,
    pub fade_out: u64,
    pub in_curve: f32,
    pub out_curve: f32,
    /// How far the clip keeps sounding before its start, under the clip ahead of it, in a
    /// crossfade. The fade in then runs over this and `fade_in` together.
    #[serde(skip_serializing_if = "is_default")]
    pub in_tail: u64,
    /// How far the clip keeps sounding after its end, under the clip after it, in a crossfade.
    #[serde(skip_serializing_if = "is_default")]
    pub out_tail: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub name: String,
    pub color: [u8; 3],
    /// Loop length in ticks.
    pub length: u64,
    pub looped: bool,
    pub content: ClipContent,
    /// The Clip Activator.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub active: bool,
    /// Where playback starts inside the content, in ticks.
    #[serde(default, skip_serializing_if = "is_default")]
    pub start: u64,
    /// Where an unlooped clip ends. `None` ends it at the end of the loop.
    #[serde(default, skip_serializing_if = "is_default")]
    pub end: Option<u64>,
    /// Where the loop starts inside the content, in ticks.
    #[serde(default, skip_serializing_if = "is_default")]
    pub loop_start: u64,
    /// For display and for Note Stretch; does not affect playback.
    #[serde(default, skip_serializing_if = "is_default")]
    pub signature: Option<TimeSignature>,
    /// A groove in the project's groove pool.
    #[serde(default, skip_serializing_if = "is_default")]
    pub groove: Option<GrooveId>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub scale: ClipScale,
    #[serde(default, skip_serializing_if = "is_default")]
    pub launch: LaunchSettings,
    #[serde(default, skip_serializing_if = "is_default")]
    pub envelopes: Vec<ClipEnvelope>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub fades: Fades,
    /// The user's info text.
    #[serde(default, skip_serializing_if = "is_default")]
    pub info: String,
}

impl Default for Clip {
    /// An empty, looped MIDI clip.
    fn default() -> Self {
        Self {
            name: String::new(),
            color: [0; 3],
            length: 0,
            looped: true,
            content: ClipContent::midi(Vec::new()),
            active: true,
            start: 0,
            end: None,
            loop_start: 0,
            signature: None,
            groove: None,
            scale: ClipScale::default(),
            launch: LaunchSettings::default(),
            envelopes: Vec::new(),
            fades: Fades::default(),
            info: String::new(),
        }
    }
}

impl Clip {
    pub fn new_midi(name: &str, color: [u8; 3], bars: u64, sig: TimeSignature) -> Self {
        Self {
            name: name.into(),
            color,
            length: bars * sig.ticks_per_bar(),
            ..Self::default()
        }
    }

    pub fn notes(&self) -> &[NoteData] {
        match &self.content {
            ClipContent::Midi { notes, .. } => notes,
            ClipContent::Audio { .. } => &[],
        }
    }

    pub fn notes_mut(&mut self) -> Option<&mut Vec<NoteData>> {
        match &mut self.content {
            ClipContent::Midi { notes, .. } => Some(notes),
            ClipContent::Audio { .. } => None,
        }
    }

    pub fn is_midi(&self) -> bool {
        matches!(self.content, ClipContent::Midi { .. })
    }

    /// Where the markers put playback.
    pub fn region(&self) -> ClipRegion {
        ClipRegion::new(
            self.start as f64,
            self.end.map(|e| e as f64),
            self.loop_start as f64,
            self.length as f64,
            self.looped,
        )
    }

    pub fn loop_end(&self) -> u64 {
        self.loop_start + self.length
    }

    /// Where the end marker is, whether or not it has been set.
    pub fn end_marker(&self) -> u64 {
        self.end.unwrap_or(self.loop_end())
    }

    pub fn is_warped(&self) -> bool {
        matches!(&self.content, ClipContent::Audio { warp, .. } if warp.enabled)
    }
}
