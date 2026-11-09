//! Headless proof of the second window and keyboard navigation (C17, C19): whole frames of the
//! app, driven by real key events through the input hook.

use super::commands::nav::Nav;
use super::commands::track::Track;
use super::commands::view::View;
use super::commands::MenuCommand as M;
use super::keymap::FocusArea;
use super::nav::{self, Group};
use super::{MainView, RliveApp, UiState};
use crate::audio::AudioOutput;
use crate::midi_input::{self, MidiInput, MidiRouting};
use crate::session::{ChainRef, ClipRef, Session};
use crate::settings::Settings;
use eframe::App;
use egui::{Event, Key, Modifiers, Pos2, RawInput, Rect, Vec2};
use rlive_core::TrackId;
use rlive_engine::{create_engine, Engine};
use rlive_plugin_host::scanner::PluginCache;
use rlive_project::{Project, TrackKind};
use std::sync::{Arc, Mutex};

struct Rig {
    _engine: Engine,
    app: RliveApp,
    ctx: egui::Context,
    time: f64,
}

fn rig() -> Rig {
    let (engine, mut handle) = create_engine(48_000.0, 512);
    let sender = handle.take_midi_sender();
    let mut session = Session::new(handle, Project::new_default(), PluginCache::default());
    session.add_track(TrackKind::Midi, None);
    session.commit_edits(false);
    let settings = Settings::default();
    let ui = UiState::initial(&session, &settings, String::new());
    let routing = MidiRouting {
        sender: Arc::new(Mutex::new(sender)),
        targets: session.midi_targets.clone(),
        selected: Arc::new(Mutex::new(None)),
        activity: Default::default(),
        system: Default::default(),
        mpe: Default::default(),
    };
    let app = RliveApp {
        session,
        channel_masks: Default::default(),
        sync_out: Default::default(),
        pending_quit: false,
        audio: AudioOutput::stopped(),
        midi: MidiInput::none(),
        routing,
        track_outs: midi_input::TrackOutputs::default(),
        track_midi: midi_input::TrackMidiFeed::default(),
        settings,
        ui,
        last_frame: std::time::Instant::now(),
        pending: Vec::new(),
        #[cfg(target_os = "macos")]
        mac_menu: None,
    };
    let mut r = Rig {
        _engine: engine,
        app,
        ctx: egui::Context::default(),
        time: 0.0,
    };
    r.frame(Vec::new());
    r.frame(Vec::new());
    r
}

impl Rig {
    fn frame(&mut self, events: Vec<Event>) {
        let mut raw = RawInput {
            events,
            time: Some(self.time),
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 860.0))),
            ..RawInput::default()
        };
        self.time += 0.02;
        self.app.raw_input_hook(&self.ctx, &mut raw);
        let app = &mut self.app;
        let mut out = self.ctx.run_ui(raw, |ui| {
            app.tick(ui.ctx());
            app.draw_root(ui);
        });
        out.textures_delta.clear();
    }

    fn key(&mut self, key: Key) {
        self.key_mod(key, Modifiers::NONE);
    }

    fn key_mod(&mut self, key: Key, modifiers: Modifiers) {
        let event = |pressed| Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat: false,
            modifiers,
        };
        self.frame(vec![event(true), event(false)]);
    }

    fn run(&mut self, c: M) {
        let ctx = self.ctx.clone();
        self.app.dispatch(&ctx, c);
        self.frame(Vec::new());
    }

    fn track(&self, i: usize) -> TrackId {
        self.app.session.project.tracks[i].id
    }

    fn track_ids(&self) -> Vec<TrackId> {
        self.app
            .session
            .project
            .tracks
            .iter()
            .map(|t| t.id)
            .collect()
    }

    fn volume(&self, id: TrackId) -> f32 {
        self.app.session.project.track(id).unwrap().volume_db
    }

    /// Runs a frame of nothing so `commit_edits` can see the keyboard edit as finished.
    fn settle(&mut self) {
        self.frame(Vec::new());
    }
}

// ---------------------------------------------------------------- C17

