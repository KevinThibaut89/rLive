//! The User Library's folders, and which default applies (manual pp. 464-477).
//!
//! Defaults are looked up in a Project's own Defaults folder first, then the User Library's,
//! and otherwise a device loads with its factory settings.

use crate::item::Role;
use crate::preset::file_name;
use rlive_core::PluginFormat;
use rlive_project::TrackKind;
use std::path::{Path, PathBuf};

const DEFAULTS: &str = "Defaults";

/// The folders a new User Library starts with.
pub const LIBRARY_FOLDERS: [&str; 7] = [
    "Clips",
    DEFAULTS,
    "Grooves",
    "Presets",
    "Samples",
    "Templates",
    "Chord Banks",
];

/// What a sample dropped somewhere loads into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropTarget {
    OnDeviceView,
    OnDrumRack,
}

impl DropTarget {
    fn folder(self) -> &'static str {
        match self {
            Self::OnDeviceView => "On Device View",
            Self::OnDrumRack => "On Drum Rack",
        }
    }
}

/// An audio-to-MIDI conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conversion {
    Drums,
    Harmony,
    Melody,
}

impl Conversion {
    fn folder(self) -> &'static str {
        match self {
            Self::Drums => "Drums to MIDI",
            Self::Harmony => "Harmony to MIDI",
            Self::Melody => "Melody to MIDI",
        }
    }
}

/// The name of the folder holding a role's presets and defaults.
pub fn role_folder(role: Role) -> &'static str {
    match role {
        Role::Instrument => "Instruments",
        Role::AudioEffect => "Audio Effects",
        Role::MidiEffect => "MIDI Effects",
    }
}

fn track_folder(kind: TrackKind) -> Option<(&'static str, &'static str)> {
    match kind {
        TrackKind::Midi => Some(("MIDI Track", "Default MIDI Track")),
        TrackKind::Audio => Some(("Audio Track", "Default Audio Track")),
        _ => None,
    }
}

fn plugin_folder(format: PluginFormat) -> &'static str {
    match format {
        PluginFormat::Vst3 => "VSTs",
        PluginFormat::AudioUnit => "Audio Units",
        PluginFormat::Clap => "CLAP",
    }
}

/// The User Library on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserLibrary {
    pub root: PathBuf,
}

impl UserLibrary {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Where it goes unless the user says otherwise.
    pub fn default_location() -> PathBuf {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map_or_else(|| PathBuf::from("."), PathBuf::from);
        home.join("Music").join("rLive").join("User Library")
    }

    /// Makes the default folders, leaving what's there alone.
    pub fn ensure_folders(&self) -> std::io::Result<()> {
        for f in LIBRARY_FOLDERS {
            std::fs::create_dir_all(self.root.join(f))?;
        }
        Ok(())
    }

    /// The folder a device's saved presets go in.
    pub fn presets_dir(&self, role: Role, device: &str) -> PathBuf {
        self.root
            .join("Presets")
            .join(role_folder(role))
            .join(file_stem_safe(device))
    }

    pub fn rack_presets_dir(&self, device: &str) -> PathBuf {
        self.root
            .join("Presets")
            .join("Racks")
            .join(file_stem_safe(device))
    }

    pub fn chain_presets_dir(&self) -> PathBuf {
        self.root.join("Presets").join("Chains")
    }

    /// Referenced samples are copied here when a preset is saved.
    pub fn imported_samples_dir(&self) -> PathBuf {
        self.root.join("Samples").join("Imported")
    }

    pub fn defaults_dir(&self) -> PathBuf {
        self.root.join(DEFAULTS)
    }

    // ------------------------------------------------------------- where defaults are saved

    pub fn device_default_path(&self, role: Role, device_name: &str) -> PathBuf {
        device_default_in(&self.defaults_dir(), role, device_name)
    }

    pub fn plugin_config_path(&self, format: PluginFormat, plugin_id: &str) -> PathBuf {
        plugin_config_in(&self.defaults_dir(), format, plugin_id)
    }

    pub fn track_default_path(&self, kind: TrackKind) -> Option<PathBuf> {
        track_default_in(&self.defaults_dir(), kind)
    }

