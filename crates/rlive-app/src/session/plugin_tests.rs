//! Plug-ins through the Session: Configure Mode, their windows, presets and hosted audio.

use super::plugins::{panel_params, MAX_DEFAULT_PANEL};
use super::*;
use rlive_core::param::plugin_sidechain as sc;
use rlive_core::param::{EnvTarget, ParamOwner, ParamRef};
use rlive_core::{ParamId, ParamInfo, PluginFormat, PPQ};
use rlive_engine::{create_engine, Engine};
use rlive_plugin_host::{HostNotification, PluginHandle};
use rlive_project::{
    Breakpoint, Clip, ClipContent, Device, DeviceKind, MapTarget, MidiMapping, MidiSource,
    NoteData, Track, TrackKind, PLUGIN_BANK_SIZE,
};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// What a [`Fake`] plug-in was asked to do. Shared, because the session owns the plug-in.
#[derive(Default)]
struct Log {
    open: bool,
    visible: bool,
    title: String,
    pending: Vec<HostNotification>,
    state: Vec<u8>,
    loads: Vec<Vec<u8>>,
}

struct Fake {
    desc: rlive_plugin_host::PluginDescriptor,
    params: Vec<ParamInfo>,
    log: Arc<Mutex<Log>>,
    has_editor: bool,
}

impl PluginHandle for Fake {
    fn descriptor(&self) -> &rlive_plugin_host::PluginDescriptor {
        &self.desc
    }
    fn params(&self) -> Vec<ParamInfo> {
        self.params.clone()
    }
    fn param_value(&self, _: u32) -> Option<f64> {
        Some(0.25)
    }
    fn format_value(&self, _: u32, _: f64) -> Option<String> {
        None
    }
    fn save_state(&mut self) -> Option<Vec<u8>> {
        Some(self.log.lock().unwrap().state.clone())
    }
    fn load_state(&mut self, data: &[u8]) -> bool {
        let mut l = self.log.lock().unwrap();
        l.state = data.to_vec();
        l.loads.push(data.to_vec());
        true
    }
    fn has_editor(&self) -> bool {
        self.has_editor
    }
    fn is_editor_open(&self) -> bool {
        self.log.lock().unwrap().open
    }
    fn open_editor(&mut self) -> rlive_plugin_host::Result<()> {
        let mut l = self.log.lock().unwrap();
        l.open = true;
        l.visible = true;
        Ok(())
    }
    fn close_editor(&mut self) {
        self.log.lock().unwrap().open = false;
    }
    fn set_editor_visible(&mut self, visible: bool) {
        self.log.lock().unwrap().visible = visible;
    }
    fn set_editor_title(&mut self, title: &str) {
        self.log.lock().unwrap().title = title.to_string();
    }
    fn idle(&mut self) -> Vec<HostNotification> {
        std::mem::take(&mut self.log.lock().unwrap().pending)
    }
}

fn session(tracks: usize) -> (Engine, Session) {
    let (engine, handle) = create_engine(48_000.0, 512);
    let mut p = Project::empty();
    for i in 0..tracks {
        p.tracks.push(Track::new(
            &format!("Track {i}"),
            TrackKind::Midi,
            [1, 2, 3],
            p.scenes.len(),
        ));
    }
    (engine, Session::new(handle, p, PluginCache::default()))
}

fn params(n: u32) -> Vec<ParamInfo> {
    (0..n)
        .map(|i| ParamInfo::continuous(i, &format!("P{i}"), 0.0, 1.0, 0.0, ""))
        .collect()
}

/// Puts a fake plug-in in a chain.
fn fake_in(
    s: &mut Session,
    chain: ChainRef,
    n: u32,
    format: PluginFormat,
) -> (DeviceId, Arc<Mutex<Log>>) {
    let device = Device::plugin(format, &"A".repeat(32), "/nonexistent".into(), "Fake");
    let id = device.id;
    s.add_device(chain, None, device);
    let log = Arc::new(Mutex::new(Log::default()));
    let rt = s.runtimes.get_mut(&id).unwrap();
    rt.error = None;
    rt.params = params(n);
    rt.plugin = Some(Box::new(Fake {
        desc: rlive_plugin_host::PluginDescriptor {
            format,
            id: "fake".into(),
            name: "Fake".into(),
            vendor: String::new(),
            version: String::new(),
            path: PathBuf::new(),
            is_instrument: false,
            subcategories: String::new(),
            is_midi_effect: false,
        },
        params: params(n),
        log: log.clone(),
        has_editor: true,
    }));
    s.refit_device_cells(chain, id);
    (id, log)
}

