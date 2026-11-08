//! The rLive project model: the UI thread's source of truth, saved as RON (`.rlive`).

mod arrangement;
mod clip;
mod device;
pub mod history;
mod project;
mod scene;
mod track;

pub use arrangement::{Arrangement, ArrangementClip};
pub use clip::{Clip, ClipContent, NoteData};
pub use device::{Device, DeviceKind};
pub use project::{Master, Project, ProjectError, FILE_EXTENSION, FORMAT_VERSION, PALETTE};
pub use scene::Scene;
pub use track::{Track, TrackKind};
