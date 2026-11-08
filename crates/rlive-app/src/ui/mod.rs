//! The rLive user interface (Ableton Live–style).

mod arrangement;
mod browser;
pub mod commands;
mod devices;
mod icons;
mod in_out;
mod info_view;
pub mod keymap;
#[cfg(target_os = "macos")]
mod mac_menu;
pub(crate) mod panels;
mod piano_roll;
mod sample_editor;
mod session_view;
mod settings_window;
pub mod theme;
mod timeline;
pub mod widgets;

use crate::audio::{self, AudioOutput};
use crate::midi_input::{MidiInput, MidiRouting};
use crate::session::InfoTarget;
use crate::session::{AudioHardware, ChainRef, ClipRef, Session};
use crate::settings::{ChannelBank, DeviceChannels, Settings};
use commands::{ClipBoard, MenuCommand};
use egui::{Align2, Key, Pos2, Rect, RichText, Sense, Stroke, UiBuilder, Vec2};
use keymap::{FocusArea, KeyContext, KeyState, Platform};
use rlive_core::{MidiMessage, Quantize, TimeSignature, TrackId};
use rlive_plugin_host::scanner::PluginCache;
use rlive_project::Project;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc, Mutex};
use theme::*;
use widgets::{info, MeterState};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MainView {
    Session,
    Arrangement,
}

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
    /// Tracks selected together, for commands like Group Tracks. Includes `selected`.
    pub selected_tracks: Vec<TrackId>,
    /// A group slot whose clips are selected, as a group and a scene.
    pub group_slot: Option<(TrackId, usize)>,
    pub selected_clip: Option<ClipRef>,
    pub detail: Detail,
    pub main_view: MainView,
    pub focus: FocusArea,
    pub keys: KeyState,
    pub clipboard: Option<ClipBoard>,
    pub browser_filter: String,
    pub browser_category: browser::Category,
    pub meters: HashMap<TrackId, MeterState>,
    pub master_meter: MeterState,
    pub devices: devices::DeviceViewState,
    pub renaming: Option<(TrackId, String)>,
    pub piano: piano_roll::PianoState,
    pub sample: sample_editor::SampleState,
    pub layout: session_view::SessionLayout,
    pub arr: arrangement::ArrangementState,
    pub show_browser: bool,
    pub show_detail: bool,
    pub show_info: bool,
    pub computer_keys: bool,
    pub octave: i32,
    /// Velocity the Computer MIDI Keyboard plays at, 0 to 1.
    pub key_velocity: f32,
    held: HashMap<Key, u8>,
    show_settings: bool,
    settings_tab: settings_window::Tab,
    /// The info text being typed, and for what.
    pub info_edit: Option<(InfoTarget, String)>,
    /// The info text editor has yet to take keyboard focus.
    info_edit_focus: bool,
    show_export: bool,
    show_about: bool,
    export_scene: usize,
    export_bars: u32,
    export_arrangement: bool,
    scan_rx: Option<mpsc::Receiver<ScanMsg>>,
    pub scan_status: String,
    pub status: String,
    pub sample_files: Vec<PathBuf>,
    pub request_scan: bool,
    /// Device lists are slow to enumerate; refreshed only when Preferences opens.
    audio_devices: Option<Vec<String>>,
    midi_ports: Option<Vec<String>>,
    tap_times: Vec<f64>,
    /// The hardware channels the In/Out choosers offer, from the Channel Configuration.
    pub hardware: AudioHardware,
    /// Whether clicking a sample in the browser plays it.
    pub preview_samples: bool,
}

pub struct RliveApp {
    session: Session,
    audio: AudioOutput,
    midi: MidiInput,
    routing: MidiRouting,
    settings: Settings,
    ui: UiState,
    last_frame: std::time::Instant,
    /// Commands queued from outside the egui pass (menus, input hook).
    pending: Vec<MenuCommand>,
    #[cfg(target_os = "macos")]
    mac_menu: Option<mac_menu::MacMenu>,
}

/// Where each focus area is on screen this frame.
#[derive(Default)]
struct FocusRects {
    control_bar: Option<Rect>,
    browser: Option<Rect>,
    detail: Option<Rect>,
    main: Option<Rect>,
}

#[cfg(not(target_os = "macos"))]
const MENU_H: f32 = 22.0;
const CONTROL_BAR_H: f32 = 34.0;
const STATUS_H: f32 = 22.0;