/// What recording does: everything moved in a plug-in window goes on the panel when it stops.
#[test]
fn what_a_recording_moved_joins_the_panel_when_it_stops() {
    let (_e, mut s) = session(1);
    let chain = track_chain(&s, 0);
    let (id, _log) = fake_in(&mut s, chain, 100, PluginFormat::Clap);
    s.note_recorded_param_for_test(id, 3);
    s.note_recorded_param_for_test(id, 8);
    s.settle_plugin_panels();
    let mut shown = s.shown_params(chain, id);
    shown.sort();
    assert_eq!(shown, vec![3, 8]);
}

fn track_chain(s: &Session, i: usize) -> ChainRef {
    ChainRef::Track(s.project.tracks[i].id)
}

fn target(id: DeviceId, param: u32) -> ParamRef {
    ParamRef {
        owner: ParamOwner::Device(id),
        id: ParamId(param),
    }
}

fn touch(s: &mut Session, log: &Mutex<Log>, param: u32) {
    log.lock()
        .unwrap()
        .pending
        .push(HostNotification::ParamChanged {
            id: param,
            value: 0.5,
        });
    s.idle();
}

#[test]
fn a_small_plugin_shows_everything_and_a_large_one_nothing() {
    let all = params(MAX_DEFAULT_PANEL as u32);
    assert_eq!(
        panel_params(&Default::default(), &all).len(),
        MAX_DEFAULT_PANEL
    );
    let too_many = params(MAX_DEFAULT_PANEL as u32 + 1);
    assert!(panel_params(&Default::default(), &too_many).is_empty());
}

#[test]
fn the_host_s_own_and_hidden_parameters_stay_off_the_panel() {
    let mut p = params(2);
    p[1].flags = rlive_core::ParamFlags::for_plugin(true);
    p.extend(rlive_plugin_host::keyed::sidechain_params());
    assert_eq!(panel_params(&Default::default(), &p), vec![0]);
}

#[test]
fn touching_a_parameter_in_configure_mode_adds_it_to_the_panel() {
    let (_e, mut s) = session(1);
    let chain = track_chain(&s, 0);
    let (id, log) = fake_in(&mut s, chain, 100, PluginFormat::Clap);
    assert!(s.shown_params(chain, id).is_empty(), "too many to show");

    touch(&mut s, &log, 7);
    assert!(s.shown_params(chain, id).is_empty(), "not configuring yet");

    s.toggle_configure(id);
    assert!(s.is_configuring(id));
    touch(&mut s, &log, 40);
    touch(&mut s, &log, 3);
    touch(&mut s, &log, 40);
    assert_eq!(
        s.shown_params(chain, id),
        vec![40, 3],
        "once each, in order"
    );

    s.toggle_configure(id);
    touch(&mut s, &log, 9);
    assert_eq!(s.shown_params(chain, id), vec![40, 3]);
}

#[test]
fn panels_reorder_and_delete_and_undo() {
    let (_e, mut s) = session(1);
    let chain = track_chain(&s, 0);
    let (id, _log) = fake_in(&mut s, chain, 4, PluginFormat::Clap);
    assert_eq!(s.shown_params(chain, id), vec![0, 1, 2, 3]);
    s.commit_edits(false);
    s.move_in_panel(chain, id, 0, 2);
    s.commit_edits(false);
    assert_eq!(s.shown_params(chain, id), vec![1, 2, 0, 3]);
    assert!(!s.param_usage(chain, id, 2).any());
    s.remove_from_panel(chain, id, 2);
    s.commit_edits(false);
    assert_eq!(s.shown_params(chain, id), vec![1, 0, 3]);
    assert!(s.undo());
    assert_eq!(s.shown_params(chain, id), vec![1, 2, 0, 3]);
    assert!(s.undo());
    assert_eq!(s.shown_params(chain, id), vec![0, 1, 2, 3]);
}

#[test]
fn a_plugin_with_no_window_can_be_reordered_but_not_cut_down() {
    let (_e, mut s) = session(1);
    let chain = track_chain(&s, 0);
    let (id, _log) = fake_in(&mut s, chain, 3, PluginFormat::Clap);
    let rt = s.runtimes.get_mut(&id).unwrap();
    rt.plugin = Some(Box::new(Fake {
        desc: rt.plugin.as_ref().unwrap().descriptor().clone(),
        params: params(3),
        log: Default::default(),
        has_editor: false,
    }));
    assert!(!s.can_remove_from_panel(id));
    s.remove_from_panel(chain, id, 1);
    assert_eq!(s.shown_params(chain, id), vec![0, 1, 2]);
    s.move_in_panel(chain, id, 2, 0);
    assert_eq!(s.shown_params(chain, id), vec![2, 0, 1]);
}

