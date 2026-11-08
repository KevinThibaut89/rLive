//! The rLive user interface.

mod browser;
mod devices;
mod piano_roll;
mod session_view;
pub mod widgets;

use crate::audio::{self, AudioOutput};
use crate::midi_input::{self, MidiInput, MidiRouting};
use crate::session::{ChainRef, Session};
use crate::settings::Settings;
use egui::{Color32, Key, RichText};
use rlive_core::{MidiMessage, Quantize, TimeSignature, TrackId};
use rlive_plugin_host::scanner::PluginCache;
use rlive_project::{Project, TrackKind};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc, Mutex};
use widgets::MeterState;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    Devices,
    Clip,
}

/// Something dragged from the browser.
#[derive(Clone)]
pub enum BrowserItem {
    Builtin(&'static str),
    Plugin(rlive_plugin_host::PluginDescriptor),
    Sample(PathBuf),
}

enum ScanMsg {
    Progress(String),
    Done(PluginCache),
}

pub struct UiState {
    pub selected: Option<ChainRef>,
    pub selected_clip: Option<(TrackId, usize)>,
    pub detail: Detail,
    pub browser_filter: String,
    pub meters: HashMap<TrackId, MeterState>,
    pub master_meter: MeterState,
    pub renaming: Option<(TrackId, String)>,
    pub piano: piano_roll::PianoState,
    pub computer_keys: bool,
    pub octave: i32,
    held: HashMap<Key, u8>,
    show_settings: bool,
    show_export: bool,
    show_about: bool,
    export_scene: usize,
    export_bars: u32,
    scan_rx: Option<mpsc::Receiver<ScanMsg>>,
    pub scan_status: String,
    pub status: String,
    pub sample_files: Vec<PathBuf>,
    tempo_edit: Option<f64>,
    pub request_scan: bool,
}

pub struct RliveApp {
    session: Session,
    audio: AudioOutput,
    midi: MidiInput,
    routing: MidiRouting,
    settings: Settings,
    ui: UiState,
    last_frame: std::time::Instant,
}

impl RliveApp {
    pub fn new(cc: &eframe::CreationContext<'_>, open: Option<PathBuf>) -> Self {
        apply_theme(&cc.egui_ctx);
        let settings = Settings::load();
        let (audio, mut handle) = audio::start(settings.audio_device.as_deref());
        let sender = handle.take_midi_sender();
        let plugins = PluginCache::load(&PluginCache::default_path());
        let mut session = Session::new(handle, Project::new_default(), plugins);
        let routing = MidiRouting {
            sender: Arc::new(Mutex::new(sender)),
            targets: session.midi_targets.clone(),
            selected: Arc::new(Mutex::new(None)),
            activity: Default::default(),
        };
        let midi = MidiInput::connect(settings.midi_input.as_deref(), routing.clone())
            .unwrap_or_else(|e| {
                log::info!("MIDI input: {e}");
                MidiInput::none()
            });
        let mut ui = UiState {
            selected: session
                .project
                .tracks
                .first()
                .map(|t| ChainRef::Track(t.id)),
            selected_clip: None,
            detail: Detail::Devices,
            browser_filter: String::new(),
            meters: HashMap::new(),
            master_meter: MeterState::default(),
            renaming: None,
            piano: piano_roll::PianoState::default(),
            computer_keys: true,
            octave: 3,
            held: HashMap::new(),
            show_settings: false,
            show_export: false,
            show_about: false,
            export_scene: 0,
            export_bars: 4,
            scan_rx: None,
            scan_status: String::new(),
            status: audio
                .error
                .clone()
                .map(|e| format!("Audio: {e}"))
                .unwrap_or_default(),
            sample_files: Vec::new(),
            tempo_edit: None,
            request_scan: false,
        };
        if let Some(dir) = &settings.sample_dir {
            ui.sample_files = browser::list_samples(dir);
        }
        if let Some(path) = open {
            if let Err(e) = session.load(&path) {
                ui.status = format!("Could not open {}: {e}", path.display());
            }
        }
        Self {
            session,
            audio,
            midi,
            routing,
            settings,
            ui,
            last_frame: std::time::Instant::now(),
        }
    }