#[test]
fn the_second_window_is_saved_with_the_set() {
    let mut r = rig();
    assert!(!r.app.session.project.window_layout.second_window);
    r.run(M::View(View::ToggleSecondWindow));
    let l = &r.app.session.project.window_layout;
    assert!(l.second_window);
    assert!(!l.detail_in_second);
    r.run(M::View(View::SwitchDetail));
    assert!(r.app.session.project.window_layout.detail_in_second, "F12");
    r.run(M::View(View::ToggleSends));
    r.run(M::View(View::ToggleReturns));
    let dir = std::env::temp_dir().join(format!("rlive_c17_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("two windows.rlive");
    r.app.session.project.save(&path).unwrap();
    let loaded = Project::load(&path).unwrap();
    std::fs::remove_dir_all(&dir).ok();
    let l = loaded.window_layout;
    assert!(l.second_window && l.detail_in_second);
    assert!(!l.show_sends && !l.show_returns);
}

#[test]
fn a_set_saved_before_the_second_window_opens_with_defaults() {
    let p = Project::new_default();
    assert_eq!(p.window_layout, rlive_project::WindowLayout::default());
    assert!(p.window_layout.show_sends);
    assert!(p.window_layout.show_returns);
}

#[test]
fn closing_the_second_window_brings_the_detail_view_back() {
    let mut r = rig();
    r.run(M::View(View::ToggleSecondWindow));
    r.run(M::View(View::SwitchDetail));
    r.run(M::View(View::ToggleSecondWindow));
    let l = &r.app.session.project.window_layout;
    assert!(!l.second_window && !l.detail_in_second);
    assert!(!r.app.detail_in_second());
}

#[test]
fn f12_flips_device_and_clip_with_one_window_and_moves_the_view_with_two() {
    let mut r = rig();
    let slot = ClipRef::slot(r.track(0), 0);
    r.app.ui.selected_clip = Some(slot);
    r.run(M::View(View::SwitchDetail));
    assert!(matches!(r.app.ui.detail, super::Detail::Clip));
    assert!(!r.app.session.project.window_layout.detail_in_second);
}

#[test]
fn tab_swaps_the_two_windows_views_and_each_view_keeps_its_focus_area() {
    let mut r = rig();
    r.run(M::View(View::ToggleSecondWindow));
    assert_eq!(
        super::second_window::other(MainView::Session),
        MainView::Arrangement
    );
    r.app.ui.focus = FocusArea::Arrangement;
    assert_eq!(r.app.ui.active_view(), MainView::Arrangement);
    r.app.ui.focus = FocusArea::Device;
    assert_eq!(r.app.ui.active_view(), MainView::Session);
}

#[test]
fn sends_and_returns_hide_from_the_view_menu_commands() {
    let mut r = rig();
    r.run(M::View(View::ToggleSends));
    assert!(!r.app.session.project.window_layout.show_sends);
    r.run(M::View(View::ToggleReturns));
    assert!(!r.app.session.project.window_layout.show_returns);
    r.frame(Vec::new());
    r.run(M::View(View::ToggleSends));
    assert!(r.app.session.project.window_layout.show_sends);
}

#[test]
fn clicking_a_group_slot_selects_every_clip_it_refers_to() {
    let mut r = rig();
    let (a, b) = (r.track(0), r.track(1));
    let group = r.app.session.group_tracks(&[a, b]).unwrap();
    for t in [a, b] {
        let c = r.app.session.new_midi_clip([1, 2, 3], Default::default());
        r.app.session.set_clip(t, 0, Some(c));
    }
    super::session_view::select_group_slot(&r.app.session, &mut r.app.ui, group, 0);
    assert_eq!(r.app.ui.group_slot, Some((group, 0)));
    assert_eq!(r.app.ui.sel.slots, vec![(a, 0), (b, 0)]);
    // A scene with no clips in the group has nothing to select.
    super::session_view::select_group_slot(&r.app.session, &mut r.app.ui, group, 5);
    assert!(r.app.ui.sel.slots.is_empty());
}

// ---------------------------------------------------------------- Session arrows

#[test]
fn the_arrow_keys_walk_the_session_grid() {
    let mut r = rig();
    let (a, b) = (r.track(0), r.track(1));
    r.app.ui.focus = FocusArea::Session;
    r.app.ui.selected_clip = Some(ClipRef::slot(a, 1));
    r.app.ui.sel.slots = vec![(a, 1)];
    r.frame(Vec::new());
    r.key(Key::ArrowDown);
    assert_eq!(r.app.ui.selected_clip, Some(ClipRef::slot(a, 2)));
    r.key(Key::ArrowUp);
    assert_eq!(r.app.ui.selected_clip, Some(ClipRef::slot(a, 1)));
    r.key(Key::ArrowRight);
    assert_eq!(r.app.ui.selected_clip, Some(ClipRef::slot(b, 1)));
    r.key(Key::ArrowLeft);
    assert_eq!(r.app.ui.selected_clip, Some(ClipRef::slot(a, 1)));
    // Up off the top reaches the title bar, and Down comes back.
    r.key(Key::ArrowUp);
    r.key(Key::ArrowUp);
    assert_eq!(r.app.ui.title_focus, Some(a));
    assert_eq!(r.app.ui.selected_clip, None);
    r.key(Key::ArrowDown);
    assert_eq!(r.app.ui.selected_clip, Some(ClipRef::slot(a, 0)));
    assert_eq!(r.app.ui.title_focus, None);
}

#[test]
fn the_title_bars_step_left_and_right() {
    let mut r = rig();
    let (a, b) = (r.track(0), r.track(1));
    r.app.ui.focus = FocusArea::Session;
    r.app.ui.title_focus = Some(a);
    r.frame(Vec::new());
    r.key(Key::ArrowRight);
    assert_eq!(r.app.ui.title_focus, Some(b));
    assert_eq!(r.app.ui.selected, Some(ChainRef::Track(b)));
    r.key(Key::ArrowLeft);
    assert_eq!(r.app.ui.title_focus, Some(a));
}

#[test]
fn page_keys_move_eight_scenes_and_home_end_cross_the_grid() {
    let mut r = rig();
    let a = r.track(0);
    let n_tracks = r.app.session.project.visible_tracks().len();
    let last = r.track(n_tracks - 1);
    for _ in 0..12 {
        r.app.session.add_scene();
    }
    r.app.ui.focus = FocusArea::Session;
    r.app.ui.selected_clip = Some(ClipRef::slot(a, 0));
    r.app.ui.sel.slots = vec![(a, 0)];
    r.frame(Vec::new());
    r.key(Key::PageDown);
    assert_eq!(r.app.ui.selected_clip, Some(ClipRef::slot(a, 8)));
    r.key(Key::PageUp);
    assert_eq!(r.app.ui.selected_clip, Some(ClipRef::slot(a, 0)));
    r.key(Key::End);
    assert_eq!(r.app.ui.selected_clip, Some(ClipRef::slot(last, 0)));
    // End again, from the last track, is the scene itself.
    r.key(Key::End);
    assert_eq!(r.app.ui.sel.scenes, vec![0]);
    r.key(Key::Home);
    assert_eq!(r.app.ui.selected_clip, Some(ClipRef::slot(a, 0)));
}

#[test]
fn right_of_the_last_track_is_the_scene_and_then_its_fields() {
    use super::session_select::SceneField;
    let mut r = rig();
    let n = r.app.session.project.visible_tracks().len();
    let last = r.track(n - 1);
    r.app.ui.focus = FocusArea::Session;
    r.app.ui.selected_clip = Some(ClipRef::slot(last, 0));
    r.app.ui.sel.slots = vec![(last, 0)];
    r.frame(Vec::new());
    r.key(Key::ArrowRight);
    assert_eq!(r.app.ui.sel.scenes, vec![0]);
    r.key(Key::ArrowRight);
    assert_eq!(r.app.ui.sel.scene_field, Some((0, SceneField::Tempo)));
    r.key(Key::ArrowRight);
    assert_eq!(r.app.ui.sel.scene_field, Some((0, SceneField::Signature)));
    r.key(Key::ArrowLeft);
    assert_eq!(r.app.ui.sel.scene_field, Some((0, SceneField::Tempo)));
}

#[test]
fn escape_goes_from_a_slot_to_the_highlighted_title() {
    let mut r = rig();
    let a = r.track(0);
    r.app.ui.focus = FocusArea::Session;
    r.app.ui.selected_clip = Some(ClipRef::slot(a, 2));
    r.app.ui.sel.slots = vec![(a, 2)];
    r.frame(Vec::new());
    r.key(Key::Escape);
    assert_eq!(r.app.ui.title_focus, Some(a));
    assert_eq!(r.app.ui.selected_clip, None);
}

// ---------------------------------------------------------------- rename across tracks

#[test]
fn tab_while_renaming_moves_to_the_next_track_and_commits_the_name() {
    let mut r = rig();
    let (a, b) = (r.track(0), r.track(1));
    r.app.ui.focus = FocusArea::Session;
    r.app.ui.selected = Some(ChainRef::Track(a));
    r.app.ui.renaming = Some((a, "Drums".to_string()));
    r.frame(Vec::new());
    r.key(Key::Tab);
    assert_eq!(r.app.session.project.track(a).unwrap().name, "Drums");
    let (now, _) = r.app.ui.renaming.clone().expect("next track renaming");
    assert_eq!(now, b);
    assert_eq!(
        r.app.ui.main_view,
        MainView::Session,
        "Tab didn't flip the view"
    );
}

// ---------------------------------------------------------------- moving tracks

#[test]
fn nonadjacent_tracks_keep_their_gaps_and_undo_in_one_step() {
    let mut r = rig();
    let extra = (0..4)
        .map(|_| r.app.session.add_track(TrackKind::Midi, None))
        .collect::<Vec<_>>();
    r.app.session.commit_edits(false);
    let order = r.track_ids();
    let (a, b) = (extra[0], extra[2]);
    r.app.ui.selected = Some(ChainRef::Track(a));
    r.app.ui.selected_tracks = vec![a, b];
    r.run(M::Track(Track::MoveTracksRight));
    let moved = r.track_ids();
    assert_ne!(moved, order);
    let at = |list: &[TrackId], t| list.iter().position(|x| *x == t).unwrap();
    assert_eq!(at(&moved, a), at(&order, a) + 1);
    assert_eq!(at(&moved, b), at(&order, b) + 1);
    r.settle();
    r.app.session.undo();
    r.frame(Vec::new());
    assert_eq!(r.track_ids(), order, "one undo puts both back");
}

#[test]
fn moving_past_the_edge_moves_nothing() {
    let mut r = rig();
    let first = r.track(0);
    let last = *r.track_ids().last().unwrap();
    r.app.ui.selected = Some(ChainRef::Track(first));
    r.app.ui.selected_tracks = vec![first, last];
    let order = r.track_ids();
    r.run(M::Track(Track::MoveTracksUp));
    assert_eq!(r.track_ids(), order);
}

#[test]
fn a_group_moves_with_its_contents() {
    let mut r = rig();
    let (a, b) = (r.track(0), r.track(1));
    r.app.session.add_track(TrackKind::Midi, None);
    r.app.session.commit_edits(false);
    let group = r.app.session.group_tracks(&[a, b]).unwrap();
    r.app.session.commit_edits(false);
    let before = r.track_ids();
    let at = |list: &[TrackId], t| list.iter().position(|x| *x == t).unwrap();
    assert!(at(&before, group) < at(&before, a));
    // The track just after the block jumps the whole group, members and all.
    let next = before[at(&before, b) + 1];
    r.app.ui.selected = Some(ChainRef::Track(next));
    r.app.ui.selected_tracks = vec![next];
    r.run(M::Track(Track::MoveTracksUp));
    let after = r.track_ids();
    assert_eq!(at(&after, next) + 1, at(&after, group));
    assert_eq!(at(&after, group) + 1, at(&after, a));
}

// ---------------------------------------------------------------- value entry

/// Runs frames until the first track's Volume has the keyboard.
fn focus_volume(r: &mut Rig) -> TrackId {
    let t = r.track(0);
    r.app.ui.main_view = MainView::Session;
    r.app.ui.focus = FocusArea::Session;
    r.frame(Vec::new());
    assert!(nav::focus_role(&r.ctx, Group::Track(t), "volume"));
    t
}

#[test]
fn arrow_keys_step_the_focused_fader_and_undo() {
    let mut r = rig();
    let t = focus_volume(&mut r);
    let before = r.volume(t);
    r.key(Key::ArrowUp);
    assert_eq!(r.volume(t), before + 0.5, "Up raises it half a dB");
    r.key_mod(Key::ArrowDown, Modifiers::SHIFT);
    assert!(
        (r.volume(t) - (before + 0.45)).abs() < 1e-5,
        "Shift is fine"
    );
    r.settle();
    r.app.session.undo();
    r.settle();
    assert!((r.volume(t) - (before + 0.5)).abs() < 1e-5);
    r.app.session.undo();
    r.settle();
    assert!((r.volume(t) - before).abs() < 1e-5);
}

#[test]
fn typed_values_apply_on_enter_and_not_on_escape() {
    let mut r = rig();
    let t = focus_volume(&mut r);
    let before = r.volume(t);
    r.key(Key::Minus);
    r.key(Key::Num6);
    r.key(Key::Period);
    r.key(Key::Num5);
    assert_eq!(nav::entry(&r.ctx).as_deref(), Some("-6.5"));
    assert_eq!(r.volume(t), before, "nothing changes until Enter");
    r.key(Key::Enter);
    assert_eq!(r.volume(t), -6.5);
    r.settle();

    r.key(Key::Num3);
    r.key(Key::Escape);
    assert_eq!(nav::entry(&r.ctx), None);
    assert_eq!(r.volume(t), -6.5, "Esc drops what was typed");
}

#[test]
fn delete_returns_a_focused_control_to_its_default() {
    let mut r = rig();
    let t = focus_volume(&mut r);
    r.key(Key::ArrowUp);
    r.settle();
    assert_ne!(r.volume(t), 0.0);
    r.key(Key::Delete);
    assert_eq!(r.volume(t), 0.0);
}

#[test]
fn escape_on_a_focused_control_with_nothing_typed_goes_up_to_the_title() {
    let mut r = rig();
    let t = focus_volume(&mut r);
    r.key(Key::Escape);
    assert_eq!(r.app.ui.title_focus, Some(t));
    assert!(nav::focused(&r.ctx).is_none());
}

// ---------------------------------------------------------------- Tab focus mode

#[test]
fn tab_mode_takes_tab_from_the_view_switch_and_walks_the_controls() {
    let mut r = rig();
    r.app.ui.focus = FocusArea::Session;
    r.app.settings.tab_moves_focus = true;
    r.frame(Vec::new());
    r.key(Key::Tab);
    assert_eq!(
        r.app.ui.main_view,
        MainView::Session,
        "Tab no longer flips views"
    );
    let first = nav::focused(&r.ctx).expect("a control has the keyboard");
    r.key(Key::Tab);
    let second = nav::focused(&r.ctx).expect("the next control");
    assert_ne!(first.id, second.id);
    r.key_mod(Key::Tab, Modifiers::SHIFT);
    assert_eq!(nav::focused(&r.ctx).unwrap().id, first.id);
}

#[test]
fn with_tab_mode_off_tab_still_flips_session_and_arrangement() {
    let mut r = rig();
    r.app.ui.focus = FocusArea::Session;
    r.frame(Vec::new());
    assert_eq!(r.app.ui.main_view, MainView::Session);
    r.key(Key::Tab);
    assert_eq!(r.app.ui.main_view, MainView::Arrangement);
}

#[test]
fn enter_presses_a_focused_button() {
    let mut r = rig();
    let t = r.track(0);
    r.app.ui.main_view = MainView::Session;
    r.app.ui.focus = FocusArea::Session;
    r.frame(Vec::new());
    assert!(nav::focus_role(&r.ctx, Group::Track(t), "solo"));
    r.key(Key::Enter);
    assert!(r.app.session.project.track(t).unwrap().solo);
    r.key(Key::Enter);
    assert!(!r.app.session.project.track(t).unwrap().solo);
}

#[test]
fn focus_mixer_reaches_the_tracks_mixer_controls() {
    let mut r = rig();
    let t = r.track(0);
    r.app.ui.main_view = MainView::Session;
    r.app.ui.focus = FocusArea::Arrangement;
    r.app.ui.selected = Some(ChainRef::Track(t));
    r.frame(Vec::new());
    r.run(M::Nav(Nav::FocusMixer));
    assert_eq!(nav::focused(&r.ctx).unwrap().group, Group::Track(t));
    assert_eq!(r.app.ui.focus, FocusArea::Session);
}

#[test]
fn delete_from_the_title_bar_removes_the_track_and_undoes() {
    let mut r = rig();
    let t = r.app.session.add_track(TrackKind::Midi, None);
    r.app.session.commit_edits(false);
    let order = r.track_ids();
    r.app.ui.focus = FocusArea::Session;
    r.app.ui.selected = Some(ChainRef::Track(t));
    r.app.ui.selected_tracks = vec![t];
    r.app.ui.title_focus = Some(t);
    r.frame(Vec::new());
    r.key(Key::Delete);
    assert_eq!(r.app.session.project.track(t).map(|_| ()), None);
    r.settle();
    r.app.session.undo();
    r.settle();
    assert_eq!(r.track_ids(), order);
}

#[test]
fn with_move_clips_off_the_arrows_collapse_the_time_selection() {
    use super::automation::LaneKey;
    use super::commands::arrange::Arrange;
    let mut r = rig();
    let t = r.track(0);
    let lane = LaneKey {
        chain: ChainRef::Track(t),
        slot: None,
        take: None,
    };
    r.app.ui.focus = FocusArea::Arrangement;
    r.app.ui.main_view = MainView::Arrangement;
    r.app.settings.move_clips_with_arrows = false;
    r.app.ui.arr.time = Some(super::arrangement::TimeSel {
        start: 960.0,
        end: 1920.0,
        lanes: vec![lane],
    });
    r.frame(Vec::new());
    r.run(M::Arrange(Arrange::NudgeRight));
    let t = r.app.ui.arr.time.clone().unwrap();
    assert_eq!((t.start, t.end), (1920.0, 1920.0));

    r.app.ui.arr.time = Some(super::arrangement::TimeSel {
        start: 960.0,
        end: 1920.0,
        lanes: vec![lane],
    });
    r.run(M::Arrange(Arrange::NudgeLeft));
    let t = r.app.ui.arr.time.clone().unwrap();
    assert_eq!((t.start, t.end), (960.0, 960.0));
}

#[test]
fn the_locators_tab_in_order_and_delete_with_the_selection() {
    let mut r = rig();
    r.app.session.set_locator(0);
    r.app.session.set_locator(960);
    r.app.ui.focus = FocusArea::Arrangement;
    r.app.settings.tab_moves_focus = true;
    r.frame(Vec::new());
    r.key(Key::Tab);
    assert_eq!(r.app.ui.arr.selected_locator, Some(0));
    r.key(Key::Tab);
    assert_eq!(r.app.ui.arr.selected_locator, Some(1));
    r.key(Key::Delete);
    assert_eq!(r.app.session.project.arrangement.locators.len(), 1);
}

// ---------------------------------------------------------------- names and the Delete key

fn start_rename(r: &mut Rig, view: MainView) -> TrackId {
    let t = r.track(0);
    r.app.ui.main_view = view;
    r.app.ui.renaming = Some((t, "Bass".into()));
    r.frame(Vec::new());
    r.frame(Vec::new());
    t
}

fn name_of(r: &Rig, t: TrackId) -> String {
    r.app.session.project.track(t).unwrap().name.clone()
}

#[test]
fn enter_leaves_a_track_name_and_keeps_it() {
    for view in [MainView::Session, MainView::Arrangement] {
        let mut r = rig();
        let t = start_rename(&mut r, view);
        r.key(Key::Enter);
        assert_eq!(r.app.ui.renaming, None, "{view:?}");
        assert_eq!(name_of(&r, t), "Bass", "{view:?}");
    }
}

#[test]
fn esc_leaves_a_track_name_and_drops_it() {
    let mut r = rig();
    let before = name_of(&r, r.track(0));
    let t = start_rename(&mut r, MainView::Session);
    r.key(Key::Escape);
    assert_eq!(r.app.ui.renaming, None);
    assert_eq!(name_of(&r, t), before);
}

#[test]
fn clicking_away_leaves_a_track_name_and_keeps_it() {
    let mut r = rig();
    let t = start_rename(&mut r, MainView::Session);
    let pos = Pos2::new(700.0, 420.0);
    let button = |pressed| Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    r.frame(vec![Event::PointerMoved(pos), button(true)]);
    r.frame(vec![button(false)]);
    r.frame(Vec::new());
    assert_eq!(r.app.ui.renaming, None);
    assert_eq!(name_of(&r, t), "Bass");
}

fn selected_first_device(r: &mut Rig) -> (ChainRef, rlive_core::DeviceId) {
    let chain = ChainRef::Track(r.track(0));
    let id = r.app.session.devices(chain)[0].id;
    r.app.ui.selected = Some(chain);
    r.app.ui.focus = FocusArea::Device;
    r.app.ui.devices.selected_devices = vec![(chain, id)];
    r.frame(Vec::new());
    (chain, id)
}

#[test]
fn delete_and_backspace_remove_the_selected_device_and_undo_brings_it_back() {
    for key in [Key::Delete, Key::Backspace] {
        let mut r = rig();
        let (chain, id) = selected_first_device(&mut r);
        r.frame(Vec::new());
        r.key(key);
        assert!(!r.app.session.devices(chain).iter().any(|d| d.id == id));
        assert!(r.app.ui.devices.selected_devices.is_empty());
        r.settle();
        r.app.session.undo();
        r.settle();
        assert!(r.app.session.devices(chain).iter().any(|d| d.id == id));
    }
}

#[test]
fn delete_leaves_a_device_selected_on_a_track_that_isnt_showing() {
    let mut r = rig();
    let (chain, id) = selected_first_device(&mut r);
    r.app.ui.selected = Some(ChainRef::Track(r.track(1)));
    r.frame(Vec::new());
    r.key(Key::Delete);
    assert!(r.app.session.devices(chain).iter().any(|d| d.id == id));
}

#[test]
fn delete_removes_the_selected_rack_chain_rather_than_the_rack() {
    use super::devices::queue::{Action, ActionQueue};
    use super::devices::rack::RackAction;
    let mut r = rig();
    let (chain, _) = selected_first_device(&mut r);
    let device = r.app.ui.devices.selected_devices[0].1;
    let mut q = ActionQueue::default();
    q.push(chain, Action::Group(device, false));
    let (session, ui) = (&mut r.app.session, &mut r.app.ui);
    q.apply(session, &mut ui.devices, &mut ui.status);
    r.settle();
    let rack = r.app.ui.devices.selected_devices[0].1;
    let chains = |r: &Rig| -> Vec<rlive_core::ChainId> {
        r.app
            .session
            .devices(chain)
            .iter()
            .find(|d| d.id == rack)
            .map(|d| d.chains().map(|c| c.id).collect())
            .unwrap_or_default()
    };
    let first = chains(&r)[0];
    let mut q = ActionQueue::default();
    q.push(chain, Action::Rack(rack, RackAction::Select(vec![first])));
    let (session, ui) = (&mut r.app.session, &mut r.app.ui);
    q.apply(session, &mut ui.devices, &mut ui.status);
    r.frame(Vec::new());
    r.key(Key::Delete);
    assert!(r.app.session.devices(chain).iter().any(|d| d.id == rack));
    assert!(!chains(&r).contains(&first));
}

#[test]
fn a_new_midi_track_has_nothing_on_it() {
    let mut r = rig();
    let t = r.app.session.add_track(TrackKind::Midi, None);
    assert!(r.app.session.devices(ChainRef::Track(t)).is_empty());
}

#[test]
fn settings_open_and_close_as_their_own_window() {
    let mut r = rig();
    r.run(M::App(super::commands::app::App::Preferences));
    r.frame(Vec::new());
    assert!(r.app.ui.show_settings);
    let pos = r.app.ui.settings_pos.expect("placed when it opened");
    r.frame(Vec::new());
    assert_eq!(r.app.ui.settings_pos, Some(pos), "stays where it is");
    r.app.ui.show_settings = false;
    r.frame(Vec::new());
    assert_eq!(r.app.ui.settings_pos, None, "centred again next time");
}

/// A rig showing the first track's device, and where that device's title was drawn.
fn device_on_screen() -> (Rig, ChainRef, rlive_core::DeviceId, Rect) {
    let mut r = rig();
    let chain = ChainRef::Track(r.track(0));
    let id = r.app.session.devices(chain)[0].id;
    r.app.ui.selected = Some(chain);
    r.app.ui.show_detail = true;
    r.app.ui.detail = super::Detail::Devices;
    r.frame(Vec::new());
    r.frame(Vec::new());
    let title = r.app.ui.devices.ui.last_title.expect("a device was drawn");
    (r, chain, id, title)
}

fn click(r: &mut Rig, pos: Pos2) {
    let button = |pressed| Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    r.frame(vec![Event::PointerMoved(pos)]);
    r.frame(vec![Event::PointerMoved(pos)]);
    r.frame(vec![button(true)]);
    r.frame(vec![button(false)]);
    r.frame(Vec::new());
}

#[test]
fn clicking_a_device_title_selects_it_and_delete_removes_it() {
    let (mut r, chain, id, title) = device_on_screen();
    click(&mut r, title.center());
    assert_eq!(r.app.ui.devices.selected_devices, [(chain, id)]);
    assert_eq!(r.app.ui.focus, FocusArea::Device);
    r.key(Key::Backspace);
    assert!(!r.app.session.devices(chain).iter().any(|d| d.id == id));
}

#[test]
fn the_power_button_in_a_title_still_gets_its_click() {
    let (mut r, chain, id, title) = device_on_screen();
    click(&mut r, Pos2::new(title.left() + 11.0, title.center().y));
    assert!(r
        .app
        .session
        .devices(chain)
        .iter()
        .any(|d| d.id == id && d.bypass));
    assert!(r.app.ui.devices.selected_devices.is_empty());
}

#[test]
fn double_clicking_a_device_title_folds_it() {
    let (mut r, chain, id, title) = device_on_screen();
    let pos = title.center();
    let button = |pressed| Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    r.frame(vec![Event::PointerMoved(pos)]);
    for _ in 0..2 {
        r.frame(vec![button(true)]);
        r.frame(vec![button(false)]);
    }
    r.frame(Vec::new());
    assert!(r
        .app
        .session
        .devices(chain)
        .iter()
        .any(|d| d.id == id && !d.expanded));
}

// ---------------------------------------------------------------- save changes

fn asking_to_save() -> Rig {
    let mut r = rig();
    r.app.ui.files.pending_action = Some(super::FileAction::NewSet);
    r.frame(Vec::new());
    r.frame(Vec::new());
    r
}

#[test]
fn esc_cancels_the_save_changes_alert() {
    let mut r = asking_to_save();
    let tracks = r.app.session.project.tracks.len();
    r.key(Key::Escape);
    assert!(r.app.ui.files.pending_action.is_none());
    assert_eq!(r.app.session.project.tracks.len(), tracks, "nothing ran");
}

#[test]
fn enter_saves_from_the_alert_and_carries_on() {
    let dir = std::env::temp_dir().join(format!("rlive_alert_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("a.rlive");
    let mut r = asking_to_save();
    r.app.session.path = Some(path.clone());
    r.key(Key::Enter);
    let saved = path.exists();
    std::fs::remove_dir_all(&dir).ok();
    assert!(saved);
    assert!(r.app.ui.files.pending_action.is_none());
    assert!(
        r.app.session.project.tracks[0].devices.is_empty(),
        "new Set"
    );
}

#[test]
fn dont_save_is_on_the_left_of_the_alert_and_carries_on() {
    let mut r = asking_to_save();
    let centre = r.ctx.content_rect().center();
    let at = Pos2::new(centre.x - 223.0 + 69.0, centre.y + 66.0 - 13.0 - 9.0);
    click(&mut r, at);
    assert!(r.app.ui.files.pending_action.is_none());
    assert!(
        r.app.session.project.tracks[0].devices.is_empty(),
        "new Set"
    );
}

// ---------------------------------------------------------------- track widths

fn drag(r: &mut Rig, from: Pos2, dx: f32, modifiers: Modifiers) {
    let button = |pos, pressed| Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers,
    };
    r.frame(vec![Event::PointerMoved(from)]);
    r.frame(vec![button(from, true)]);
    let mut pos = from;
    for _ in 0..4 {
        pos.x += dx / 4.0;
        r.frame(vec![Event::PointerMoved(pos)]);
    }
    r.frame(vec![button(pos, false)]);
    r.frame(Vec::new());
}

fn width_handle(r: &mut Rig) -> Pos2 {
    r.app.ui.main_view = MainView::Session;
    r.frame(Vec::new());
    r.frame(Vec::new());
    r.app
        .ui
        .layout
        .last_width_handle
        .expect("a track was drawn")
        .center()
}

#[test]
fn dragging_a_title_edge_resizes_only_that_track() {
    let mut r = rig();
    let from = width_handle(&mut r);
    drag(&mut r, from, 60.0, Modifiers::NONE);
    let tracks = &r.app.session.project.tracks;
    assert!(tracks.last().unwrap().ui.width.is_some());
    assert_eq!(tracks[0].ui.width, None);
    assert!(r.app.session.dirty);
}

#[test]
fn option_dragging_a_title_edge_resizes_every_track() {
    let mut r = rig();
    let from = width_handle(&mut r);
    r.frame(vec![Event::ModifiersChanged(Modifiers::ALT)]);
    drag(&mut r, from, 60.0, Modifiers::ALT);
    let tracks = &r.app.session.project.tracks;
    let width = tracks[0].ui.width.expect("resized");
    assert!(tracks.iter().all(|t| t.ui.width == Some(width)));
}

fn drag_mixer_edge(r: &mut Rig, dy: f32) {
    let from = r
        .app
        .ui
        .layout
        .last_splitter
        .expect("the mixer was drawn")
        .center();
    let button = |pos, pressed| Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    r.frame(vec![Event::PointerMoved(from)]);
    r.frame(vec![button(from, true)]);
    let mut pos = from;
    for _ in 0..5 {
        pos.y += dy / 5.0;
        r.frame(vec![Event::PointerMoved(pos)]);
    }
    r.frame(vec![button(pos, false)]);
    r.frame(Vec::new());
}

#[test]
fn dragging_the_mixer_edge_resizes_it_with_the_in_out_section_showing() {
    let mut r = rig();
    r.app.ui.main_view = MainView::Session;
    r.app.ui.layout.show_in_out = true;
    r.frame(Vec::new());
    r.frame(Vec::new());
    let start = r.app.ui.layout.mixer_h;
    drag_mixer_edge(&mut r, 30.0);
    let shorter = r.app.ui.layout.mixer_h;
    assert!(shorter < start, "{shorter} after {start}");
    assert!(
        start - shorter < 40.0,
        "moved by the drag, not by the section's height"
    );
    drag_mixer_edge(&mut r, -20.0);
    assert!(r.app.ui.layout.mixer_h > shorter);
}

#[test]
fn the_in_out_section_can_be_hidden_and_the_choice_is_kept() {
    let mut r = rig();
    let shown = r.app.ui.layout.show_in_out;
    r.run(M::View(View::ToggleInOut));
    assert_eq!(r.app.ui.layout.show_in_out, !shown);
    assert_eq!(
        r.app.settings.show_in_out, !shown,
        "kept for the next launch"
    );
}
