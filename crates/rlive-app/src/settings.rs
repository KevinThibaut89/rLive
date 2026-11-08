//! Persistent user preferences, as set in the Settings window.

use crate::ui::theme::ThemeSettings;
use rlive_project::{Project, PALETTE};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// How many colours "Reduced Automatic Colors" draws from.
const REDUCED_COLORS: usize = 12;

/// When scroll bars are drawn.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum ScrollBars {
    #[default]
    WhenScrolling,
    Always,
}

/// What colour a newly created clip takes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum ClipColor {
    #[default]
    Track,
    Random,
}

/// The Track and Clip Colors section of the Theme & Colors tab.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ColorSettings {
    pub auto_assign: bool,
    pub default_track_color: [u8; 3],
    pub reduced: bool,
    pub new_clip: ClipColor,
}

impl Default for ColorSettings {
    fn default() -> Self {
        Self {
            auto_assign: true,
            default_track_color: PALETTE[0],
            reduced: false,
            new_clip: ClipColor::Track,
        }
    }
}

impl ColorSettings {
    /// The colour for a track added to `project` now.
    pub fn track_color(&self, project: &Project) -> [u8; 3] {
        if !self.auto_assign {
            return self.default_track_color;
        }
        if self.reduced {
            PALETTE[(project.tracks.len() * 5 + 9) % REDUCED_COLORS.min(PALETTE.len())]
        } else {
            project.next_color()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub audio_device: Option<String>,
    pub midi_input: Option<String>,
    pub sample_dir: Option<PathBuf>,
    pub plugin_dirs: Vec<PathBuf>,
    /// UI zoom factor (like Live's "Zoom Display").
    pub zoom: Option<f32>,
    pub show_browser: Option<bool>,
    pub show_detail: Option<bool>,
    pub show_info: bool,
    pub theme: ThemeSettings,
    pub colors: ColorSettings,
    /// Draw a border around the view that has keyboard focus.
    pub outline_focus: bool,
    pub scroll_bars: ScrollBars,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            audio_device: None,
            midi_input: None,
            sample_dir: None,
            plugin_dirs: Vec::new(),
            zoom: None,
            show_browser: None,
            show_detail: None,
            show_info: true,
            theme: ThemeSettings::default(),
            colors: ColorSettings::default(),
            outline_focus: false,
            scroll_bars: ScrollBars::default(),
        }
    }
}

impl Settings {
    pub fn zoom(&self) -> f32 {
        self.zoom.unwrap_or(1.0).clamp(0.7, 2.0)
    }

    pub fn path() -> PathBuf {
        rlive_plugin_host::scanner::PluginCache::default_path().with_file_name("settings.json")
    }

    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    pub fn load_from(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        self.save_to(&Self::path());
    }

    pub fn save_to(&self, path: &Path) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(path, serde_json::to_string_pretty(self).unwrap_or_default())
        {
            log::warn!("could not save settings: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::{Appearance, ThemeName, Tone};

    fn temp_file(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("rlive-settings-{}-{name}", std::process::id()))
    }

    #[test]
    fn round_trips_every_setting_through_a_file() {
        let settings = Settings {
            audio_device: Some("Interface".into()),
            midi_input: Some("Keys".into()),
            sample_dir: Some(PathBuf::from("/samples")),
            plugin_dirs: vec![PathBuf::from("/plugins")],
            zoom: Some(1.3),
            show_browser: Some(false),
            show_detail: Some(false),
            show_info: false,
            theme: ThemeSettings {
                name: ThemeName::Slate,
                appearance: Appearance::Light,
                tone: Tone::Warm,
                high_contrast: true,
                grid_opacity: 0.4,
                brightness: 0.2,
                color_intensity: 1.5,
                hue: 30.0,
            },
            colors: ColorSettings {
                auto_assign: false,
                default_track_color: [1, 2, 3],
                reduced: true,
                new_clip: ClipColor::Random,
            },
            outline_focus: true,
            scroll_bars: ScrollBars::Always,
        };
        let path = temp_file("round-trip.json");
        settings.save_to(&path);
        let loaded = Settings::load_from(&path);
        let _ = std::fs::remove_file(&path);
        assert_eq!(loaded, settings);
    }

    #[test]
    fn settings_saved_by_an_older_version_still_load() {
        let old = r#"{ "audio_device": "Old", "zoom": 1.2, "show_detail": false }"#;
        let loaded: Settings = serde_json::from_str(old).unwrap();
        assert_eq!(loaded.audio_device.as_deref(), Some("Old"));
        assert_eq!(loaded.zoom, Some(1.2));
        assert_eq!(loaded.theme, ThemeSettings::default());
        assert!(loaded.show_info, "the Info View starts visible");
    }

    #[test]
    fn a_corrupt_file_falls_back_to_defaults() {
        let path = temp_file("corrupt.json");
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(Settings::load_from(&path), Settings::default());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn track_colors_follow_the_settings() {
        let p = Project::new_default();
        let auto = ColorSettings::default();
        assert_eq!(auto.track_color(&p), p.next_color());
        let fixed = ColorSettings {
            auto_assign: false,
            default_track_color: [9, 9, 9],
            ..auto
        };
        assert_eq!(fixed.track_color(&p), [9, 9, 9]);
        let reduced = ColorSettings {
            reduced: true,
            ..auto
        };
        assert!(PALETTE[..REDUCED_COLORS].contains(&reduced.track_color(&p)));
    }
}
