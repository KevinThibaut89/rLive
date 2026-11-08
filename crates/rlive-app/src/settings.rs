//! Persistent user preferences, as set in the Settings window.

use crate::session::WarpDefaults;
use crate::ui::theme::ThemeSettings;
use rlive_audio_io::{BitDepth, CacheConfig, FileType, LoaderConfig};
use rlive_project::clip_envelope::SessionAutomationIn;
use rlive_project::{ExternalRoute, Project, PALETTE};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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

const GIB: f32 = 1024.0 * 1024.0 * 1024.0;

/// The File & Folder tab's sample settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SampleSettings {
    /// Write an analysis file next to each sample when it's first loaded.
    pub create_analysis_files: bool,
    /// How much of the disk to leave free, in GB.
    pub minimum_free_space_gb: f32,
    /// Whether the decoding cache has a size limit.
    pub limit_cache_size: bool,
    pub maximum_cache_size_gb: f32,
    /// Where decoded files go; `None` is the system's cache folder.
    pub cache_folder: Option<PathBuf>,
}

impl Default for SampleSettings {
    fn default() -> Self {
        Self {
            create_analysis_files: true,
            minimum_free_space_gb: 2.0,
            limit_cache_size: true,
            maximum_cache_size_gb: 20.0,
            cache_folder: None,
        }
    }
}

impl SampleSettings {
    pub fn loader_config(&self) -> LoaderConfig {
        LoaderConfig {
            create_analysis_files: self.create_analysis_files,
            cache: CacheConfig {
                folder: self
                    .cache_folder
                    .clone()
                    .unwrap_or_else(LoaderConfig::default_cache_folder),
                minimum_free_space: (self.minimum_free_space_gb.max(0.0) * GIB) as u64,
                maximum_size: self
                    .limit_cache_size
                    .then_some((self.maximum_cache_size_gb.max(0.0) * GIB) as u64),
            },
        }
    }
}

/// One hardware channel in the Channel Configuration dialog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelConfig {
    pub enabled: bool,
    /// The user's name for it. Empty keeps its number.
    pub name: String,
}

impl Default for ChannelConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            name: String::new(),
        }
    }
}

/// Which channels of an interface are used, how they pair up and what they are called.
/// Changed names are always associated with their audio device (manual p. 375).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelBank {
    /// One per hardware channel, however many have been seen.
    pub channels: Vec<ChannelConfig>,
    /// One per odd/even pair: whether the pair is one stereo channel or two mono ones. A pair
    /// not listed is stereo.
    pub mono_pairs: Vec<bool>,
}

impl ChannelBank {
    pub fn channel(&self, n: usize) -> ChannelConfig {
        self.channels.get(n).cloned().unwrap_or_default()
    }

    pub fn channel_mut(&mut self, n: usize) -> &mut ChannelConfig {
        if self.channels.len() <= n {
            self.channels.resize(n + 1, ChannelConfig::default());
        }
        &mut self.channels[n]
    }

    pub fn is_mono(&self, pair: usize) -> bool {
        self.mono_pairs.get(pair).copied().unwrap_or(false)
    }

    pub fn set_mono(&mut self, pair: usize, mono: bool) {
        if self.mono_pairs.len() <= pair {
            self.mono_pairs.resize(pair + 1, false);
        }
        self.mono_pairs[pair] = mono;
    }

    fn label(&self, n: usize) -> String {
        let name = self.channel(n).name;
        if name.is_empty() {
            (n + 1).to_string()
        } else {
            name
        }
    }

