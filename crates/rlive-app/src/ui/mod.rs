//! The rLive user interface (Ableton Live–style).

mod arrangement;
mod browser;
pub mod commands;
mod devices;
mod icons;
#[cfg(target_os = "macos")]
mod mac_menu;
pub(crate) mod panels;
mod piano_roll;
mod session_view;
pub mod theme;
mod timeline;
pub mod widgets;

use crate::audio::{self, AudioOutput};
use crate::midi_input::{self, MidiInput, MidiRouting};
use crate::session::{ChainRef, ClipRef, Session};
use crate::settings::Settings;
use commands::{ClipBoard, FocusArea, MenuCommand};
use egui::{Align2, Key, Pos2, Rect, RichText, Sense, Stroke, UiBuilder, Vec2};
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
    pub clipboard: Option<ClipBoard>,
    pub browser_filter: String,
    pub browser_category: browser::Category,
    pub meters: HashMap<TrackId, MeterState>,
    pub master_meter: MeterState,
    pub devices: devices::DeviceViewState,
    pub renaming: Option<(TrackId, String)>,
    pub piano: piano_roll::PianoState,
    pub layout: session_view::SessionLayout,
    pub arr: arrangement::ArrangementState,
    pub show_browser: bool,
    pub show_detail: bool,
    pub computer_keys: bool,
    pub octave: i32,
    held: HashMap<Key, u8>,
    show_settings: bool,
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

#[cfg(not(target_os = "macos"))]
const MENU_H: f32 = 22.0;
const CONTROL_BAR_H: f32 = 34.0;
const STATUS_H: f32 = 22.0;

