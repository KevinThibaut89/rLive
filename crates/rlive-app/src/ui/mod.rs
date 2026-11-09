//! The rLive user interface (Ableton Live–style).

mod arrangement;
mod automation;
pub(crate) mod browser;
pub mod clip_envelopes;
pub(crate) mod clip_view;
pub mod commands;
pub(crate) mod devices;
mod groove_pool;
mod history_window;
mod icons;
mod in_out;
mod info_view;
pub mod keymap;
mod launch_panel;
#[cfg(target_os = "macos")]
mod mac_menu;
pub mod midi_settings;
pub(crate) mod panels;
pub(crate) mod piano_roll;
mod rack_mixer;
pub mod remote_map;
mod sample_editor;
mod scene_view;
mod session_select;
mod session_status;
mod session_view;
mod settings_window;
pub mod theme;
mod timeline;
mod tuning_section;
pub mod widgets;

use crate::audio::{self, AudioOutput};
use crate::midi_input::{self, MidiInput, MidiRouting, OutputBank, PortSwitches};
use crate::session::InfoTarget;
use crate::session::{AudioHardware, ChainRef, ClipRef, RemotePorts, Session};
use crate::settings::{ChannelBank, DeviceChannels, Settings};
use commands::{ClipBoard, MenuCommand};
use egui::{Align2, Key, Pos2, Rect, RichText, Sense, Stroke, UiBuilder, Vec2};
use keymap::{FocusArea, KeyContext, KeyState, Platform};
use rlive_core::param::{ParamId, ParamOwner, ParamRef};
use rlive_core::{MidiMessage, Quantize, TimeSignature, TrackId};
use rlive_plugin_host::scanner::PluginCache;
use rlive_project::Project;
use rlive_project::{MapTarget, RemoteControl};
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
    Scene,
}

