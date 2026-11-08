//! The Settings window: one tab for each section of Live's settings.
//!
//! Controls whose feature exists are live. The others are drawn disabled, with the milestone that
//! will build them in the tooltip, so the window already shows everything that's coming.

use super::commands::{options::Options, MenuCommand};
use super::theme::{pal, Appearance, ThemeName, ThemeSettings, Tone};
use super::{session_view, widgets, RliveApp};
use crate::audio;
use crate::midi_input::{self, MidiInput};
use crate::settings::{ClipColor, ScrollBars};
use egui::{Color32, RichText, Sense, Stroke, StrokeKind, Vec2};

/// A page of the Settings window.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Tab {
    #[default]
    Display,
    Theme,
    Audio,
    Link,
    TempoMidi,
    FileFolder,
    Library,
    PlugIns,
    RecordWarpLaunch,
    Licenses,
}

impl Tab {
    pub const ALL: [Tab; 10] = [
        Self::Display,
        Self::Theme,
        Self::Audio,
        Self::Link,
        Self::TempoMidi,
        Self::FileFolder,
        Self::Library,
        Self::PlugIns,
        Self::RecordWarpLaunch,
        Self::Licenses,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Display => "Display & Input",
            Self::Theme => "Theme & Colors",
            Self::Audio => "Audio",
            Self::Link => "Link",
            Self::TempoMidi => "Tempo & MIDI",
            Self::FileFolder => "File & Folder",
            Self::Library => "Library",
            Self::PlugIns => "Plug-Ins",
            Self::RecordWarpLaunch => "Record, Warp & Launch",
            Self::Licenses => "Licenses & Updates",
        }
    }
}

/// Applies the scroll bar setting to egui's style.
pub fn apply_scroll_bars(ctx: &egui::Context, mode: ScrollBars) {
    ctx.all_styles_mut(|s| {
        s.spacing.scroll = match mode {
            ScrollBars::WhenScrolling => egui::style::ScrollStyle::thin(),
            ScrollBars::Always => egui::style::ScrollStyle::solid(),
        };
    });
}

fn coming(milestone: &str) -> String {
    match milestone {
        "out" => "out of scope for rLive".to_string(),
        "later" => "coming in a later milestone".to_string(),
        m => format!("coming in {m}"),
    }
}

/// A control that doesn't work yet: a label and a greyed-out value.
fn soon(ui: &mut egui::Ui, label: &str, value: &str, milestone: &str) {
    ui.label(RichText::new(label).color(pal().text_faint));
    let r = ui.add_enabled(false, egui::Button::new(value));
    r.on_disabled_hover_text(coming(milestone));
    ui.end_row();
}

/// A switch that doesn't work yet.
fn soon_check(ui: &mut egui::Ui, label: &str, on: bool, milestone: &str) {
    ui.label(RichText::new(label).color(pal().text_faint));
    let mut value = on;
    let r = ui.add_enabled(false, egui::Checkbox::without_text(&mut value));
    r.on_disabled_hover_text(coming(milestone));
    ui.end_row();
}

fn grid(ui: &mut egui::Ui, salt: &str, rows: impl FnOnce(&mut egui::Ui)) {
    egui::Grid::new(salt)
        .num_columns(2)
        .spacing([18.0, 6.0])
        .min_col_width(170.0)
        .show(ui, rows);
}

fn label(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).color(pal().text));
}

