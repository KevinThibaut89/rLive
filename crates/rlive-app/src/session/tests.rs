use super::*;
use rlive_core::MidiMessage;
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