    pub fn dropping_dir(&self, target: DropTarget) -> PathBuf {
        dropping_in(&self.defaults_dir(), target)
    }

    pub fn slicing_dir(&self) -> PathBuf {
        self.defaults_dir().join("Slicing")
    }

    pub fn audio_to_midi_dir(&self, kind: Conversion) -> PathBuf {
        audio_to_midi_in(&self.defaults_dir(), kind)
    }
}

fn file_stem_safe(name: &str) -> String {
    name.replace(['/', '\\', ':'], "-")
}

fn device_default_in(defaults: &Path, role: Role, device_name: &str) -> PathBuf {
    defaults
        .join(role_folder(role))
        .join(file_name(device_name))
}

fn plugin_config_in(defaults: &Path, format: PluginFormat, plugin_id: &str) -> PathBuf {
    defaults
        .join("Plug-In Configurations")
        .join(plugin_folder(format))
        .join(file_name(&file_stem_safe(plugin_id)))
}

fn track_default_in(defaults: &Path, kind: TrackKind) -> Option<PathBuf> {
    let (folder, name) = track_folder(kind)?;
    Some(
        defaults
            .join("Creating Tracks")
            .join(folder)
            .join(file_name(name)),
    )
}

fn dropping_in(defaults: &Path, target: DropTarget) -> PathBuf {
    defaults.join("Dropping Samples").join(target.folder())
}

fn audio_to_midi_in(defaults: &Path, kind: Conversion) -> PathBuf {
    defaults.join("Audio to MIDI").join(kind.folder())
}

/// Where defaults are looked up: the Project's Defaults folder if there is one, then the User
/// Library's.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DefaultsLookup {
    pub project: Option<PathBuf>,
    pub library: Option<UserLibrary>,
}

impl DefaultsLookup {
    pub fn new(library: Option<UserLibrary>, project_folder: Option<&Path>) -> Self {
        Self {
            project: project_folder.map(|p| p.join(DEFAULTS)),
            library,
        }
    }

    fn folders(&self) -> impl Iterator<Item = PathBuf> + '_ {
        self.project
            .iter()
            .cloned()
            .chain(self.library.iter().map(UserLibrary::defaults_dir))
    }

    /// The first of a set of candidate paths that exists, a Project's before the Library's.
    fn first(&self, of: impl Fn(&Path) -> Option<PathBuf>) -> Option<PathBuf> {
        self.folders().filter_map(|d| of(&d)).find(|p| p.is_file())
    }

    /// The default preset of a device. The file has to carry the device's own name (p. 466).
    pub fn device_preset(&self, role: Role, device_name: &str) -> Option<PathBuf> {
        self.first(|d| Some(device_default_in(d, role, device_name)))
    }

    pub fn plugin_config(&self, format: PluginFormat, plugin_id: &str) -> Option<PathBuf> {
        self.first(|d| Some(plugin_config_in(d, format, plugin_id)))
    }

    pub fn track(&self, kind: TrackKind) -> Option<PathBuf> {
        self.first(|d| track_default_in(d, kind))
    }

    /// The preset a dropped sample loads into: of the first folder with one, the last added.
    pub fn dropping(&self, target: DropTarget) -> Option<PathBuf> {
        self.folders()
            .find_map(|d| newest_preset(&dropping_in(&d, target)))
    }

    /// The presets the Slice dialog offers, a Project's first.
    pub fn slicing(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for d in self.folders() {
            out.extend(presets_in(&d.join("Slicing")));
        }
        out
    }

    pub fn audio_to_midi(&self, kind: Conversion) -> Option<PathBuf> {
        self.folders()
            .find_map(|d| newest_preset(&audio_to_midi_in(&d, kind)))
    }
}

/// The presets directly in a folder, newest first.
pub fn presets_in(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|x| x == crate::preset::PRESET_EXTENSION)
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    found.into_iter().map(|(_, p)| p).collect()
}

fn newest_preset(dir: &Path) -> Option<PathBuf> {
    presets_in(dir).into_iter().next()
}