impl RliveApp {
    /// Shows the window, and saves if anything in it changed.
    pub(super) fn settings_window(&mut self, ctx: &egui::Context) {
        let before = self.settings.clone();
        let mut open = self.ui.show_settings;
        egui::Window::new("Settings")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .default_width(640.0)
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(150.0);
                        for tab in Tab::ALL {
                            ui.selectable_value(&mut self.ui.settings_tab, tab, tab.label());
                        }
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.set_width(460.0);
                        ui.set_min_height(380.0);
                        egui::ScrollArea::vertical().show(ui, |ui| match self.ui.settings_tab {
                            Tab::Display => self.display_tab(ui),
                            Tab::Theme => self.theme_tab(ui),
                            Tab::Audio => self.audio_tab(ui),
                            Tab::Link => link_tab(ui),
                            Tab::TempoMidi => self.tempo_midi_tab(ui),
                            Tab::FileFolder => self.file_folder_tab(ui),
                            Tab::Library => self.library_tab(ui),
                            Tab::PlugIns => self.plugins_tab(ui),
                            Tab::RecordWarpLaunch => record_warp_launch_tab(ui),
                            Tab::Licenses => licenses_tab(ui),
                        });
                    });
                });
            });
        self.ui.show_settings = open;
        if self.settings != before {
            self.settings.save();
            if self.settings.samples != before.samples {
                self.session
                    .samples
                    .configure(self.settings.samples.loader_config());
            }
        }
    }

    fn display_tab(&mut self, ui: &mut egui::Ui) {
        widgets::section_label(ui, "DISPLAY");
        grid(ui, "display", |ui| {
            soon(ui, "Language", "English", "later");
            label(ui, "Zoom Display");
            let mut z = ui.ctx().zoom_factor();
            let r = ui.add(
                egui::Slider::new(&mut z, 0.7..=2.0)
                    .step_by(0.1)
                    .suffix("×"),
            );
            if r.changed() {
                self.set_zoom(&ui.ctx().clone(), z);
            }
            ui.end_row();
            soon(ui, "Second Window Zoom", "100%", "C17");
            soon(ui, "Settings Window Zoom", "100%", "C17");
        });
        ui.add_space(10.0);
        widgets::section_label(ui, "DISPLAY OPTIONS");
        grid(ui, "display_options", |ui| {
            label(ui, "Outline View in Focus");
            ui.checkbox(&mut self.settings.outline_focus, "");
            ui.end_row();
            label(ui, "Scroll Bars");
            let before = self.settings.scroll_bars;
            egui::ComboBox::from_id_salt("scroll_bars")
                .width(160.0)
                .selected_text(match before {
                    ScrollBars::WhenScrolling => "When Scrolling",
                    ScrollBars::Always => "Always",
                })
                .show_ui(ui, |ui| {
                    let v = &mut self.settings.scroll_bars;
                    ui.selectable_value(v, ScrollBars::WhenScrolling, "When Scrolling");
                    ui.selectable_value(v, ScrollBars::Always, "Always");
                });
            if self.settings.scroll_bars != before {
                apply_scroll_bars(ui.ctx(), self.settings.scroll_bars);
            }
            ui.end_row();
            soon(ui, "Follow Behavior", "Page", "C2");
            soon_check(ui, "Show UI Labels", true, "C19");
        });
        ui.add_space(10.0);
        widgets::section_label(ui, "NAVIGATION AND KEYBOARD");
        grid(ui, "navigation", |ui| {
            soon_check(ui, "Use Tab to Move Focus", false, "C19");
            soon_check(ui, "Wrap Tab Navigation", false, "C19");
            soon_check(ui, "Move Clips with Arrow Keys", true, "C4");
        });
        ui.add_space(10.0);
        widgets::section_label(ui, "MOUSE AND PEN INPUT");
        grid(ui, "mouse", |ui| {
            soon_check(ui, "Pen Tablet Mode", false, "C19");
            soon_check(ui, "Permanent Scrub Areas", true, "C2");
            soon_check(ui, "Pitch-Locked Drawing", false, "C9");
        });
        ui.add_space(10.0);
        widgets::section_label(ui, "APPLICATION OPTIONS");
        grid(ui, "application", |ui| {
            soon(ui, "Don't Show Again Warnings", "Restore", "C13");
        });
    }

    fn theme_tab(&mut self, ui: &mut egui::Ui) {
        let t = &mut self.settings.theme;
        widgets::section_label(ui, "THEME");
        grid(ui, "theme", |ui| {
            label(ui, "Theme");
            egui::ComboBox::from_id_salt("theme_name")
                .width(160.0)
                .selected_text(t.name.label())
                .show_ui(ui, |ui| {
                    for name in ThemeName::ALL {
                        ui.selectable_value(&mut t.name, name, name.label());
                    }
                });
            ui.end_row();
            label(ui, "Appearance");
            egui::ComboBox::from_id_salt("theme_appearance")
                .width(160.0)
                .selected_text(t.appearance.label())
                .show_ui(ui, |ui| {
                    for a in Appearance::ALL {
                        ui.selectable_value(&mut t.appearance, a, a.label());
                    }
                });
            ui.end_row();
            label(ui, "Tone");
            egui::ComboBox::from_id_salt("theme_tone")
                .width(160.0)
                .selected_text(t.tone.label())
                .show_ui(ui, |ui| {
                    for tone in Tone::ALL {
                        ui.selectable_value(&mut t.tone, tone, tone.label());
                    }
                });
            ui.end_row();
            label(ui, "High Contrast");
            ui.checkbox(&mut t.high_contrast, "");
            ui.end_row();
        });
        ui.add_space(10.0);
        widgets::section_label(ui, "CUSTOMIZATION");
        grid(ui, "customization", |ui| {
            label(ui, "Grid Line Opacity");
            ui.add(egui::Slider::new(&mut t.grid_opacity, 0.0..=1.0));
            ui.end_row();
            label(ui, "Brightness");
            ui.add(egui::Slider::new(&mut t.brightness, -1.0..=1.0));
            ui.end_row();
            label(ui, "Color Intensity");
            ui.add(egui::Slider::new(&mut t.color_intensity, 0.0..=2.0));
            ui.end_row();
            label(ui, "Color Hue");
            ui.add(egui::Slider::new(&mut t.hue, -180.0..=180.0).suffix("°"));
            ui.end_row();
        });
        if ui.button("Reset Theme").clicked() {
            *t = ThemeSettings::default();
        }
        ui.add_space(10.0);
        widgets::section_label(ui, "TRACK AND CLIP COLORS");
        let c = &mut self.settings.colors;
        grid(ui, "track_colors", |ui| {
            label(ui, "Auto-Assign Track Colors");
            ui.checkbox(&mut c.auto_assign, "");
            ui.end_row();
            label(ui, "Default Track Color");
            let mut picked = None;
            ui.add_enabled_ui(!c.auto_assign, |ui| {
                ui.menu_button("Choose…", |ui| {
                    if let Some(color) = session_view::color_picker(ui) {
                        picked = Some(color);
                        ui.close();
                    }
                });
            });
            if let Some(color) = picked {
                c.default_track_color = color;
            }
            ui.end_row();
            label(ui, "Reduced Automatic Colors");
            ui.checkbox(&mut c.reduced, "");
            ui.end_row();
            label(ui, "Clip Color");
            egui::ComboBox::from_id_salt("clip_color")
                .width(160.0)
                .selected_text(match c.new_clip {
                    ClipColor::Track => "Track Color",
                    ClipColor::Random => "Random",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut c.new_clip, ClipColor::Track, "Track Color");
                    ui.selectable_value(&mut c.new_clip, ClipColor::Random, "Random");
                });
            ui.end_row();
            let swatch = c.default_track_color;
            label(ui, "");
            let (r, _) = ui.allocate_exact_size(Vec2::new(40.0, 12.0), Sense::hover());
            ui.painter()
                .rect_filled(r, 1.0, Color32::from_rgb(swatch[0], swatch[1], swatch[2]));
            ui.painter().rect_stroke(
                r,
                1.0,
                Stroke::new(1.0, pal().separator),
                StrokeKind::Inside,
            );
            ui.end_row();
        });
    }

    fn audio_tab(&mut self, ui: &mut egui::Ui) {
        widgets::section_label(ui, "AUDIO DEVICE");
        let devices = self
            .ui
            .audio_devices
            .get_or_insert_with(audio::output_device_names)
            .clone();
        let current = self
            .settings
            .audio_device
            .clone()
            .unwrap_or_else(|| "System default".into());
        let mut choice = self.settings.audio_device.clone();
        let mut refresh = false;
        grid(ui, "audio", |ui| {
            soon(ui, "Driver Type", "Default", "A5");
            soon(ui, "Audio Input Device", "None", "A11");
            label(ui, "Audio Output Device");
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("audio_dev")
                    .width(200.0)
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut choice, None, "System default");
                        for d in devices {
                            ui.selectable_value(&mut choice, Some(d.clone()), d);
                        }
                    });
                refresh = ui.small_button("Refresh").clicked();
            });
            ui.end_row();
            soon(ui, "Channel Configuration", "Stereo", "A5");
            soon(ui, "Sample Rate", "Device default", "A5");
            soon(ui, "Buffer Size", "Device default", "A5");
            soon(ui, "Output Latency", "—", "A4");
            soon(ui, "Test Tone", "Off", "A5");
        });
        if refresh {
            self.ui.audio_devices = None;
        }
        if choice != self.settings.audio_device {
            self.settings.audio_device = choice;
            self.settings.save();
            self.restart_audio();
        }
        ui.label(
            RichText::new(format!(
                "Running: {} @ {} Hz",
                self.audio.device_name, self.audio.sample_rate
            ))
            .color(pal().text_dim),
        );
    }

    fn tempo_midi_tab(&mut self, ui: &mut egui::Ui) {
        widgets::section_label(ui, "MIDI INPUT");
        let ports = self
            .ui
            .midi_ports
            .get_or_insert_with(midi_input::port_names)
            .clone();
        let mut port = self.settings.midi_input.clone();
        let mut reconnect = false;
        grid(ui, "midi", |ui| {
            label(ui, "Input");
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("midi_port")
                    .width(200.0)
                    .selected_text(port.clone().unwrap_or_else(|| "First available".into()))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut port, None, "First available");
                        for p in ports {
                            ui.selectable_value(&mut port, Some(p.clone()), p);
                        }
                    });
                reconnect = ui.small_button("Reconnect").clicked();
            });
            ui.end_row();
        });
        if port != self.settings.midi_input || reconnect {
            self.ui.midi_ports = None;
            self.settings.midi_input = port;
            self.settings.save();
            self.midi = MidiInput::none();
            self.midi =
                MidiInput::connect(self.settings.midi_input.as_deref(), self.routing.clone())
                    .unwrap_or_else(|e| {
                        self.ui.status = format!("MIDI: {e}");
                        MidiInput::none()
                    });
        }
        ui.add_space(10.0);
        widgets::section_label(ui, "COMPUTER MIDI KEYBOARD");
        grid(ui, "computer_keys", |ui| {
            label(ui, "Enabled");
            let mut on = self.ui.computer_keys;
            if ui.checkbox(&mut on, "").changed() {
                self.pending
                    .push(MenuCommand::Options(Options::ToggleComputerKeys));
            }
            ui.end_row();
        });
        ui.add_space(10.0);
        widgets::section_label(ui, "CONTROL SURFACES AND SYNC");
        grid(ui, "surfaces", |ui| {
            soon(ui, "Control Surface", "None", "A15");
            soon(ui, "Takeover Mode", "Pick-Up", "A15");
            soon(ui, "Input Tracks / Sync / Remote", "Per port", "A15");
            soon(ui, "Tempo Follower", "Off", "A16");
            soon(ui, "Sync Output", "Off", "A16");
        });
    }

    fn library_tab(&mut self, ui: &mut egui::Ui) {
        widgets::section_label(ui, "LOCATIONS");
        grid(ui, "library", |ui| {
            label(ui, "Samples Folder");
            ui.horizontal(|ui| {
                let shown = self
                    .settings
                    .sample_dir
                    .as_ref()
                    .map_or("Not set".to_string(), |p| p.display().to_string());
                ui.label(RichText::new(shown).color(pal().text_dim));
                if ui.small_button("Choose…").clicked() {
                    if let Some(d) = rfd::FileDialog::new().pick_folder() {
                        self.ui.sample_files = super::browser::list_samples(&d);
                        self.settings.sample_dir = Some(d);
                    }
                }
            });
            ui.end_row();
            soon(ui, "User Library", "Default", "C12");
            soon(ui, "Installation Folder for Packs", "Default", "out");
        });
        ui.add_space(10.0);
        widgets::section_label(ui, "FILES");
        grid(ui, "library_files", |ui| {
            soon(ui, "Collect Files on Export", "Ask", "C13");
        });
        ui.add_space(10.0);
        widgets::section_label(ui, "PLACES");
        grid(ui, "places", |ui| {
            soon_check(ui, "Show Cloud in Places", false, "out");
            soon_check(ui, "Show Push in Places", false, "out");
            soon_check(ui, "Show Splice in Places", false, "out");
        });
    }

    fn file_folder_tab(&mut self, ui: &mut egui::Ui) {
        let samples = &mut self.settings.samples;
        widgets::section_label(ui, "FILES");
        grid(ui, "files", |ui| {
            label(ui, "Create Analysis Files");
            ui.checkbox(&mut samples.create_analysis_files, "");
            ui.end_row();
            soon(ui, "Sample Editor", "None", "C13");
            soon(ui, "Temporary Folder", "Default", "A11");
        });
        ui.add_space(10.0);
        widgets::section_label(ui, "DECODING CACHE");
        grid(ui, "cache", |ui| {
            label(ui, "Minimum Free Space");
            ui.add(
                egui::DragValue::new(&mut samples.minimum_free_space_gb)
                    .range(0.0..=1000.0)
                    .speed(0.1)
                    .suffix(" GB"),
            );
            ui.end_row();
            label(ui, "Maximum Cache Size");
            ui.horizontal(|ui| {
                ui.checkbox(&mut samples.limit_cache_size, "");
                ui.add_enabled(
                    samples.limit_cache_size,
                    egui::DragValue::new(&mut samples.maximum_cache_size_gb)
                        .range(0.1..=4000.0)
                        .speed(0.5)
                        .suffix(" GB"),
                );
            });
            ui.end_row();
            label(ui, "Cache Folder");
            ui.horizontal(|ui| {
                let shown = samples
                    .cache_folder
                    .as_ref()
                    .map_or("Default".to_string(), |p| p.display().to_string());
                ui.label(RichText::new(shown).color(pal().text_dim));
                if ui.small_button("Browse…").clicked() {
                    if let Some(d) = rfd::FileDialog::new().pick_folder() {
                        samples.cache_folder = Some(d);
                    }
                }
                if samples.cache_folder.is_some() && ui.small_button("Use Default").clicked() {
                    samples.cache_folder = None;
                }
            });
            ui.end_row();
            label(ui, "Cleanup");
            if ui
                .small_button("Delete files not used by this Set")
                .clicked()
            {
                let done = self.session.cleanup_decoding_cache();
                self.ui.status = format!(
                    "Removed {} decoded files ({:.1} MB)",
                    done.files,
                    done.bytes as f64 / (1024.0 * 1024.0)
                );
            }
            ui.end_row();
        });
        ui.add_space(10.0);
        widgets::section_label(ui, "MAX");
        grid(ui, "max", |ui| soon(ui, "Max Application", "None", "out"));
    }

    fn plugins_tab(&mut self, ui: &mut egui::Ui) {
        widgets::section_label(ui, "PLUG-IN SOURCES");
        ui.label(
            RichText::new("System CLAP/VST3 folders and Audio Units are searched, plus:")
                .color(pal().text_dim),
        );
        let mut remove = None;
        for (i, d) in self.settings.plugin_dirs.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(d.display().to_string());
                if ui.small_button("Remove").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            self.settings.plugin_dirs.remove(i);
        }
        ui.horizontal(|ui| {
            if ui.button("Add Folder…").clicked() {
                if let Some(d) = rfd::FileDialog::new().pick_folder() {
                    self.settings.plugin_dirs.push(d);
                }
            }
            if ui
                .add_enabled(self.ui.scan_rx.is_none(), egui::Button::new("Rescan"))
                .clicked()
            {
                self.start_plugin_scan();
            }
        });
        ui.label(RichText::new(&self.ui.scan_status).color(pal().text_dim));
        let failed: Vec<_> = self
            .session
            .plugins
            .files
            .iter()
            .filter(|f| !f.ok)
            .collect();
        if !failed.is_empty() {
            ui.collapsing(
                format!("{} plugin files failed to scan", failed.len()),
                |ui| {
                    for f in failed {
                        ui.label(format!(
                            "{}: {}",
                            f.path.display(),
                            f.error.clone().unwrap_or_default()
                        ));
                    }
                },
            );
        }
        ui.add_space(10.0);
        widgets::section_label(ui, "PLUG-IN WINDOWS");
        grid(ui, "plugin_windows", |ui| {
            soon_check(ui, "Auto-Open Plug-In Windows", true, "A17");
            soon_check(ui, "Multiple Plug-In Windows", true, "A17");
            soon_check(ui, "Auto-Hide Plug-In Windows", true, "A17");
        });
    }
}