/// Something dragged from the browser.
#[derive(Clone)]
pub enum BrowserItem {
    Builtin(&'static str),
    Plugin(rlive_plugin_host::PluginDescriptor),
    Sample(PathBuf),
    /// A device, rack or chain preset.
    Preset(browser::PresetSource),
    /// A groove from the Library.
    Groove(browser::AssetSource),
    /// A tuning system from the Library.
    Tuning(browser::AssetSource),
    /// A `.stacks` chord bank.
    ChordBank(PathBuf),
    /// A saved clip.
    Clip(PathBuf),
    /// A Set.
    Set(PathBuf),
    /// A device already in the Set, from Current Project.
    ProjectDevice(rlive_core::DeviceId),
    /// A clip already in the Set, from Current Project.
    ProjectClip(ClipRef),
    /// A multi-selection.
    Many(Vec<BrowserItem>),
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
    /// What's selected in the Session grid.
    pub sel: session_select::SessionSelection,
    pub main_view: MainView,
    pub focus: FocusArea,
    pub keys: KeyState,
    pub clipboard: Option<ClipBoard>,
    pub browser: browser::BrowserState,
    /// Options > Retune Set On Loading Tuning Systems, mirrored for the browser.
    pub retune_on_load: bool,
    pub show_undo_history: bool,
    /// The step picked in the Undo History, before it's chosen.
    pub undo_history_cursor: Option<usize>,
    pub meters: HashMap<TrackId, MeterState>,
    pub master_meter: MeterState,
    pub devices: devices::DeviceViewState,
    pub renaming: Option<(TrackId, String)>,
    pub piano: piano_roll::PianoState,
    pub sample: sample_editor::SampleState,
    pub env: clip_envelopes::ClipEnvState,
    pub clip_view: clip_view::ClipViewState,
    /// Follow Behavior is Scroll, so views keep moving with the playhead.
    pub follow_scrolls: bool,
    /// MIDI Envelope Auto-Reset, mirrored for the Sample Editor's context menu to show.
    pub midi_auto_reset: bool,
    /// The context menu asked for MIDI Envelope Auto-Reset to be toggled.
    pub toggle_midi_auto_reset: bool,
    pub layout: session_view::SessionLayout,
    pub arr: arrangement::ArrangementState,
    pub show_browser: bool,
    pub show_groove_pool: bool,
    pub show_tuning: bool,
    /// Lowest and Highest are showing.
    pub tuning_open: bool,
    pub pending_tuning: Option<tuning_section::PendingTuning>,
    pub mpe_dialog: Option<midi_settings::MpeTarget>,
    pub layout_dialog: Option<rlive_core::TrackId>,
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
    /// What Copy picked up, for Paste Bounced Audio to render when it is used.
    pub bounce_copy: Option<crate::session::BounceSource>,
    /// Tracks waiting their turn in a Freeze of several at once.
    pub pending_freezes: Vec<TrackId>,
    pub request_scan: bool,
    /// Device lists are slow to enumerate; refreshed only when Preferences opens.
    audio_devices: Option<Vec<String>>,
    audio_inputs: Option<Vec<String>>,
    midi_ports: Option<Vec<String>>,
    /// Output port names, listed when the Link, Tempo & MIDI tab opens.
    midi_outputs: Option<Vec<String>>,
    /// The row picked in the Mapping Browser, for Delete.
    pub mapping_row: Option<usize>,
    /// The time this frame started, in seconds.
    pub frame_time: f64,
    tap_times: Vec<f64>,
    /// The hardware channels the In/Out choosers offer, from the Channel Configuration.
    pub hardware: AudioHardware,
}

pub struct RliveApp {
    session: Session,
    audio: AudioOutput,
    midi: MidiInput,
    routing: MidiRouting,
    /// What the track MIDI output thread plays out of.
    track_outs: midi_input::TrackOutputs,
    /// What it reads from.
    track_midi: midi_input::TrackMidiFeed,
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

/// The Song Tempo, as a mapping target.
fn tempo_param() -> ParamRef {
    ParamRef {
        owner: ParamOwner::Transport,
        id: ParamId(0),
    }
}

/// Where the small light goes that says a song has signature changes.
fn r_led_pos(ui: &egui::Ui) -> Pos2 {
    let at = ui.cursor().min;
    Pos2::new(at.x - 4.0, at.y + 3.0)
}

/// Reads a bar.beat.sixteenth position back into ticks.
fn parse_position(text: &str, sig: TimeSignature) -> Option<f64> {
    let mut parts = text.split(['.', ':']).map(str::parse::<u64>);
    let bar = parts.next()?.ok()?.max(1) - 1;
    let beat = parts.next().unwrap_or(Ok(1)).ok()?.max(1) - 1;
    let sixteenth = parts.next().unwrap_or(Ok(1)).ok()?.max(1) - 1;
    Some(
        (bar * sig.ticks_per_bar()
            + beat * sig.ticks_per_beat()
            + sixteenth * (rlive_core::PPQ / 4)) as f64,
    )
}

/// A length as bars.beats.sixteenths, zero-based, as Live's length fields show it.
fn format_length(ticks: f64, sig: TimeSignature) -> String {
    let t = ticks.max(0.0) as u64;
    let bars = t / sig.ticks_per_bar();
    let rest = t % sig.ticks_per_bar();
    let beats = rest / sig.ticks_per_beat();
    let sixteenths = (rest % sig.ticks_per_beat()) / (rlive_core::PPQ / 4);
    format!("{bars}.{beats}.{sixteenths}")
}

/// The inverse of [`format_length`].
fn parse_length(text: &str, sig: TimeSignature) -> Option<f64> {
    let mut parts = text.split(['.', ':']).map(str::parse::<u64>);
    let bars = parts.next()?.ok()?;
    let beats = parts.next().unwrap_or(Ok(0)).ok()?;
    let sixteenths = parts.next().unwrap_or(Ok(0)).ok()?;
    Some(
        (bars * sig.ticks_per_bar()
            + beats * sig.ticks_per_beat()
            + sixteenths * (rlive_core::PPQ / 4)) as f64,
    )
}

#[cfg(not(target_os = "macos"))]
const MENU_H: f32 = 22.0;
const CONTROL_BAR_H: f32 = 34.0;
const STATUS_H: f32 = 22.0;

/// The count-in as negative bars, beats and sixteenths: `-2.1.1.` for two bars to go, up to
/// `1.1.1.` when recording starts.
fn count_in_label(left: f64, sig: rlive_core::TimeSignature) -> String {
    let bar = sig.ticks_per_bar() as f64;
    let beat = sig.ticks_per_beat() as f64;
    let bars = (left / bar).ceil().max(1.0);
    let elapsed = bars * bar - left;
    let beat_n = (elapsed / beat).floor();
    let sixteenth = ((elapsed - beat_n * beat) / (rlive_core::PPQ as f64 / 4.0)).floor();
    format!(
        "-{}.  {}.  {}",
        bars as u32,
        beat_n as u32 + 1,
        sixteenth as u32 + 1
    )
}

impl UiState {
    /// The UI's state on opening, with `status` on the status bar.
    pub(crate) fn initial(session: &Session, settings: &Settings, status: String) -> Self {
        Self {
            selected: session
                .project
                .tracks
                .first()
                .map(|t| ChainRef::Track(t.id)),
            selected_tracks: Vec::new(),
            group_slot: None,
            selected_clip: None,
            detail: Detail::Devices,
            sel: session_select::SessionSelection::default(),
            main_view: MainView::Session,
            focus: FocusArea::Session,
            keys: KeyState::default(),
            clipboard: None,
            browser: browser::BrowserState::default(),
            retune_on_load: false,
            show_undo_history: false,
            undo_history_cursor: None,
            meters: HashMap::new(),
            master_meter: MeterState::default(),
            devices: devices::DeviceViewState::default(),
            renaming: None,
            piano: piano_roll::PianoState::default(),
            sample: sample_editor::SampleState::new(),
            env: clip_envelopes::ClipEnvState::default(),
            clip_view: clip_view::ClipViewState::with_layout(settings.clip_panel_layout),
            follow_scrolls: false,
            midi_auto_reset: false,
            toggle_midi_auto_reset: false,
            layout: session_view::SessionLayout::default(),
            arr: arrangement::ArrangementState::default(),
            show_browser: settings.show_browser.unwrap_or(true),
            show_groove_pool: false,
            show_tuning: false,
            tuning_open: false,
            pending_tuning: None,
            mpe_dialog: None,
            layout_dialog: None,
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
            bounce_copy: None,
            pending_freezes: Vec::new(),
            status,
            request_scan: false,
            audio_devices: None,
            audio_inputs: None,
            midi_ports: None,
            midi_outputs: None,
            mapping_row: None,
            frame_time: 0.0,
            tap_times: Vec::new(),
            hardware: AudioHardware::default(),
        }
    }
}

impl RliveApp {
    pub fn new(cc: &eframe::CreationContext<'_>, open: Option<PathBuf>) -> Self {
        theme::apply(&cc.egui_ctx);
        let settings = Settings::load();
        theme::begin_frame(&cc.egui_ctx, &settings.theme);
        settings_window::apply_scroll_bars(&cc.egui_ctx, settings.scroll_bars);
        cc.egui_ctx.set_zoom_factor(settings.zoom());
        let (audio, mut handle) =
            audio::start(settings.audio_device.as_deref(), &settings.audio_input);
        let sender = handle.take_midi_sender();
        let hw_midi = handle.take_hw_midi();
        let plugins = PluginCache::load(&PluginCache::default_path());
        let mut session = Session::new(handle, Project::new_default(), plugins);
        session
            .samples
            .configure(settings.samples.loader_config(settings.warp.auto_warp_long));
        session.send_midi_envelope_auto_reset(settings.midi_envelope_auto_reset);
        session.set_session_automation_in(settings.record_session_automation_in);
        session.set_chase(settings.chase_notes);
        let routing = MidiRouting {
            sender: Arc::new(Mutex::new(sender)),
            targets: session.midi_targets.clone(),
            selected: Arc::new(Mutex::new(None)),
            activity: Default::default(),
            system: Default::default(),
            mpe: Default::default(),
        };
        let midi = MidiInput::none();
        let track_outs = midi_input::TrackOutputs::default();
        let track_midi = midi_input::TrackMidiFeed::default();
        *track_midi.lock().unwrap() = hw_midi;
        midi_input::spawn_track_output(track_midi.clone(), track_outs.clone());
        let repaint = cc.egui_ctx.clone();
        session
            .remote
            .hub
            .shared
            .set_waker(move || repaint.request_repaint());
        let status = audio
            .error
            .clone()
            .map(|e| format!("Audio: {e}"))
            .unwrap_or_default();
        let mut ui = UiState::initial(&session, &settings, status);
        let user_library = settings
            .user_library
            .clone()
            .unwrap_or_else(rlive_browser::defaults::UserLibrary::default_location);
        session.library = Some(rlive_browser::defaults::UserLibrary::new(user_library));
        session.backups = true;
        ui.browser.open(&cc.egui_ctx, &settings);
        // The old Choose Folder setting carries over as a folder in Places.
        if let Some(dir) = &settings.sample_dir {
            ui.browser.op(rlive_browser::Op::AddFolder(dir.clone()));
        }
        if let Some(path) = open {
            if let Err(e) = session.load(&path) {
                ui.status = format!("Could not open {}: {e}", path.display());
            }
        }
        let mut app = Self {
            session,
            audio,
            midi,
            routing,
            track_outs,
            track_midi,
            settings,
            ui,
            last_frame: std::time::Instant::now(),
            pending: Vec::new(),
            #[cfg(target_os = "macos")]
            mac_menu: mac_menu::MacMenu::install(cc),
        };
        app.apply_control_surfaces();
        app.connect_midi();
        app
    }