#[test]
fn removing_a_parameter_in_use_is_reported_and_takes_its_mappings_with_it() {
    let (_e, mut s) = session(1);
    let chain = track_chain(&s, 0);
    let (id, _log) = fake_in(&mut s, chain, 4, PluginFormat::Clap);
    let t = target(id, 2);
    s.set_envelope(
        chain,
        EnvTarget::Param(t),
        vec![Breakpoint::new(0.0, 0.0), Breakpoint::new(PPQ as f64, 1.0)],
    );
    s.project.midi_mappings.push(MidiMapping {
        channel: 0,
        source: MidiSource::Note(60),
        target: MapTarget::Param(t),
        min: 0.0,
        max: 1.0,
    });
    let usage = s.param_usage(chain, id, 2);
    assert!(
        usage.automation && usage.midi_or_key && !usage.macro_mapped,
        "{usage:?}"
    );
    assert!(usage.any());
    assert!(!s.param_usage(chain, id, 1).any(), "the others are free");

    s.remove_from_panel(chain, id, 2);
    assert!(!s.shown_params(chain, id).contains(&2));
    assert!(!s.param_usage(chain, id, 2).any(), "nothing left behind");
    assert!(s.project.midi_mappings.is_empty());
}

#[test]
fn parameters_on_the_panel_work_as_envelope_and_mapping_targets() {
    let (_e, mut s) = session(1);
    let chain = track_chain(&s, 0);
    let (id, log) = fake_in(&mut s, chain, 200, PluginFormat::Clap);
    let offered = |s: &Session| -> Vec<u32> {
        s.env_catalog(chain)
            .into_iter()
            .filter(|g| g.device == Some(id))
            .flat_map(|g| g.params)
            .filter_map(|p| match p.target {
                EnvTarget::Param(r) if r.id != rlive_core::ACTIVATOR => Some(r.id.0),
                _ => None,
            })
            .collect()
    };
    assert!(
        offered(&s).is_empty(),
        "an unconfigured big plug-in offers nothing"
    );
    s.toggle_configure(id);
    touch(&mut s, &log, 11);
    s.toggle_configure(id);
    assert_eq!(offered(&s), vec![11]);
    assert!(s.device_cell(chain, id, 11).is_some(), "it has a cell");

    s.set_param(chain, id, 11, 0.75);
    assert_eq!(
        s.devices(chain)[0].params.get(&11).copied(),
        Some(0.75),
        "and a value in the project"
    );
}

#[test]
fn a_touched_parameter_is_a_temporary_chooser_entry_until_it_is_used() {
    let (_e, mut s) = session(1);
    let chain = track_chain(&s, 0);
    let (id, log) = fake_in(&mut s, chain, 100, PluginFormat::Clap);
    let offered = |s: &Session| {
        s.env_catalog(chain)
            .into_iter()
            .filter(|g| g.device == Some(id))
            .flat_map(|g| g.params)
            .filter(|p| matches!(p.target, EnvTarget::Param(r) if r.id != rlive_core::ACTIVATOR))
            .count()
    };
    touch(&mut s, &log, 5);
    assert_eq!(offered(&s), 1);
    touch(&mut s, &log, 6);
    assert_eq!(offered(&s), 1, "the last one replaces it");
    assert!(s.shown_params(chain, id).is_empty());

    // Drawing its automation makes the entry permanent.
    s.set_envelope(
        chain,
        EnvTarget::Param(target(id, 6)),
        vec![Breakpoint::new(0.0, 0.5)],
    );
    for _ in 0..16 {
        s.idle();
    }
    assert_eq!(s.shown_params(chain, id), vec![6]);

    // Looking at it in a chooser doesn't, but choosing something else does.
    touch(&mut s, &log, 9);
    s.envelope_chooser_picked(Some(EnvTarget::Param(target(id, 9))));
    assert_eq!(s.shown_params(chain, id), vec![6]);
    s.envelope_chooser_picked(None);
    assert_eq!(s.shown_params(chain, id), vec![6, 9]);

    // And choosing it for the X-Y field.
    touch(&mut s, &log, 12);
    s.set_xy_param(chain, id, 1, Some(12));
    assert_eq!(s.shown_params(chain, id), vec![6, 9, 12]);
    assert_eq!(s.xy_params(chain, id), [None, Some(12)]);
}

