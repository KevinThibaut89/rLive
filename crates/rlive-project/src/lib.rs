//! The rLive project model: the UI thread's source of truth, saved as RON (`.rlive`).

mod arrangement;
pub mod arrangement_edit;
mod clip;
pub mod clip_edit;
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
pub mod midi_file;
pub mod midi_tools;
mod migrate;
pub mod mpe_edit;
pub mod note_edit;
pub mod note_select;
pub mod note_view;
mod project;
pub mod project_folder;
mod recording;
mod routing;
mod scene;
pub mod takes;
mod tempo_leader;
mod track;
pub mod tuning;
pub mod warp_edit;

pub use arrangement::{
    Arrangement, ArrangementClip, Locator, SignatureMarker, TakeLane, TempoAutomation,
};
pub use clip::{
    BeatsLoop, BeatsPreserve, Clip, ClipContent, ClipScale, Fades, FollowAction, FollowActions,
    FrozenAudio, LaunchMode, LaunchSettings, MidiProgram, MpeGrid, NoteData, NoteExpression,
    NoteGroup, TransientEdits, Warp, WarpMarker, WarpMode, WarpParams,
};
pub use device::{
    grow_macros, Chain, ChainMix, ChainSend, CompareSlot, Device, DeviceCompare, DeviceKind,
    DeviceState, DrumPad, ExternalIo, ExternalMidi, ExternalReturn, Macro, MacroMapping,
    ModMapping, PluginBank, PluginPanel, PluginPreset, RackKind, SidechainSource, Variation, Zone,
    Zones, PLUGIN_BANK_SIZE,
};
pub use envelope::{Breakpoint, ClipEnvelope, EnvLoop, EnvMode, Envelope};
pub use file_ref::FileRef;
pub use groove::{
    commit_notes, extract_from_events, extract_from_notes, factory_grooves, groove_markers, Groove,
    GroovePattern, GroovePool, BASE_CHOICES, GROOVE_EXTENSION,
};
pub use id_walk::walk_ids;
pub use mapping::{
    CcEncoding, ControlKind, CrossfaderPoint, KeyMapping, MapTarget, MidiMapping, MidiSource,
    RemoteControl,
};
pub use project::{Crossfader, Project, ProjectError, FILE_EXTENSION, FORMAT_VERSION, PALETTE};
pub use recording::{BeatDivision, MetronomeOptions, MetronomeSound, RecordQuantize};
pub use routing::SEND_OFF_DB;
pub use scene::Scene;
pub use track::{
    ChainTap, Crossfade, ExternalRoute, MainLane, Monitor, PanMode, Route, Routing, Tap, Track,
    TrackDelay, TrackFreeze, TrackKind, TrackSend, TrackUi,
};
pub use tuning::{factory_tunings, CustomLayout, ScaleNote, Tuning, TuningLayout};
