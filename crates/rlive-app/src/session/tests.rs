use super::*;
use rlive_core::param::mixer;
use rlive_core::{MidiMessage, ParamInfo, ParamKind};
use rlive_engine::create_engine;
use rlive_project::{ArrangementClip, Clip};

fn session() -> (rlive_engine::Engine, Session) {
    let (engine, handle) = create_engine(48_000.0, 512);
    (
        engine,
        Session::new(handle, Project::new_default(), PluginCache::default()),
    )
}

fn render(engine: &mut rlive_engine::Engine, s: &mut Session, frames: usize) -> Vec<f32> {
    let frames = frames.next_multiple_of(512);
    let (left, _) = rlive_testkit::render_chunks(frames, 512, |l, r| {
        engine.process(l, r);
        s.idle();
    });
    left
}

fn commit(s: &mut Session) {
    s.commit_edits(false);
}

fn poly_param(s: &Session, t: TrackId) -> (DeviceId, u32) {
    let d = s.devices(ChainRef::Track(t))[0].id;
    (d, s.runtimes[&d].params[0].id)
}

#[test]
fn undo_restores_an_edited_clip_and_leaves_others_playing() {
    let (mut engine, mut s) = session();
    let (playing, other) = (s.project.tracks[0].id, s.project.tracks[1].id);
    s.launch_clip(playing, 0);
    render(&mut engine, &mut s, 4_800);
    let shared = s.track_shared[&playing].clone();
    let clip = Clip::new_midi("new", [1, 2, 3], 1, s.project.time_signature);
    s.set_clip(other, 1, Some(clip));
    commit(&mut s);
    assert!(s.undo());
    assert!(s.clip(other, 1).is_none());
    assert!(
        Arc::ptr_eq(&shared, &s.track_shared[&playing]),
        "no rebuild"
    );
    render(&mut engine, &mut s, 4_800);
    assert_eq!(
        shared
            .playing_slot
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    assert!(s.redo());
    assert_eq!(s.clip(other, 1).unwrap().name, "new");
}

#[test]
fn knob_moves_make_no_step_and_survive_undo() {
    let (_engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let clip = Clip::new_midi("x", [1, 2, 3], 1, s.project.time_signature);
    s.set_clip(t, 2, Some(clip));
    commit(&mut s);
    let (dev, param) = poly_param(&s, t);
    s.set_param(ChainRef::Track(t), dev, param, 0.25);
    commit(&mut s);
    assert_eq!(s.history.undo_label(), Some("Edit Clip"));
    assert!(s.undo());
    assert_eq!(s.project.tracks[0].devices[0].params[&param], 0.25);
    assert!(!s.undo(), "the knob move must not be a step");
}

#[test]
fn deleting_a_track_is_undoable_with_its_devices() {
    let (_engine, mut s) = session();
    let (t, survivor) = (s.project.tracks[1].id, s.project.tracks[0].id);
    let shared = s.track_shared[&survivor].clone();
    let (dev, param) = poly_param(&s, t);
    s.set_param(ChainRef::Track(t), dev, param, 0.75);
    s.project.track_mut(t).unwrap().volume_db = -9.0;
    let ids: Vec<DeviceId> = s.devices(ChainRef::Track(t)).iter().map(|d| d.id).collect();
    s.delete_track(t);
    commit(&mut s);
    assert!(s.project.track(t).is_none());
    assert!(s.undo());
    let restored = s.project.track(t).expect("track is back");
    assert_eq!(
        restored.devices.iter().map(|d| d.id).collect::<Vec<_>>(),
        ids
    );
    assert_eq!(restored.devices[0].params[&param], 0.75);
    assert_eq!(restored.volume_db, -9.0);
    assert_eq!(s.project.track_index(t), Some(1));
    assert!(s.runtimes.contains_key(&dev));
    assert!(
        Arc::ptr_eq(&shared, &s.track_shared[&survivor]),
        "no rebuild"
    );
}

#[test]
fn removing_a_device_is_undoable_in_place() {
    let (_engine, mut s) = session();
    let t = s.project.tracks[1].id;
    let chain = ChainRef::Track(t);
    let ids: Vec<DeviceId> = s.devices(chain).iter().map(|d| d.id).collect();
    s.remove_device(chain, ids[0]);
    commit(&mut s);
    assert_eq!(s.devices(chain).len(), ids.len() - 1);
    assert!(s.undo());
    let now: Vec<DeviceId> = s.devices(chain).iter().map(|d| d.id).collect();
    assert_eq!(now, ids);
}

#[test]
fn a_drag_of_direct_edits_is_one_step() {
    let (_engine, mut s) = session();
    let t = s.project.tracks[0].id;
    for name in ["a", "ab", "abc"] {
        s.project.track_mut(t).unwrap().name = name.into();
        s.commit_edits(true);
    }
    s.commit_edits(false);
    assert_eq!(s.history.undo_label(), Some("Edit"));
    s.undo();
    assert_eq!(s.project.track(t).unwrap().name, "1 Poly");
    assert!(!s.undo());
}

#[test]
fn dirty_clears_at_the_saved_state_unless_a_knob_moved() {
    let (_engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let clip = Clip::new_midi("x", [1, 2, 3], 1, s.project.time_signature);
    s.set_clip(t, 2, Some(clip.clone()));
    commit(&mut s);
    assert!(s.dirty);
    s.undo();
    assert!(!s.dirty, "back at the state the session started from");

    s.set_clip(t, 2, Some(clip));
    commit(&mut s);
    let (dev, param) = poly_param(&s, t);
    s.set_param(ChainRef::Track(t), dev, param, 0.5);
    s.undo();
    assert!(s.dirty, "the knob move still needs saving");
}

fn on(key: u8) -> MidiMessage {
    MidiMessage::NoteOn {
        channel: 0,
        key,
        velocity: 0.8,
    }
}

fn off(key: u8) -> MidiMessage {
    MidiMessage::NoteOff {
        channel: 0,
        key,
        velocity: 0.5,
    }
}

fn silent_project(s: &mut Session) -> TrackId {
    for i in 0..s.project.tracks.len() {
        s.project.tracks[i].clips.iter_mut().for_each(|c| *c = None);
    }
    s.project.tracks[0].id
}

#[test]
fn a_timeline_clip_is_audible_from_the_marker() {
    let (mut engine, mut s) = session();
    let t = silent_project(&mut s);
    let mut clip = Clip::new_midi("x", [1, 2, 3], 1, s.project.time_signature);
    clip.notes_mut().unwrap().push(rlive_project::NoteData {
        start: 0,
        length: 3000,
        key: 60,
        velocity: 1.0,
    });
    s.arr_place(t, ArrangementClip::new(clip, 3840));
    s.seek(3840.0);
    render(&mut engine, &mut s, 1024);
    s.play();
    let out = render(&mut engine, &mut s, 24_000);
    assert!(out.iter().any(|x| x.abs() > 0.01), "the clip should sound");
}

#[test]
fn trims_moves_and_deletes_are_one_undo_step_each() {
    let (_engine, mut s) = session();
    let t = silent_project(&mut s);
    let clip = Clip::new_midi("x", [1, 2, 3], 2, s.project.time_signature);
    let placed = ArrangementClip::new(clip, 0);
    let id = placed.id;
    s.arr_place(t, placed);
    commit(&mut s);
    s.arr_trim(t, id, TrimEdge::Right(1920));
    commit(&mut s);
    assert_eq!(
        s.project.track(t).unwrap().arr_clip(id).unwrap().length,
        1920
    );
    s.undo();
    assert_eq!(
        s.project.track(t).unwrap().arr_clip(id).unwrap().length,
        7680
    );
    s.arr_delete(&[(t, id)]);
    commit(&mut s);
    assert!(s.project.track(t).unwrap().arrangement.is_empty());
    s.undo();
    assert!(s.project.track(t).unwrap().arr_clip(id).is_some());
}

#[test]
fn duplicating_lands_right_after_the_selection() {
    let (_engine, mut s) = session();
    let t = silent_project(&mut s);
    let clip = Clip::new_midi("x", [1, 2, 3], 1, s.project.time_signature);
    let placed = ArrangementClip::new(clip, 960);
    let id = placed.id;
    s.arr_place(t, placed);
    let new = s.arr_duplicate(&[(t, id)]);
    let c = s.project.track(t).unwrap().arr_clip(new[0].1).unwrap();
    assert_eq!(c.start, 960 + 3840);
}

#[test]
fn exporting_the_arrangement_covers_its_extent_or_loop() {
    let (mut engine, mut s) = session();
    let t = silent_project(&mut s);
    let clip = Clip::new_midi("x", [1, 2, 3], 2, s.project.time_signature);
    s.arr_place(t, ArrangementClip::new(clip, 0));
    render(&mut engine, &mut s, 1024);
    let dir = std::env::temp_dir().join(format!("rlive-export-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("a.wav");

    s.export_arrangement(&path).unwrap();
    let whole = rlive_devices::load_wav(&path).unwrap();
    assert_eq!(whole.len(), 192_000, "two bars at 120 BPM and 48 kHz");

    s.set_loop(true, 0, 1920);
    s.export_arrangement(&path).unwrap();
    let part = rlive_devices::load_wav(&path).unwrap();
    assert_eq!(part.len(), 48_000, "one beat-pair at 120 BPM");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn an_empty_arrangement_does_not_export() {
    let (_engine, mut s) = session();
    let path = std::env::temp_dir().join("rlive-never-written.wav");
    assert!(s.export_arrangement(&path).is_err());
}

#[test]
fn recording_needs_an_armed_midi_track() {
    let (_engine, mut s) = session();
    assert!(!s.start_recording());
    assert!(!s.is_recording());
}

#[test]
fn a_recorded_take_lands_on_the_armed_track_as_one_undo_step() {
    let (mut engine, mut s) = session();
    let t = silent_project(&mut s);
    let unarmed = s.project.tracks[1].id;
    s.set_arm(t, true, true);
    assert!(s.start_recording());
    render(&mut engine, &mut s, 4_096);
    s.play_midi(t, on(60));
    s.play_midi(unarmed, on(72));
    s.midi_in(t, on(64)); // a piano-roll preview, which must not be recorded
    s.commit_edits(s.is_recording());
    render(&mut engine, &mut s, 12_000);
    s.play_midi(t, off(60));
    s.play_midi(unarmed, off(72));
    s.midi_in(t, off(64));
    render(&mut engine, &mut s, 4_096);
    s.stop();
    render(&mut engine, &mut s, 1_024);
    assert!(!s.is_recording());
    commit(&mut s);

    let lane = &s.project.track(t).unwrap().arrangement;
    assert_eq!(lane.len(), 1);
    assert_eq!(lane[0].clip.notes().len(), 1);
    assert_eq!(lane[0].clip.notes()[0].key, 60);
    assert!(s.project.track(unarmed).unwrap().arrangement.is_empty());

    assert_eq!(s.history.undo_label(), Some("Record"));
    assert!(s.undo());
    assert!(s.project.track(t).unwrap().arrangement.is_empty());
    assert!(!s.undo(), "the take is a single step");
}

#[test]
fn loop_recording_overdubs_into_one_clip() {
    let (mut engine, mut s) = session();
    let t = silent_project(&mut s);
    s.set_arm(t, true, true);
    s.set_loop(true, 0, 3840); // two seconds at 120 BPM
    commit(&mut s);
    assert!(s.start_recording());
    render(&mut engine, &mut s, 4_096);
    s.play_midi(t, on(60));
    render(&mut engine, &mut s, 4_096);
    s.play_midi(t, off(60));
    render(&mut engine, &mut s, 96_000); // past the wrap
    s.play_midi(t, on(62));
    render(&mut engine, &mut s, 4_096);
    s.play_midi(t, off(62));
    render(&mut engine, &mut s, 4_096);
    s.stop();
    render(&mut engine, &mut s, 1_024);
    let lane = &s.project.track(t).unwrap().arrangement;
    assert_eq!(lane.len(), 1);
    let keys: Vec<u8> = lane[0].clip.notes().iter().map(|n| n.key).collect();
    assert!(keys.contains(&60) && keys.contains(&62), "got {keys:?}");
}

#[test]
fn default_project_plays_demo_clip() {
    let (mut engine, mut s) = session();
    let id = s.project.tracks[0].id;
    s.launch_clip(id, 0);
    let out = render(&mut engine, &mut s, 48_000);
    assert!(
        out.iter().any(|x| x.abs() > 0.01),
        "demo clip should make sound"
    );
    assert!(s.is_playing());
}

#[test]
fn edits_reach_engine() {
    let (mut engine, mut s) = session();
    let t = s.add_track(TrackKind::Midi, None);
    assert_eq!(s.project.tracks.len(), 4);
    let mut clip = Clip::new_midi("x", [1, 2, 3], 1, s.project.time_signature);
    clip.notes_mut().unwrap().push(rlive_project::NoteData {
        start: 0,
        length: 960,
        key: 60,
        velocity: 1.0,
    });
    s.set_clip(t, 0, Some(clip));
    s.add_builtin(ChainRef::Track(t), "rlive.reverb");
    assert_eq!(s.devices(ChainRef::Track(t)).len(), 2);
    s.launch_clip(t, 0);
    let out = render(&mut engine, &mut s, 24_000);
    assert!(out.iter().any(|x| x.abs() > 0.01));

    // Muting everything silences the output.
    for i in 0..s.project.tracks.len() {
        s.project.tracks[i].mute = true;
        let id = s.project.tracks[i].id;
        s.sync_mix(id);
    }
    let out = render(&mut engine, &mut s, 24_000);
    assert!(out[out.len() - 1000..].iter().all(|x| x.abs() < 1e-3));

    let dev = s.devices(ChainRef::Track(t))[1].id;
    s.remove_device(ChainRef::Track(t), dev);
    s.delete_track(t);
    render(&mut engine, &mut s, 2048);
    assert_eq!(s.project.tracks.len(), 3);
}

#[test]
fn save_load_roundtrip_and_export() {
    let (mut engine, mut s) = session();
    let dir = std::env::temp_dir().join(format!("rlive-session-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("song.rlive");
    s.set_tempo(100.0);
    s.save(&path).unwrap();
    s.new_project();
    assert_eq!(s.project.tempo, 120.0);
    s.load(&path).unwrap();
    assert_eq!(s.project.tempo, 100.0);
    render(&mut engine, &mut s, 1024);

    let wav = dir.join("bounce.wav");
    s.export_scene(0, 1, &wav).unwrap();
    let b = rlive_devices::load_wav(&wav).unwrap();
    // 1 bar at 100 BPM = 2.4 s.
    assert!((b.duration_secs() - 2.4).abs() < 0.01);
    assert!(b.left.iter().any(|x| x.abs() > 0.01));
    std::fs::remove_dir_all(&dir).ok();
}

/// A plugin's main-thread half that reports what it is told to.
struct FakePlugin {
    desc: PluginDescriptor,
    params: Vec<ParamInfo>,
    pending: Vec<HostNotification>,
    latency: u32,
}

impl FakePlugin {
    fn new(params: Vec<ParamInfo>, pending: Vec<HostNotification>) -> Box<Self> {
        Box::new(Self {
            desc: PluginDescriptor {
                format: PluginFormat::Clap,
                id: "fake".into(),
                name: "Fake".into(),
                vendor: String::new(),
                version: String::new(),
                path: PathBuf::new(),
                is_instrument: false,
            },
            params,
            pending,
            latency: 0,
        })
    }
}

impl rlive_plugin_host::PluginHandle for FakePlugin {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.desc
    }
    fn params(&self) -> Vec<ParamInfo> {
        self.params.clone()
    }
    fn param_value(&self, id: u32) -> Option<f64> {
        self.params.iter().find(|p| p.id == id).map(|p| p.default)
    }
    fn format_value(&self, _: u32, _: f64) -> Option<String> {
        None
    }
    fn save_state(&mut self) -> Option<Vec<u8>> {
        None
    }
    fn load_state(&mut self, _: &[u8]) -> bool {
        true
    }
    fn has_editor(&self) -> bool {
        false
    }
    fn is_editor_open(&self) -> bool {
        false
    }
    fn open_editor(&mut self) -> rlive_plugin_host::Result<()> {
        Ok(())
    }
    fn close_editor(&mut self) {}
    fn idle(&mut self) -> Vec<HostNotification> {
        std::mem::take(&mut self.pending)
    }
    fn latency(&self) -> u32 {
        self.latency
    }
}

fn poly_info(s: &Session, dev: DeviceId) -> ParamInfo {
    let continuous = |p: &&ParamInfo| matches!(p.kind, ParamKind::Continuous { .. });
    s.runtimes[&dev]
        .params
        .iter()
        .find(continuous)
        .unwrap()
        .clone()
}

#[test]
fn every_parameter_has_a_cell_that_survives_insert_remove_and_move() {
    let (mut engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let chain = ChainRef::Track(t);
    let poly = s.devices(chain)[0].id;
    let info = poly_info(&s, poly);
    let cell = s.device_cell(chain, poly, info.id).unwrap();
    let activator = s.activator_cell(chain, poly).unwrap();
    assert_ne!(activator, cell);
    assert_eq!(
        s.mixer_cell(chain, mixer::SOLO),
        Some(rlive_engine::mix_cell::SOLO)
    );
    assert_eq!(
        s.mixer_cell(ChainRef::Master, mixer::VOLUME),
        Some(rlive_engine::mix_cell::VOLUME)
    );
    assert_eq!(s.mixer_cell(ChainRef::Master, mixer::SOLO), None);

    let knob = info.from_normalized(0.8);
    s.set_param(chain, poly, info.id, knob);
    render(&mut engine, &mut s, 2048);
    let settled = s.effective_norm(chain, poly, info.id).unwrap();
    assert!((settled - 0.8).abs() < 1e-3, "{settled}");

    // A device in front, one behind, a removal and a move: the poly's cell doesn't budge.
    s.add_builtin(chain, "rlive.reverb");
    s.add_builtin(chain, "rlive.delay");
    let (reverb, delay) = (s.devices(chain)[1].id, s.devices(chain)[2].id);
    let reverb_cell = s.device_cell(chain, reverb, 0).unwrap();
    let delay_cell = s.device_cell(chain, delay, 0).unwrap();
    assert_eq!(s.device_cell(chain, poly, info.id), Some(cell));
    s.remove_device(chain, reverb);
    assert_eq!(s.device_cell(chain, reverb, 0), None);
    assert_eq!(s.device_cell(chain, delay, 0), Some(delay_cell));
    s.move_device(chain, delay, 0);
    assert_eq!(s.device_cell(chain, delay, 0), Some(delay_cell));
    assert_eq!(s.device_cell(chain, poly, info.id), Some(cell));
    render(&mut engine, &mut s, 2048);

    // Reuse the freed range: it's a new device, with a fresh value.
    s.add_builtin(chain, "rlive.reverb");
    let new_reverb = s.devices(chain).last().unwrap().id;
    assert_eq!(s.device_cell(chain, new_reverb, 0), Some(reverb_cell));
    render(&mut engine, &mut s, 2048);

    // The surviving cell still has its value and the UI reads it from the running engine.
    let after = s.effective_norm(chain, poly, info.id).unwrap();
    assert!((after - 0.8).abs() < 1e-3, "{after}");
    assert_eq!(s.project.tracks[0].devices[1].params[&info.id], knob);
}

#[test]
fn a_track_rebuilt_by_undo_gets_cells_with_its_values() {
    let (mut engine, mut s) = session();
    let t = s.project.tracks[1].id;
    let chain = ChainRef::Track(t);
    let poly = s.devices(chain)[0].id;
    let info = poly_info(&s, poly);
    s.set_param(chain, poly, info.id, info.from_normalized(0.3));
    commit(&mut s);
    s.delete_track(t);
    commit(&mut s);
    assert!(s.device_cell(chain, poly, info.id).is_none());
    assert!(s.undo());
    let cell = s.device_cell(chain, poly, info.id).expect("cells are back");
    assert!((s.layouts[&chain].effective(cell).unwrap() - 0.3).abs() < 1e-3);
    render(&mut engine, &mut s, 2048);
    assert!((s.effective_norm(chain, poly, info.id).unwrap() - 0.3).abs() < 1e-3);
}

#[test]
fn mixer_controls_and_the_activator_are_parameters() {
    let (mut engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let chain = ChainRef::Track(t);
    let poly = s.devices(chain)[0].id;
    let volume = mixer::infos()[0].clone();

    s.project.track_mut(t).unwrap().volume_db = -12.0;
    s.sync_mix(t);
    s.set_master_volume(-3.0);
    s.set_bypass(chain, poly, true);
    render(&mut engine, &mut s, 1024);

    let layout = &s.layouts[&chain];
    assert_eq!(layout.base(rlive_engine::mix_cell::VOLUME), Some(-12.0));
    let effective = layout.effective(rlive_engine::mix_cell::VOLUME).unwrap();
    assert!((effective as f64 - volume.to_normalized(-12.0)).abs() < 1e-5);
    let activator = layout.activator_cell(poly).unwrap();
    assert_eq!(layout.effective(activator), Some(0.0));
    assert_eq!(
        s.layouts[&ChainRef::Master].base(rlive_engine::mix_cell::VOLUME),
        Some(-3.0)
    );
    assert!(s.project.tracks[0].devices[0].bypass);
}

#[test]
fn a_plugin_editor_change_reaches_the_project_and_the_engine() {
    let (mut engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let chain = ChainRef::Track(t);
    s.add_builtin(chain, "rlive.reverb");
    let dev = s.devices(chain)[1].id;
    let info = s.runtimes[&dev].params[0].clone();
    let value = info.from_normalized(0.9);
    s.runtimes.get_mut(&dev).unwrap().plugin = Some(FakePlugin::new(
        s.runtimes[&dev].params.clone(),
        vec![HostNotification::ParamChanged { id: info.id, value }],
    ));
    render(&mut engine, &mut s, 1024);

    assert_eq!(s.project.tracks[0].devices[1].params[&info.id], value);
    assert!(s.history.untracked_dirty, "the project is marked changed");
    render(&mut engine, &mut s, 1024);
    assert!((s.effective_norm(chain, dev, info.id).unwrap() - 0.9).abs() < 1e-3);
}

#[test]
fn a_plugin_that_rescans_its_params_gets_new_cells_and_keeps_old_values() {
    let (mut engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let chain = ChainRef::Track(t);
    s.add_builtin(chain, "rlive.reverb");
    let dev = s.devices(chain)[1].id;
    let old = s.runtimes[&dev].params.clone();
    let knob = old[0].from_normalized(0.4);
    s.set_param(chain, dev, old[0].id, knob);

    // The plugin grows a parameter.
    let mut rescanned = old.clone();
    let extra = rescanned.iter().map(|p| p.id).max().unwrap() + 1;
    rescanned.push(ParamInfo::continuous(extra, "New", 0.0, 1.0, 0.25, ""));
    s.runtimes.get_mut(&dev).unwrap().plugin = Some(FakePlugin::new(
        rescanned,
        vec![HostNotification::ParamsRescanned],
    ));
    render(&mut engine, &mut s, 2048);

    assert_eq!(s.runtimes[&dev].params.len(), old.len() + 1);
    let new_cell = s
        .device_cell(chain, dev, extra)
        .expect("the new param has a cell");
    assert_eq!(s.layouts[&chain].base(new_cell), Some(0.25));
    render(&mut engine, &mut s, 2048);
    assert!((s.effective_norm(chain, dev, old[0].id).unwrap() - 0.4).abs() < 1e-3);
    assert_eq!(s.effective_norm(chain, dev, extra), Some(0.25));
}

#[test]
fn a_plugin_reporting_a_new_latency_updates_the_shared_atomic() {
    let (mut engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let chain = ChainRef::Track(t);
    s.add_builtin(chain, "rlive.reverb");
    let dev = s.devices(chain)[1].id;
    let latency = s.runtimes[&dev].latency.clone();
    assert_eq!(latency.load(std::sync::atomic::Ordering::Relaxed), 0);
    let mut plugin = FakePlugin::new(Vec::new(), vec![HostNotification::LatencyChanged]);
    plugin.latency = 512;
    s.runtimes.get_mut(&dev).unwrap().plugin = Some(plugin);
    render(&mut engine, &mut s, 512);
    assert_eq!(latency.load(std::sync::atomic::Ordering::Relaxed), 512);
}

#[test]
fn midi_effects_go_before_the_instrument_and_instruments_before_audio() {
    let (_engine, mut s) = session();
    let t = s.project.tracks[1].id; // Poly Synth then Delay.
    let chain = ChainRef::Track(t);
    s.add_builtin(chain, "rlive.pitch");
    s.add_builtin(chain, "rlive.chord");
    let names = |s: &Session| {
        s.devices(chain)
            .iter()
            .map(|d| d.name.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&s), ["Pitch", "Chord", "Poly Synth", "Delay"]);
    s.add_builtin(chain, "rlive.sampler");
    assert_eq!(
        names(&s),
        ["Pitch", "Chord", "Poly Synth", "Sampler", "Delay"]
    );
    s.add_builtin(chain, "rlive.reverb");
    assert_eq!(
        names(&s),
        ["Pitch", "Chord", "Poly Synth", "Sampler", "Delay", "Reverb"]
    );
}

#[test]
fn a_midi_effect_in_a_session_chain_is_marked_for_the_engine() {
    let (mut engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let chain = ChainRef::Track(t);
    s.add_builtin(chain, "rlive.pitch");
    assert_eq!(
        s.runtimes[&s.devices(chain)[0].id].role,
        rlive_devices::DeviceRole::MidiEffect
    );
    // A pure passthrough: playing into it still sounds the instrument after it.
    s.launch_clip(t, 0);
    s.play();
    let out = render(&mut engine, &mut s, 96_000);
    assert!(out.iter().any(|x| x.abs() > 0.01));
}

#[test]
fn swapping_a_sample_keeps_held_notes_sounding_and_recycles_the_old_one() {
    let dir = std::env::temp_dir().join(format!("rlive-swap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (a, b) = (dir.join("a.wav"), dir.join("b.wav"));
    let tone = |v: f32| vec![v; 96_000];
    rlive_devices::write_wav(&a, &tone(0.8), &tone(0.8), 48_000).unwrap();
    rlive_devices::write_wav(&b, &tone(0.4), &tone(0.4), 48_000).unwrap();

    let (mut engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let chain = ChainRef::Track(t);
    s.remove_device(chain, s.devices(chain)[0].id);
    s.add_builtin(chain, "rlive.sampler");
    let dev = s.devices(chain)[0].id;
    s.set_device_sample(chain, dev, a.clone());
    render(&mut engine, &mut s, 512);

    let mut midi = s.engine.take_midi_sender().unwrap();
    assert!(midi.send(
        t,
        MidiMessage::NoteOn {
            channel: 0,
            key: 60,
            velocity: 1.0
        }
    ));
    let held = render(&mut engine, &mut s, 4_800);
    let level = *held.last().unwrap();
    assert!(level > 0.05, "playing: {level}");

    let old = s.samples.get(&a).unwrap();
    let refs_with_device = Arc::strong_count(&old);
    s.set_device_sample(chain, dev, b.clone());
    let (mut l, mut r) = (vec![0.0; 512], vec![0.0; 512]);
    rlive_testkit::assert_no_alloc(|| engine.process(&mut l, &mut r));
    assert!(
        (l[0] - level * 0.5).abs() < 0.01 && (l[511] - level * 0.5).abs() < 0.01,
        "the voice carries on with the new sample: {} {level}",
        l[0]
    );

    s.engine.collect_garbage();
    assert_eq!(
        Arc::strong_count(&old),
        refs_with_device - 1,
        "the engine's reference came back and was dropped here"
    );
    assert!(matches!(
        &s.project.tracks[0].devices[0].kind,
        DeviceKind::Builtin { sample: Some(p), .. } if *p == b
    ));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_sample_that_will_not_load_leaves_the_current_one_in_place() {
    let (mut engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let chain = ChainRef::Track(t);
    s.add_builtin(chain, "rlive.sampler");
    let dev = s.devices(chain)[1].id;
    s.set_device_sample(chain, dev, PathBuf::from("/nonexistent/x.wav"));
    assert!(s.runtimes[&dev].error.is_some());
    assert!(matches!(
        &s.project.tracks[0].devices[1].kind,
        DeviceKind::Builtin { sample: None, .. }
    ));
    render(&mut engine, &mut s, 512);
}
