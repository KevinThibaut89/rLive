//! What the browser lists.

use crate::tags::TagId;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// An item's identity, stable across sessions: what tags and collections are stored against.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ItemKey(pub String);

impl ItemKey {
    pub fn file(path: &Path) -> Self {
        Self(format!("file:{}", path.display()))
    }

    pub fn builtin(type_id: &str) -> Self {
        Self(format!("device:{type_id}"))
    }

    pub fn plugin(format: &str, id: &str) -> Self {
        Self(format!("plugin:{format}:{id}"))
    }

    pub fn factory(preset: &str) -> Self {
        Self(format!("factory:{preset}"))
    }

    pub fn project(part: &str) -> Self {
        Self(format!("project:{part}"))
    }

    /// The factory preset this names, for a key made by [`factory`](Self::factory).
    pub fn as_factory(&self) -> Option<&str> {
        self.0.strip_prefix("factory:")
    }

    /// The part of the Current Project this names.
    pub fn as_project(&self) -> Option<&str> {
        self.0.strip_prefix("project:")
    }
}

/// What a device does in a chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Role {
    Instrument,
    AudioEffect,
    MidiEffect,
}

/// The kind of thing an item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ItemKind {
    Folder,
    /// A built-in device.
    Device,
    Plugin,
    Preset,
    Sample,
    Clip,
    Groove,
    /// A Set in the Templates folder.
    Template,
    /// A Set.
    Set,
    Tuning,
    /// A `.stacks` chord bank for the Stacks generator.
    ChordBank,
    /// Part of the Set that is open: a track, a clip, a device.
    ProjectPart,
    Other,
}

impl ItemKind {
    /// What the Type column shows.
    pub fn label(self) -> &'static str {
        match self {
            Self::Folder => "Folder",
            Self::Device => "Device",
            Self::Plugin => "Plug-In",
            Self::Preset => "Preset",
            Self::Sample => "Sample",
            Self::Clip => "Clip",
            Self::Groove => "Groove",
            Self::Template => "Template",
            Self::Set => "Set",
            Self::Tuning => "Tuning",
            Self::ChordBank => "Chord Bank",
            Self::ProjectPart => "Project",
            Self::Other => "File",
        }
    }

    /// Whether the item opens to show others.
    pub fn is_container(self) -> bool {
        matches!(self, Self::Folder | Self::Device | Self::ProjectPart)
    }
}

/// The Library section of the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Label {
    All,
    Sounds,
    Drums,
    Instruments,
    AudioEffects,
    MidiEffects,
    Modulators,
    MaxForLive,
    Plugins,
    Clips,
    Samples,
    Grooves,
    Templates,
    Tunings,
}

impl Label {
    pub const ALL: [Label; 14] = [
        Label::All,
        Label::Sounds,
        Label::Drums,
        Label::Instruments,
        Label::AudioEffects,
        Label::MidiEffects,
        Label::Modulators,
        Label::MaxForLive,
        Label::Plugins,
        Label::Clips,
        Label::Samples,
        Label::Grooves,
        Label::Templates,
        Label::Tunings,
    ];

    /// The name the sidebar gives it unless renamed.
    pub fn name(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Sounds => "Sounds",
            Self::Drums => "Drums",
            Self::Instruments => "Instruments",
            Self::AudioEffects => "Audio Effects",
            Self::MidiEffects => "MIDI Effects",
            Self::Modulators => "Modulators",
            Self::MaxForLive => "Max for Live",
            Self::Plugins => "Plug-Ins",
            Self::Clips => "Clips",
            Self::Samples => "Samples",
            Self::Grooves => "Grooves",
            Self::Templates => "Templates",
            Self::Tunings => "Tunings",
        }
    }

    fn bit(self) -> u32 {
        1 << self as u32
    }
}

/// The Library labels an item is listed under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LabelSet(pub u32);

impl LabelSet {
    pub fn of(labels: &[Label]) -> Self {
        Self(labels.iter().fold(0, |a, l| a | l.bit()))
    }

    pub fn contains(self, label: Label) -> bool {
        label == Label::All || self.0 & label.bit() != 0
    }

    pub fn insert(&mut self, label: Label) {
        self.0 |= label.bit();
    }
}

/// One thing in the browser.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub key: ItemKey,
    pub name: String,
    pub kind: ItemKind,
    /// The file or folder, for what's on disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// The file's extension, without its dot.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ext: String,
    #[serde(default)]
    pub size: u64,
    /// Seconds since the Unix epoch.
    #[serde(default)]
    pub modified: u64,
    /// The folder or Current Project part it is in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<ItemKey>,
    /// For a preset, the device whose folder it appears in: a type id, `rack:<kind>` or
    /// `plugin:<format>:<id>`. For a device, its own type id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    #[serde(default)]
    pub labels: LabelSet,
    /// A plug-in's vendor.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub vendor: String,
    /// Tags it comes with, which can't be taken off.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub factory_tags: Vec<TagId>,
}

impl Item {
    pub fn new(key: ItemKey, name: impl Into<String>, kind: ItemKind) -> Self {
        Self {
            key,
            name: name.into(),
            kind,
            path: None,
            ext: String::new(),
            size: 0,
            modified: 0,
            parent: None,
            device: None,
            role: None,
            labels: LabelSet::default(),
            vendor: String::new(),
            factory_tags: Vec::new(),
        }
    }

    /// What the search matches: the name, and for a plug-in or preset who it's by or for.
    pub fn haystack(&self) -> String {
        let mut s = self.name.to_lowercase();
        if !self.ext.is_empty() {
            s.push(' ');
            s.push_str(&self.ext.to_lowercase());
        }
        if !self.vendor.is_empty() {
            s.push(' ');
            s.push_str(&self.vendor.to_lowercase());
        }
        if let Some(device) = &self.device {
            s.push(' ');
            s.push_str(
                &device
                    .rsplit('.')
                    .next()
                    .unwrap_or(device)
                    .replace('_', " "),
            );
        }
        s
    }
}
