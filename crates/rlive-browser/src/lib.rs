//! The browser's data: what can be found, how it is tagged and collected, and the presets and
//! defaults kept in the User Library. There is no UI here.
//!
//! [`db::BrowserDb`] runs on a worker thread of its own, so nothing the UI asks of it ever waits
//! for a disk: requests go down a channel and answers are picked up when they are ready. Folders
//! are scanned on further threads, which hand the worker what they find in small batches.
//!
//! Machine-learned tags and similarity search arrive with A19: [`library::Op::SetAutoTags`] is
//! where its analysis hands in tags, and [`query::Scope::Similar`] is where a similarity search
//! would put its results.

pub mod backup;
pub mod catalog;
pub mod db;
pub mod defaults;
pub mod factory;
pub mod item;
pub mod library;
pub mod model;
pub mod preset;
pub mod query;
pub mod scan;
pub mod tags;

pub use db::{BrowserDb, Config, ScanState};
pub use item::{Item, ItemKey, ItemKind, Label, LabelSet, Role};
pub use library::{Collection, CustomLabel, LibraryData, Op, UserFolder};
pub use model::{Model, NavState};
pub use preset::{Payload, Preset, PresetError, PRESET_EXTENSION, PRESET_FORMAT};
pub use query::{Column, Query, Results, Row, Scope, Sort};
pub use tags::{GroupId, Tag, TagDb, TagGroup, TagId};
