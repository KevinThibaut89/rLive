//! The rLive project model: the UI thread's source of truth, saved as RON (`.rlive`).

mod arrangement;
mod clip;
mod defaults;
mod device;
mod envelope;
mod file_ref;
mod groove;
pub mod history;
mod id_walk;
mod mapping;
mod migrate;
mod project;
mod scene;
mod track;

pub use arrangement::{
    Arrangement, ArrangementClip, Locator, SignatureMarker, TakeLane, TempoAutomation,
};
pub use clip::{
    BeatsLoop, BeatsPreserve, Clip, ClipContent, ClipScale, Fades, FollowAction, FollowActions,
    LaunchMode, LaunchSettings, MidiProgram, NoteData, NoteExpression, Warp, WarpMarker, WarpMode,
    WarpParams,
};
pub use device::{
    Chain, ChainMix, ChainSend, Device, DeviceKind, DrumPad, Macro, MacroMapping, RackKind,
    Variation, Zone, Zones,
};
pub use envelope::{Breakpoint, ClipEnvelope, EnvLoop, EnvMode, Envelope};
pub use file_ref::FileRef;
pub use groove::{Groove, GroovePool, Tuning};
pub use id_walk::walk_ids;
pub use mapping::{CcEncoding, KeyMapping, MapTarget, MidiMapping, MidiSource};
pub use project::{Crossfader, Project, ProjectError, FILE_EXTENSION, FORMAT_VERSION, PALETTE};
pub use scene::Scene;
pub use track::{
    Crossfade, ExternalRoute, Monitor, PanMode, Route, Routing, Tap, Track, TrackDelay, TrackKind,
    TrackSend, TrackUi,
};