#[test]
fn a_touch_in_a_mapping_mode_adds_the_parameter_and_selects_it() {
    let (_e, mut s) = session(1);
    let chain = track_chain(&s, 0);
    let (id, log) = fake_in(&mut s, chain, 100, PluginFormat::Clap);
    s.set_map_mode(Some(remote::MapMode::Midi));
    touch(&mut s, &log, 21);
    assert_eq!(s.shown_params(chain, id), vec![21]);
    assert_eq!(s.map_target(), Some(MapTarget::Param(target(id, 21))));
    assert_eq!(s.take_last_editor_touch(), Some((chain, target(id, 21))));
    assert_eq!(s.take_last_editor_touch(), None);
}

/// A real CLAP plug-in's panel, sidechain settings and project entry, through Save and Load.
#[test]
fn configure_settings_survive_save_and_load() {
    let (path, found) = clap_fixture("rlive-configure");
    let dir = path.parent().unwrap().to_path_buf();
    let gain = found.iter().find(|p| p.id == "rs.rlive.test-gain").unwrap();
    let (_e, mut s) = session(1);
    let chain = track_chain(&s, 0);
    s.add_plugin(chain, gain);
    let id = s.devices(chain)[0].id;
    assert_eq!(s.runtimes[&id].error, None);
    assert!(s.runtimes[&id].sidechain_input);
    assert_eq!(s.runtimes[&id].aux_names, vec!["Aux".to_string()]);
    assert!(s.device_cell(chain, id, sc::GAIN.0).is_some());
    assert_eq!(
        s.shown_params(chain, id),
        vec![0],
        "sidechain controls aren't part of it"
    );

    s.set_xy_param(chain, id, 0, Some(0));
    s.set_param(chain, id, sc::MIX.0, 40.0);
    s.set_param(chain, id, sc::GAIN.0, -6.0);
    s.set_device_mpe(chain, id, Some(rlive_core::MpeOutput::default()));

    let file = dir.join("set.rlive");
    s.save(&file).unwrap();

    let (_e2, handle) = create_engine(48_000.0, 512);
    let mut loaded = Session::new(handle, Project::empty(), PluginCache::default());
    loaded.load(&file).unwrap();
    let chain = track_chain(&loaded, 0);
    let dev = &loaded.devices(chain)[0];
    assert_eq!(dev.id, id);
    assert_eq!(dev.params.get(&sc::MIX.0).copied(), Some(40.0));
    assert_eq!(dev.params.get(&sc::GAIN.0).copied(), Some(-6.0));
    assert_eq!(dev.mpe, Some(rlive_core::MpeOutput::default()));
    assert_eq!(loaded.shown_params(chain, id), vec![0]);
    assert_eq!(loaded.xy_params(chain, id), [Some(0), None]);
    std::fs::remove_dir_all(&dir).ok();
}

fn midi_note(pitch: u8) -> Clip {
    Clip {
        length: PPQ * 4,
        looped: true,
        content: ClipContent::midi(vec![NoteData::new(0, PPQ * 4, pitch, 1.0)]),
        ..Clip::default()
    }
}