impl RliveApp {
    pub fn new(cc: &eframe::CreationContext<'_>, open: Option<PathBuf>) -> Self {
        theme::apply(&cc.egui_ctx);
        let settings = Settings::load();
        cc.egui_ctx.set_zoom_factor(settings.zoom());
        let (audio, mut handle) = audio::start(settings.audio_device.as_deref());
        let sender = handle.take_midi_sender();
        let plugins = PluginCache::load(&PluginCache::default_path());
        let mut session = Session::new(handle, Project::new_default(), plugins);
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
            clipboard: None,
            browser_filter: String::new(),
            browser_category: browser::Category::Instruments,
            meters: HashMap::new(),
            master_meter: MeterState::default(),
            devices: devices::DeviceViewState::default(),
            renaming: None,
            piano: piano_roll::PianoState::default(),
            layout: session_view::SessionLayout::default(),
            arr: arrangement::ArrangementState::default(),
            show_browser: settings.show_browser.unwrap_or(true),
            show_detail: settings.show_detail.unwrap_or(true),
            computer_keys: true,
            octave: 3,
            held: HashMap::new(),
            show_settings: false,
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

    /// Edit commands act on whichever area was clicked last.
    fn track_edit_focus(&mut self, ctx: &egui::Context, detail: Option<Rect>) {
        if !ctx.input(|i| i.pointer.primary_pressed()) {
            return;
        }
        let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) else {
            return;
        };
        self.ui.focus = if detail.is_some_and(|r| r.contains(pos)) && self.ui.detail == Detail::Clip
        {
            FocusArea::Notes
        } else if self.ui.main_view == MainView::Arrangement {
            FocusArea::Arrangement
        } else {
            FocusArea::Session
        };
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
        #[cfg(not(target_os = "macos"))]
        self.menu_shortcuts(ctx);
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let (space, delete, record) = ctx.input(|i| {
            (
                i.key_pressed(Key::Space),
                i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace),
                i.key_pressed(Key::F9),
            )
        });
        if record {
            self.toggle_recording();
        }
        if space {
            if self.session.is_playing() {
                self.session.stop();
            } else {
                self.session.play();
            }
        }
        if delete {
            self.dispatch(ctx, MenuCommand::Delete);
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
        let (pressed, released, z, x, modifier) = ctx.input(|i| {
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
                i.modifiers.command || i.modifiers.alt,
            )
        });
        if modifier {
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
                self.session.play_midi(
                    *t,
                    MidiMessage::NoteOn {
                        channel: 0,
                        key: note,
                        velocity: 0.8,
                        note_id: 0,
                        tuning: 0.0,
                    },
                );
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

    /// Live-style control bar: tempo & signature left, transport centre, status right.
    fn control_bar(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        let p = ui.painter().clone();
        p.rect_filled(rect, 0.0, BAR_BG);
        p.line_segment(
            [rect.left_bottom(), rect.right_bottom()],
            Stroke::new(1.0, SEPARATOR),
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
        let r = widgets::state_button(&mut lu, Vec2::new(34.0, h), false, ACCENT, "TAP");
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
        info(r, "Time signature numerator");
        lu.label(RichText::new("/").color(TEXT_DIM));
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
        info(r.response, "Time signature denominator");
        if changed {
            self.session.set_time_signature(TimeSignature { ..sig });
        }
        let metro = self.session.project.metronome;
        let r = widgets::icon_button(&mut lu, Vec2::new(26.0, h), metro, ACCENT, |p, r, c| {
            icons::metronome(p, r, c)
        });
        if info(r, "Metronome").clicked() {
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
            PLAY_GREEN,
            icons::play,
        );
        if info(r, "Play (Space)").clicked() {
            self.session.play();
        }
        let r = widgets::icon_button(&mut cu, Vec2::new(30.0, h), false, ACCENT, |p, r, c| {
            icons::stop(p, r, c)
        });
        if info(r, "Stop (Space); stop twice to return to the start").clicked() {
            self.session.stop();
        }
        let recording = self.session.is_recording();
        let r = widgets::icon_button(
            &mut cu,
            Vec2::new(30.0, h),
            recording,
            ARM_RED,
            icons::record,
        );
        if info(r, "Record MIDI onto armed tracks in the Arrangement (F9)").clicked() {
            self.toggle_recording();
        }
        let armed = self.session.automation_arm();
        let r = widgets::icon_button(
            &mut cu,
            Vec2::new(26.0, h),
            armed,
            ARM_RED,
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
            ACCENT,
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
        cu.painter().rect_filled(pr, 1.0, FIELD_BG);
        cu.painter().text(
            pr.center(),
            Align2::CENTER_CENTER,
            pos,
            egui::FontId::monospace(13.0),
            if playing { PLAY_GREEN } else { TEXT },
        );
        info(r, "Song position (bar . beat . sixteenth)");
        cu.add_space(6.0);
        let a = self.session.project.arrangement.clone();
        let r = widgets::state_button(&mut cu, Vec2::new(40.0, h), a.loop_enabled, ACCENT, "Loop");
        if info(r, "Loop the Arrangement between the loop brace's ends").clicked() {
            self.session
                .set_loop(!a.loop_enabled, a.loop_start, a.loop_end);
        }
        let back = arrangement::overridden(&self.session);
        let r = widgets::state_button(&mut cu, Vec2::new(44.0, h), back, ACCENT, "Back");
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
        ru.painter().rect_filled(cr, 1.0, FIELD_BG);
        let bar = Rect::from_min_size(cr.min, Vec2::new(cr.width() * load, cr.height()));
        ru.painter()
            .rect_filled(bar, 1.0, if load > 0.8 { ARM_RED } else { hex(0x3c5a3c) });
        ru.painter().text(
            cr.center(),
            Align2::CENTER_CENTER,
            if narrow {
                format!("{:.0}%", load * 100.0)
            } else {
                format!("CPU {:.0}%", load * 100.0)
            },
            theme::font(10.5),
            TEXT,
        );
        info(r, "Audio engine load");
        // MIDI activity LED
        let midi_on = self.routing.activity.swap(false, Ordering::Relaxed);
        let (lr, r) = ru.allocate_exact_size(Vec2::new(34.0, h), Sense::hover());
        ru.painter().circle_filled(
            Pos2::new(lr.right() - 5.0, lr.center().y),
            3.5,
            if midi_on { ACCENT } else { hex(0x4a4a4a) },
        );
        ru.painter().text(
            Pos2::new(lr.left(), lr.center().y),
            Align2::LEFT_CENTER,
            "MIDI",
            theme::font(10.0),
            TEXT_DIM,
        );
        info(
            r,
            format!(
                "MIDI input: {}",
                self.midi
                    .port_name
                    .clone()
                    .unwrap_or_else(|| "none connected".into())
            ),
        );
        if !narrow {
            let keys = self.ui.computer_keys;
            let r = widgets::icon_button(&mut ru, Vec2::new(28.0, h), keys, ACCENT, |p, r, c| {
                icons::keyboard(p, r, c)
            });
            if info(
                r,
                format!(
                    "Computer MIDI keyboard (octave {}): A–K play notes, Z/X change octave",
                    self.ui.octave
                ),
            )
            .clicked()
            {
                self.ui.computer_keys = !keys;
            }
        }
        let arr = self.ui.main_view == MainView::Arrangement;
        let r = widgets::state_button(&mut ru, Vec2::new(56.0, h), arr, hex(0x8a8a8a), "Arrange");
        if info(r, "Arrangement View (Tab)").clicked() {
            self.ui.main_view = MainView::Arrangement;
        }
        let r = widgets::state_button(&mut ru, Vec2::new(54.0, h), !arr, hex(0x8a8a8a), "Session");
        if info(r, "Session View (Tab)").clicked() {
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
        info(r.response, "Global launch quantization");
        if q != self.session.project.quantize {
            self.session.set_quantize(q);
        }
    }

    /// Info view (left), messages (middle), panel toggles (right).
    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        let p = ui.painter().clone();
        p.rect_filled(rect, 0.0, BAR_BG);
        p.line_segment(
            [rect.left_top(), rect.right_top()],
            Stroke::new(1.0, SEPARATOR),
        );
        let mut u = ui.new_child(
            UiBuilder::new()
                .max_rect(rect.shrink2(Vec2::new(4.0, 2.0)))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        u.spacing_mut().item_spacing.x = 6.0;
        let b = self.ui.show_browser;
        let r = widgets::bare_icon(&mut u, Vec2::new(22.0, 18.0), |p, r, c| {
            icons::panel_toggle(p, r, icons::PanelSide::Left, b, if b { ACCENT } else { c })
        });
        if info(r, "Show/hide the Browser (⌥⌘B)").clicked() {
            self.toggle_browser();
        }
        let info_text = widgets::info_text(u.ctx()).unwrap_or_default();
        let right_w = 330.0;
        let avail = (u.available_width() - right_w).max(50.0);
        let (ir, _) = u.allocate_exact_size(Vec2::new(avail, 18.0), Sense::hover());
        let msg = if !info_text.is_empty() {
            (info_text, TEXT)
        } else if !self.ui.status.is_empty() {
            (self.ui.status.clone(), TEXT_DIM)
        } else {
            (
                self.session.messages.last().cloned().unwrap_or_default(),
                TEXT_DIM,
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
                    if d { ACCENT } else { c },
                )
            });
            if info(r, "Show/hide the Detail View (⌥⌘L)").clicked() {
                self.toggle_detail();
            }
            if self.ui.selected_clip.is_some() {
                let clip_on = self.ui.detail == Detail::Clip;
                let r =
                    widgets::state_button(u, Vec2::new(40.0, 16.0), clip_on, hex(0x8a8a8a), "Clip");
                if info(r, "Clip View (Shift+Tab toggles)").clicked() {
                    self.ui.detail = Detail::Clip;
                    self.ui.show_detail = true;
                }
            }
            let dev_on = self.ui.detail == Detail::Devices;
            let r =
                widgets::state_button(u, Vec2::new(52.0, 16.0), dev_on, hex(0x8a8a8a), "Devices");
            if info(r, "Device View (Shift+Tab toggles)").clicked() {
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
                    .color(TEXT_DIM),
            );
            if !self.ui.scan_status.is_empty() {
                u.label(
                    RichText::new(&self.ui.scan_status)
                        .size(11.0)
                        .color(TEXT_DIM),
                );
            }
        });
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        let mut open = self.ui.show_settings;
        egui::Window::new("Preferences")
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                widgets::section_label(ui, "AUDIO");
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
                ui.horizontal(|ui| {
                    ui.label("Output");
                    egui::ComboBox::from_id_salt("audio_dev")
                        .width(240.0)
                        .selected_text(current)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut choice, None, "System default");
                            for d in devices {
                                ui.selectable_value(&mut choice, Some(d.clone()), d);
                            }
                        });
                    if ui.small_button("Refresh").clicked() {
                        self.ui.audio_devices = None;
                    }
                });
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
                    .color(TEXT_DIM),
                );
                ui.add_space(6.0);
                widgets::section_label(ui, "MIDI");
                let ports = self
                    .ui
                    .midi_ports
                    .get_or_insert_with(midi_input::port_names)
                    .clone();
                let mut port = self.settings.midi_input.clone();
                let mut reconnect = false;
                ui.horizontal(|ui| {
                    ui.label("Input ");
                    egui::ComboBox::from_id_salt("midi_port")
                        .width(240.0)
                        .selected_text(port.clone().unwrap_or_else(|| "First available".into()))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut port, None, "First available");
                            for p in ports {
                                ui.selectable_value(&mut port, Some(p.clone()), p);
                            }
                        });
                    reconnect = ui.small_button("Reconnect").clicked();
                });
                if port != self.settings.midi_input || reconnect {
                    self.ui.midi_ports = None;
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
                ui.add_space(6.0);
                widgets::section_label(ui, "LOOK");
                ui.horizontal(|ui| {
                    ui.label("Zoom");
                    let mut z = ui.ctx().zoom_factor();
                    if ui
                        .add(
                            egui::Slider::new(&mut z, 0.7..=2.0)
                                .step_by(0.1)
                                .suffix("×"),
                        )
                        .changed()
                    {
                        self.set_zoom(&ui.ctx().clone(), z);
                    }
                });
                ui.add_space(6.0);
                widgets::section_label(ui, "PLUG-INS");
                ui.label(
                    RichText::new("System CLAP/VST3 folders and Audio Units are searched, plus:")
                        .color(TEXT_DIM),
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
                    self.settings.save();
                }
                ui.horizontal(|ui| {
                    if ui.button("Add Folder…").clicked() {
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
                ui.label(RichText::new(&self.ui.scan_status).color(TEXT_DIM));
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
            ui.label(RichText::new("UI font: Inter (SIL Open Font License 1.1)").color(TEXT_DIM));
        });
        self.ui.show_about = open;
    }
}

