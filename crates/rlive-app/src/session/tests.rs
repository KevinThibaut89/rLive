use super::*;
use rlive_core::param::mixer;
use rlive_core::{ChainId, MidiMessage, ParamInfo, ParamKind};
use rlive_engine::create_engine;
use rlive_project::{ArrangementClip, Chain, Clip, RackKind};

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
        engine.process_stereo(l, r);
        s.idle();
    });
    left
}

fn commit(s: &mut Session) {
    s.commit_edits(false);
}

/// Waits for loading samples to finish and hands them over, as the next UI frame would.
fn samples_arrive(s: &mut Session) {
    s.samples.wait_idle();
    s.idle();
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
fn a_knob_move_is_one_step_and_undo_restores_it() {
    let (_engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let clip = Clip::new_midi("x", [1, 2, 3], 1, s.project.time_signature);
    s.set_clip(t, 2, Some(clip));
    commit(&mut s);
    let (dev, param) = poly_param(&s, t);
    // A drag: many values while the pointer is down, one step when it lifts.
    for v in [0.1, 0.2, 0.25] {
        s.set_param(ChainRef::Track(t), dev, param, v);
        s.commit_edits(true);
    }
    commit(&mut s);
    assert_eq!(s.history.undo_label(), Some("Change Parameter"));
    assert!(s.undo());
    assert!(
        !s.project.tracks[0].devices[0].params.contains_key(&param),
        "back to never having been moved"
    );
    assert_eq!(s.history.undo_label(), Some("Edit Clip"));
    assert!(s.redo());
    assert_eq!(s.project.tracks[0].devices[0].params[&param], 0.25);
    assert!(s.undo());
    assert!(s.undo(), "and then the clip edit");
    assert!(!s.undo(), "the gesture was one step");
}

#[test]
fn deleting_a_track_is_undoable_with_its_devices() {
    let (_engine, mut s) = session();
    let (t, survivor) = (s.project.tracks[1].id, s.project.tracks[0].id);
    let shared = s.track_shared[&survivor].clone();
    let (dev, param) = poly_param(&s, t);
    s.set_param(ChainRef::Track(t), dev, param, 0.75);
    s.project.track_mut(t).unwrap().volume_db = -9.0;
    commit(&mut s);
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
fn dirty_clears_at_the_saved_state_including_after_a_knob_move() {
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
    assert!(s.dirty, "the clip edit still needs saving");
    s.undo();
    assert!(
        !s.dirty,
        "undoing the knob and the clip reaches the saved state"
    );
}

fn on(key: u8) -> MidiMessage {
    MidiMessage::NoteOn {
        channel: 0,
        key,
        velocity: 0.8,
        note_id: 0,
        tuning: 0.0,
    }
}

fn off(key: u8) -> MidiMessage {
    MidiMessage::NoteOff {
        channel: 0,
        key,
        velocity: 0.5,
        note_id: 0,
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
    clip.notes_mut()
        .unwrap()
        .push(rlive_project::NoteData::new(0, 3000, 60, 1.0));
    s.arr_place(t, ArrangementClip::new(clip, 3840));
    s.seek(3840.0);
    render(&mut engine, &mut s, 1024);
    s.play();
    let out = render(&mut engine, &mut s, 24_000);
    assert!(out.iter().any(|x| x.abs() > 0.01), "the clip should sound");
}

#[test]
fn a_muted_note_is_silent() {
    let (mut engine, mut s) = session();
    let t = silent_project(&mut s);
    let mut clip = Clip::new_midi("x", [1, 2, 3], 1, s.project.time_signature);
    let mut note = rlive_project::NoteData::new(0, 3000, 60, 1.0);
    note.muted = true;
    clip.notes_mut().unwrap().push(note);
    s.arr_place(t, ArrangementClip::new(clip, 3840));
    s.seek(3840.0);
    render(&mut engine, &mut s, 1024);
    s.play();
    let out = render(&mut engine, &mut s, 24_000);
    assert!(
        out.iter().all(|x| x.abs() < 1e-6),
        "muted notes make no sound"
    );
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
    clip.notes_mut()
        .unwrap()
        .push(rlive_project::NoteData::new(0, 960, 60, 1.0));
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
fn undo_of_a_parameter_goes_down_the_parameter_path_without_a_rebuild() {
    let (mut engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let chain = ChainRef::Track(t);
    let poly = s.devices(chain)[0].id;
    let info = poly_info(&s, poly);
    let cell = s.device_cell(chain, poly, info.id).unwrap();
    let shared = s.track_shared[&t].clone();
    let layout_base = s.layouts[&chain].base(cell).unwrap();
    render(&mut engine, &mut s, 1024);
    let knob = info.from_normalized(0.8);
    s.set_param(chain, poly, info.id, knob);
    commit(&mut s);
    render(&mut engine, &mut s, 2048);
    assert!((s.effective_norm(chain, poly, info.id).unwrap() - 0.8).abs() < 1e-3);

    s.undo();
    assert!(Arc::ptr_eq(&shared, &s.track_shared[&t]), "no rebuild");
    assert_eq!(s.device_cell(chain, poly, info.id), Some(cell));
    assert_eq!(s.layouts[&chain].base(cell), Some(layout_base));
    render(&mut engine, &mut s, 2048);
    assert!(
        (s.effective_norm(chain, poly, info.id).unwrap() as f64 - info.to_normalized(layout_base))
            .abs()
            < 1e-3
    );

    s.redo();
    render(&mut engine, &mut s, 2048);
    assert!((s.effective_norm(chain, poly, info.id).unwrap() - 0.8).abs() < 1e-3);
}

#[test]
fn undo_restores_the_mixer_and_transport_through_the_engine() {
    let (mut engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let chain = ChainRef::Track(t);
    let shared = s.track_shared[&t].clone();
    render(&mut engine, &mut s, 1024);
    {
        let track = s.project.track_mut(t).unwrap();
        track.volume_db = -12.0;
        track.pan = 0.5;
    }
    s.sync_mix(t);
    s.set_master_volume(-6.0);
    s.set_tempo(90.0);
    s.set_metronome(true);
    s.set_quantize(rlive_core::Quantize::Beat);
    commit(&mut s);

    assert!(s.undo());
    assert_eq!(s.project.tracks[0].volume_db, 0.0);
    assert_eq!(s.project.tracks[0].pan, 0.0);
    assert_eq!(s.project.master.volume_db, 0.0);
    assert_eq!(s.project.tempo, 120.0);
    assert!(!s.project.metronome);
    assert_eq!(s.project.quantize, rlive_core::Quantize::Bar);
    assert!(Arc::ptr_eq(&shared, &s.track_shared[&t]), "no rebuild");
    assert_eq!(
        s.layouts[&chain].base(rlive_engine::mix_cell::VOLUME),
        Some(0.0)
    );
    assert_eq!(
        s.layouts[&ChainRef::Master].base(rlive_engine::mix_cell::VOLUME),
        Some(0.0)
    );
    render(&mut engine, &mut s, 512);

    assert!(s.redo());
    assert_eq!(s.project.tracks[0].volume_db, -12.0);
    assert_eq!(s.project.tempo, 90.0);
    assert_eq!(
        s.layouts[&chain].base(rlive_engine::mix_cell::VOLUME),
        Some(-12.0)
    );
}

#[test]
fn undo_of_a_time_signature_change_restores_it() {
    let (_engine, mut s) = session();
    s.set_time_signature(rlive_core::TimeSignature {
        numerator: 3,
        denominator: 4,
    });
    commit(&mut s);
    assert_eq!(s.history.undo_label(), Some("Change Time Signature"));
    s.undo();
    assert_eq!(s.project.time_signature.numerator, 4);
}

#[test]
fn arm_and_expanded_are_not_undoable() {
    let (_engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let chain = ChainRef::Track(t);
    let dev = s.devices(chain)[0].id;
    s.set_arm(t, true, true);
    s.set_device_expanded(chain, dev, false);
    commit(&mut s);
    assert!(s.history.undo_label().is_none(), "neither made a step");
    s.set_bypass(chain, dev, true);
    commit(&mut s);
    assert!(s.project.track(t).unwrap().arm);
    s.undo();
    assert!(s.project.track(t).unwrap().arm, "undo leaves arm alone");
    assert!(!s.devices(chain)[0].expanded, "and the view state");
    assert!(!s.devices(chain)[0].bypass, "but restores the bypass");
    let activator = s.activator_cell(chain, dev).unwrap();
    assert_eq!(s.layouts[&chain].base(activator), Some(1.0));
}

/// A track holding a rack, its chain, and a device inside it.
fn session_with_rack() -> (
    rlive_engine::Engine,
    Session,
    TrackId,
    DeviceId,
    ChainId,
    DeviceId,
) {
    let mut p = Project::new_default();
    let t = p.tracks[0].id;
    let mut rack = Device::rack(RackKind::Instrument, "Rack");
    let rack_id = rack.id;
    let mut chain = Chain::new("A");
    let chain_id = chain.id;
    let inner = Device::builtin("rlive.reverb", "Inner");
    let inner_id = inner.id;
    chain.devices.push(inner);
    chain
        .devices
        .push(Device::builtin("rlive.delay", "Inner FX"));
    if let DeviceKind::Rack { chains, .. } = &mut rack.kind {
        chains.push(chain);
    }
    p.tracks[0].devices.push(rack);
    let (engine, handle) = create_engine(48_000.0, 512);
    let s = Session::new(handle, p, PluginCache::default());
    (engine, s, t, rack_id, chain_id, inner_id)
}

#[test]
fn undo_of_a_nested_chain_edit_restores_it_without_a_rebuild() {
    let (mut engine, mut s, t, rack, chain_id, inner) = session_with_rack();
    let root = ChainRef::Track(t);
    let nested = root.child(rack, chain_id).unwrap();
    let shared = s.track_shared[&t].clone();
    render(&mut engine, &mut s, 1024);
    assert_eq!(s.devices(nested).len(), 2);
    assert!(s.runtimes.contains_key(&inner));

    // A parameter on a nested device, then removing that device.
    let info = s.runtimes[&inner].params[0].clone();
    let value = info.from_normalized(0.7);
    let cell = s.device_cell(nested, inner, info.id).unwrap();
    s.set_param(nested, inner, info.id, value);
    commit(&mut s);
    assert_eq!(s.devices(nested)[0].params[&info.id], value);
    assert_eq!(s.history.undo_label(), Some("Change Parameter"));

    s.remove_device(nested, inner);
    commit(&mut s);
    assert_eq!(s.devices(nested).len(), 1);
    assert!(!s.runtimes.contains_key(&inner));

    assert!(s.undo(), "undo the removal");
    assert_eq!(s.devices(nested).len(), 2);
    assert_eq!(s.devices(nested)[0].id, inner);
    assert_eq!(s.devices(nested)[0].params[&info.id], value);
    assert!(s.runtimes.contains_key(&inner));
    assert!(Arc::ptr_eq(&shared, &s.track_shared[&t]), "no rebuild");

    assert!(s.undo(), "undo the parameter");
    let cell_now = s.device_cell(nested, inner, info.id).unwrap();
    assert_eq!(cell_now, cell, "the nested device kept its cell");
    assert!(!s.devices(nested)[0].params.contains_key(&info.id));
    render(&mut engine, &mut s, 1024);

    assert!(s.redo());
    assert_eq!(s.devices(nested)[0].params[&info.id], value);
    assert!(s.redo());
    assert_eq!(s.devices(nested).len(), 1);
    render(&mut engine, &mut s, 1024);
}

#[test]
fn undo_brings_back_a_rack_chain_with_its_devices() {
    let (mut engine, mut s, t, rack, chain_id, inner) = session_with_rack();
    let root = ChainRef::Track(t);
    let nested = root.child(rack, chain_id).unwrap();
    render(&mut engine, &mut s, 1024);
    s.remove_rack_chain(root, rack, chain_id);
    commit(&mut s);
    assert!(!s.runtimes.contains_key(&inner));
    assert!(s.devices(nested).is_empty());
    render(&mut engine, &mut s, 1024);

    assert!(s.undo());
    assert_eq!(s.devices(nested).len(), 2);
    assert!(s.runtimes.contains_key(&inner));
    assert!(s.device_cell(nested, inner, 0).is_some());
    render(&mut engine, &mut s, 1024);
}

#[test]
fn undo_of_a_nested_chain_setting_changes_only_that_chain() {
    let (mut engine, mut s, t, rack, chain_id, inner) = session_with_rack();
    let root = ChainRef::Track(t);
    render(&mut engine, &mut s, 1024);
    let before = s.devices(root)[1].clone();
    let shared = s.track_shared[&t].clone();
    let rack_ref = s.project.track_mut(t).unwrap().device_mut(rack).unwrap();
    rack_ref
        .chains_mut()
        .find(|c| c.id == chain_id)
        .unwrap()
        .mix
        .volume_db = -3.0;
    commit_direct(&mut s);
    assert_eq!(s.history.undo_label(), Some("Edit"));
    s.undo();
    assert_eq!(s.devices(root)[1], before);
    assert!(Arc::ptr_eq(&shared, &s.track_shared[&t]), "no rebuild");
    assert!(s.runtimes.contains_key(&inner));
}

fn commit_direct(s: &mut Session) {
    s.commit_edits(true);
    s.commit_edits(false);
}

/// A plugin that counts how often the host touches its state.
struct CountingPlugin {
    inner: Box<FakePlugin>,
    loads: Arc<std::sync::atomic::AtomicUsize>,
    saves: Arc<std::sync::atomic::AtomicUsize>,
}

impl rlive_plugin_host::PluginHandle for CountingPlugin {
    fn descriptor(&self) -> &PluginDescriptor {
        self.inner.descriptor()
    }
    fn params(&self) -> Vec<ParamInfo> {
        self.inner.params()
    }
    fn param_value(&self, id: u32) -> Option<f64> {
        self.inner.param_value(id)
    }
    fn format_value(&self, id: u32, v: f64) -> Option<String> {
        self.inner.format_value(id, v)
    }
    fn save_state(&mut self) -> Option<Vec<u8>> {
        self.saves
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(vec![7, 7, 7])
    }
    fn load_state(&mut self, _: &[u8]) -> bool {
        self.loads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
        self.inner.idle()
    }
    fn latency(&self) -> u32 {
        0
    }
}

#[test]
fn undo_does_not_touch_an_unaffected_plugin() {
    let (mut engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let chain = ChainRef::Track(t);
    s.add_builtin(chain, "rlive.reverb");
    let plugin_dev = s.devices(chain)[1].id;
    let params = s.runtimes[&plugin_dev].params.clone();
    let loads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let saves = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    s.runtimes.get_mut(&plugin_dev).unwrap().plugin = Some(Box::new(CountingPlugin {
        inner: FakePlugin::new(params, Vec::new()),
        loads: loads.clone(),
        saves: saves.clone(),
    }));
    s.commit_edits(false);

    // Edit a different device on the same track, undo and redo it.
    let poly = s.devices(chain)[0].id;
    let info = poly_info(&s, poly);
    s.set_param(chain, poly, info.id, info.from_normalized(0.4));
    commit(&mut s);
    s.undo();
    s.redo();
    render(&mut engine, &mut s, 1024);

    use std::sync::atomic::Ordering::Relaxed;
    assert_eq!(loads.load(Relaxed), 0, "state wasn't reloaded");
    assert_eq!(saves.load(Relaxed), 0, "state wasn't read");
    assert!(s.runtimes[&plugin_dev].plugin.is_some());
}

#[test]
fn a_removed_plugin_gets_its_state_back_from_the_parked_blob() {
    let (_engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let chain = ChainRef::Track(t);
    let device = Device::plugin(PluginFormat::Clap, "fake", "/nonexistent".into(), "Fake");
    let dev = device.id;
    s.add_device(chain, None, device);
    s.runtimes.get_mut(&dev).unwrap().plugin = Some(Box::new(CountingPlugin {
        inner: FakePlugin::new(Vec::new(), Vec::new()),
        loads: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        saves: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    }));
    commit(&mut s);
    s.remove_device(chain, dev);
    commit(&mut s);
    assert_eq!(
        s.history.parked_devices[&dev].plugin_state,
        Some(vec![7, 7, 7]),
        "the blob is parked, not in the snapshot"
    );
    assert!(s.undo());
    let restored = s.devices(chain).iter().find(|d| d.id == dev).unwrap();
    assert!(matches!(
        &restored.kind,
        DeviceKind::Plugin { state: Some(blob), .. } if blob == &[7, 7, 7]
    ));
}

#[test]
fn a_plugin_editors_changes_settle_into_one_step_and_undo_restores_them() {
    let (mut engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let chain = ChainRef::Track(t);
    s.add_builtin(chain, "rlive.reverb");
    let dev = s.devices(chain)[1].id;
    let info = s.runtimes[&dev].params[0].clone();
    commit(&mut s);
    let start = s.history.undo_label();

    // A drag in the plugin's own window: values over several frames.
    for v in [0.2, 0.4, 0.6] {
        let value = info.from_normalized(v);
        s.runtimes.get_mut(&dev).unwrap().plugin = Some(FakePlugin::new(
            s.runtimes[&dev].params.clone(),
            vec![HostNotification::ParamChanged { id: info.id, value }],
        ));
        render(&mut engine, &mut s, 512);
        s.commit_edits(false);
    }
    assert_eq!(s.history.undo_label(), start, "still settling");
    for _ in 0..super::undo::SETTLE_FRAMES {
        s.commit_edits(false);
    }
    assert_eq!(s.history.undo_label(), Some("Change Parameter"));
    assert!(s.undo());
    assert!(!s.project.tracks[0].devices[1].params.contains_key(&info.id));
    assert_eq!(s.history.undo_label(), start, "one step for the whole drag");
}

#[test]
fn a_device_repeating_a_value_it_was_given_makes_no_step() {
    let (mut engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let chain = ChainRef::Track(t);
    s.add_builtin(chain, "rlive.reverb");
    let dev = s.devices(chain)[1].id;
    commit(&mut s);
    let info = s.runtimes[&dev].params[0].clone();
    let value = info.from_normalized(0.3);
    s.set_param(chain, dev, info.id, value);
    commit(&mut s);
    s.undo();
    // Undo pushed the old value down; a plugin echoing it back must not count as an edit.
    let old = info.default;
    s.runtimes.get_mut(&dev).unwrap().plugin = Some(FakePlugin::new(
        s.runtimes[&dev].params.clone(),
        vec![HostNotification::ParamChanged {
            id: info.id,
            value: old,
        }],
    ));
    render(&mut engine, &mut s, 512);
    assert!(s.history.pending.is_none(), "an echo isn't an edit");
    assert_eq!(s.history.redo_label(), Some("Change Parameter"));
    render(&mut engine, &mut s, 512);
}

#[test]
fn undoing_a_plugins_first_knob_move_restores_its_loaded_value_not_its_default() {
    let (mut engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let chain = ChainRef::Track(t);
    let device = Device::plugin(PluginFormat::Clap, "fake", "/nonexistent".into(), "Fake");
    let dev = device.id;
    s.add_device(chain, None, device);
    commit(&mut s);
    let info = ParamInfo::continuous(5, "Knob", 0.0, 1.0, 0.25, "");
    s.runtimes.get_mut(&dev).unwrap().params = vec![info.clone()];
    s.refit_device_cells(chain, dev);
    render(&mut engine, &mut s, 512);
    let cell = s.device_cell(chain, dev, 5).unwrap();
    // The value the plugin was seeded with, which isn't its declared default.
    s.layouts.get_mut(&chain).unwrap().note_value(cell, 0.6);
    s.set_param(chain, dev, 5, 0.9);
    commit(&mut s);
    assert!(s.undo());
    assert_eq!(s.layouts[&chain].base(cell), Some(0.6));
    assert!(!s.devices(chain)[1].params.contains_key(&5));
    render(&mut engine, &mut s, 512);
}

#[test]
fn undo_restores_a_samplers_sample() {
    let dir = std::env::temp_dir().join(format!("rlive-undo-sample-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (a, b) = (dir.join("a.wav"), dir.join("b.wav"));
    rlive_devices::write_wav(&a, &[0.5; 4800], &[0.5; 4800], 48_000).unwrap();
    rlive_devices::write_wav(&b, &[0.25; 4800], &[0.25; 4800], 48_000).unwrap();
    let (mut engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let chain = ChainRef::Track(t);
    s.add_builtin(chain, "rlive.sampler");
    let dev = s.devices(chain)[1].id;
    commit(&mut s);
    s.set_device_sample(chain, dev, a.clone());
    samples_arrive(&mut s);
    commit(&mut s);
    render(&mut engine, &mut s, 512);
    s.set_device_sample(chain, dev, b.clone());
    samples_arrive(&mut s);
    commit(&mut s);
    assert_eq!(s.devices(chain)[1].sample(0).unwrap().path, b);
    s.undo();
    assert_eq!(s.devices(chain)[1].sample(0).unwrap().path, a);
    render(&mut engine, &mut s, 512);
    s.undo();
    assert!(s.devices(chain)[1].sample(0).is_none(), "back to no sample");
    render(&mut engine, &mut s, 512);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn a_v1_fixture_loads_into_a_session_and_keeps_working() {
    let (mut engine, mut s) = session();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../rlive-project/tests/fixtures/v1/audio_and_device_state.rlive");
    let dir = std::env::temp_dir().join(format!("rlive-v1-load-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let copy = dir.join("song.rlive");
    std::fs::copy(&path, &copy).unwrap();
    s.load(&copy).unwrap();
    assert_eq!(s.project.version, rlive_project::FORMAT_VERSION);
    assert_eq!(s.project.master.devices.len(), 2);
    assert_eq!(
        s.project.tracks[1].devices[0].sample(0).unwrap().path,
        PathBuf::from("/samples/piano C3.wav")
    );
    render(&mut engine, &mut s, 1024);
    assert!(dir.join("song.v1.bak").exists());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_rack_in_a_project_reaches_the_engine() {
    let (mut engine, mut s, t, rack, chain_id, inner) = session_with_rack();
    let root = ChainRef::Track(t);
    let nested = root.child(rack, chain_id).unwrap();
    assert_eq!(s.devices(nested).len(), 2);
    assert_eq!(s.chain_of_device(inner), Some(nested));
    // The chain's cells live on the track's bus.
    let info = s.runtimes[&inner].params[0].clone();
    let value = info.from_normalized(0.5);
    s.set_param(nested, inner, info.id, value);
    render(&mut engine, &mut s, 2048);
    assert!((s.effective_norm(nested, inner, info.id).unwrap() - 0.5).abs() < 1e-3);
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
    samples_arrive(&mut s);
    render(&mut engine, &mut s, 512);

    let mut midi = s.engine.take_midi_sender().unwrap();
    assert!(midi.send(
        t,
        MidiMessage::NoteOn {
            channel: 0,
            key: 60,
            velocity: 1.0,
            note_id: 0,
            tuning: 0.0,
        }
    ));
    let held = render(&mut engine, &mut s, 4_800);
    let level = *held.last().unwrap();
    assert!(level > 0.05, "playing: {level}");

    let old = s.samples.get(&a).unwrap();
    let refs_with_device = Arc::strong_count(&old);
    s.set_device_sample(chain, dev, b.clone());
    samples_arrive(&mut s);
    let (mut l, mut r) = (vec![0.0; 512], vec![0.0; 512]);
    rlive_testkit::assert_no_alloc(|| engine.process_stereo(&mut l, &mut r));
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
    assert_eq!(s.project.tracks[0].devices[0].sample(0).unwrap().path, b);
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
    samples_arrive(&mut s);
    assert!(s.runtimes[&dev].error.is_some());
    assert!(s.project.tracks[0].devices[1].sample(0).is_none());
    render(&mut engine, &mut s, 512);
}

// ------------------------------------------------------------- device views

#[test]
fn only_devices_whose_layout_reads_one_get_a_view_channel() {
    let (_engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let chain = ChainRef::Track(t);
    let plain = s.devices(chain)[0].id;
    s.add_builtin(chain, "rlive.compressor");
    s.add_builtin(chain, "rlive.cc_control");
    let find = |name: &str| {
        let found = s.devices(chain).iter().find(|d| d.name == name);
        found.expect(name).id
    };
    let (comp, stub) = (find("Compressor"), find("CC Control"));

    assert!(s.runtimes[&plain].view.is_none(), "auto layout");
    assert!(s.runtimes[&stub].view.is_none(), "stub");
    let channel = s.runtimes[&comp].view.as_ref().expect("compressor");
    assert_eq!(channel.len(), rlive_devices::effects::comp::VIEW_LEN);
}

#[test]
fn a_racks_view_channel_is_the_device_inside_it() {
    let (_engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let chain = ChainRef::Track(t);
    s.add_device(chain, None, Device::rack(RackKind::Audio, "Rack"));
    let rack = s.devices(chain)[1].id;
    let mut c = Chain::new("A");
    let comp = Device::builtin("rlive.compressor", "Compressor");
    let comp_id = comp.id;
    c.devices.push(comp);
    s.insert_rack_chain(chain, rack, 0, c);
    assert!(s.runtimes[&comp_id].view.is_some());
    assert!(
        s.runtimes[&rack].view.is_none(),
        "the rack itself shows nothing"
    );
}

#[test]
fn a_compressor_publishes_to_its_panel_without_allocating() {
    use rlive_core::ViewId;
    use rlive_devices::effects::comp;
    let (mut engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let chain = ChainRef::Track(t);
    s.add_builtin(chain, "rlive.compressor");
    let comp_id = s.devices(chain)[1].id;
    render(&mut engine, &mut s, 1_024);

    let reader = s.runtimes[&comp_id].view.as_ref().unwrap().attach();
    s.play_midi(t, on(60));
    for _ in 0..20 {
        let (mut l, mut r) = ([0.0; 512], [0.0; 512]);
        rlive_testkit::assert_no_alloc(|| engine.process_stereo(&mut l, &mut r));
        s.idle();
    }

    let mut view = [0.0; comp::VIEW_LEN];
    assert!(reader.read(ViewId(0), &mut view));
    assert!(view[1] > 0.0 && view[2] > 0.0, "output level: {view:?}");
    assert!(view[0] > 0.0, "gain reduction: {view:?}");
}