    /// What the choosers offer, given how many channels the hardware has.
    pub fn routes(&self, count: usize) -> Vec<(String, ExternalRoute)> {
        let mut out = Vec::new();
        for first in (0..count).step_by(2) {
            let left = self.channel(first).enabled;
            let has_right = first + 1 < count;
            let right = has_right && self.channel(first + 1).enabled;
            if has_right && !self.is_mono(first / 2) {
                if left && right {
                    out.push((
                        format!("{}/{}", self.label(first), self.label(first + 1)),
                        ExternalRoute::Audio {
                            channel: first as u16,
                            stereo: true,
                        },
                    ));
                }
                continue;
            }
            let mono = |n: usize| {
                (
                    self.label(n),
                    ExternalRoute::Audio {
                        channel: n as u16,
                        stereo: false,
                    },
                )
            };
            if left {
                out.push(mono(first));
            }
            if right {
                out.push(mono(first + 1));
            }
        }
        out
    }
}

/// The Channel Configuration of one audio device.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceChannels {
    pub inputs: ChannelBank,
    pub outputs: ChannelBank,
}

/// Which input device rLive listens to.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum InputDevice {
    /// Nothing: no stream is opened, so the system never asks about the microphone.
    #[default]
    None,
    SystemDefault,
    Named(String),
}

/// The Record, Warp & Launch settings that shape recording.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecordSettings {
    pub file_type: FileType,
    pub bit_depth: BitDepth,
    /// Pressing Record starts playback; Shift inverts it.
    pub start_playback_with_record: bool,
    /// Launching a scene records into its empty slots on armed tracks.
    pub start_recording_on_scene_launch: bool,
    /// Arming a track disarms the others unless Cmd is held.
    pub exclusive_arm: bool,
    /// Where recordings go until the Set is saved; `None` is a folder in the system's.
    pub temp_folder: Option<PathBuf>,
}

impl Default for RecordSettings {
    fn default() -> Self {
        Self {
            file_type: FileType::default(),
            bit_depth: BitDepth::default(),
            start_playback_with_record: true,
            start_recording_on_scene_launch: false,
            exclusive_arm: true,
            temp_folder: None,
        }
    }
}

/// The Launch section of the Record, Warp & Launch tab.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LaunchPrefs {
    pub default_mode: rlive_core::LaunchMode,
    /// `None` is Global.
    pub default_quantize: Option<rlive_core::Quantize>,
    /// Launching a clip or scene also selects it (manual p. 196).
    pub select_on_launch: bool,
    /// The scene below a launched one is selected next (manual p. 190).
    pub select_next_scene: bool,
}

impl Default for LaunchPrefs {
    fn default() -> Self {
        Self {
            default_mode: rlive_core::LaunchMode::default(),
            default_quantize: None,
            select_on_launch: true,
            select_next_scene: true,
        }
    }
}

