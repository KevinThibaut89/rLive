//! The Settings window: one tab for each section of Live's settings.
//!
//! Controls whose feature exists are live. The others are drawn disabled, with the milestone that
//! will build them in the tooltip, so the window already shows everything that's coming.

use super::commands::{options::Options, MenuCommand};
use super::theme::{pal, Appearance, ThemeName, ThemeSettings, Tone};
use super::{sample_editor, session_view, widgets, RliveApp};
use crate::audio;
use crate::midi_input;
use crate::session::ShortSampleMode;
use crate::settings::{ChannelBank, ClipColor, FollowBehavior, InputDevice, ScrollBars, Settings};
use egui::{Color32, RichText, Sense, Stroke, StrokeKind, Vec2};
use rlive_audio_io::{BitDepth, FileType};
use rlive_project::clip_envelope::SessionAutomationIn;
use rlive_project::WarpMode;
use rlive_remote::TakeoverMode;
use std::path::PathBuf;

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
/// The Track, Sync and Remote switches of a MIDI port.
fn port_switches(ui: &mut egui::Ui, cfg: &mut crate::settings::MidiPortConfig) -> bool {
    let track = ui.checkbox(&mut cfg.track, "").changed();
    let sync = ui.checkbox(&mut cfg.sync, "").changed();
    let remote = ui.checkbox(&mut cfg.remote, "").changed();
    track || sync || remote
}

/// An input port's MPE switch: its per-note channels are expression.
fn port_mpe(ui: &mut egui::Ui, cfg: &mut crate::settings::MidiPortConfig) -> bool {
    ui.checkbox(&mut cfg.mpe, "")
        .on_hover_text("MPE Mode — treat this controller's per-note channels as expression")
        .changed()
}