    fn restart_audio(&mut self) {
        // Drop the old stream first so the device is free.
        self.audio = AudioOutput::stopped();
        let (audio, mut handle) = audio::start(self.settings.audio_device.as_deref());
        *self.routing.sender.lock().unwrap() = handle.take_midi_sender();
        self.audio = audio;
        self.session.replace_engine(handle);
        self.ui.status = match &self.audio.error {
            Some(e) => format!("Audio: {e}"),
            None => format!(
                "Audio: {} @ {} Hz",
                self.audio.device_name, self.audio.sample_rate
            ),
        };
    }

    fn start_plugin_scan(&mut self) {
        let (tx, rx) = mpsc::channel();
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("rlive"));
        let mut cache = self.session.plugins.clone();
        let extra = self.settings.plugin_dirs.clone();
        std::thread::spawn(move || {
            cache.rescan(&exe, &extra, |p| {
                let _ = tx.send(ScanMsg::Progress(p.display().to_string()));
            });
            if let Err(e) = cache.save(&PluginCache::default_path()) {
                log::warn!("could not save plugin cache: {e}");
            }
            let _ = tx.send(ScanMsg::Done(cache));
        });
        self.ui.scan_rx = Some(rx);
        self.ui.scan_status = "Scanning plugins…".into();
    }

    fn poll_scan(&mut self) {
        let Some(rx) = &self.ui.scan_rx else { return };
        while let Ok(msg) = rx.try_recv() {
            match msg {
                ScanMsg::Progress(p) => self.ui.scan_status = format!("Scanning {p}"),
                ScanMsg::Done(cache) => {
                    let n = cache.plugins.len();
                    let failed = cache.files.iter().filter(|f| !f.ok).count();
                    self.session.plugins = cache;
                    self.ui.scan_status = format!(
                        "{n} plugins found{}",
                        if failed > 0 {
                            format!(", {failed} failed")
                        } else {
                            String::new()
                        }
                    );
                    self.ui.scan_rx = None;
                    return;
                }
            }
        }
    }