/// Plug-in MIDI output, through the engine: the note effect's octave is what the sine plays.
#[test]
fn a_plugins_midi_output_reaches_the_next_device() {
    let (path, found) = clap_fixture("rlive-midi-chain");
    let by_id = |id: &str| found.iter().find(|p| p.id == id).unwrap().clone();
    let (mut engine, mut s) = session(1);
    let chain = track_chain(&s, 0);
    let t = s.project.tracks[0].id;
    s.add_plugin(chain, &by_id("rs.rlive.test-sine"));
    s.add_plugin(chain, &by_id("rs.rlive.test-midi"));
    assert_eq!(
        s.devices(chain)
            .iter()
            .map(|d| d.name.as_str())
            .collect::<Vec<_>>(),
        ["rLive Test MIDI", "rLive Test Sine"],
        "a note effect goes in front of the instrument"
    );
    s.set_clip(t, 0, Some(midi_note(69)));
    s.launch_clip(t, 0);

    let (mut l, mut r) = (vec![0.0; 512], vec![0.0; 512]);
    for _ in 0..40 {
        engine.process_stereo(&mut l, &mut r);
        s.idle();
    }
    let mut all = Vec::new();
    for _ in 0..94 {
        engine.process_stereo(&mut l, &mut r);
        all.extend_from_slice(&l);
        s.idle();
    }
    let crossings = all.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
    let hz = crossings as f64 * 48_000.0 / all.len() as f64;
    assert!((hz - 880.0).abs() < 6.0, "played an octave up: {hz}");
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

/// VST3 can only turn a bus on while it's stopped, so routing to one means building it again.
#[test]
fn routing_to_a_vst3_sidechain_rebuilds_it_with_the_bus_on() {
    let (bundle, desc) = vst3_fixture("rlive-vst3-wiring");
    let (_e, handle) = create_engine(48_000.0, 512);
    let mut p = Project::empty();
    for name in ["key", "keyed"] {
        p.tracks.push(Track::new(
            name,
            TrackKind::Audio,
            [1, 2, 3],
            p.scenes.len(),
        ));
    }
    let mut s = Session::new(handle, p, PluginCache::default());
    let (source, chain) = (s.project.tracks[0].id, track_chain(&s, 1));
    s.add_plugin(chain, &desc);
    let id = s.devices(chain)[0].id;
    assert_eq!(s.runtimes[&id].error, None);
    assert!(s.runtimes[&id].sidechain_input);
    assert_eq!(s.runtimes[&id].aux_names, vec!["Aux".to_string()]);
    assert!(!s.wiring.sidechain.contains(&id));
    let captured = |s: &Session| {
        matches!(
            &s.devices(chain)[0].kind,
            DeviceKind::Plugin { state: Some(_), .. }
        )
    };
    assert!(!captured(&s), "nothing has needed its state yet");

    assert!(s.set_sidechain(
        chain,
        id,
        Some(rlive_project::SidechainSource {
            track: source,
            tap: rlive_project::Tap::PostMixer,
        })
    ));
    assert!(s.wiring.sidechain.contains(&id));
    let rt = &s.runtimes[&id];
    assert_eq!(rt.error, None);
    assert!(rt.plugin.is_some());
    assert!(
        captured(&s),
        "reloading carries its state over to the new instance"
    );

    assert!(s.set_sidechain(chain, id, None));
    assert!(!s.wiring.sidechain.contains(&id));
    std::fs::remove_dir_all(bundle.parent().unwrap()).ok();
}

// ---------------------------------------------------------------- windows

fn open(log: &Mutex<Log>) -> bool {
    log.lock().unwrap().open
}

fn visible(log: &Mutex<Log>) -> bool {
    let l = log.lock().unwrap();
    l.open && l.visible
}

#[test]
fn one_window_at_a_time_unless_multiple_is_on_or_the_modifier_is_held() {
    let (_e, mut s) = session(1);
    let chain = track_chain(&s, 0);
    let (a, la) = fake_in(&mut s, chain, 1, PluginFormat::Clap);
    let (b, lb) = fake_in(&mut s, chain, 1, PluginFormat::Clap);
    let (c, lc) = fake_in(&mut s, chain, 1, PluginFormat::Clap);

    s.set_window_prefs(plugins::PluginWindowPrefs {
        multiple: false,
        ..Default::default()
    });
    s.open_plugin_window(a, false).unwrap();
    s.open_plugin_window(b, false).unwrap();
    assert!(!open(&la) && open(&lb), "only the new one");
    s.open_plugin_window(c, true).unwrap();
    assert!(open(&lb) && open(&lc), "the modifier keeps the others");

    s.set_window_prefs(plugins::PluginWindowPrefs {
        multiple: true,
        ..Default::default()
    });
    s.open_plugin_window(a, false).unwrap();
    assert!(open(&la) && open(&lb) && open(&lc));
}

#[test]
fn plugin_windows_hide_and_show_together() {
    let (_e, mut s) = session(1);
    let chain = track_chain(&s, 0);
    let (a, la) = fake_in(&mut s, chain, 1, PluginFormat::Clap);
    let (_b, lb) = fake_in(&mut s, chain, 1, PluginFormat::Clap);
    s.set_window_prefs(plugins::PluginWindowPrefs {
        multiple: true,
        ..Default::default()
    });
    s.open_plugin_window(a, false).unwrap();
    assert!(visible(&la) && !visible(&lb));

    s.toggle_plugin_windows();
    assert!(s.plugin_windows_hidden());
    assert!(open(&la) && !visible(&la), "hidden, not closed");
    assert!(!open(&lb), "and a closed one is not opened by it");
    s.toggle_plugin_windows();
    assert!(visible(&la));
}

#[test]
fn auto_hide_shows_only_the_selected_tracks_windows() {
    let (_e, mut s) = session(2);
    let (one, two) = (track_chain(&s, 0), track_chain(&s, 1));
    let (a, la) = fake_in(&mut s, one, 1, PluginFormat::Clap);
    let (b, lb) = fake_in(&mut s, two, 1, PluginFormat::Clap);
    s.set_window_prefs(plugins::PluginWindowPrefs {
        multiple: true,
        auto_hide: true,
        ..Default::default()
    });
    s.open_plugin_window(a, false).unwrap();
    s.open_plugin_window(b, false).unwrap();
    s.set_selected_chain(Some(two));
    assert!(!visible(&la) && visible(&lb));
    s.set_selected_chain(Some(one));
    assert!(visible(&la) && !visible(&lb));
}

#[test]
fn a_window_names_its_track_and_follows_a_rename() {
    let (_e, mut s) = session(1);
    let chain = track_chain(&s, 0);
    let (a, la) = fake_in(&mut s, chain, 1, PluginFormat::Clap);
    s.open_plugin_window(a, false).unwrap();
    assert_eq!(la.lock().unwrap().title, "Track 0: Fake");
    let t = s.project.tracks[0].id;
    s.project.track_mut(t).unwrap().name = "Bass".into();
    s.idle();
    assert_eq!(la.lock().unwrap().title, "Bass: Fake");
}

#[test]
fn closing_a_window_works_and_opening_needs_a_plugin() {
    let (_e, mut s) = session(1);
    let chain = track_chain(&s, 0);
    let (a, la) = fake_in(&mut s, chain, 1, PluginFormat::Clap);
    s.open_plugin_window(a, false).unwrap();
    s.close_plugin_window(a);
    assert!(!open(&la));
    assert!(s.open_plugin_window(DeviceId::new(), false).is_err());
}

// ---------------------------------------------------------------- presets

#[test]
fn a_bank_keeps_each_presets_state_and_name() {
    let (_e, mut s) = session(1);
    let chain = track_chain(&s, 0);
    let (id, log) = fake_in(&mut s, chain, 2, PluginFormat::Vst3);
    log.lock().unwrap().state = vec![1];

    s.select_plugin_preset(chain, id, 3);
    assert!(
        log.lock().unwrap().loads.is_empty(),
        "a blank preset starts from where you are"
    );
    assert_eq!(s.plugin_bank(chain, id).unwrap().current, 3);
    log.lock().unwrap().state = vec![2];
    s.rename_plugin_preset(chain, id, 3, "Bright".into());

    s.select_plugin_preset(chain, id, 0);
    assert_eq!(
        log.lock().unwrap().loads,
        vec![vec![1]],
        "back to what it was"
    );
    s.select_plugin_preset(chain, id, 3);
    assert_eq!(log.lock().unwrap().loads.last().unwrap(), &vec![2]);

    let bank = s.plugin_bank(chain, id).unwrap();
    assert_eq!(bank.name_of(3), "Bright");
    assert_eq!(bank.name_of(0), "Preset 1");
    s.select_plugin_preset(chain, id, PLUGIN_BANK_SIZE);
    assert_eq!(s.plugin_bank(chain, id).unwrap().current, 3, "out of range");
}

fn rlv3(comp: &[u8], ctrl: &[u8]) -> Vec<u8> {
    let mut v = b"RLV3".to_vec();
    v.extend_from_slice(&(comp.len() as u32).to_le_bytes());
    v.extend_from_slice(comp);
    v.extend_from_slice(&(ctrl.len() as u32).to_le_bytes());
    v.extend_from_slice(ctrl);
    v
}

#[test]
fn a_preset_goes_to_a_file_and_comes_back() {
    let dir = temp_dir("rlive-preset-file");
    let (_e, mut s) = session(1);
    let chain = track_chain(&s, 0);
    let (id, log) = fake_in(&mut s, chain, 2, PluginFormat::Vst3);
    let state = rlv3(b"component", b"controller");
    log.lock().unwrap().state = state.clone();

    let file = dir.join("one.vstpreset");
    s.save_plugin_preset(chain, id, &file).unwrap();
    log.lock().unwrap().state = rlv3(b"other", b"");
    s.load_plugin_preset(chain, id, &file).unwrap();
    assert_eq!(log.lock().unwrap().loads.last().unwrap(), &state);

    // A preset for some other plug-in is refused.
    let (other, _) = fake_in(&mut s, chain, 2, PluginFormat::Vst3);
    if let Some(DeviceKind::Plugin { plugin_id, .. }) = s
        .chain_mut(chain)
        .unwrap()
        .iter_mut()
        .find(|d| d.id == other)
        .map(|d| &mut d.kind)
    {
        *plugin_id = "B".repeat(32);
    }
    assert!(s.load_plugin_preset(chain, other, &file).is_err());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_whole_bank_goes_to_a_file_and_comes_back() {
    let dir = temp_dir("rlive-bank-file");
    let (_e, mut s) = session(1);
    let chain = track_chain(&s, 0);
    let (id, log) = fake_in(&mut s, chain, 2, PluginFormat::Vst3);
    log.lock().unwrap().state = vec![9];
    s.rename_plugin_preset(chain, id, 0, "Warm".into());
    let file = dir.join("all.rlbank");
    s.save_plugin_bank(chain, id, &file).unwrap();

    s.rename_plugin_preset(chain, id, 0, "Lost".into());
    log.lock().unwrap().state = vec![0];
    s.load_plugin_bank(chain, id, &file).unwrap();
    assert_eq!(s.plugin_bank(chain, id).unwrap().name_of(0), "Warm");
    assert_eq!(log.lock().unwrap().state, vec![9]);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_bank_is_saved_with_the_set() {
    let (_e, mut s) = session(1);
    let chain = track_chain(&s, 0);
    let (id, log) = fake_in(&mut s, chain, 2, PluginFormat::Vst3);
    log.lock().unwrap().state = vec![4];
    s.rename_plugin_preset(chain, id, 2, "Kept".into());
    s.select_plugin_preset(chain, id, 2);
    let text = s.project.to_ron().unwrap();
    let back = Project::from_ron(&text).unwrap();
    let DeviceKind::Plugin { bank, .. } = &back.tracks[0].devices[0].kind else {
        panic!("not a plug-in");
    };
    assert_eq!(bank.current, 2);
    assert_eq!(bank.name_of(2), "Kept");
    assert_eq!(bank.presets[0].state, Some(vec![4]));
}

fn panel_of(s: &Session, chain: ChainRef, id: DeviceId) -> rlive_project::PluginPanel {
    match &s.devices(chain).iter().find(|d| d.id == id).unwrap().kind {
        DeviceKind::Plugin { panel, .. } => panel.clone(),
        _ => panic!("not a plug-in"),
    }
}

fn user_library(dir: &Path) -> rlive_browser::defaults::UserLibrary {
    let lib = rlive_browser::defaults::UserLibrary::new(dir.join("library"));
    lib.ensure_folders().unwrap();
    lib
}

#[test]
fn a_plugin_preset_brings_its_panel_with_it() {
    use rlive_browser::preset::Preset;
    let dir = temp_dir("rlive-plugin-preset");
    let (_e, mut s) = session(1);
    s.library = Some(user_library(&dir));
    let chain = track_chain(&s, 0);
    let (id, _log) = fake_in(&mut s, chain, 100, PluginFormat::Clap);
    s.add_to_panel(chain, id, 40);
    s.add_to_panel(chain, id, 3);
    s.set_xy_param(chain, id, 0, Some(3));
    let panel = panel_of(&s, chain, id);
    assert_eq!(panel.params, Some(vec![40, 3]));

    let path = s.save_device_preset(chain, id, None).unwrap();
    let preset = Preset::load(&path).unwrap();

    let loaded = s.load_preset(chain, None, &preset).unwrap();
    assert_ne!(loaded, id, "a new instance");
    assert_eq!(panel_of(&s, chain, loaded), panel);

    // Hot-Swap puts the saved panel in place of what was there.
    let (other, _) = fake_in(&mut s, chain, 100, PluginFormat::Clap);
    assert!(panel_of(&s, chain, other).params.is_none());
    s.hot_swap(
        &SwapTarget::Device { chain, id: other },
        SwapItem::Preset(&preset),
    )
    .unwrap();
    assert_eq!(panel_of(&s, chain, other), panel);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_default_configuration_is_the_panel_and_no_values() {
    use rlive_browser::preset::{Payload, PluginConfig, Preset};
    let dir = temp_dir("rlive-plugin-default");
    let (_e, mut s) = session(1);
    let lib = user_library(&dir);
    s.library = Some(lib.clone());
    let chain = track_chain(&s, 0);
    let (id, _log) = fake_in(&mut s, chain, 100, PluginFormat::Clap);
    s.add_to_panel(chain, id, 40);
    s.add_to_panel(chain, id, 3);
    s.set_xy_param(chain, id, 1, Some(40));
    s.set_param(chain, id, 40, 0.9);

    let path = s.save_default_plugin_config(chain, id).unwrap();
    let Payload::PluginConfig(config) = Preset::load(&path).unwrap().payload else {
        panic!("not a configuration");
    };
    assert_eq!(config.params, vec![40, 3]);
    assert_eq!(config.xy, [None, Some(40)]);

    // A new instance of a real plug-in picks up its default configuration.
    let (plugin, found) = clap_fixture("rlive-default-config");
    let gain = found.iter().find(|p| p.id == "rs.rlive.test-gain").unwrap();
    let saved = PluginConfig {
        format: gain.format,
        plugin_id: gain.id.clone(),
        params: vec![0],
        xy: [Some(0), None],
    };
    Preset::of_plugin_config(saved, "Default")
        .save(&lib.plugin_config_path(gain.format, &gain.id))
        .unwrap();
    s.add_plugin(chain, gain);
    let new = s
        .devices(chain)
        .iter()
        .find(|d| d.name == gain.name)
        .unwrap();
    let new_id = new.id;
    assert!(new.params.is_empty(), "no values come with it");
    let panel = panel_of(&s, chain, new_id);
    assert_eq!(panel.params, Some(vec![0]));
    assert_eq!(panel.xy, [Some(0), None]);
    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(plugin.parent().unwrap()).ok();
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---------------------------------------------------------------- the fixture

/// Builds the VST3 fixture once per test run, in a bundle of its own for each caller.
fn vst3_fixture(prefix: &str) -> (PathBuf, rlive_plugin_host::PluginDescriptor) {
    static LIB: OnceLock<PathBuf> = OnceLock::new();
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let lib = LIB.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let target = std::env::var_os("CARGO_TARGET_DIR")
            .map_or_else(|| root.join("target"), PathBuf::from)
            .join("test-plugins");
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let status = Command::new(cargo)
            .args(["build", "-q", "-p", "rlive-test-vst3", "--target-dir"])
            .arg(&target)
            .current_dir(&root)
            .status()
            .expect("run cargo");
        assert!(status.success(), "building rlive-test-vst3 failed");
        target.join("debug").join(format!(
            "{}rlive_test_vst3.{}",
            std::env::consts::DLL_PREFIX,
            std::env::consts::DLL_EXTENSION
        ))
    });
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{n}", std::process::id()));
    let bundle = dir.join("rlive-test.vst3");
    let (sub, name) = if cfg!(target_os = "macos") {
        ("MacOS", "rlive-test".to_string())
    } else if cfg!(target_os = "windows") {
        ("x86_64-win", "rlive-test.vst3".to_string())
    } else if cfg!(target_arch = "aarch64") {
        ("aarch64-linux", "rlive-test.so".to_string())
    } else {
        ("x86_64-linux", "rlive-test.so".to_string())
    };
    let bin = bundle.join("Contents").join(sub);
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::copy(lib, bin.join(name)).unwrap();
    let found = rlive_plugin_host::vst3::scan(&bundle).unwrap();
    (bundle, found.into_iter().next().unwrap())
}

/// Builds the CLAP fixture once per test run, in a copy of its own for each caller.
pub(super) fn clap_fixture(prefix: &str) -> (PathBuf, Vec<rlive_plugin_host::PluginDescriptor>) {
    static LIB: OnceLock<PathBuf> = OnceLock::new();
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let lib = LIB.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let target = std::env::var_os("CARGO_TARGET_DIR")
            .map_or_else(|| root.join("target"), PathBuf::from)
            .join("test-plugins");
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let status = Command::new(cargo)
            .args(["build", "-q", "-p", "rlive-test-clap", "--target-dir"])
            .arg(&target)
            .current_dir(&root)
            .status()
            .expect("run cargo");
        assert!(status.success(), "building rlive-test-clap failed");
        target.join("debug").join(format!(
            "{}rlive_test_clap.{}",
            std::env::consts::DLL_PREFIX,
            std::env::consts::DLL_EXTENSION
        ))
    });
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dest = dir.join("rlive-test.clap");
    std::fs::copy(lib, &dest).unwrap();
    let found = rlive_plugin_host::clap::scan(&dest).unwrap();
    (dest, found)
}