/// A chooser of one MIDI port or none.
fn port_chooser(
    ui: &mut egui::Ui,
    salt: (&'static str, usize),
    ports: &[String],
    value: &mut Option<String>,
) -> bool {
    let before = value.clone();
    egui::ComboBox::from_id_salt(salt)
        .selected_text(value.clone().unwrap_or_else(|| "None".into()))
        .width(120.0)
        .show_ui(ui, |ui| {
            ui.selectable_value(value, None, "None");
            for p in ports {
                ui.selectable_value(value, Some(p.clone()), p);
            }
        });
    before != *value
}

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
                            Tab::Link => {
                                if link_tab(ui, &mut self.settings) {
                                    self.settings.save();
                                    self.apply_sync_settings();
                                }
                            }
                            Tab::TempoMidi => self.tempo_midi_tab(ui),
                            Tab::FileFolder => self.file_folder_tab(ui),
                            Tab::Library => self.library_tab(ui),
                            Tab::PlugIns => self.plugins_tab(ui),
                            Tab::RecordWarpLaunch => record_warp_launch_tab(ui, &mut self.settings),
                            Tab::Licenses => licenses_tab(ui),
                        });
                    });
                });
            });
        self.ui.show_settings = open;
        if self.settings != before {
            self.settings.save();
            let auto_warp = self.settings.warp.auto_warp_long;
            if self.settings.samples != before.samples || auto_warp != before.warp.auto_warp_long {
                self.session
                    .samples
                    .configure(self.settings.samples.loader_config(auto_warp));
            }
            if self.settings.record_session_automation_in != before.record_session_automation_in {
                let tracks = self.settings.record_session_automation_in;
                self.session.set_session_automation_in(tracks);
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
            label(ui, "Follow Behavior");
            egui::ComboBox::from_id_salt("follow_behavior")
                .selected_text(match self.settings.follow_behavior {
                    FollowBehavior::Page => "Page",
                    FollowBehavior::Scroll => "Scroll",
                })
                .show_ui(ui, |ui| {
                    let v = &mut self.settings.follow_behavior;
                    ui.selectable_value(v, FollowBehavior::Page, "Page");
                    ui.selectable_value(v, FollowBehavior::Scroll, "Scroll");
                });
            ui.end_row();
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
            label(ui, "Permanent Scrub Areas");
            ui.checkbox(&mut self.settings.permanent_scrub_areas, "");
            ui.end_row();
            label(ui, "Draw Mode with Pitch Lock");
            ui.checkbox(&mut self.settings.pitch_lock, "");
            ui.end_row();
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
        let mut input_changed = false;
        grid(ui, "audio", |ui| {
            soon(ui, "Driver Type", "Default", "A5");
            label(ui, "Audio Input Device");
            let names = self
                .ui
                .audio_inputs
                .get_or_insert_with(crate::audio::input_device_names)
                .clone();
            let shown = match &self.settings.audio_input {
                InputDevice::None => "None".to_string(),
                InputDevice::SystemDefault => "System default".to_string(),
                InputDevice::Named(name) => name.clone(),
            };
            let before = self.settings.audio_input.clone();
            egui::ComboBox::from_id_salt("audio_in")
                .width(200.0)
                .selected_text(shown)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.settings.audio_input, InputDevice::None, "None");
                    ui.selectable_value(
                        &mut self.settings.audio_input,
                        InputDevice::SystemDefault,
                        "System default",
                    );
                    for name in names {
                        ui.selectable_value(
                            &mut self.settings.audio_input,
                            InputDevice::Named(name.clone()),
                            name,
                        );
                    }
                });
            ui.end_row();
            input_changed = self.settings.audio_input != before;
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
            soon(ui, "Sample Rate", "Device default", "A5");
            soon(ui, "Buffer Size", "Device default", "A5");
            soon(ui, "Output Latency", "—", "A4");
            soon(ui, "Test Tone", "Off", "A5");
        });
        if refresh {
            self.ui.audio_devices = None;
            self.ui.audio_inputs = None;
        }
        if choice != self.settings.audio_device || input_changed {
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
        ui.add_space(8.0);
        widgets::section_label(ui, "CHANNEL CONFIGURATION");
        let device = self.audio.device_name.clone();
        let (inputs, outputs) = (self.audio.input_channels, self.audio.output_channels);
        let mut changed = false;
        {
            let config = self.settings.audio_channels.entry(device).or_default();
            changed |= channel_bank(ui, "Input Config", "in", &mut config.inputs, inputs);
            changed |= channel_bank(ui, "Output Config", "out", &mut config.outputs, outputs);
        }
        if changed {
            self.settings.save();
            self.refresh_hardware();
        }
    }

    fn tempo_midi_tab(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        let inputs = self
            .ui
            .midi_ports
            .get_or_insert_with(midi_input::port_names)
            .clone();
        let outputs = self
            .ui
            .midi_outputs
            .get_or_insert_with(midi_input::output_names)
            .clone();
        widgets::section_label(ui, "MIDI PORTS");
        if ui.small_button("Rescan").clicked() {
            self.ui.midi_ports = None;
            self.ui.midi_outputs = None;
            changed = true;
        }
        let legacy = self.settings.midi_input.clone();
        egui::Grid::new("midi_ports")
            .num_columns(5)
            .spacing([18.0, 4.0])
            .show(ui, |ui| {
                label(ui, "Port");
                label(ui, "Track");
                label(ui, "Sync");
                label(ui, "Remote");
                label(ui, "MPE");
                ui.end_row();
                for name in &inputs {
                    let mut cfg = self.settings.remote.input(name, legacy.as_deref());
                    ui.label(RichText::new(format!("In: {name}")).color(pal().text));
                    changed |= port_switches(ui, &mut cfg);
                    changed |= port_mpe(ui, &mut cfg);
                    ui.end_row();
                    self.settings.remote.set_input(cfg);
                }
                for name in &outputs {
                    let mut cfg = self.settings.remote.output(name);
                    ui.label(RichText::new(format!("Out: {name}")).color(pal().text));
                    changed |= port_switches(ui, &mut cfg);
                    ui.end_row();
                    self.settings.remote.set_output(cfg);
                }
                if inputs.is_empty() && outputs.is_empty() {
                    ui.label(RichText::new("No MIDI ports found").color(pal().text_dim));
                    ui.end_row();
                }
            });
        ui.add_space(10.0);
        widgets::section_label(ui, "CONTROL SURFACES");
        egui::Grid::new("control_surfaces")
            .num_columns(4)
            .spacing([12.0, 4.0])
            .show(ui, |ui| {
                label(ui, "Control Surface");
                label(ui, "Input");
                label(ui, "Output");
                ui.label("");
                ui.end_row();
                for i in 0..rlive_remote::MAX_SURFACES {
                    changed |= self.surface_row(ui, i, &inputs, &outputs);
                    ui.end_row();
                }
            });
        ui.add_space(6.0);
        grid(ui, "takeover", |ui| {
            label(ui, "Takeover Mode");
            let mut mode = self.settings.remote.takeover;
            egui::ComboBox::from_id_salt("takeover_mode")
                .selected_text(mode.label())
                .show_ui(ui, |ui| {
                    for m in TakeoverMode::ALL {
                        ui.selectable_value(&mut mode, m, m.label());
                    }
                });
            if mode != self.settings.remote.takeover {
                self.settings.remote.takeover = mode;
                self.session.set_takeover(mode);
                self.settings.save();
            }
            ui.end_row();
        });
        if changed {
            self.settings.save();
            self.apply_control_surfaces();
            self.connect_midi();
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
        changed |= self.sync_options(ui, &inputs, &outputs);
        self.follower_options(ui);
        if changed {
            self.apply_sync_settings();
        }
    }

    /// Sync Type, timecode options and Sync Delay of each port with its Sync switch on.
    fn sync_options(&mut self, ui: &mut egui::Ui, inputs: &[String], outputs: &[String]) -> bool {
        let legacy = self.settings.midi_input.clone();
        let mut changed = false;
        ui.add_space(10.0);
        widgets::section_label(ui, "SYNC");
        egui::Grid::new("sync_ports")
            .num_columns(2)
            .spacing([12.0, 4.0])
            .show(ui, |ui| {
                let mut any = false;
                for name in inputs {
                    let mut cfg = self.settings.remote.input(name, legacy.as_deref());
                    if !cfg.sync {
                        continue;
                    }
                    any = true;
                    changed |= input_sync_row(ui, &mut cfg);
                    self.settings.remote.set_input(cfg);
                }
                for name in outputs {
                    let mut cfg = self.settings.remote.output(name);
                    if !cfg.sync {
                        continue;
                    }
                    any = true;
                    label(ui, &format!("Out: {name}: Sync Delay (ms)"));
                    changed |= ui
                        .add(egui::DragValue::new(&mut cfg.sync_delay_ms).range(-1000.0..=1000.0))
                        .changed();
                    ui.end_row();
                    self.settings.remote.set_output(cfg);
                }
                if !any {
                    ui.label(RichText::new("No port has its Sync switch on").color(pal().text_dim));
                    ui.end_row();
                }
            });
        if changed {
            self.settings.save();
        }
        changed
    }

    fn follower_options(&mut self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        widgets::section_label(ui, "TEMPO FOLLOWER");
        let channels = self.session.engine.shared.input_meters.len();
        let mut changed = false;
        let sync = &mut self.settings.sync;
        grid(ui, "tempo_follower", |ui| {
            label(ui, "Input Channel (Ext. In)");
            let selected = sync
                .follower_channel
                .map_or("None".to_string(), |c| format!("{}", c + 1));
            egui::ComboBox::from_id_salt("follower_channel")
                .selected_text(selected)
                .show_ui(ui, |ui| {
                    changed |= ui
                        .selectable_value(&mut sync.follower_channel, None, "None")
                        .changed();
                    for c in 0..channels.min(16) as u16 {
                        changed |= ui
                            .selectable_value(
                                &mut sync.follower_channel,
                                Some(c),
                                format!("{}", c + 1),
                            )
                            .changed();
                    }
                });
            ui.end_row();
            label(ui, "Show Tempo Follower Toggle");
            changed |= ui.checkbox(&mut sync.show_follower_toggle, "").changed();
            ui.end_row();
        });
        if changed {
            self.settings.save();
            self.apply_sync_settings();
        }
    }

    /// One row of the Control Surface table.
    fn surface_row(
        &mut self,
        ui: &mut egui::Ui,
        i: usize,
        inputs: &[String],
        outputs: &[String],
    ) -> bool {
        let mut changed = false;
        let row = self.settings.remote.surface_mut(i);
        let mut script = row.script.clone();
        egui::ComboBox::from_id_salt(("surface", i))
            .selected_text(script.clone().unwrap_or_else(|| "None".into()))
            .width(140.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut script, None, "None");
                for name in rlive_remote::SurfaceHost::available() {
                    ui.selectable_value(&mut script, Some(name.to_string()), name);
                }
            });
        changed |= script != row.script;
        row.script = script;
        changed |= port_chooser(ui, ("surface_in", i), inputs, &mut row.input);
        changed |= port_chooser(ui, ("surface_out", i), outputs, &mut row.output);
        let dump = self
            .session
            .remote
            .surfaces
            .slots
            .get(i)
            .and_then(|s| s.as_ref())
            .is_some_and(|s| s.surface.preset_dump().is_some());
        if ui
            .add_enabled(dump, egui::Button::new("Dump").small())
            .clicked()
        {
            self.session.dump_surface(i);
        }
        changed
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
                        self.ui.browser.op(rlive_browser::Op::AddFolder(d.clone()));
                        self.settings.sample_dir = Some(d);
                    }
                }
            });
            ui.end_row();
            label(ui, "User Library");
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(self.ui.browser.library_path.display().to_string())
                        .color(pal().text_dim),
                );
                if ui.small_button("Choose…").clicked() {
                    if let Some(d) = rfd::FileDialog::new().pick_folder() {
                        self.settings.user_library = Some(d.clone());
                        self.ui.browser.set_user_library(&mut self.session, d);
                    }
                }
            });
            ui.end_row();
            soon(ui, "Installation Folder for Packs", "Default", "out");
        });
        ui.add_space(10.0);
        widgets::section_label(ui, "FILES");
        grid(ui, "library_files", |ui| {
            label(ui, "Collect Files on Export");
            egui::ComboBox::from_id_salt("collect_on_export")
                .selected_text(self.settings.collect_on_export.label())
                .show_ui(ui, |ui| {
                    for c in crate::settings::CollectFiles::ALL {
                        ui.selectable_value(&mut self.settings.collect_on_export, c, c.label());
                    }
                });
            ui.end_row();
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
            label(ui, "Sample Editor");
            ui.horizontal(|ui| {
                let shown = self
                    .settings
                    .sample_editor
                    .as_ref()
                    .map_or_else(|| "None".to_string(), |p| p.display().to_string());
                ui.label(shown);
                if ui.small_button("Browse").clicked() {
                    if let Some(app) = rfd::FileDialog::new().pick_file() {
                        self.settings.sample_editor = Some(app);
                    }
                }
                if ui.small_button("Clear").clicked() {
                    self.settings.sample_editor = None;
                }
            });
            ui.end_row();
            label(ui, "Temporary Folder");
            ui.horizontal(|ui| {
                let shown = self
                    .settings
                    .record
                    .temp_folder
                    .as_ref()
                    .map_or_else(|| "Default".to_string(), |p| p.display().to_string());
                ui.label(shown);
                if ui.small_button("Browse").clicked() {
                    if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                        self.settings.record.temp_folder = Some(folder);
                    }
                }
                if ui.small_button("Reset").clicked() {
                    self.settings.record.temp_folder = None;
                }
            });
            ui.end_row();
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
        let mut changed = false;
        grid(ui, "plugin_sources", |ui| {
            let plugins = &mut self.settings.plugins;
            let mut row = |ui: &mut egui::Ui, label: &str, value: &mut bool| {
                ui.label(label);
                changed |= ui.checkbox(value, "").changed();
                ui.end_row();
            };
            row(
                ui,
                "Use CLAP and VST3 System Folders",
                &mut plugins.use_system_folders,
            );
            row(ui, "Use Custom Folders", &mut plugins.use_custom_folders);
            if cfg!(target_os = "macos") {
                row(ui, "Use Audio Units", &mut plugins.use_audio_units);
            }
        });
        ui.label(RichText::new("Custom folders, searched for every format:").color(pal().text_dim));
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
            changed = true;
        }
        ui.horizontal(|ui| {
            if ui.button("Add Folder…").clicked() {
                if let Some(d) = rfd::FileDialog::new().pick_folder() {
                    self.settings.plugin_dirs.push(d);
                    changed = true;
                }
            }
            let label = if self.settings.plugins.activated {
                "Rescan"
            } else {
                "Activate"
            };
            let rescan = ui
                .add_enabled(self.ui.scan_rx.is_none(), egui::Button::new(label))
                .on_hover_text("Hold Option to delete the plug-in database and start clean");
            if rescan.clicked() {
                self.start_plugin_scan();
            }
        });
        ui.label(RichText::new(&self.ui.scan_status).color(pal().text_dim));
        let disabled: Vec<PathBuf> = self
            .session
            .plugins
            .files
            .iter()
            .filter(|f| f.disabled)
            .map(|f| f.path.clone())
            .collect();
        if !disabled.is_empty() {
            let mut enable = None;
            ui.collapsing(format!("{} disabled plug-ins", disabled.len()), |ui| {
                for p in &disabled {
                    ui.horizontal(|ui| {
                        ui.label(p.display().to_string());
                        if ui.small_button("Enable").clicked() {
                            enable = Some(p.clone());
                        }
                    });
                }
            });
            if let Some(path) = enable {
                self.session.plugins.set_disabled(&path, false);
                changed = true;
            }
        }
        let failed: Vec<_> = self
            .session
            .plugins
            .files
            .iter()
            .filter(|f| !f.ok && !f.disabled)
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
            let plugins = &mut self.settings.plugins;
            let mut row = |ui: &mut egui::Ui, label: &str, value: &mut bool| {
                ui.label(label);
                changed |= ui.checkbox(value, "").changed();
                ui.end_row();
            };
            row(
                ui,
                "Auto-Open Plug-In Windows",
                &mut plugins.auto_open_windows,
            );
            row(
                ui,
                "Multiple Plug-In Windows",
                &mut plugins.multiple_windows,
            );
            row(
                ui,
                "Auto-Hide Plug-In Windows",
                &mut plugins.auto_hide_windows,
            );
        });
        if changed {
            self.settings.save();
        }
    }
}