    fn open_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("rLive project", &[rlive_project::FILE_EXTENSION])
            .pick_file()
        {
            match self.session.load(&path) {
                Ok(()) => {
                    self.ui.selected = self
                        .session
                        .project
                        .tracks
                        .first()
                        .map(|t| ChainRef::Track(t.id));
                    self.ui.selected_clip = None;
                    self.ui.status = format!("Opened {}", path.display());
                }
                Err(e) => self.ui.status = format!("Open failed: {e}"),
            }
        }
    }

    fn save(&mut self, save_as: bool) {
        let path = match (&self.session.path, save_as) {
            (Some(p), false) => Some(p.clone()),
            _ => rfd::FileDialog::new()
                .add_filter("rLive project", &[rlive_project::FILE_EXTENSION])
                .set_file_name("Untitled.rlive")
                .save_file(),
        };
        if let Some(path) = path {
            match self.session.save(&path) {
                Ok(()) => self.ui.status = format!("Saved {}", path.display()),
                Err(e) => self.ui.status = format!("Save failed: {e}"),
            }
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let typing = ctx.egui_wants_keyboard_input();
        let (space, cmd_s, cmd_shift_s, cmd_o, cmd_n, del, cmd_d, cmd_t) = ctx.input(|i| {
            let cmd = i.modifiers.command;
            (
                i.key_pressed(Key::Space),
                cmd && !i.modifiers.shift && i.key_pressed(Key::S),
                cmd && i.modifiers.shift && i.key_pressed(Key::S),
                cmd && i.key_pressed(Key::O),
                cmd && i.key_pressed(Key::N),
                i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace),
                cmd && i.key_pressed(Key::D),
                cmd && i.key_pressed(Key::T),
            )
        });
        if cmd_s {
            self.save(false);
        }
        if cmd_shift_s {
            self.save(true);
        }
        if cmd_o {
            self.open_dialog();
        }
        if cmd_n {
            self.session.new_project();
            self.ui.selected_clip = None;
        }
        if cmd_t {
            let id = self.session.add_track(TrackKind::Midi, None);
            self.ui.selected = Some(ChainRef::Track(id));
        }
        if typing {
            return;
        }
        if space {
            if self.session.is_playing() {
                self.session.stop();
            } else {
                self.session.play();
            }
        }
        if let Some((t, slot)) = self.ui.selected_clip {
            if del && !self.ui.piano.has_selection() {
                self.session.set_clip(t, slot, None);
                self.ui.selected_clip = None;
            }
            if cmd_d {
                session_view::duplicate_clip(&mut self.session, t, slot);
            }
        }
        if self.ui.computer_keys {
            self.computer_keyboard(ctx);
        }
    }

    /// Ableton-style computer MIDI keyboard: A W S E D F T G Y H U J K = C..C, Z/X = octave.
    fn computer_keyboard(&mut self, ctx: &egui::Context) {
        const KEYS: [(Key, u8); 13] = [
            (Key::A, 0),
            (Key::W, 1),
            (Key::S, 2),
            (Key::E, 3),
            (Key::D, 4),
            (Key::F, 5),
            (Key::T, 6),
            (Key::G, 7),
            (Key::Y, 8),
            (Key::H, 9),
            (Key::U, 10),
            (Key::J, 11),
            (Key::K, 12),
        ];
        let Some(target) = self.midi_target() else {
            return;
        };
        let (pressed, released, z, x, cmd) = ctx.input(|i| {
            let pressed: Vec<Key> = KEYS
                .iter()
                .filter(|(k, _)| i.key_pressed(*k))
                .map(|(k, _)| *k)
                .collect();
            let released: Vec<Key> = KEYS
                .iter()
                .filter(|(k, _)| i.key_released(*k))
                .map(|(k, _)| *k)
                .collect();
            (
                pressed,
                released,
                i.key_pressed(Key::Z),
                i.key_pressed(Key::X),
                i.modifiers.command,
            )
        });
        if cmd {
            return;
        }
        if z {
            self.ui.octave = (self.ui.octave - 1).max(-2);
        }
        if x {
            self.ui.octave = (self.ui.octave + 1).min(8);
        }
        for k in pressed {
            if self.ui.held.contains_key(&k) {
                continue;
            }
            let offset = KEYS
                .iter()
                .find(|(kk, _)| *kk == k)
                .map(|(_, o)| *o)
                .unwrap_or(0);
            let note = ((self.ui.octave + 2) * 12 + offset as i32).clamp(0, 127) as u8;
            self.ui.held.insert(k, note);
            for t in &target {
                self.session.midi_in(
                    *t,
                    MidiMessage::NoteOn {
                        channel: 0,
                        key: note,
                        velocity: 0.8,
                    },
                );
            }
        }
        for k in released {
            if let Some(note) = self.ui.held.remove(&k) {
                for t in &target {
                    self.session.midi_in(
                        *t,
                        MidiMessage::NoteOff {
                            channel: 0,
                            key: note,
                            velocity: 0.5,
                        },
                    );
                }
            }
        }
    }

    /// Armed tracks, or the selected track if none is armed.
    fn midi_target(&self) -> Option<Vec<TrackId>> {
        let armed: Vec<TrackId> = self
            .session
            .project
            .tracks
            .iter()
            .filter(|t| t.arm)
            .map(|t| t.id)
            .collect();
        if !armed.is_empty() {
            return Some(armed);
        }
        match self.ui.selected {
            Some(ChainRef::Track(t)) => Some(vec![t]),
            _ => None,
        }
    }

    fn update_meters(&mut self) {
        let now = std::time::Instant::now();
        let dt = (now - self.last_frame).as_secs_f32();
        self.last_frame = now;
        for (id, shared) in &self.session.track_shared {
            let peaks = shared.meter.take();
            self.ui.meters.entry(*id).or_default().update(peaks, dt);
        }
        let m = self.session.engine.shared.master_meter.take();
        self.ui.master_meter.update(m, dt);
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("New Live Set").clicked() {
                    self.session.new_project();
                    self.ui.selected_clip = None;
                    ui.close();
                }
                if ui.button("Open…").clicked() {
                    ui.close();
                    self.open_dialog();
                }
                if ui.button("Save").clicked() {
                    ui.close();
                    self.save(false);
                }
                if ui.button("Save As…").clicked() {
                    ui.close();
                    self.save(true);
                }
                ui.separator();
                if ui.button("Export Audio…").clicked() {
                    self.ui.show_export = true;
                    ui.close();
                }
            });
            ui.menu_button("Create", |ui| {
                if ui.button("MIDI Track").clicked() {
                    let id = self.session.add_track(TrackKind::Midi, None);
                    self.ui.selected = Some(ChainRef::Track(id));
                    ui.close();
                }
                if ui.button("Audio Track").clicked() {
                    let id = self.session.add_track(TrackKind::Audio, None);
                    self.ui.selected = Some(ChainRef::Track(id));
                    ui.close();
                }
                if ui.button("Scene").clicked() {
                    self.session.add_scene();
                    ui.close();
                }
            });
            ui.menu_button("Options", |ui| {
                if ui.button("Audio, MIDI & Plugins…").clicked() {
                    self.ui.show_settings = true;
                    ui.close();
                }
                if ui.button("Rescan Plugins").clicked() {
                    self.start_plugin_scan();
                    ui.close();
                }
                ui.checkbox(&mut self.ui.computer_keys, "Computer MIDI Keyboard");
            });
            ui.menu_button("Help", |ui| {
                if ui.button("About rLive").clicked() {
                    self.ui.show_about = true;
                    ui.close();
                }
            });
        });
    }

    fn transport_bar(&mut self, ui: &mut egui::Ui) {
        let s = &mut self.session;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            // Tempo
            let mut tempo = self.ui.tempo_edit.unwrap_or(s.project.tempo);
            let r = ui.add(
                egui::DragValue::new(&mut tempo)
                    .range(20.0..=999.0)
                    .speed(0.2)
                    .fixed_decimals(2)
                    .suffix(" BPM"),
            );
            if r.changed() {
                s.set_tempo(tempo);
            }
            // Time signature
            let mut sig = s.project.time_signature;
            let mut changed = false;
            changed |= ui
                .add(egui::DragValue::new(&mut sig.numerator).range(1..=32))
                .changed();
            ui.label("/");
            egui::ComboBox::from_id_salt("sig_den")
                .width(40.0)
                .selected_text(sig.denominator.to_string())
                .show_ui(ui, |ui| {
                    for d in [2u8, 4, 8, 16] {
                        changed |= ui
                            .selectable_value(&mut sig.denominator, d, d.to_string())
                            .changed();
                    }
                });
            if changed {
                s.set_time_signature(TimeSignature { ..sig });
            }
            let mut metro = s.project.metronome;
            if widgets::toggle_box(ui, metro, "Met", widgets::ACCENT, "Metronome").clicked() {
                metro = !metro;
                s.set_metronome(metro);
            }
            ui.separator();

            // Transport buttons
            let playing = s.is_playing();
            let (rect, play) = ui.allocate_exact_size(egui::vec2(28.0, 22.0), egui::Sense::click());
            ui.painter().rect_filled(
                rect,
                3.0,
                if playing {
                    widgets::PLAY_GREEN
                } else {
                    Color32::from_gray(60)
                },
            );
            widgets::play_triangle(
                ui.painter(),
                rect,
                if playing {
                    Color32::BLACK
                } else {
                    Color32::WHITE
                },
            );
            if play.on_hover_text("Play (Space)").clicked() {
                s.play();
            }
            let (rect, stop) = ui.allocate_exact_size(egui::vec2(28.0, 22.0), egui::Sense::click());
            ui.painter().rect_filled(rect, 3.0, Color32::from_gray(60));
            widgets::stop_square(ui.painter(), rect, Color32::WHITE);
            if stop.on_hover_text("Stop (Space)").clicked() {
                s.stop();
            }
            let tick = s.engine.shared.tick.load();
            ui.label(
                RichText::new(s.project.time_signature.format_position(tick))
                    .monospace()
                    .size(15.0)
                    .color(if playing {
                        widgets::PLAY_GREEN
                    } else {
                        Color32::from_gray(210)
                    }),
            );
            ui.separator();
            ui.label("Quantize");
            let mut q = s.project.quantize;
            egui::ComboBox::from_id_salt("quant")
                .width(70.0)
                .selected_text(q.label())
                .show_ui(ui, |ui| {
                    for opt in Quantize::ALL {
                        ui.selectable_value(&mut q, opt, opt.label());
                    }
                });
            if q != s.project.quantize {
                s.set_quantize(q);
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let load = s.engine.shared.cpu_load.load();
                ui.label(
                    RichText::new(format!("CPU {:>3.0}%", load * 100.0))
                        .monospace()
                        .color(if load > 0.8 {
                            Color32::RED
                        } else {
                            widgets::TEXT_DIM
                        }),
                );
                let midi_on = self.routing.activity.swap(false, Ordering::Relaxed);
                ui.label(RichText::new("MIDI").color(if midi_on {
                    widgets::ACCENT
                } else {
                    Color32::from_gray(80)
                }))
                .on_hover_text(
                    self.midi
                        .port_name
                        .clone()
                        .unwrap_or_else(|| "No MIDI input".into()),
                );
                if self.ui.computer_keys {
                    ui.label(
                        RichText::new(format!("Keys Oct {}", self.ui.octave))
                            .color(widgets::TEXT_DIM),
                    )
                    .on_hover_text("Computer MIDI keyboard: A–K play notes, Z/X change octave");
                }
            });
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let msg = self.session.messages.last().cloned();
            let text = if !self.ui.status.is_empty() {
                self.ui.status.clone()
            } else {
                msg.unwrap_or_default()
            };
            ui.label(RichText::new(text).color(widgets::TEXT_DIM).size(11.0));
            if !self.ui.scan_status.is_empty() {
                ui.separator();
                ui.label(
                    RichText::new(&self.ui.scan_status)
                        .color(widgets::TEXT_DIM)
                        .size(11.0),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let name = self
                    .session
                    .path
                    .as_ref()
                    .and_then(|p| p.file_stem())
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Untitled".into());
                let dirty = if self.session.dirty { " *" } else { "" };
                ui.label(RichText::new(format!("{name}{dirty}")).size(11.0));
                ui.label(
                    RichText::new(format!(
                        "{} · {:.0} Hz",
                        self.audio.device_name, self.audio.sample_rate
                    ))
                    .size(11.0)
                    .color(widgets::TEXT_DIM),
                );
            });
        });
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        let mut open = self.ui.show_settings;
        egui::Window::new("Preferences")
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                ui.heading("Audio");
                let devices = audio::output_device_names();
                let current = self
                    .settings
                    .audio_device
                    .clone()
                    .unwrap_or_else(|| "System default".into());
                let mut choice = self.settings.audio_device.clone();
                egui::ComboBox::from_id_salt("audio_dev")
                    .width(260.0)
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut choice, None, "System default");
                        for d in devices {
                            ui.selectable_value(&mut choice, Some(d.clone()), d);
                        }
                    });
                if choice != self.settings.audio_device {
                    self.settings.audio_device = choice;
                    self.settings.save();
                    self.restart_audio();
                }
                ui.label(format!(
                    "Running: {} @ {} Hz",
                    self.audio.device_name, self.audio.sample_rate
                ));
                ui.separator();
                ui.heading("MIDI input");
                let ports = midi_input::port_names();
                let mut port = self.settings.midi_input.clone();
                egui::ComboBox::from_id_salt("midi_port")
                    .width(260.0)
                    .selected_text(port.clone().unwrap_or_else(|| "First available".into()))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut port, None, "First available");
                        for p in ports {
                            ui.selectable_value(&mut port, Some(p.clone()), p);
                        }
                    });
                if port != self.settings.midi_input || ui.button("Reconnect").clicked() {
                    self.settings.midi_input = port;
                    self.settings.save();
                    self.midi = MidiInput::none();
                    self.midi = MidiInput::connect(
                        self.settings.midi_input.as_deref(),
                        self.routing.clone(),
                    )
                    .unwrap_or_else(|e| {
                        self.ui.status = format!("MIDI: {e}");
                        MidiInput::none()
                    });
                }
                ui.separator();
                ui.heading("Plugins");
                ui.label("Searched: system CLAP/VST3 folders, Audio Units, plus:");
                let mut remove = None;
                for (i, d) in self.settings.plugin_dirs.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(d.display().to_string());
                        if ui.small_button("x").clicked() {
                            remove = Some(i);
                        }
                    });
                }
                if let Some(i) = remove {
                    self.settings.plugin_dirs.remove(i);
                    self.settings.save();
                }
                ui.horizontal(|ui| {
                    if ui.button("Add folder…").clicked() {
                        if let Some(d) = rfd::FileDialog::new().pick_folder() {
                            self.settings.plugin_dirs.push(d);
                            self.settings.save();
                        }
                    }
                    if ui
                        .add_enabled(self.ui.scan_rx.is_none(), egui::Button::new("Rescan"))
                        .clicked()
                    {
                        self.start_plugin_scan();
                    }
                });
                ui.label(&self.ui.scan_status);
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
            });
        self.ui.show_settings = open;

        let mut open = self.ui.show_export;
        egui::Window::new("Export Audio")
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("Renders a scene offline to a 32-bit float WAV file.");
                let scenes = self.session.project.scenes.len();
                ui.horizontal(|ui| {
                    ui.label("Scene");
                    ui.add(
                        egui::DragValue::new(&mut self.ui.export_scene)
                            .range(0..=scenes.saturating_sub(1))
                            .custom_formatter(|v, _| format!("{}", v as usize + 1)),
                    );
                    ui.label("Length");
                    ui.add(
                        egui::DragValue::new(&mut self.ui.export_bars)
                            .range(1..=512)
                            .suffix(" bars"),
                    );
                });
                if ui.button("Export…").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("WAV", &["wav"])
                        .set_file_name("Export.wav")
                        .save_file()
                    {
                        self.ui.status = match self.session.export_scene(
                            self.ui.export_scene,
                            self.ui.export_bars,
                            &path,
                        ) {
                            Ok(()) => format!("Exported {}", path.display()),
                            Err(e) => format!("Export failed: {e}"),
                        };
                        self.ui.show_export = false;
                    }
                }
            });
        self.ui.show_export &= open;

        let mut open = self.ui.show_about;
        egui::Window::new("About rLive").open(&mut open).resizable(false).collapsible(false).show(ctx, |ui| {
            ui.heading("rLive");
            ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
            ui.label("A session-oriented DAW written in Rust, inspired by Ableton Live and Bitwig Studio.");
            ui.label("Hosts CLAP, VST3 and Audio Unit plugins.");
        });
        self.ui.show_about = open;
    }
}

