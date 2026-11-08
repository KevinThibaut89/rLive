//! The rLive project model: the UI thread's source of truth, saved as RON (`.rlive`).

mod arrangement;
mod clip;
pub mod clip_envelope;
mod defaults;
mod device;
mod envelope;
pub mod envelope_edit;
mod file_ref;
mod groove;
pub mod history;
mod id_walk;
mod mapping;
mod migrate;
mod project;
mod recording;
mod routing;
mod scene;
mod track;
pub mod warp_edit;

pub use arrangement::{
    Arrangement, ArrangementClip, Locator, SignatureMarker, TakeLane, TempoAutomation,
};
pub use clip::{
    BeatsLoop, BeatsPreserve, Clip, ClipContent, ClipScale, Fades, FollowAction, FollowActions,
    LaunchMode, LaunchSettings, MidiProgram, NoteData, NoteExpression, TransientEdits, Warp,
    WarpMarker, WarpMode, WarpParams,
};
pub use device::{
    grow_macros, Chain, ChainMix, ChainSend, Device, DeviceKind, DrumPad, Macro, MacroMapping,
    RackKind, SidechainSource, Variation, Zone, Zones,
};
pub use envelope::{Breakpoint, ClipEnvelope, EnvLoop, EnvMode, Envelope};
pub use file_ref::FileRef;
pub use groove::{Groove, GroovePool, Tuning};
pub use id_walk::walk_ids;
pub use mapping::{CcEncoding, KeyMapping, MapTarget, MidiMapping, MidiSource};
pub use project::{Crossfader, Project, ProjectError, FILE_EXTENSION, FORMAT_VERSION, PALETTE};
pub use recording::{BeatDivision, MetronomeOptions, MetronomeSound, RecordQuantize};
pub use routing::SEND_OFF_DB;
pub use scene::Scene;
pub use track::{
    ChainTap, Crossfade, ExternalRoute, MainLane, Monitor, PanMode, Route, Routing, Tap, Track,
    TrackDelay, TrackKind, TrackSend, TrackUi,
};