/// One side of the Channel Configuration dialog: which channels are used, whether each odd/even
/// pair is stereo or two monos, and their names. Returns whether anything changed.
fn channel_bank(
    ui: &mut egui::Ui,
    title: &str,
    salt: &str,
    bank: &mut ChannelBank,
    count: usize,
) -> bool {
    let mut changed = false;
    ui.label(RichText::new(title).color(pal().text_dim));
    if count == 0 {
        ui.label(RichText::new("None").color(pal().text_faint));
        return false;
    }
    grid(ui, salt, |ui| {
        for first in (0..count).step_by(2) {
            let pair = first / 2;
            let mut mono = bank.is_mono(pair);
            for n in first..(first + 2).min(count) {
                let channel = bank.channel_mut(n);
                changed |= ui
                    .checkbox(&mut channel.enabled, (n + 1).to_string())
                    .changed();
                changed |= ui
                    .add(
                        egui::TextEdit::singleline(&mut channel.name)
                            .desired_width(110.0)
                            .hint_text("Name"),
                    )
                    .changed();
                if n == first && first + 1 < count {
                    changed |= ui.checkbox(&mut mono, "Mono").changed();
                }
                ui.end_row();
            }
            bank.set_mono(pair, mono);
        }
    });
    changed
}

/// One input port's sync options, and how far it has them from what it was.
fn input_sync_row(ui: &mut egui::Ui, cfg: &mut crate::settings::MidiPortConfig) -> bool {
    use crate::settings::SyncKind;
    use rlive_core::sync::FrameRate;
    let mut changed = false;
    label(ui, &format!("In: {}", cfg.name));
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt(("sync_kind", cfg.name.clone()))
            .selected_text(match cfg.sync_kind {
                SyncKind::Clock => "MIDI Clock",
                SyncKind::Timecode => "MIDI Timecode",
            })
            .show_ui(ui, |ui| {
                changed |= ui
                    .selectable_value(&mut cfg.sync_kind, SyncKind::Clock, "MIDI Clock")
                    .changed();
                changed |= ui
                    .selectable_value(&mut cfg.sync_kind, SyncKind::Timecode, "MIDI Timecode")
                    .changed();
            });
        if cfg.sync_kind == SyncKind::Timecode {
            egui::ComboBox::from_id_salt(("mtc_rate", cfg.name.clone()))
                .selected_text(cfg.mtc_rate.map_or("SMPTE All", FrameRate::label))
                .show_ui(ui, |ui| {
                    changed |= ui
                        .selectable_value(&mut cfg.mtc_rate, None, "SMPTE All")
                        .changed();
                    for rate in FrameRate::ALL {
                        changed |= ui
                            .selectable_value(&mut cfg.mtc_rate, Some(rate), rate.label())
                            .changed();
                    }
                });
            ui.label("Offset (s)");
            changed |= ui
                .add(egui::DragValue::new(&mut cfg.mtc_offset).range(0.0..=86_400.0))
                .changed();
        }
        ui.label("Delay (ms)");
        changed |= ui
            .add(egui::DragValue::new(&mut cfg.sync_delay_ms).range(-1000.0..=1000.0))
            .changed();
    });
    ui.end_row();
    changed
}