impl RliveApp {
    pub fn new(cc: &eframe::CreationContext<'_>, open: Option<PathBuf>) -> Self {
        theme::apply(&cc.egui_ctx);
        let settings = Settings::load();
        theme::begin_frame(&cc.egui_ctx, &settings.theme);
        settings_window::apply_scroll_bars(&cc.egui_ctx, settings.scroll_bars);
        cc.egui_ctx.set_zoom_factor(settings.zoom());
        let (audio, mut handle) = audio::start(settings.audio_device.as_deref());
        let sender = handle.take_midi_sender();
        let plugins = PluginCache::load(&PluginCache::default_path());
        let mut session = Session::new(handle, Project::new_default(), plugins);
        session.samples.configure(settings.samples.loader_config());
        let routing = MidiRouting {
            sender: Arc::new(Mutex::new(sender)),
            targets: session.midi_targets.clone(),
            selected: Arc::new(Mutex::new(None)),
            activity: Default::default(),
            system: Default::default(),
            mpe: Default::default(),
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
            selected_tracks: Vec::new(),
            group_slot: None,
            selected_clip: None,
            detail: Detail::Devices,
            main_view: MainView::Session,
            focus: FocusArea::Session,
            keys: KeyState::default(),
            clipboard: None,
            browser_filter: String::new(),
            browser_category: browser::Category::Instruments,
            meters: HashMap::new(),
            master_meter: MeterState::default(),
            devices: devices::DeviceViewState::default(),
            renaming: None,
            piano: piano_roll::PianoState::default(),
            sample: sample_editor::SampleState::new(),
            layout: session_view::SessionLayout::default(),
            arr: arrangement::ArrangementState::default(),
            show_browser: settings.show_browser.unwrap_or(true),
            show_detail: settings.show_detail.unwrap_or(true),
            show_info: settings.show_info,
            computer_keys: true,
            octave: 3,
            key_velocity: 0.8,
            held: HashMap::new(),
            show_settings: false,
            settings_tab: settings_window::Tab::default(),
            info_edit: None,
            info_edit_focus: false,
            show_export: false,
            show_about: false,
            export_scene: 0,
            export_bars: 4,
            export_arrangement: false,
            scan_rx: None,
            scan_status: String::new(),
            status: audio
                .error
                .clone()
                .map(|e| format!("Audio: {e}"))
                .unwrap_or_default(),
            sample_files: Vec::new(),
            request_scan: false,
            audio_devices: None,
            midi_ports: None,
            tap_times: Vec::new(),
            hardware: AudioHardware::default(),
            preview_samples: false,
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
            pending: Vec::new(),
            #[cfg(target_os = "macos")]
            mac_menu: mac_menu::MacMenu::install(cc),
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
                    self.ui.scan_status = if failed > 0 {
                        format!("{n} plugins found, {failed} failed")
                    } else {
                        format!("{n} plugins found")
                    };
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

    /// Focus follows the area that was clicked last.
    fn track_focus(&mut self, ctx: &egui::Context, rects: &FocusRects) {
        if !ctx.input(|i| i.pointer.primary_pressed()) {
            return;
        }
        let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) else {
            return;
        };
        let inside = |r: Option<Rect>| r.is_some_and(|r| r.contains(pos));
        self.ui.focus = if inside(rects.control_bar) {
            FocusArea::ControlBar
        } else if inside(rects.browser) {
            FocusArea::Browser
        } else if inside(rects.detail) {
            match self.ui.detail {
                Detail::Clip => FocusArea::Clip,
                Detail::Devices => FocusArea::Device,
            }
        } else if self.ui.main_view == MainView::Arrangement {
            FocusArea::Arrangement
        } else {
            FocusArea::Session
        };
    }

    /// Draws the "Outline View in Focus" border.
    fn paint_focus_outline(&self, ctx: &egui::Context, rects: &FocusRects) {
        if !self.settings.outline_focus {
            return;
        }
        let rect = match self.ui.focus {
            FocusArea::ControlBar => rects.control_bar,
            FocusArea::Browser => rects.browser,
            FocusArea::Clip | FocusArea::Device => rects.detail,
            FocusArea::Session | FocusArea::Arrangement => rects.main,
        };
        if let Some(r) = rect {
            ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::unique("focus_outline"),
            ))
            .rect_stroke(
                r.shrink(0.5),
                0.0,
                Stroke::new(1.0, pal().accent),
                egui::StrokeKind::Inside,
            );
        }
    }

    fn toggle_recording(&mut self) {
        if self.session.is_recording() {
            self.session.stop_recording();
        } else if self.session.start_recording() {
            self.ui.main_view = MainView::Arrangement;
        } else {
            self.ui.status = "Arm a MIDI track, or Automation Arm, to record".into();
        }
    }

    fn shutdown(&mut self) {
        self.session.stop();
        self.settings.save();
    }

    fn set_zoom(&mut self, ctx: &egui::Context, zoom: f32) {
        let z = (zoom * 10.0).round() / 10.0;
        let z = z.clamp(0.7, 2.0);
        ctx.set_zoom_factor(z);
        self.settings.zoom = Some(z);
        self.settings.save();
    }

    fn toggle_info_view(&mut self) {
        self.ui.show_info = !self.ui.show_info;
        self.settings.show_info = self.ui.show_info;
        self.settings.save();
    }

    fn toggle_browser(&mut self) {
        self.ui.show_browser = !self.ui.show_browser;
        self.settings.show_browser = Some(self.ui.show_browser);
        self.settings.save();
    }

    fn toggle_detail(&mut self) {
        self.ui.show_detail = !self.ui.show_detail;
        self.settings.show_detail = Some(self.ui.show_detail);
        self.settings.save();
    }

    fn tap_tempo(&mut self, now: f64) {
        let taps = &mut self.ui.tap_times;
        if taps.last().is_some_and(|t| now - t > 2.0) {
            taps.clear();
        }
        taps.push(now);
        if taps.len() > 5 {
            taps.remove(0);
        }
        if taps.len() >= 2 {
            let avg = (taps[taps.len() - 1] - taps[0]) / (taps.len() - 1) as f64;
            if avg > 0.0 {
                self.session.set_tempo((60.0 / avg).clamp(20.0, 999.0));
            }
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        if self.ui.computer_keys && !ctx.egui_wants_keyboard_input() {
            self.computer_keyboard(ctx);
        }
    }

    /// What the keymap needs to resolve this frame's key presses.
    fn key_context(&self, ctx: &egui::Context) -> KeyContext {
        KeyContext {
            platform: Platform::current(),
            focus: self.ui.focus,
            clip_is_midi: self
                .ui
                .selected_clip
                .and_then(|r| self.session.clip_at(r))
                .is_some_and(|c| c.is_midi()),
            computer_keys: self.ui.computer_keys,
            text_focused: ctx.text_edit_focused(),
            keyboard_taken: ctx.egui_wants_keyboard_input(),
            dialog_open: self.ui.show_about || self.ui.show_export || self.ui.show_settings,
            group_selected: self
                .selected_track_ids()
                .iter()
                .any(|t| self.session.project.track(*t).is_some_and(|t| t.is_group())),
            warp_selected: self.ui.sample.has_selection(),
        }
    }

    /// Ableton-style computer MIDI keyboard: A W S E D F T G Y H U J K = C..C. Z, X, C and V go
    /// through the keymap. Letters are matched by what they type, as for every other shortcut.
    fn computer_keyboard(&mut self, ctx: &egui::Context) {
        let Some(target) = self.midi_target() else {
            return;
        };
        let keys = keymap::MIDI_NOTE_KEYS;
        let (pressed, released, modified) = ctx.input(|i| {
            let pressed: Vec<(usize, Key)> = keys
                .iter()
                .enumerate()
                .filter(|(_, k)| i.key_pressed(**k))
                .map(|(o, k)| (o, *k))
                .collect();
            let released: Vec<Key> = keys
                .iter()
                .filter(|k| i.key_released(**k))
                .copied()
                .collect();
            let m = i.modifiers;
            (pressed, released, m.command || m.alt || m.shift)
        });
        if !modified {
            for (offset, k) in pressed {
                if self.ui.held.contains_key(&k) {
                    continue;
                }
                let note = ((self.ui.octave + 2) * 12 + offset as i32).clamp(0, 127) as u8;
                self.ui.held.insert(k, note);
                for t in &target {
                    self.session.play_midi(
                        *t,
                        MidiMessage::NoteOn {
                            channel: 0,
                            key: note,
                            velocity: self.ui.key_velocity,
                            note_id: 0,
                            tuning: 0.0,
                        },
                    );
                }
            }
        }
        for k in released {
            if let Some(note) = self.ui.held.remove(&k) {
                for t in &target {
                    self.session.play_midi(
                        *t,
                        MidiMessage::NoteOff {
                            channel: 0,
                            key: note,
                            velocity: 0.5,
                            note_id: 0,
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

    /// Returns true while any meter is still moving.
    fn update_meters(&mut self) -> bool {
        let now = std::time::Instant::now();
        let dt = (now - self.last_frame).as_secs_f32().min(0.1);
        self.last_frame = now;
        let mut active = false;
        for (id, shared) in &self.session.track_shared {
            let peaks = shared.meter.take();
            let m = self.ui.meters.entry(*id).or_default();
            m.update(peaks, dt);
            active |= m.active();
        }
        let m = self.session.engine.shared.master_meter.take();
        self.ui.master_meter.update(m, dt);
        active || self.ui.master_meter.active()
    }

    /// What the In/Out choosers can address: the device's channels, as configured.
    fn refresh_hardware(&mut self) {
        let config = self.settings.audio_channels.get(&self.audio.device_name);
        let bank = |pick: fn(&DeviceChannels) -> &ChannelBank, count: usize| {
            config.map(pick).cloned().unwrap_or_default().routes(count)
        };
        self.ui.hardware = AudioHardware {
            outputs: bank(|c| &c.outputs, self.audio.output_channels),
            inputs: bank(|c| &c.inputs, self.audio.input_channels),
        };
    }

    /// Live-style control bar: tempo & signature left, transport centre, status right.
    fn control_bar(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        let p = ui.painter().clone();
        p.rect_filled(rect, 0.0, pal().bar_bg);
        p.line_segment(
            [rect.left_bottom(), rect.right_bottom()],
            Stroke::new(1.0, pal().separator),
        );
        let h = 22.0;
        let cy = rect.center().y;
        let narrow = rect.width() < 1100.0;

        // ---- left: tap, tempo, signature, metronome
        let left = Rect::from_min_max(
            Pos2::new(rect.left() + 8.0, cy - h / 2.0),
            Pos2::new(rect.center().x - 120.0, cy + h / 2.0),
        );
        let mut lu = ui.new_child(
            UiBuilder::new()
                .max_rect(left)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        lu.spacing_mut().item_spacing.x = 4.0;
        let r = widgets::state_button(&mut lu, Vec2::new(34.0, h), false, pal().accent, "TAP");
        if info(r, "Tap Tempo — click in time to set the tempo").clicked() {
            let now = lu.input(|i| i.time);
            self.tap_tempo(now);
        }
        let mut tempo = self.session.project.tempo;
        let r = lu.add_sized(
            [70.0, h],
            egui::DragValue::new(&mut tempo)
                .range(20.0..=999.0)
                .speed(0.25)
                .fixed_decimals(2),
        );
        if r.changed() {
            self.session.set_tempo(tempo);
        }
        info(r, "Tempo (BPM) — drag or double-click to type");
        let mut sig = self.session.project.time_signature;
        let mut changed = false;
        let r = lu.add_sized(
            [26.0, h],
            egui::DragValue::new(&mut sig.numerator).range(1..=32),
        );
        changed |= r.changed();
        info(r, "Time Signature Numerator — beats in a bar");
        lu.label(RichText::new("/").color(pal().text_dim));
        let r = egui::ComboBox::from_id_salt("sig_den")
            .width(34.0)
            .selected_text(sig.denominator.to_string())
            .show_ui(&mut lu, |ui| {
                for d in [2u8, 4, 8, 16] {
                    changed |= ui
                        .selectable_value(&mut sig.denominator, d, d.to_string())
                        .changed();
                }
            });
        info(
            r.response,
            "Time Signature Denominator — the note value of a beat",
        );
        if changed {
            self.session.set_time_signature(TimeSignature { ..sig });
        }
        let metro = self.session.project.metronome;
        let r = widgets::icon_button(
            &mut lu,
            Vec2::new(26.0, h),
            metro,
            pal().accent,
            icons::metronome,
        );
        if info(r, "Metronome — click on every beat while playing (O)").clicked() {
            self.session.set_metronome(!metro);
        }

        // ---- centre: transport + position
        let playing = self.session.is_playing();
        let centre = Rect::from_center_size(Pos2::new(rect.center().x, cy), Vec2::new(390.0, h));
        let mut cu = ui.new_child(
            UiBuilder::new()
                .max_rect(centre)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        cu.spacing_mut().item_spacing.x = 3.0;
        let r = widgets::icon_button(
            &mut cu,
            Vec2::new(30.0, h),
            playing,
            pal().play_green,
            icons::play,
        );
        if info(r, "Play — start playback (Space)").clicked() {
            self.session.play();
        }
        let r = widgets::icon_button(
            &mut cu,
            Vec2::new(30.0, h),
            false,
            pal().accent,
            icons::stop,
        );
        if info(
            r,
            "Stop — stop playback; stop twice to return to the start (Space)",
        )
        .clicked()
        {
            self.session.stop();
        }
        let recording = self.session.is_recording();
        let r = widgets::icon_button(
            &mut cu,
            Vec2::new(30.0, h),
            recording,
            pal().arm_red,
            icons::record,
        );
        if info(
            r,
            "Record — record MIDI onto armed tracks in the Arrangement (F9)",
        )
        .clicked()
        {
            self.toggle_recording();
        }
        let armed = self.session.automation_arm();
        let r = widgets::icon_button(
            &mut cu,
            Vec2::new(26.0, h),
            armed,
            pal().arm_red,
            icons::automation_arm,
        );
        if info(
            r,
            "Automation Arm — while recording, moving a control records it as automation",
        )
        .clicked()
        {
            self.session.set_automation_arm(!armed);
        }
        let overridden = self.session.any_overridden();
        let r = widgets::icon_button(
            &mut cu,
            Vec2::new(26.0, h),
            overridden,
            pal().accent,
            icons::re_enable,
        );
        if info(
            r,
            "Re-Enable Automation — hand every overridden control back to its envelope",
        )
        .clicked()
            && overridden
        {
            self.session.re_enable_all();
        }
        cu.add_space(6.0);
        let tick = self.session.engine.shared.tick.load();
        let pos = self
            .session
            .project
            .time_signature
            .format_position(tick)
            .replace('.', ".  ");
        let (pr, r) = cu.allocate_exact_size(Vec2::new(96.0, h), Sense::hover());
        cu.painter().rect_filled(pr, 1.0, pal().field_bg);
        cu.painter().text(
            pr.center(),
            Align2::CENTER_CENTER,
            pos,
            egui::FontId::monospace(13.0),
            if playing {
                pal().play_green
            } else {
                pal().text
            },
        );
        info(r, "Song Position — the playhead, as bar . beat . sixteenth");
        cu.add_space(6.0);
        let a = self.session.project.arrangement.clone();
        let r = widgets::state_button(
            &mut cu,
            Vec2::new(40.0, h),
            a.loop_enabled,
            pal().accent,
            "Loop",
        );
        if info(
            r,
            "Loop — repeat the Arrangement between the loop brace's ends (Cmd+L)",
        )
        .clicked()
        {
            self.session
                .set_loop(!a.loop_enabled, a.loop_start, a.loop_end);
        }
        let back = arrangement::overridden(&self.session);
        let r = widgets::state_button(&mut cu, Vec2::new(44.0, h), back, pal().accent, "Back");
        if info(
            r,
            "Back to Arrangement — Session clips stop, the timeline plays again",
        )
        .clicked()
        {
            self.session.back_to_arrangement();
        }

        // ---- right: quantize, MIDI LED, keys, CPU
        let right = Rect::from_min_max(
            Pos2::new(rect.center().x + 200.0, cy - h / 2.0),
            Pos2::new(rect.right() - 8.0, cy + h / 2.0),
        );
        let mut ru = ui.new_child(
            UiBuilder::new()
                .max_rect(right)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        ru.spacing_mut().item_spacing.x = 6.0;
        // CPU meter
        let load = self.session.engine.shared.cpu_load.load().clamp(0.0, 1.0);
        let (cr, r) = ru.allocate_exact_size(
            Vec2::new(if narrow { 34.0 } else { 70.0 }, h),
            Sense::hover(),
        );
        ru.painter().rect_filled(cr, 1.0, pal().field_bg);
        let bar = Rect::from_min_size(cr.min, Vec2::new(cr.width() * load, cr.height()));
        ru.painter().rect_filled(
            bar,
            1.0,
            if load > 0.8 {
                pal().arm_red
            } else {
                shade(0x3c5a3c)
            },
        );
        ru.painter().text(
            cr.center(),
            Align2::CENTER_CENTER,
            if narrow {
                format!("{:.0}%", load * 100.0)
            } else {
                format!("CPU {:.0}%", load * 100.0)
            },
            theme::font(10.5),
            pal().text,
        );
        info(
            r,
            "CPU Load — how much of the audio deadline the engine is using",
        );
        // MIDI activity LED
        let midi_on = self.routing.activity.swap(false, Ordering::Relaxed);
        let (lr, r) = ru.allocate_exact_size(Vec2::new(34.0, h), Sense::hover());
        ru.painter().circle_filled(
            Pos2::new(lr.right() - 5.0, lr.center().y),
            3.5,
            if midi_on {
                pal().accent
            } else {
                shade(0x4a4a4a)
            },
        );
        ru.painter().text(
            Pos2::new(lr.left(), lr.center().y),
            Align2::LEFT_CENTER,
            "MIDI",
            theme::font(10.0),
            pal().text_dim,
        );
        info(
            r,
            format!(
                "MIDI Input — {}",
                self.midi
                    .port_name
                    .clone()
                    .unwrap_or_else(|| "none connected".into())
            ),
        );
        if !narrow {
            let keys = self.ui.computer_keys;
            let r = widgets::icon_button(
                &mut ru,
                Vec2::new(28.0, h),
                keys,
                pal().accent,
                icons::keyboard,
            );
            if info(
                r,
                format!(
                    "Computer MIDI Keyboard — octave {}: A to K play notes, Z and X change octave, C and V change velocity (M)",
                    self.ui.octave
                ),
            )
            .clicked()
            {
                self.ui.computer_keys = !keys;
            }
        }
        let arr = self.ui.main_view == MainView::Arrangement;
        let r = widgets::state_button(&mut ru, Vec2::new(56.0, h), arr, shade(0x8a8a8a), "Arrange");
        if info(r, "Arrangement View — the timeline (Tab)").clicked() {
            self.ui.main_view = MainView::Arrangement;
        }
        let r = widgets::state_button(
            &mut ru,
            Vec2::new(54.0, h),
            !arr,
            shade(0x8a8a8a),
            "Session",
        );
        if info(r, "Session View — clips launched in any order (Tab)").clicked() {
            self.ui.main_view = MainView::Session;
        }
        let mut q = self.session.project.quantize;
        let r = egui::ComboBox::from_id_salt("quant")
            .width(64.0)
            .selected_text(q.label())
            .show_ui(&mut ru, |ui| {
                for opt in Quantize::ALL {
                    ui.selectable_value(&mut q, opt, opt.label());
                }
            });
        info(
            r.response,
            "Global Quantization — when launched clips start (Cmd+6 to Cmd+0)",
        );
        if q != self.session.project.quantize {
            self.session.set_quantize(q);
        }
    }

    /// Info view (left), messages (middle), panel toggles (right).
    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        let p = ui.painter().clone();
        p.rect_filled(rect, 0.0, pal().bar_bg);
        p.line_segment(
            [rect.left_top(), rect.right_top()],
            Stroke::new(1.0, pal().separator),
        );
        let mut u = ui.new_child(
            UiBuilder::new()
                .max_rect(rect.shrink2(Vec2::new(4.0, 2.0)))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        u.spacing_mut().item_spacing.x = 6.0;
        let b = self.ui.show_browser;
        let r = widgets::bare_icon(&mut u, Vec2::new(22.0, 18.0), |p, r, c| {
            icons::panel_toggle(
                p,
                r,
                icons::PanelSide::Left,
                b,
                if b { pal().accent } else { c },
            )
        });
        if info(r, "Browser — show or hide the Browser (⌥⌘B)").clicked() {
            self.toggle_browser();
        }
        // The Info View takes over while it's showing; otherwise hovering writes here.
        let info_text = if self.ui.show_info {
            String::new()
        } else {
            widgets::info_text(u.ctx()).unwrap_or_default()
        };
        let right_w = 330.0;
        let avail = (u.available_width() - right_w).max(50.0);
        let (ir, _) = u.allocate_exact_size(Vec2::new(avail, 18.0), Sense::hover());
        let msg = if !info_text.is_empty() {
            (info_text, pal().text)
        } else if !self.ui.status.is_empty() {
            (self.ui.status.clone(), pal().text_dim)
        } else {
            (
                self.session.messages.last().cloned().unwrap_or_default(),
                pal().text_dim,
            )
        };
        let galley = u.painter().layout_no_wrap(msg.0, theme::font(11.0), msg.1);
        u.painter().with_clip_rect(ir).galley(
            Pos2::new(ir.left(), ir.center().y - galley.size().y / 2.0),
            galley,
            msg.1,
        );

        u.with_layout(egui::Layout::right_to_left(egui::Align::Center), |u| {
            let d = self.ui.show_detail;
            let r = widgets::bare_icon(u, Vec2::new(22.0, 18.0), |p, r, c| {
                icons::panel_toggle(
                    p,
                    r,
                    icons::PanelSide::Bottom,
                    d,
                    if d { pal().accent } else { c },
                )
            });
            if info(
                r,
                "Detail View — show or hide the Clip and Device Views (⌥⌘L)",
            )
            .clicked()
            {
                self.toggle_detail();
            }
            if self.ui.selected_clip.is_some() {
                let clip_on = self.ui.detail == Detail::Clip;
                let r = widgets::state_button(
                    u,
                    Vec2::new(40.0, 16.0),
                    clip_on,
                    shade(0x8a8a8a),
                    "Clip",
                );
                if info(r, "Clip View — edit the selected clip (Shift+Tab toggles)").clicked() {
                    self.ui.detail = Detail::Clip;
                    self.ui.show_detail = true;
                }
            }
            let dev_on = self.ui.detail == Detail::Devices;
            let r =
                widgets::state_button(u, Vec2::new(52.0, 16.0), dev_on, shade(0x8a8a8a), "Devices");
            if info(
                r,
                "Device View — the selected track's devices (Shift+Tab toggles)",
            )
            .clicked()
            {
                self.ui.detail = Detail::Devices;
                self.ui.show_detail = true;
            }
            let name = self
                .session
                .path
                .as_ref()
                .and_then(|p| p.file_stem())
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Untitled".into());
            let dirty = if self.session.dirty { "*" } else { "" };
            u.label(
                RichText::new(format!("{name}{dirty}"))
                    .size(11.0)
                    .color(pal().text_dim),
            );
            if !self.ui.scan_status.is_empty() {
                u.label(
                    RichText::new(&self.ui.scan_status)
                        .size(11.0)
                        .color(pal().text_dim),
                );
            }
        });
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        self.settings_window(ctx);

        let mut open = self.ui.show_export;
        egui::Window::new("Export Audio")
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("Renders offline to a 32-bit float WAV file.");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut self.ui.export_arrangement, false, "Scene");
                    ui.radio_value(&mut self.ui.export_arrangement, true, "Arrangement");
                });
                let scenes = self.session.project.scenes.len();
                if self.ui.export_arrangement {
                    let a = &self.session.project.arrangement;
                    ui.label(if a.loop_enabled {
                        "Renders the loop range."
                    } else {
                        "Renders from the start to the last clip."
                    });
                } else {
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
                }
                if ui.button("Export…").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("WAV", &["wav"])
                        .set_file_name("Export.wav")
                        .save_file()
                    {
                        let result = if self.ui.export_arrangement {
                            self.session.export_arrangement(&path)
                        } else {
                            self.session.export_scene(
                                self.ui.export_scene,
                                self.ui.export_bars,
                                &path,
                            )
                        };
                        self.ui.status = match result {
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
            ui.label(RichText::new("UI font: Inter (SIL Open Font License 1.1)").color(pal().text_dim));
        });
        self.ui.show_about = open;
    }
}

impl eframe::App for RliveApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        theme::begin_frame(ctx, &self.settings.theme);
        widgets::info_begin_frame(ctx);
        self.session.colors = self.settings.colors;
        self.session.warp_defaults = self.settings.warp;
        self.session.idle();
        self.poll_scan();
        if std::mem::take(&mut self.ui.request_scan) && self.ui.scan_rx.is_none() {
            self.start_plugin_scan();
        }
        if let Some(ChainRef::Track(t)) = self.ui.selected {
            *self.routing.selected.lock().unwrap() = Some(t);
        }
        for c in std::mem::take(&mut self.pending) {
            self.dispatch(ctx, c);
        }
        self.handle_shortcuts(ctx);
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        #[cfg(target_os = "macos")]
        if let Some(menu) = &self.mac_menu {
            menu.drain_into(ctx.text_edit_focused(), raw, &mut self.pending);
        }
        // Taking shortcuts here keeps Tab from moving egui's keyboard focus, which would silence
        // Space and the computer keyboard.
        let cx = self.key_context(ctx);
        self.ui.keys.process(raw, &cx, &mut self.pending);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.ui.devices.begin_frame();
        self.refresh_hardware();
        let meters_moving = self.update_meters();
        // Repaint continuously only while something animates; otherwise egui repaints on input.
        if self.session.is_playing() || meters_moving || self.ui.scan_rx.is_some() {
            ctx.request_repaint();
        } else if self.session.samples.any_loading() {
            // Soon enough to show a sample becoming ready without waiting for input.
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }

        #[cfg(not(target_os = "macos"))]
        egui::Panel::top("menu")
            .exact_size(MENU_H)
            .frame(
                egui::Frame::new()
                    .fill(pal().bar_bg)
                    .inner_margin(egui::Margin::symmetric(6, 1)),
            )
            .show(ui, |ui| self.menu_bar(ui));
        let mut rects = FocusRects::default();
        let panel = egui::Panel::top("control_bar")
            .exact_size(CONTROL_BAR_H)
            .frame(egui::Frame::new())
            .show(ui, |ui| self.control_bar(ui));
        rects.control_bar = Some(panel.response.rect);
        egui::Panel::bottom("status")
            .exact_size(STATUS_H)
            .frame(egui::Frame::new())
            .show(ui, |ui| self.status_bar(ui));
        if self.ui.show_browser || self.ui.show_info {
            let panel = egui::Panel::left("browser")
                .resizable(true)
                .default_size(250.0)
                .size_range(180.0..=520.0)
                .frame(
                    egui::Frame::new()
                        .fill(pal().panel)
                        .stroke(Stroke::new(1.0, pal().separator)),
                )
                .show(ui, |ui| {
                    let full = ui.max_rect();
                    ui.expand_to_include_rect(full);
                    let info_h = if self.ui.show_info {
                        info_view::HEIGHT
                    } else {
                        0.0
                    };
                    let split = full.bottom() - info_h;
                    if self.ui.show_browser {
                        let top = Rect::from_min_max(full.min, Pos2::new(full.right(), split));
                        let mut bu = ui.new_child(UiBuilder::new().max_rect(top));
                        browser::show(&mut bu, &mut self.session, &mut self.ui, &mut self.settings);
                    }
                    if self.ui.show_info {
                        let bottom = Rect::from_min_max(Pos2::new(full.left(), split), full.max);
                        let mut iu = ui.new_child(UiBuilder::new().max_rect(bottom));
                        self.info_view(&mut iu);
                    }
                });
            rects.browser = Some(panel.response.rect);
        }
        if self.ui.show_detail {
            let panel = egui::Panel::bottom("detail")
                .resizable(true)
                .default_size(250.0)
                .size_range(130.0..=700.0)
                .frame(
                    egui::Frame::new()
                        .fill(pal().panel_dark)
                        .stroke(Stroke::new(1.0, pal().separator)),
                )
                .show(ui, |ui| match self.ui.detail {
                    Detail::Clip if self.ui.selected_clip.is_some() => {
                        piano_roll::show(ui, &mut self.session, &mut self.ui)
                    }
                    _ => devices::show(ui, &mut self.session, &mut self.ui),
                });
            rects.detail = Some(panel.response.rect);
        }
        let panel = egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(pal().grid_bg))
            .show(ui, |ui| match self.ui.main_view {
                MainView::Session => session_view::show(ui, &mut self.session, &mut self.ui),
                MainView::Arrangement => arrangement::show(ui, &mut self.session, &mut self.ui),
            });
        rects.main = Some(panel.response.rect);
        self.track_focus(&ctx, &rects);
        self.paint_focus_outline(&ctx, &rects);
        let pointer_down = ctx.input(|i| i.pointer.any_down());
        self.session.pointer_frame(pointer_down);
        let busy = pointer_down || ctx.text_edit_focused() || self.session.is_recording();
        self.session.commit_edits(busy);
        self.ui.devices.end_frame();
        #[cfg(target_os = "macos")]
        if let Some(menu) = &self.mac_menu {
            menu.publish(self.menu_state(&ctx));
        }
        self.dialogs(&ctx);
    }

    #[cfg(target_os = "macos")]
    fn on_exit(&mut self) {
        self.shutdown();
    }

    #[cfg(not(target_os = "macos"))]
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.shutdown();
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        pal().app_bg.to_normalized_gamma_f32()
    }
}