impl LaunchPrefs {
    pub fn defaults(&self) -> crate::session::LaunchDefaults {
        crate::session::LaunchDefaults {
            mode: self.default_mode,
            quantize: self.default_quantize,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub audio_device: Option<String>,
    /// The device audio is recorded from. Off until someone picks one.
    pub audio_input: InputDevice,
    /// Channel Configuration, by audio device name.
    pub audio_channels: BTreeMap<String, DeviceChannels>,
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
    pub samples: SampleSettings,
    pub warp: WarpDefaults,
    pub record: RecordSettings,
    pub launch: LaunchPrefs,
    /// MIDI Envelope Auto-Reset: new clips set some controllers back first (manual p. 534).
    pub midi_envelope_auto_reset: bool,
    /// Where Session Record's automation goes (manual p. 512).
    pub record_session_automation_in: SessionAutomationIn,
    /// Short default fades at clip edges, and crossfades where audio clips touch.
    pub create_fades: bool,
    pub chase_notes: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            audio_device: None,
            audio_input: InputDevice::None,
            audio_channels: BTreeMap::new(),
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
            samples: SampleSettings::default(),
            launch: LaunchPrefs::default(),
            warp: WarpDefaults::default(),
            record: RecordSettings::default(),
            midi_envelope_auto_reset: false,
            record_session_automation_in: SessionAutomationIn::default(),
            create_fades: true,
            chase_notes: true,
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
    use crate::session::ShortSampleMode;
    use crate::ui::theme::{Appearance, ThemeName, Tone};
    use rlive_project::WarpMode;

    #[test]
    fn channels_are_stereo_pairs_until_split_and_named_per_device() {
        let mut bank = ChannelBank::default();
        let routes = |bank: &ChannelBank, n| {
            bank.routes(n)
                .into_iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>()
        };
        assert_eq!(routes(&bank, 4), ["1/2", "3/4"]);
        bank.set_mono(1, true);
        bank.channel_mut(2).name = "Phones".into();
        assert_eq!(routes(&bank, 4), ["1/2", "Phones", "4"]);
        bank.channel_mut(3).enabled = false;
        assert_eq!(routes(&bank, 4), ["1/2", "Phones"]);
        assert_eq!(
            bank.routes(4)[1].1,
            ExternalRoute::Audio {
                channel: 2,
                stereo: false
            }
        );
        // An odd channel out is mono, and a disabled half of a pair takes the pair with it.
        assert_eq!(routes(&ChannelBank::default(), 3), ["1/2", "3"]);
        let mut half = ChannelBank::default();
        half.channel_mut(1).enabled = false;
        assert!(routes(&half, 2).is_empty());
    }

    fn temp_file(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("rlive-settings-{}-{name}", std::process::id()))
    }

    #[test]
    fn round_trips_every_setting_through_a_file() {
        let settings = Settings {
            audio_device: Some("Interface".into()),
            audio_channels: BTreeMap::from([(
                "Interface".to_string(),
                DeviceChannels {
                    inputs: ChannelBank::default(),
                    outputs: ChannelBank {
                        channels: vec![ChannelConfig {
                            enabled: false,
                            name: "Monitors".into(),
                        }],
                        mono_pairs: vec![true],
                    },
                },
            )]),
            audio_input: InputDevice::Named("Line In".into()),
            record: RecordSettings {
                file_type: FileType::Aiff,
                bit_depth: BitDepth::Bits16,
                start_playback_with_record: false,
                start_recording_on_scene_launch: true,
                exclusive_arm: false,
                temp_folder: Some(PathBuf::from("/tmp/recordings")),
            },
            launch: LaunchPrefs {
                default_mode: rlive_core::LaunchMode::Gate,
                default_quantize: Some(rlive_core::Quantize::Beat),
                select_on_launch: false,
                select_next_scene: false,
            },
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
            samples: SampleSettings {
                create_analysis_files: false,
                minimum_free_space_gb: 5.0,
                limit_cache_size: false,
                maximum_cache_size_gb: 40.0,
                cache_folder: Some(PathBuf::from("/fast/cache")),
            },
            warp: WarpDefaults {
                short_samples: ShortSampleMode::WarpedOneShot,
                mode: WarpMode::RePitch,
            },
            midi_envelope_auto_reset: true,
            record_session_automation_in: SessionAutomationIn::AllTracks,
            create_fades: false,
            chase_notes: false,
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
    fn sample_settings_become_the_loader_config() {
        let defaults = SampleSettings::default().loader_config();
        assert!(defaults.create_analysis_files);
        assert_eq!(defaults.cache.maximum_size, Some(20 * 1024 * 1024 * 1024));
        assert_eq!(defaults.cache.folder, LoaderConfig::default_cache_folder());
        let chosen = SampleSettings {
            create_analysis_files: false,
            minimum_free_space_gb: 1.0,
            limit_cache_size: false,
            maximum_cache_size_gb: 1.0,
            cache_folder: Some(PathBuf::from("/x")),
        }
        .loader_config();
        assert!(!chosen.create_analysis_files);
        assert_eq!(chosen.cache.minimum_free_space, 1024 * 1024 * 1024);
        assert_eq!(chosen.cache.maximum_size, None, "no limit set");
        assert_eq!(chosen.cache.folder, PathBuf::from("/x"));
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