fn link_tab(ui: &mut egui::Ui, settings: &mut Settings) -> bool {
    let mut changed = false;
    widgets::section_label(ui, "LINK");
    if !crate::link_backend::available() {
        ui.label(
            RichText::new("Link isn't in this build: it is a GPL build option (--features link).")
                .color(pal().text_dim),
        );
    }
    let sync = &mut settings.sync;
    grid(ui, "link", |ui| {
        label(ui, "Show Link Toggle");
        changed |= ui.checkbox(&mut sync.show_link_toggle, "").changed();
        ui.end_row();
        label(ui, "Start Stop Sync");
        changed |= ui.checkbox(&mut sync.start_stop_sync, "").changed();
        ui.end_row();
        soon_check(ui, "Link Audio", false, "A16");
    });
    changed
}

fn record_warp_launch_tab(ui: &mut egui::Ui, settings: &mut Settings) {
    let warp = &mut settings.warp;
    let clip = &mut settings.clip;
    let record = &mut settings.record;
    let launch = &mut settings.launch;
    let sync = &mut settings.sync;
    let session_automation = &mut settings.record_session_automation_in;
    let create_fades = &mut settings.create_fades;
    widgets::section_label(ui, "RECORD");
    grid(ui, "record", |ui| {
        label(ui, "File Type");
        egui::ComboBox::from_id_salt("record_file_type")
            .selected_text(record.file_type.label())
            .show_ui(ui, |ui| {
                for kind in FileType::ALL {
                    ui.selectable_value(&mut record.file_type, kind, kind.label());
                }
            });
        ui.end_row();
        label(ui, "Bit Depth");
        egui::ComboBox::from_id_salt("record_bit_depth")
            .selected_text(record.bit_depth.label())
            .show_ui(ui, |ui| {
                for depth in BitDepth::ALL {
                    ui.selectable_value(&mut record.bit_depth, depth, depth.label());
                }
            });
        ui.end_row();
        label(ui, "Record Session automation in");
        egui::ComboBox::from_id_salt("session_automation_in")
            .width(160.0)
            .selected_text(match session_automation {
                SessionAutomationIn::ArmedTracks => "Armed Tracks",
                SessionAutomationIn::AllTracks => "All Tracks",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    session_automation,
                    SessionAutomationIn::ArmedTracks,
                    "Armed Tracks",
                );
                ui.selectable_value(
                    session_automation,
                    SessionAutomationIn::AllTracks,
                    "All Tracks",
                );
            });
        ui.end_row();
        label(ui, "Start Playback with Record");
        ui.checkbox(&mut record.start_playback_with_record, "");
        ui.end_row();
        label(ui, "Start Playback with Tap Tempo");
        ui.checkbox(&mut sync.start_playback_with_tap, "");
        ui.end_row();
        label(ui, "Start Recording on Scene Launch");
        ui.checkbox(&mut record.start_recording_on_scene_launch, "");
        ui.end_row();
        label(ui, "Exclusive Arm");
        ui.checkbox(&mut record.exclusive_arm, "");
        ui.end_row();
        soon_check(ui, "Exclusive Solo", true, "A1");
    });
    ui.add_space(10.0);
    widgets::section_label(ui, "WARP");
    grid(ui, "warp", |ui| {
        label(ui, "Auto-Warp Long Samples");
        ui.checkbox(&mut warp.auto_warp_long, "");
        ui.end_row();
        ui.label("Loop/Warp Short Samples");
        egui::ComboBox::from_id_salt("short_samples")
            .selected_text(warp.short_samples.label())
            .show_ui(ui, |ui| {
                for mode in ShortSampleMode::ALL {
                    ui.selectable_value(&mut warp.short_samples, mode, mode.label());
                }
            });
        ui.end_row();
        ui.label("Default Warp Mode");
        egui::ComboBox::from_id_salt("default_warp_mode")
            .selected_text(sample_editor::mode_label(warp.mode))
            .show_ui(ui, |ui| {
                for mode in [
                    WarpMode::Beats,
                    WarpMode::Tones,
                    WarpMode::Texture,
                    WarpMode::RePitch,
                    WarpMode::Complex,
                    WarpMode::ComplexPro,
                ] {
                    ui.selectable_value(&mut warp.mode, mode, sample_editor::mode_label(mode));
                }
            });
        ui.end_row();
        ui.label("Create Fades on Clip Edges");
        ui.checkbox(create_fades, "");
        ui.end_row();
    });
    ui.add_space(10.0);
    widgets::section_label(ui, "LAUNCH");
    grid(ui, "launch", |ui| {
        ui.label("Default Launch Mode");
        egui::ComboBox::from_id_salt("default_launch_mode")
            .selected_text(launch.default_mode.label())
            .show_ui(ui, |ui| {
                for m in rlive_core::LaunchMode::ALL {
                    ui.selectable_value(&mut launch.default_mode, m, m.label());
                }
            });
        ui.end_row();
        ui.label("Default Launch Quantization");
        egui::ComboBox::from_id_salt("default_launch_quantize")
            .selected_text(launch.default_quantize.map_or("Global", |q| q.label()))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut launch.default_quantize, None, "Global");
                for q in rlive_core::Quantize::ALL {
                    ui.selectable_value(&mut launch.default_quantize, Some(q), q.label());
                }
            });
        ui.end_row();
        ui.label("Select on Launch");
        ui.checkbox(&mut launch.select_on_launch, "");
        ui.end_row();
        ui.label("Select Next Scene on Launch");
        ui.checkbox(&mut launch.select_next_scene, "");
        ui.end_row();
        ui.label("Clip Update Rate");
        egui::ComboBox::from_id_salt("clip_update_rate")
            .selected_text(clip.update_rate.label())
            .show_ui(ui, |ui| {
                for rate in crate::settings::ClipUpdateRate::ALL {
                    ui.selectable_value(&mut clip.update_rate, rate, rate.label());
                }
            });
        ui.end_row();
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