    /// Takes the Control Surface rows from the settings.
    fn apply_control_surfaces(&mut self) {
        for i in 0..rlive_remote::MAX_SURFACES {
            let row = self
                .settings
                .remote
                .surfaces
                .get(i)
                .cloned()
                .unwrap_or_default();
            let surfaces = &mut self.session.remote.surfaces;
            surfaces.set(i, row.script.as_deref());
            surfaces.set_ports(i, row.input, row.output);
        }
    }

    /// Opens every MIDI port with its Track, Sync and Remote switches as they're set, and tells
    /// the session which ports do what.
    pub(crate) fn connect_midi(&mut self) {
        self.midi = MidiInput::none();
        self.session.remote.hub.close_ports();
        let legacy = self.settings.midi_input.clone();
        let inputs = midi_input::port_names();
        let outputs = midi_input::output_names();
        let surface_in: Vec<String> = self
            .settings
            .remote
            .surfaces
            .iter()
            .filter(|s| s.script.is_some())
            .filter_map(|s| s.input.clone())
            .collect();
        let mut in_mask = 0u64;
        self.session.mpe_ports.clear();
        for (i, name) in inputs.iter().enumerate().take(64) {
            let cfg = self.settings.remote.input(name, legacy.as_deref());
            if cfg.mpe {
                self.session.mpe_ports.insert(name.clone());
            }
            if cfg.remote {
                in_mask |= 1 << i;
            }
            if !(cfg.track || cfg.sync || cfg.remote || surface_in.contains(name)) {
                continue;
            }
            let switches = Arc::new(PortSwitches::new(cfg.track, cfg.sync, cfg.remote));
            // MPE Mode is a setting of the controller, so each port has its own switch.
            let routing = MidiRouting {
                mpe: Arc::new(std::sync::atomic::AtomicBool::new(cfg.mpe)),
                ..self.routing.clone()
            };
            let port = self
                .session
                .remote
                .hub
                .open_port(i as u32, routing, switches);
            if let Err(e) = self.midi.connect_port(name, port) {
                log::info!("MIDI input {name}: {e}");
            }
        }
        let surface_out: Vec<String> = self
            .settings
            .remote
            .surfaces
            .iter()
            .filter(|s| s.script.is_some())
            .filter_map(|s| s.output.clone())
            .collect();
        let mut out_mask = 0u64;
        let mut bank = OutputBank::new(self.session.remote.hub.shared.clone());
        let mut track_outs = Vec::new();
        for (i, name) in outputs.iter().enumerate().take(64) {
            let cfg = self.settings.remote.output(name);
            if cfg.remote {
                out_mask |= 1 << i;
            }
            let wanted = cfg.track || cfg.sync || cfg.remote || surface_out.contains(name);
            bank.push(if wanted {
                midi_input::connect_output(name)
                    .map_err(|e| log::info!("MIDI output {name}: {e}"))
                    .ok()
            } else {
                None
            });
            let connection = cfg.track.then(|| midi_input::connect_output(name).ok());
            track_outs.push(connection.flatten());
        }
        *self.track_outs.lock().unwrap() = track_outs;
        self.session.set_midi_out_ports(outputs.clone());
        self.session.set_remote_ports(RemotePorts {
            in_names: inputs,
            in_mask,
            out_names: outputs,
            out_mask,
        });
        self.session.set_feedback_output(Some(Box::new(bank)));
        self.session.set_takeover(self.settings.remote.takeover);
    }

    fn restart_audio(&mut self) {
        // Drop the old stream first so the device is free.
        self.audio = AudioOutput::stopped();
        let (audio, mut handle) = audio::start(
            self.settings.audio_device.as_deref(),
            &self.settings.audio_input,
        );
        *self.routing.sender.lock().unwrap() = handle.take_midi_sender();
        self.audio = audio;
        if let Some(rx) = handle.take_hw_midi() {
            *self.track_midi.lock().unwrap() = Some(rx);
        }
        self.session.replace_engine(handle);
        self.session.set_chase(self.settings.chase_notes);
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
            self.open_path(&path);
        }
    }

    fn open_path(&mut self, path: &std::path::Path) {
        match self.session.load(path) {
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
                Detail::Devices | Detail::Scene => FocusArea::Device,
            }
        } else if self.ui.main_view == MainView::Arrangement {
            FocusArea::Arrangement
        } else {
            FocusArea::Session
        };
    }

    /// The Clip View over the Device View, in one panel.
    fn stacked_detail(&mut self, ui: &mut egui::Ui) {
        let full = ui.max_rect();
        ui.expand_to_include_rect(full);
        let top = Rect::from_min_size(full.min, Vec2::new(full.width(), full.height() * 0.55));
        let bottom = Rect::from_min_max(Pos2::new(full.left(), top.bottom() + 1.0), full.max);
        let mut cu = ui.new_child(UiBuilder::new().max_rect(top));
        cu.set_clip_rect(top.intersect(ui.clip_rect()));
        launch_panel::with_launch_strip(&mut cu, &mut self.session, &mut self.ui, clip_view::show);
        let mut du = ui.new_child(UiBuilder::new().max_rect(bottom));
        du.set_clip_rect(bottom.intersect(ui.clip_rect()));
        devices::show(&mut du, &mut self.session, &mut self.ui);
    }