fn link_tab(ui: &mut egui::Ui) {
    widgets::section_label(ui, "LINK");
    grid(ui, "link", |ui| {
        soon_check(ui, "Show Link Toggle", false, "A16");
        soon_check(ui, "Start Stop Sync", false, "A16");
        soon_check(ui, "Link Audio", false, "A16");
    });
}

fn record_warp_launch_tab(ui: &mut egui::Ui) {
    widgets::section_label(ui, "RECORD");
    grid(ui, "record", |ui| {
        soon(ui, "File Type", "WAV", "A11");
        soon(ui, "Bit Depth", "24", "A11");
        soon(ui, "Count-In", "None", "A11");
        soon_check(ui, "Start Recording on Scene Launch", false, "A11");
        soon_check(ui, "Exclusive Arm", true, "A11");
        soon_check(ui, "Exclusive Solo", true, "A1");
    });
    ui.add_space(10.0);
    widgets::section_label(ui, "WARP");
    grid(ui, "warp", |ui| {
        soon_check(ui, "Auto-Warp Long Samples", true, "A9");
        soon(ui, "Default Warp Mode", "Beats", "A9");
        soon_check(ui, "Create Fades on Clip Edges", true, "C5");
    });
    ui.add_space(10.0);
    widgets::section_label(ui, "LAUNCH");
    grid(ui, "launch", |ui| {
        soon(ui, "Default Launch Mode", "Trigger", "C7");
        soon(ui, "Default Launch Quantization", "Global", "C7");
        soon_check(ui, "Select on Launch", true, "C7");
        soon_check(ui, "Select Next Scene on Launch", false, "C7");
        soon(ui, "Clip Update Rate", "1/16", "C7");
    });
}

fn licenses_tab(ui: &mut egui::Ui) {
    widgets::section_label(ui, "LICENSES AND UPDATES");
    grid(ui, "licenses", |ui| {
        soon(ui, "Authorization", "Not used", "out");
        soon_check(ui, "Automatic Updates", false, "out");
        soon_check(ui, "Send Usage Data", false, "out");
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_has_the_manuals_ten_tabs() {
        assert_eq!(Tab::ALL.len(), 10);
        let labels: std::collections::HashSet<_> = Tab::ALL.iter().map(|t| t.label()).collect();
        assert_eq!(labels.len(), 10);
    }

    #[test]
    fn disabled_controls_name_their_milestone() {
        assert_eq!(coming("A16"), "coming in A16");
        assert_eq!(coming("out"), "out of scope for rLive");
    }
}
