//! Persistent user preferences (audio device, MIDI port, folders).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub audio_device: Option<String>,
    pub midi_input: Option<String>,
    pub sample_dir: Option<PathBuf>,
    pub plugin_dirs: Vec<PathBuf>,
}

impl Settings {
    pub fn path() -> PathBuf {
        rlive_plugin_host::scanner::PluginCache::default_path().with_file_name("settings.json")
    }

    pub fn load() -> Self {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(
            &path,
            serde_json::to_string_pretty(self).unwrap_or_default(),
        ) {
            log::warn!("could not save settings: {e}");
        }
    }
}