impl eframe::App for RliveApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        widgets::info_begin_frame(ctx);
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
        let text_focused = ctx.text_edit_focused();
        #[cfg(target_os = "macos")]
        if let Some(menu) = &self.mac_menu {
            menu.drain_into(text_focused, raw, &mut self.pending);
        }
        // Tab switches views unless a text field has focus; left to egui it
        // would move keyboard focus and silence Space and the computer keyboard.
        if text_focused {
            return;
        }
        let pending = &mut self.pending;
        raw.events.retain(|e| match e {
            egui::Event::Key {
                key: Key::Tab,
                pressed,
                repeat,
                modifiers,
                ..
            } => {
                if *pressed && !*repeat {
                    pending.push(if modifiers.shift {
                        MenuCommand::SwitchDetail
                    } else {
                        MenuCommand::ToggleMainView
                    });
                }
                false
            }
            _ => true,
        });
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.ui.devices.begin_frame();
        let meters_moving = self.update_meters();
        // Repaint continuously only while something animates; otherwise egui repaints on input.
        if self.session.is_playing() || meters_moving || self.ui.scan_rx.is_some() {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }

        #[cfg(not(target_os = "macos"))]
        egui::Panel::top("menu")
            .exact_size(MENU_H)
            .frame(
                egui::Frame::new()
                    .fill(BAR_BG)
                    .inner_margin(egui::Margin::symmetric(6, 1)),
            )
            .show(ui, |ui| self.menu_bar(ui));
        egui::Panel::top("control_bar")
            .exact_size(CONTROL_BAR_H)
            .frame(egui::Frame::new())
            .show(ui, |ui| self.control_bar(ui));
        egui::Panel::bottom("status")
            .exact_size(STATUS_H)
            .frame(egui::Frame::new())
            .show(ui, |ui| self.status_bar(ui));
        if self.ui.show_browser {
            egui::Panel::left("browser")
                .resizable(true)
                .default_size(250.0)
                .size_range(180.0..=520.0)
                .frame(
                    egui::Frame::new()
                        .fill(PANEL)
                        .stroke(Stroke::new(1.0, SEPARATOR)),
                )
                .show(ui, |ui| {
                    browser::show(ui, &mut self.session, &mut self.ui, &mut self.settings)
                });
        }
        let mut detail_rect = None;
        if self.ui.show_detail {
            let panel = egui::Panel::bottom("detail")
                .resizable(true)
                .default_size(250.0)
                .size_range(130.0..=700.0)
                .frame(
                    egui::Frame::new()
                        .fill(PANEL_DARK)
                        .stroke(Stroke::new(1.0, SEPARATOR)),
                )
                .show(ui, |ui| match self.ui.detail {
                    Detail::Clip if self.ui.selected_clip.is_some() => {
                        piano_roll::show(ui, &mut self.session, &mut self.ui)
                    }
                    _ => devices::show(ui, &mut self.session, &mut self.ui),
                });
            detail_rect = Some(panel.response.rect);
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(GRID_BG))
            .show(ui, |ui| match self.ui.main_view {
                MainView::Session => session_view::show(ui, &mut self.session, &mut self.ui),
                MainView::Arrangement => arrangement::show(ui, &mut self.session, &mut self.ui),
            });
        self.track_edit_focus(&ctx, detail_rect);
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
        APP_BG.to_normalized_gamma_f32()
    }
}