impl eframe::App for RliveApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.session.idle();
        self.poll_scan();
        if std::mem::take(&mut self.ui.request_scan) && self.ui.scan_rx.is_none() {
            self.start_plugin_scan();
        }
        if let Some(ChainRef::Track(t)) = self.ui.selected {
            *self.routing.selected.lock().unwrap() = Some(t);
        }
        self.handle_shortcuts(ctx);
        // Keep meters and playheads moving.
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.update_meters();
        egui::Panel::top("menu").show(ui, |ui| {
            self.menu_bar(ui);
        });
        egui::Panel::top("transport")
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_gray(45))
                    .inner_margin(6.0),
            )
            .show(ui, |ui| self.transport_bar(ui));
        egui::Panel::bottom("status")
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_gray(30))
                    .inner_margin(egui::Margin::symmetric(8, 3)),
            )
            .show(ui, |ui| self.status_bar(ui));
        egui::Panel::left("browser")
            .resizable(true)
            .default_size(210.0)
            .frame(egui::Frame::new().fill(widgets::PANEL_BG).inner_margin(6.0))
            .show(ui, |ui| {
                browser::show(ui, &mut self.session, &mut self.ui, &mut self.settings)
            });
        egui::Panel::bottom("detail")
            .resizable(true)
            .default_size(260.0)
            .min_size(150.0)
            .frame(egui::Frame::new().fill(widgets::PANEL_BG).inner_margin(6.0))
            .show(ui, |ui| match self.ui.detail {
                Detail::Clip if self.ui.selected_clip.is_some() => {
                    piano_roll::show(ui, &mut self.session, &mut self.ui)
                }
                _ => devices::show(ui, &mut self.session, &mut self.ui),
            });
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_gray(32))
                    .inner_margin(4.0),
            )
            .show(ui, |ui| {
                session_view::show(ui, &mut self.session, &mut self.ui)
            });
        self.dialogs(&ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.session.stop();
        self.settings.save();
    }
}

fn apply_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = widgets::PANEL_BG;
    visuals.window_fill = Color32::from_gray(40);
    visuals.extreme_bg_color = Color32::from_gray(24);
    visuals.selection.bg_fill = widgets::ACCENT.gamma_multiply(0.6);
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, widgets::ACCENT);
    ctx.set_visuals(visuals);
    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(6.0, 4.0);
        s.spacing.button_padding = egui::vec2(6.0, 2.0);
    });
}