    /// Dragging the Detail View's top border to the bottom of the window closes it.
    fn detail_edge(&mut self, ctx: &egui::Context, rect: Rect) {
        let (pressed, down, released, origin, pos, window) = ctx.input(|i| {
            (
                i.pointer.primary_pressed(),
                i.pointer.primary_down(),
                i.pointer.primary_released(),
                i.pointer.press_origin(),
                i.pointer.latest_pos(),
                i.viewport_rect(),
            )
        });
        let view = &mut self.ui.clip_view;
        if pressed && origin.is_some_and(|o| (o.y - rect.top()).abs() < 6.0) {
            view.resizing = true;
        }
        if view.resizing && down {
            ctx.set_cursor_icon(egui::CursorIcon::ResizeVertical);
        }
        if released {
            let closing =
                view.resizing && pos.is_some_and(|p| p.y >= window.bottom() - STATUS_H - 6.0);
            view.resizing = false;
            if closing {
                self.ui.show_detail = false;
                self.settings.show_detail = Some(false);
            }
        }
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

    /// "A default for this device exists: overwrite it?" (manual p. 466).
    fn confirm_default_overwrite(&mut self, ctx: &egui::Context) {
        let Some((chain, id)) = self.ui.devices.confirm_default else {
            return;
        };
        let mut answer = None;
        egui::Window::new("Overwrite Default Preset?")
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("This device already has a default preset. Replace it?");
                ui.horizontal(|ui| {
                    if ui.button("Overwrite").clicked() {
                        answer = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        answer = Some(false);
                    }
                });
            });
        if let Some(overwrite) = answer {
            self.ui.devices.confirm_default = None;
            if overwrite {
                self.ui.status = match self.session.save_default_preset(chain, id) {
                    Ok(path) => format!("Saved {}", path.display()),
                    Err(e) => e,
                };
            }
        }
    }

    /// The Arrangement Record button. `inverted` is Shift held: the opposite of the Start
    /// Playback with Record preference.
    fn toggle_recording(&mut self, inverted: bool) {
        if self.session.is_recording() {
            self.session.stop_recording();
            return;
        }
        if inverted {
            self.session.start_recording_inverted();
        } else {
            self.session.start_recording();
        }
    }

    /// The scene Session recording and New work from: the selected scene or clip's.
    fn selected_scene(&self) -> usize {
        if let Some(s) = self.ui.sel.scenes.iter().max() {
            return *s;
        }
        self.ui
            .selected_clip
            .and_then(ClipRef::as_slot)
            .map_or(0, |(_, slot)| slot)
    }

    /// Selects a scene by selecting a slot in it on the track already selected.
    fn select_scene(&mut self, scene: usize) {
        if let Some(track) = self
            .ui
            .selected_clip
            .map(ClipRef::track)
            .or_else(|| self.session.project.tracks.first().map(|t| t.id))
        {
            self.ui.selected_clip = Some(ClipRef::slot(track, scene));
        }
    }

    fn session_record(&mut self) {
        let scene = self.selected_scene();
        self.session.session_record(scene);
    }

    fn capture_midi(&mut self) {
        let arrangement = self.ui.main_view == MainView::Arrangement;
        let scene = self.selected_scene();
        self.session.capture_midi(arrangement, scene);
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
        self.draw_key(ctx);
        self.fade_key(ctx);
        if self.ui.computer_keys && !ctx.egui_wants_keyboard_input() {
            self.computer_keyboard(ctx);
        }
    }

    /// B toggles Draw Mode, and held down while it's off, switches it on just for as long.
    fn draw_key(&mut self, ctx: &egui::Context) {
        let area = matches!(self.ui.focus, FocusArea::Arrangement | FocusArea::Clip);
        if !area || ctx.egui_wants_keyboard_input() {
            return;
        }
        let (pressed, released, modified, now) = ctx.input(|i| {
            (
                i.key_pressed(Key::B),
                i.key_released(Key::B),
                i.modifiers.any(),
                i.time,
            )
        });
        if modified {
            return;
        }
        let auto = &mut self.ui.arr.auto;
        if pressed {
            auto.draw = auto.draw_key.pressed(auto.draw, now);
        }
        if released {
            auto.draw = auto.draw_key.released(auto.draw, now);
        }
    }

    /// F held in the Arrangement shows the fade handles; with the computer keyboard on, F is a
    /// note, so as for other single keys it takes Shift.
    fn fade_key(&mut self, ctx: &egui::Context) {
        let area = self.ui.focus == FocusArea::Arrangement;
        let computer_keys = self.ui.computer_keys;
        self.ui.arr.fades_key = area
            && !ctx.egui_wants_keyboard_input()
            && ctx.input(|i| i.key_down(Key::F) && (!computer_keys || i.modifiers.shift));
    }

    /// The MIDI and KEY switches that enter the map modes.
    fn map_switches(&mut self, ru: &mut egui::Ui, h: f32) {
        use crate::session::MapMode;
        let (midi, key) = (MapMode::Midi, MapMode::Key);
        let now = self.session.map_mode();
        let key_color = remote_map::mode_color(key);
        let r = widgets::state_button(ru, Vec2::new(30.0, h), now == Some(key), key_color, "KEY");
        if info(
            r,
            "Key Map Mode — click a control, then press a key (Cmd+K)",
        )
        .clicked()
        {
            self.session.set_map_mode((now != Some(key)).then_some(key));
        }
        let midi_color = remote_map::mode_color(midi);
        let r = widgets::state_button(
            ru,
            Vec2::new(34.0, h),
            now == Some(midi),
            midi_color,
            "MIDI",
        );
        if info(
            r,
            "MIDI Map Mode — click a control, then send a MIDI message (Cmd+M)",
        )
        .clicked()
        {
            self.session
                .set_map_mode((now != Some(midi)).then_some(midi));
        }
    }

    /// The Control Bar's MIDI lights: a pair, in over out, for each of track and remote traffic,
    /// with a third for sync only while something's synced.
    fn midi_lights(&mut self, ru: &mut egui::Ui, h: f32) {
        let lights = &self.session.remote.hub.shared.lights;
        let track_in = self.routing.activity.swap(false, Ordering::Relaxed)
            | lights.track_in.swap(false, Ordering::Relaxed);
        let track_out = lights.track_out.swap(false, Ordering::Relaxed);
        let remote_in = lights.remote_in.swap(false, Ordering::Relaxed);
        let remote_out = lights.remote_out.swap(false, Ordering::Relaxed);
        let sync_in = lights.sync_in.swap(false, Ordering::Relaxed);
        let sync_out = lights.sync_out.swap(false, Ordering::Relaxed);
        let syncing = self.settings.remote.inputs.iter().any(|p| p.sync);
        let (rect, r) = ru.allocate_exact_size(Vec2::new(38.0, h), Sense::hover());
        let p = ru.painter();
        let lit = |on: bool| if on { pal().accent } else { shade(0x4a4a4a) };
        let mut x = rect.right() - 3.0;
        let mut pair = |label: &str, top: bool, bottom: bool| {
            let cx = Pos2::new(x, rect.center().y);
            p.circle_filled(cx + Vec2::new(0.0, -3.5), 2.2, lit(top));
            p.circle_filled(cx + Vec2::new(0.0, 3.5), 2.2, lit(bottom));
            p.text(
                cx + Vec2::new(-9.0, 0.0),
                Align2::LEFT_CENTER,
                label,
                theme::font(9.0),
                pal().text_dim,
            );
            x -= 12.0;
        };
        pair("T", track_in, track_out);
        pair("R", remote_in, remote_out);
        if syncing {
            pair("S", sync_in, sync_out);
        }
        info(
            r,
            format!(
                "MIDI lights, in over out: T track, R remote, S sync — {}",
                if self.midi.port_names.is_empty() {
                    "no input ports open".to_string()
                } else {
                    self.midi.port_names.join(", ")
                }
            ),
        );
    }

    /// The device a control surface should follow: the one picked in the Device View, or the
    /// first on the selected chain.
    fn selected_device(&self) -> Option<rlive_core::DeviceId> {
        if let Some((_, id)) = self.ui.devices.selected_devices.last() {
            return Some(*id);
        }
        let chain = self.ui.selected?;
        self.session.devices_of(chain).first().map(|d| d.id)
    }

    /// The browser panel, with the Groove Pool, tuning section and Info View under it.
    fn browser_panel(&mut self, ui: &mut egui::Ui, rects: &mut FocusRects) {
        let mapping = self.ui.devices.ui.map.is_some();
        let remote_mapping = self.session.map_mode().is_some();
        if self.ui.show_browser || self.ui.show_info || mapping || remote_mapping {
            let panel = egui::Panel::left("browser")
                .resizable(true)
                .default_size(320.0)
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
                    let top = Rect::from_min_max(full.min, Pos2::new(full.right(), split));
                    if remote_mapping {
                        let inner = top.shrink(6.0);
                        let mut mu = ui.new_child(UiBuilder::new().max_rect(inner));
                        remote_map::browser(&mut mu, &mut self.session, &mut self.ui);
                    } else if mapping {
                        let inner = top.shrink(6.0);
                        let mut mu = ui.new_child(UiBuilder::new().max_rect(inner));
                        devices::rack::mapping_browser::show(
                            &mut mu,
                            &mut self.session,
                            &mut self.ui,
                        );
                    } else if self.ui.show_browser {
                        let groove_h = if self.ui.show_groove_pool {
                            groove_pool::HEIGHT
                        } else {
                            0.0
                        };
                        let tuning_h = match (self.ui.show_tuning, self.ui.tuning_open) {
                            (false, _) => 0.0,
                            (true, false) => tuning_section::HEIGHT,
                            (true, true) => tuning_section::HEIGHT_OPEN,
                        };
                        let browser_bottom = (top.bottom() - groove_h - tuning_h).max(top.top());
                        let browser_rect =
                            Rect::from_min_max(top.min, Pos2::new(top.right(), browser_bottom));
                        let mut bu = ui.new_child(UiBuilder::new().max_rect(browser_rect));
                        browser::show(&mut bu, &mut self.session, &mut self.ui, &mut self.settings);
                        if self.ui.show_tuning {
                            let at = Rect::from_min_size(
                                Pos2::new(top.left(), browser_bottom),
                                Vec2::new(top.width(), tuning_h),
                            );
                            ui.painter().rect_filled(at, 0.0, pal().panel_dark);
                            tuning_section::show(
                                ui,
                                at,
                                &mut self.session,
                                &mut self.ui,
                                self.settings.retune_on_load,
                            );
                        }
                        if self.ui.show_groove_pool {
                            let at = Rect::from_min_max(
                                Pos2::new(top.left(), top.bottom() - groove_h),
                                top.max,
                            );
                            ui.painter().rect_filled(at, 0.0, pal().panel_dark);
                            groove_pool::show(ui, at, &mut self.session, &mut self.ui);
                        }
                    }
                    if self.ui.show_info {
                        let bottom = Rect::from_min_max(Pos2::new(full.left(), split), full.max);
                        let mut iu = ui.new_child(UiBuilder::new().max_rect(bottom));
                        self.info_view(&mut iu);
                    }
                });
            rects.browser = Some(panel.response.rect);
        }
    }

    /// The Detail View's panel.
    fn detail_panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, rects: &mut FocusRects) {
        if self.ui.show_detail {
            let mut detail = egui::Panel::bottom("detail")
                .resizable(true)
                .default_size(250.0)
                .size_range(130.0..=700.0)
                .frame(
                    egui::Frame::new()
                        .fill(pal().panel_dark)
                        .stroke(Stroke::new(1.0, pal().separator)),
                );
            if self.ui.clip_view.full_size && self.ui.detail == Detail::Clip {
                detail = detail.exact_size((ui.available_height() - 40.0).max(200.0));
            }
            let stacked = self.ui.clip_view.stacked
                && self.ui.selected_clip.is_some()
                && self.ui.detail != Detail::Scene;
            let panel = detail.show(ui, |ui| {
                // A frozen track's Device and Clip Views can be looked at, not changed.
                let frozen = self.detail_is_frozen(stacked);
                if frozen {
                    ui.colored_label(pal().text_dim, "Frozen: unfreeze the track to edit");
                }
                ui.add_enabled_ui(!frozen, |ui| {
                    if stacked {
                        self.stacked_detail(ui);
                    } else {
                        match self.ui.detail {
                            Detail::Clip if self.ui.selected_clip.is_some() => {
                                launch_panel::with_launch_strip(
                                    ui,
                                    &mut self.session,
                                    &mut self.ui,
                                    clip_view::show,
                                )
                            }
                            Detail::Scene => scene_view::show(ui, &mut self.session, &mut self.ui),
                            _ => devices::show(ui, &mut self.session, &mut self.ui),
                        }
                    }
                });
            });
            rects.detail = Some(panel.response.rect);
            self.detail_edge(ctx, panel.response.rect);
        }
    }

    /// What the keymap needs to resolve this frame's key presses.
    fn key_context(&self, ctx: &egui::Context) -> KeyContext {
        KeyContext {
            slot_selected: self.ui.selected_clip.and_then(ClipRef::as_slot).is_some()
                || !self.ui.sel.slots.is_empty(),
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
            dialog_open: self.ui.show_about
                || self.ui.show_export
                || self.ui.show_settings
                || self.session.render_status().is_some(),
            group_selected: self
                .selected_track_ids()
                .iter()
                .any(|t| self.session.project.track(*t).is_some_and(|t| t.is_group())),
            warp_selected: self.ui.sample.has_selection(),
            chain_selected: !self.ui.devices.mixer_selection.is_empty(),
            time_selection: self.ui.focus == FocusArea::Arrangement
                && self.ui.arr.has_selection()
                && !self.ui.arr.loop_selected,
            loop_brace: match self.ui.focus {
                FocusArea::Arrangement => self.ui.arr.loop_selected,
                FocusArea::Clip => self.ui.clip_view.marker.is_some(),
                _ => false,
            },
            track_header: self.ui.focus == FocusArea::Arrangement && self.ui.arr.header_selected,
            enter_held: ctx.input(|i| i.key_down(Key::Enter)),
            lane_focused: self.ui.arr.auto.mode && self.ui.arr.auto.focus.is_some(),
            notes_free: self.ui.focus == FocusArea::Clip
                && self.ui.clip_view.marker.is_none()
                && self
                    .ui
                    .selected_clip
                    .and_then(|r| self.session.clip_at(r))
                    .is_some_and(|c| c.is_midi()),
            hot_swap: self.ui.browser.hot_swap.is_some(),
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
            let pulses = shared.expr_pulse.load(std::sync::atomic::Ordering::Relaxed);
            let m = self.ui.meters.entry(*id).or_default();
            m.update(peaks, dt);
            m.update_expression(pulses, dt);
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

    /// The menu next to the metronome.
    fn metronome_menu(&mut self, ui: &mut egui::Ui) {
        let session = &mut self.session;
        let mut o = session.project.metronome_options;
        let sig = session.project.time_signature;
        ui.label("Count-In");
        for (bars, label) in [(0u8, "None"), (1, "1 Bar"), (2, "2 Bars"), (4, "4 Bars")] {
            ui.selectable_value(&mut o.count_in, bars, label);
        }
        ui.separator();
        ui.label("Sound");
        for sound in rlive_project::MetronomeSound::ALL {
            ui.selectable_value(&mut o.sound, sound, sound.label());
        }
        ui.separator();
        ui.label("Rhythm");
        for division in rlive_project::BeatDivision::ALL {
            let fits = division.fits(sig);
            ui.add_enabled_ui(fits, |ui| {
                ui.selectable_value(&mut o.division, division, division.label());
            });
        }
        ui.separator();
        ui.checkbox(&mut o.only_while_recording, "Enable Only While Recording");
        session.set_metronome_options(o);
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
        // The map-mode switches squeeze the right-hand group in anything but a wide window.
        let cramped = rect.width() < 1520.0;

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
        remote_map::overlay(&lu, r.rect, MapTarget::Control(RemoteControl::TapTempo));
        if info(r, "Tap Tempo — click in time to set the tempo").clicked() {
            let now = lu.input(|i| i.time);
            self.tap_tempo(now);
        }
        let mut tempo = self.session.project.tempo;
        let led = self.session.tempo_is_led();
        let r = lu
            .add_enabled_ui(!led, |ui| {
                ui.add_sized(
                    [70.0, h],
                    egui::DragValue::new(&mut tempo)
                        .range(20.0..=999.0)
                        .speed(0.25)
                        .fixed_decimals(2),
                )
            })
            .inner;
        remote_map::overlay(&lu, r.rect, MapTarget::Param(tempo_param()));
        if r.changed() {
            self.session.set_tempo(tempo);
        }
        let field = r.rect;
        info(r, "Tempo (BPM) — drag or double-click to type");
        // A deactivated field takes no clicks, so the menu is on the place it sits.
        lu.interact(
            field,
            lu.scope_id().with("tempo_menu"),
            egui::Sense::click(),
        )
        .context_menu(|ui| {
            let unfollow = egui::Button::new("Unfollow Tempo Automation");
            if ui.add_enabled(led, unfollow).clicked() {
                self.session.unfollow_tempo_automation();
                ui.close();
            }
        });
        let tick_now = self.session.engine.shared.tick.load();
        let has_markers = !self.session.project.arrangement.signatures.is_empty();
        let mut sig = self.session.project.bar_map().signature_at(tick_now);
        let mut changed = false;
        let r = lu.add_sized(
            [26.0, h],
            egui::DragValue::new(&mut sig.numerator).range(1..=99),
        );
        changed |= r.changed();
        info(r, "Time Signature Numerator — beats in a bar");
        if has_markers {
            let c = r_led_pos(&lu);
            lu.painter().circle_filled(c, 2.0, pal().accent);
        }
        lu.label(RichText::new("/").color(pal().text_dim));
        let r = egui::ComboBox::from_id_salt("sig_den")
            .width(34.0)
            .selected_text(sig.denominator.to_string())
            .show_ui(&mut lu, |ui| {
                for d in [1u8, 2, 4, 8, 16] {
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
            self.session.set_signature_at(tick_now, sig);
        }
        let metro = self.session.project.metronome;
        let r = widgets::icon_button(
            &mut lu,
            Vec2::new(26.0, h),
            metro,
            pal().accent,
            icons::metronome,
        );
        remote_map::overlay(&lu, r.rect, MapTarget::Control(RemoteControl::Metronome));
        if info(r, "Metronome — click on every beat while playing (O)").clicked() {
            self.session.set_metronome(!metro);
        }
        let r = widgets::icon_button(
            &mut lu,
            Vec2::new(14.0, h),
            false,
            pal().accent,
            icons::menu_arrow,
        );
        let r = info(r, "Metronome Settings — count-in, sound and beat division");
        egui::Popup::menu(&r).show(|ui| self.metronome_menu(ui));
        let draw = self.ui.arr.auto.draw;
        let r = widgets::state_button(&mut lu, Vec2::new(38.0, h), draw, pal().accent, "Draw");
        if info(
            r,
            "Draw Mode — draw envelopes freely in the automation lanes (B)",
        )
        .clicked()
        {
            self.ui.arr.auto.draw = !draw;
        }

        if !narrow {
            lu.add_space(6.0);
            self.follow_and_loop_fields(&mut lu, h);
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
        remote_map::overlay(&cu, r.rect, MapTarget::Control(RemoteControl::Play));
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
        remote_map::overlay(&cu, r.rect, MapTarget::Control(RemoteControl::Stop));
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
        remote_map::overlay(&cu, r.rect, MapTarget::Control(RemoteControl::Record));
        if info(
            r,
            "Arrangement Record — record onto armed tracks in the Arrangement (F9). Shift: the \
             opposite of Start Playback with Record",
        )
        .clicked()
        {
            let shift = cu.input(|i| i.modifiers.shift);
            self.toggle_recording(shift);
        }
        let overdub = self.session.project.midi_arrangement_overdub;
        let r = widgets::state_button(&mut cu, Vec2::new(22.0, h), overdub, pal().accent, "OD");
        remote_map::overlay(&cu, r.rect, MapTarget::Control(RemoteControl::MidiOverdub));
        if info(
            r,
            "MIDI Arrangement Overdub — recorded MIDI mixes with what is already on the track",
        )
        .clicked()
        {
            self.session.set_midi_arrangement_overdub(!overdub);
        }
        let a = self.session.project.arrangement.clone();
        let r = widgets::state_button(&mut cu, Vec2::new(22.0, h), a.punch_in, pal().accent, "In");
        remote_map::overlay(&cu, r.rect, MapTarget::Control(RemoteControl::PunchIn));
        if info(
            r,
            "Punch-In — keep recording out of everything before the loop start",
        )
        .clicked()
        {
            self.session.set_punch(!a.punch_in, a.punch_out);
        }
        let r = widgets::state_button(
            &mut cu,
            Vec2::new(24.0, h),
            a.punch_out,
            pal().accent,
            "Out",
        );
        remote_map::overlay(&cu, r.rect, MapTarget::Control(RemoteControl::PunchOut));
        if info(
            r,
            "Punch-Out — keep recording out of everything after the loop end",
        )
        .clicked()
        {
            self.session.set_punch(a.punch_in, !a.punch_out);
        }
        cu.add_space(4.0);
        let session_rec = self.session.any_slot_recording()
            || self.session.any_overdubbing()
            || self.session.session_automation_record();
        let r = widgets::icon_button(
            &mut cu,
            Vec2::new(26.0, h),
            session_rec,
            pal().arm_red,
            icons::session_record,
        );
        remote_map::overlay(
            &cu,
            r.rect,
            MapTarget::Control(RemoteControl::SessionRecord),
        );
        if info(
            r,
            "Session Record — record the selected scene on armed tracks; again to play the \
             new clips (Cmd+Shift+F9)",
        )
        .clicked()
        {
            self.session_record();
        }
        let can_capture = self.session.can_capture();
        let r = widgets::icon_button(
            &mut cu,
            Vec2::new(26.0, h),
            false,
            if can_capture {
                pal().accent
            } else {
                pal().text_dim
            },
            icons::capture_midi,
        );
        remote_map::overlay(&cu, r.rect, MapTarget::Control(RemoteControl::CaptureMidi));
        if info(
            r,
            "Capture MIDI — make a clip of what you just played on armed or monitored tracks",
        )
        .clicked()
            && can_capture
        {
            self.capture_midi();
        }
        let armed = self.session.automation_arm();
        let r = widgets::icon_button(
            &mut cu,
            Vec2::new(26.0, h),
            armed,
            pal().arm_red,
            icons::automation_arm,
        );
        remote_map::overlay(
            &cu,
            r.rect,
            MapTarget::Control(RemoteControl::AutomationArm),
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
        remote_map::overlay(
            &cu,
            r.rect,
            MapTarget::Control(RemoteControl::ReEnableAutomation),
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
        let sig = self.session.project.time_signature;
        let counting = self.session.engine.shared.count_in_left.load();
        let pos = if counting > 0.0 {
            count_in_label(counting, sig)
        } else {
            sig.format_position(tick).replace('.', ".  ")
        };
        let (pr, r) = cu.allocate_exact_size(Vec2::new(96.0, h), Sense::hover());
        cu.painter().rect_filled(pr, 1.0, pal().field_bg);
        cu.painter().text(
            pr.center(),
            Align2::CENTER_CENTER,
            pos,
            egui::FontId::monospace(13.0),
            if counting > 0.0 {
                pal().accent
            } else if playing {
                pal().play_green
            } else {
                pal().text
            },
        );
        info(r, "Song Position — the playhead, as bar . beat . sixteenth");
        cu.add_space(6.0);
        let r = widgets::state_button(
            &mut cu,
            Vec2::new(40.0, h),
            a.loop_enabled,
            pal().accent,
            "Loop",
        );
        remote_map::overlay(&cu, r.rect, MapTarget::Control(RemoteControl::Loop));
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
        remote_map::overlay(
            &cu,
            r.rect,
            MapTarget::Control(RemoteControl::BackToArrangement),
        );
        if info(
            r,
            "Back to Arrangement — Session clips stop, the timeline plays again",
        )
        .clicked()
        {
            self.session.back_to_arrangement();
        }
        let follow_used = self.session.has_follow_actions();
        let follow_on = self.session.project.follow_actions_on;
        cu.add_enabled_ui(follow_used, |ui| {
            let r = widgets::state_button(
                ui,
                Vec2::new(22.0, h),
                follow_on && follow_used,
                pal().accent,
                "F",
            );
            remote_map::overlay(ui, r.rect, MapTarget::Control(RemoteControl::FollowActions));
            if info(
                r,
                "Enable Follow Actions Globally — lets you edit running clips",
            )
            .clicked()
            {
                self.session.set_follow_actions_on(!follow_on);
            }
        });

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
        ru.spacing_mut().item_spacing.x = if cramped { 3.0 } else { 6.0 };
        self.map_switches(&mut ru, h);
        // Global Amount appears only once a clip is using a groove.
        if self.session.any_groove_in_use() {
            let mut amount = self.session.project.groove_pool.amount * 100.0;
            let r = ru.add_sized(
                [56.0, h],
                egui::DragValue::new(&mut amount)
                    .range(0.0..=130.0)
                    .speed(0.5)
                    .suffix(" %")
                    .max_decimals(0),
            );
            if r.changed() {
                self.session.set_groove_amount(amount / 100.0);
            }
            info(
                r,
                "Global Groove Amount — scales Timing, Random and Velocity, up to 130%",
            );
        }
        // CPU meter
        let load = self.session.engine.shared.cpu_load.load().clamp(0.0, 1.0);
        let (cr, r) = ru.allocate_exact_size(
            Vec2::new(if cramped { 34.0 } else { 70.0 }, h),
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
            if cramped {
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
        self.midi_lights(&mut ru, h);
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
        let hover_clip = r.dnd_hover_payload::<session_view::ClipDrag>().is_some();
        if info(r, "Arrangement View — the timeline (Tab)").clicked() || hover_clip {
            self.ui.main_view = MainView::Arrangement;
        }
        let r = widgets::state_button(
            &mut ru,
            Vec2::new(54.0, h),
            !arr,
            shade(0x8a8a8a),
            "Session",
        );
        let released_here = r.contains_pointer()
            && self.ui.arr.dragging_clips()
            && ru.input(|i| i.pointer.any_released());
        if released_here {
            self.send_selection_to_session();
        }
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

    /// Follow, and the loop's position and length.
    fn follow_and_loop_fields(&mut self, ui: &mut egui::Ui, h: f32) {
        let follow = self.ui.arr.follow.on;
        let r = widgets::state_button(ui, Vec2::new(44.0, h), follow, pal().accent, "Follow");
        remote_map::overlay(ui, r.rect, MapTarget::Control(RemoteControl::Follow));
        if info(r, "Follow — the Arrangement scrolls with the song position").clicked() {
            self.ui.arr.follow.on = !follow;
        }
        let a = self.session.project.arrangement.clone();
        let sig = self.session.project.time_signature;
        let mut start = a.loop_start as f64;
        let r = ui.add_sized(
            [58.0, h],
            egui::DragValue::new(&mut start)
                .speed(sig.ticks_per_beat() as f64 * 0.25)
                .custom_formatter(|v, _| sig.format_position(v))
                .custom_parser(|text| parse_position(text, sig)),
        );
        if r.changed() {
            let new = start.max(0.0) as u64;
            let len = a.loop_end - a.loop_start;
            self.session.set_loop(a.loop_enabled, new, new + len);
        }
        info(r, "Loop Start — bar.beat.sixteenth, type or drag");
        let mut length = (a.loop_end - a.loop_start) as f64;
        let r = ui.add_sized(
            [58.0, h],
            egui::DragValue::new(&mut length)
                .speed(sig.ticks_per_beat() as f64 * 0.25)
                .range(1.0..=f64::MAX)
                .custom_formatter(|v, _| format_length(v, sig))
                .custom_parser(|text| parse_length(text, sig)),
        );
        if r.changed() {
            self.session.set_loop(
                a.loop_enabled,
                a.loop_start,
                a.loop_start + length.max(1.0) as u64,
            );
        }
        info(r, "Loop Length — bars.beats.sixteenths, type or drag");
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
        let dropped = self.session.take_dropped();
        let job = self.session.clip_job_progress();
        let msg = if let Some((what, done)) = job {
            (format!("{what}… {:.0}%", done * 100.0), pal().text)
        } else if !info_text.is_empty() {
            (info_text, pal().text)
        } else if dropped > 0 {
            (
                format!("Recording: {dropped} frames lost to a slow disk"),
                pal().arm_red,
            )
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
                let clip_on = self.ui.detail == Detail::Clip || self.ui.clip_view.stacked;
                let r = widgets::state_button(
                    u,
                    Vec2::new(40.0, 16.0),
                    clip_on,
                    shade(0x8a8a8a),
                    "Clip",
                );
                if info(
                    r,
                    "Clip View — edit the selected clip (Shift+Tab toggles); Alt-click to show it \
                     with the Device View",
                )
                .clicked()
                {
                    if u.input(|i| i.modifiers.alt) {
                        self.ui.clip_view.stacked = !self.ui.clip_view.stacked;
                    } else {
                        self.ui.clip_view.stacked = false;
                        self.ui.detail = Detail::Clip;
                    }
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
        self.confirm_default_overwrite(ctx);
        self.settings_window(ctx);
        if let Some(t) = self.ui.devices.ui.mpe_settings.take() {
            self.ui.mpe_dialog = Some(t);
        }
        tuning_section::retune_dialog(ctx, &mut self.session, &mut self.ui);
        midi_settings::mpe_dialog(ctx, &mut self.session, &mut self.ui);
        midi_settings::layout_dialog(ctx, &mut self.session, &mut self.ui);

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
        self.session.clip_defaults.create_fades = self.settings.create_fades;
        self.session.set_clip_update_rate(
            self.settings
                .clip
                .update_rate
                .ticks(self.session.project.time_signature),
        );
        self.session.sample_editor = self.settings.sample_editor.clone();
        self.ui.clip_view.permanent_scrub = self.settings.permanent_scrub_areas;
        self.ui.piano.pitch_lock = self.settings.pitch_lock;
        self.ui.follow_scrolls =
            self.settings.follow_behavior == crate::settings::FollowBehavior::Scroll;
        self.session.record = self.settings.record.clone();
        self.session.launch_defaults = self.settings.launch.defaults();
        self.ui.sel.prefs = self.settings.launch;
        self.ui.midi_auto_reset = self.settings.midi_envelope_auto_reset;
        if std::mem::take(&mut self.ui.toggle_midi_auto_reset) {
            self.pending.push(commands::MenuCommand::Options(
                commands::options::Options::ToggleMidiEnvelopeAutoReset,
            ));
        }
        self.session.set_edge_fades(self.settings.create_fades);
        self.ui.frame_time = ctx.input(|i| i.time);
        self.session.remote.selected_device = self.selected_device();
        self.session.idle();
        for sysex in self.routing.system.take_sysex() {
            self.session.surface_sysex(&sysex);
        }
        for (target, gesture) in self.session.take_remote_actions() {
            self.remote_action(target, gesture);
        }
        self.ui.browser.idle(&self.session);
        self.ui.retune_on_load = self.settings.retune_on_load;
        browser::poll_requests(&self.session, &mut self.ui);
        if let Some(path) = self.ui.browser.open_set.take() {
            self.open_path(&path);
        }
        self.end_hot_swap_on_navigation();
        self.poll_scan();
        if std::mem::take(&mut self.ui.request_scan) && self.ui.scan_rx.is_none() {
            self.start_plugin_scan();
        }
        if let Some(ChainRef::Track(t)) = self.ui.selected {
            *self.routing.selected.lock().unwrap() = Some(t);
        }
        self.pending.append(&mut self.ui.piano.queued);
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
        if !ctx.text_edit_focused() {
            let mut keep = Vec::with_capacity(raw.events.len());
            for e in std::mem::take(&mut raw.events) {
                let taken = match &e {
                    egui::Event::Key {
                        key,
                        pressed,
                        repeat: false,
                        modifiers,
                        ..
                    } => remote_map::key_event(self, *key, *modifiers, *pressed),
                    _ => false,
                };
                if !taken {
                    keep.push(e);
                }
            }
            raw.events = keep;
        }
        // Taking shortcuts here keeps Tab from moving egui's keyboard focus, which would silence
        // Space and the computer keyboard.
        let cx = self.key_context(ctx);
        self.ui.keys.process(raw, &cx, &mut self.pending);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        remote_map::begin_frame(&ctx, &self.session);
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
        if self.session.map_mode().is_some() {
            egui::Panel::bottom("remote_strip")
                .exact_size(24.0)
                .frame(
                    egui::Frame::new()
                        .fill(pal().panel_dark)
                        .inner_margin(egui::Margin::symmetric(6, 2)),
                )
                .show(ui, |ui| {
                    egui::ScrollArea::horizontal()
                        .id_salt("remote_strip_scroll")
                        .show(ui, |ui| {
                            remote_map::strip(ui, &self.session, &self.ui);
                        });
                });
        }
        if self.settings.full_height_browser {
            self.browser_panel(ui, &mut rects);
            self.detail_panel(ui, &ctx, &mut rects);
        } else {
            self.detail_panel(ui, &ctx, &mut rects);
            self.browser_panel(ui, &mut rects);
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
        let busy = pointer_down
            || self.ui.piano.chop.is_some()
            || ctx.text_edit_focused()
            || self.session.is_recording()
            || self.session.any_overdubbing();
        self.session.commit_edits(busy);
        remote_map::end_frame(&ctx, &mut self.session);
        self.ui.devices.end_frame();
        // A chop is adjusted for as long as Cmd stays down.
        if self.ui.piano.chop.is_some() && !ctx.input(|i| i.modifiers.command) {
            self.ui.piano.chop = None;
        }
        #[cfg(target_os = "macos")]
        if let Some(menu) = &self.mac_menu {
            menu.publish(self.menu_state(&ctx));
        }
        self.dialogs(&ctx);
        self.undo_history_window(&ctx);
        self.render_progress(&ctx);
        self.run_pending_freezes();
        automation::show_popups(&ctx, &mut self.session, &mut self.ui);
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

#[cfg(test)]
mod count_in_tests {
    use super::count_in_label;
    use rlive_core::{TimeSignature, PPQ};

    #[test]
    fn the_count_in_reads_from_negative_bars_up_to_the_first() {
        let bar = 4 * PPQ;
        let sig = TimeSignature::default();
        assert_eq!(count_in_label(2.0 * bar as f64, sig), "-2.  1.  1");
        assert_eq!(count_in_label(bar as f64, sig), "-1.  1.  1");
        assert_eq!(count_in_label(bar as f64 * 1.5, sig), "-2.  3.  1");
        assert_eq!(count_in_label(120.0, sig), "-1.  4.  4", "almost there");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loop_fields_read_back_what_they_show() {
        let sig = TimeSignature::default();
        assert_eq!(parse_position("3.2.2", sig), Some(8880.0));
        assert_eq!(parse_position("1", sig), Some(0.0));
        assert_eq!(parse_position("x", sig), None);
        assert_eq!(format_length(8880.0, sig), "2.1.1");
        assert_eq!(parse_length("2.1.1", sig), Some(8880.0));
        assert_eq!(parse_length("0.1", sig), Some(960.0));
    }
}
