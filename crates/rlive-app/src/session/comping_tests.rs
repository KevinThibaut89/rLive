//! Take lanes, comps and linked tracks through the session: what reaches the engine, and undo.

use super::*;
use rlive_core::{SampleBuffer, PPQ};
use rlive_engine::{create_engine, Engine, EngineHandle};
use rlive_project::takes::TakeStep;
use rlive_project::{ArrangementClip, Clip, ClipContent, TakeLane};

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;
const BAR: u64 = 4 * PPQ;
/// One bar of 4/4 at the default 120 BPM, in frames.
const BAR_FRAMES: usize = 96_000;

fn session() -> (Engine, Session) {
    let (engine, handle): (Engine, EngineHandle) = create_engine(SR, BLOCK);
    let s = Session::new(handle, Project::empty(), PluginCache::default());
    (engine, s)
}

fn commit(s: &mut Session) {
    s.commit_edits(false);
}

fn render(engine: &mut Engine, s: &mut Session, frames: usize) -> Vec<f32> {
    let frames = frames.next_multiple_of(BLOCK);
    let (left, _) = rlive_testkit::render_chunks(frames, BLOCK, |l, r| {
        engine.process_stereo(l, r);
        s.idle();
    });
    left
}

/// Installs a constant-level sample so what plays says which clip it came from.
fn install(s: &mut Session, name: &str, level: f32) -> PathBuf {
    let path = PathBuf::from(format!("/tmp/{name}.wav"));
    let n = BAR_FRAMES * 4;
    s.samples.install(
        &path,
        Arc::new(SampleBuffer {
            name: name.into(),
            left: vec![level; n],
            right: vec![level; n],
            sample_rate: SR,
        }),
    );
    path
}

fn audio_clip(path: &Path, bars: u64) -> Clip {
    Clip {
        name: "c".into(),
        length: bars * BAR,
        looped: false,
        content: ClipContent::audio(path, 0.0),
        ..Clip::default()
    }
}

fn placed(path: &Path, start: u64, bars: u64) -> ArrangementClip {
    ArrangementClip {
        length: bars * BAR,
        ..ArrangementClip::new(audio_clip(path, bars), start)
    }
}

fn audio_track(s: &mut Session) -> TrackId {
    s.add_track(TrackKind::Audio, None)
}

/// A track with a loud clip on its main lane and quiet ones on two take lanes.
fn comp_track(s: &mut Session, t: TrackId) {
    let main = install(s, "main", 0.75);
    let a = install(s, "a", 0.1);
    let b = install(s, "b", 0.4);
    let tr = s.project.track_mut(t).unwrap();
    tr.place_clip(placed(&main, 0, 2));
    tr.take_lanes.push(TakeLane {
        name: "Take 1".into(),
        color: None,
        clips: vec![placed(&a, 0, 2)],
    });
    tr.take_lanes.push(TakeLane {
        name: "Take 2".into(),
        color: None,
        clips: vec![placed(&b, 0, 2)],
    });
    s.sync_lane(t);
    commit(s);
}

fn midi_track(s: &mut Session) -> TrackId {
    s.add_track(TrackKind::Midi, None)
}

fn on(key: u8) -> rlive_core::MidiMessage {
    rlive_core::MidiMessage::NoteOn {
        channel: 0,
        key,
        velocity: 0.8,
        note_id: 0,
        tuning: 0.0,
    }
}

fn off(key: u8) -> rlive_core::MidiMessage {
    rlive_core::MidiMessage::NoteOff {
        channel: 0,
        key,
        velocity: 0.5,
        note_id: 0,
    }
}

fn file_of(c: &ArrangementClip) -> PathBuf {
    match &c.clip.content {
        ClipContent::Audio { file, .. } => file.path.clone(),
        _ => panic!("audio clip"),
    }
}

/// The level of the sound after the short run-in fade.
fn level_of(out: &[f32]) -> f32 {
    let at = out.len() / 2;
    out[at].abs()
}

#[test]
fn only_the_auditioned_lane_is_heard_while_audition_lasts() {
    let (mut engine, mut s) = session();
    let t = audio_track(&mut s);
    comp_track(&mut s, t);
    s.play();
    let out = render(&mut engine, &mut s, BAR_FRAMES / 2);
    assert!(
        (level_of(&out) - 0.75).abs() < 0.05,
        "main lane: {}",
        level_of(&out)
    );

    s.toggle_audition(t, 0);
    let out = render(&mut engine, &mut s, BAR_FRAMES / 2);
    assert!(
        (level_of(&out) - 0.1).abs() < 0.05,
        "take 1: {}",
        level_of(&out)
    );
    assert_eq!(s.audition_of(t), Some(0));

    s.toggle_audition(t, 1);
    let out = render(&mut engine, &mut s, BAR_FRAMES / 2);
    assert!(
        (level_of(&out) - 0.4).abs() < 0.05,
        "one lane per track: {}",
        level_of(&out)
    );
    assert_eq!(s.audition_of(t), Some(1));

    s.toggle_audition(t, 1);
    let out = render(&mut engine, &mut s, BAR_FRAMES / 2);
    assert!(
        (level_of(&out) - 0.75).abs() < 0.05,
        "back: {}",
        level_of(&out)
    );
    assert_eq!(s.audition_of(t), None);
}

#[test]
fn a_take_lane_is_silent_unless_auditioned() {
    let (mut engine, mut s) = session();
    let t = audio_track(&mut s);
    let quiet = install(&mut s, "only", 0.5);
    let tr = s.project.track_mut(t).unwrap();
    tr.take_lanes.push(TakeLane {
        name: "Take 1".into(),
        color: None,
        clips: vec![placed(&quiet, 0, 2)],
    });
    s.sync_lane(t);
    s.play();
    let out = render(&mut engine, &mut s, BAR_FRAMES / 2);
    assert!(out.iter().all(|x| x.abs() < 1e-6), "take lanes don't sound");
}

#[test]
fn the_audition_survives_the_project_being_rebuilt() {
    // What is sent on load comes through the builder, and it plays the auditioned lane too.
    let (mut engine, mut s) = session();
    let t = audio_track(&mut s);
    comp_track(&mut s, t);
    s.project.track_mut(t).unwrap().ui.audition_lane = Some(1);
    let dir = std::env::temp_dir().join(format!("rlive-rebuild-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("t.rlive");
    s.save(&path).unwrap();
    s.load(&path).unwrap();
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(s.audition_of(t), Some(1));
    let main = PathBuf::from("/tmp/b.wav");
    s.samples.wait_idle();
    s.samples.install(
        &main,
        Arc::new(SampleBuffer {
            name: "b".into(),
            left: vec![0.4; BAR_FRAMES * 4],
            right: vec![0.4; BAR_FRAMES * 4],
            sample_rate: SR,
        }),
    );
    s.sync_lane(t);
    s.play();
    let out = render(&mut engine, &mut s, BAR_FRAMES / 2);
    assert!(
        (level_of(&out) - 0.4).abs() < 0.05,
        "take 2: {}",
        level_of(&out)
    );
}

#[test]
fn undo_takes_audition_away_with_the_lane_it_was_on() {
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    comp_track(&mut s, t);
    let n = s.insert_take_lane(&[t], None);
    assert_eq!(n, vec![(t, 2)]);
    commit(&mut s);
    s.toggle_audition(t, 2);
    assert_eq!(s.audition_of(t), Some(2));
    assert!(s.undo());
    assert_eq!(s.project.track(t).unwrap().take_lanes.len(), 2);
    assert_eq!(s.audition_of(t), None);
}

#[test]
fn copying_a_selection_writes_the_region_as_its_own_clip() {
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    comp_track(&mut s, t);
    assert!(s.comp_selection(&[(t, 1)], BAR / 2, BAR));
    let lane = &s.project.track(t).unwrap().arrangement;
    let spans: Vec<(u64, u64, u64)> = lane.iter().map(|c| (c.start, c.length, c.offset)).collect();
    assert_eq!(
        spans,
        vec![
            (0, BAR / 2, 0),
            (BAR / 2, BAR / 2, BAR / 2),
            (BAR, BAR, BAR)
        ]
    );
    assert_eq!(file_of(&lane[1]), PathBuf::from("/tmp/b.wav"));
    assert_eq!(file_of(&lane[2]), PathBuf::from("/tmp/main.wav"));
    let take_ids: Vec<_> = s.project.track(t).unwrap().take_lanes[1]
        .clips
        .iter()
        .map(|c| c.id)
        .collect();
    assert!(!take_ids.contains(&lane[1].id), "an independent copy");
}

#[test]
fn a_comp_is_heard_where_it_was_made_and_crossfades_at_its_splits() {
    let (mut engine, mut s) = session();
    let t = audio_track(&mut s);
    comp_track(&mut s, t);
    s.set_edge_fades(true);
    s.comp_selection(&[(t, 1)], BAR / 2, BAR);
    commit(&mut s);
    let edge = crate::session::edge_fade_ticks(s.project.tempo);
    let fades = s.project.track(t).unwrap().effective_fades(Some(edge));
    assert_eq!(
        fades[0].tail,
        edge / 2,
        "default crossfade across the split"
    );
    assert_eq!(fades[1].fade_in, edge / 2);
    s.play();
    let out = render(&mut engine, &mut s, BAR_FRAMES);
    let at = |frac: f64| out[(frac * out.len() as f64) as usize].abs();
    assert!((at(0.25) - 0.75).abs() < 0.05);
    assert!((at(0.75) - 0.4).abs() < 0.05);
}

#[test]
fn next_and_previous_take_replace_the_main_lane_and_undo_in_one_step() {
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    comp_track(&mut s, t);
    s.comp_selection(&[(t, 0)], 0, BAR);
    commit(&mut s);
    assert_eq!(s.step_take(t, None, 0, BAR, TakeStep::Previous), Some(1));
    commit(&mut s);
    let main = |s: &Session| file_of(&s.project.track(t).unwrap().arrangement[0]);
    assert_eq!(main(&s), PathBuf::from("/tmp/b.wav"));
    assert_eq!(s.step_take(t, None, 0, BAR, TakeStep::Next), Some(0));
    commit(&mut s);
    assert_eq!(main(&s), PathBuf::from("/tmp/a.wav"));
    assert!(s.undo());
    assert_eq!(main(&s), PathBuf::from("/tmp/b.wav"));
    assert!(s.undo());
    assert_eq!(main(&s), PathBuf::from("/tmp/a.wav"));
}

#[test]
fn next_take_on_selected_clips_works_clip_by_clip() {
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    comp_track(&mut s, t);
    s.comp_selection(&[(t, 0)], 0, BAR);
    let id = s.project.track(t).unwrap().arrangement[0].id;
    assert!(s.step_take_clips(&[(t, id)], TakeStep::Previous));
    let lane = &s.project.track(t).unwrap().arrangement;
    assert_eq!(file_of(&lane[0]), PathBuf::from("/tmp/b.wav"));
    assert_eq!(lane[0].length, BAR);
}

#[test]
fn every_lane_edit_undoes_in_one_step() {
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    comp_track(&mut s, t);
    let lanes = |s: &Session| {
        s.project
            .track(t)
            .unwrap()
            .take_lanes
            .iter()
            .map(|l| l.name.clone())
            .collect::<Vec<_>>()
    };
    let start = lanes(&s);
    for edit in 0..6 {
        match edit {
            0 => {
                s.insert_take_lane(&[t], None);
            }
            1 => {
                s.duplicate_take_lane(t, 0);
            }
            2 => s.delete_take_lane(t, 0),
            3 => s.rename_take_lane(t, 0, "Verse".into()),
            4 => {
                s.move_take_lane(t, 0, 1);
            }
            _ => s.delete_all_take_lanes(t),
        }
        commit(&mut s);
        assert_ne!(lanes(&s), start, "edit {edit}");
        assert!(s.undo(), "edit {edit}");
        assert_eq!(lanes(&s), start, "edit {edit} undid in one step");
    }
    s.comp_selection(&[(t, 0)], 0, BAR);
    commit(&mut s);
    s.delete_unused_take_lanes(t);
    commit(&mut s);
    assert_eq!(lanes(&s), vec!["Take 1".to_string()]);
    assert!(s.undo());
    assert_eq!(lanes(&s).len(), 2);
}

#[test]
fn moving_a_clip_on_a_take_lane_keeps_it_there_and_undoes() {
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    comp_track(&mut s, t);
    let id = s.project.track(t).unwrap().take_lanes[0].clips[0].id;
    s.arr_move(
        &[ArrMove {
            track: t,
            id,
            to_track: t,
            start: BAR,
        }],
        false,
    );
    commit(&mut s);
    let tr = s.project.track(t).unwrap();
    assert_eq!(tr.take_lanes[0].clips[0].start, BAR);
    assert_eq!(tr.arrangement[0].start, 0, "the main lane is another lane");
    assert!(s.undo());
    assert_eq!(s.project.track(t).unwrap().take_lanes[0].clips[0].start, 0);
}

#[test]
fn a_take_lane_clip_can_be_moved_to_the_main_lane() {
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    comp_track(&mut s, t);
    let id = s.project.track(t).unwrap().take_lanes[0].clips[0].id;
    s.arr_move_to_lanes(
        &[ArrMove {
            track: t,
            id,
            to_track: t,
            start: 0,
        }],
        &[None],
        false,
    );
    let tr = s.project.track(t).unwrap();
    assert!(tr.take_lanes[0].clips.is_empty());
    assert_eq!(file_of(&tr.arrangement[0]), PathBuf::from("/tmp/a.wav"));
}

#[test]
fn deleting_splitting_and_trimming_work_on_take_lane_clips() {
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    comp_track(&mut s, t);
    s.arr_split_lanes(&[(t, Some(0))], &[BAR]);
    {
        let tr = s.project.track(t).unwrap();
        assert_eq!(tr.take_lanes[0].clips.len(), 2);
        assert_eq!(tr.arrangement.len(), 1, "the main lane isn't touched");
    }
    let id = s.project.track(t).unwrap().take_lanes[0].clips[1].id;
    s.arr_trim(t, id, TrimEdge::Right(BAR + 960));
    assert_eq!(
        s.project.track(t).unwrap().take_lanes[0].clips[1].length,
        960
    );
    s.arr_delete(&[(t, id)]);
    assert_eq!(s.project.track(t).unwrap().take_lanes[0].clips.len(), 1);
}

#[test]
fn deleted_time_takes_its_take_lanes_with_it() {
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    comp_track(&mut s, t);
    s.delete_time(BAR / 2, BAR);
    commit(&mut s);
    let tr = s.project.track(t).unwrap();
    assert_eq!(
        tr.take_lanes[1].clips.iter().map(|c| c.length).sum::<u64>(),
        2 * BAR - BAR / 2
    );
    assert!(s.undo());
    assert_eq!(
        s.project.track(t).unwrap().take_lanes[1]
            .clips
            .iter()
            .map(|c| c.length)
            .sum::<u64>(),
        2 * BAR
    );
}

/// Two audio tracks with the same material, linked.
fn linked_pair(s: &mut Session) -> (TrackId, TrackId) {
    let a = audio_track(s);
    let b = audio_track(s);
    comp_track(s, a);
    comp_track(s, b);
    assert!(s.link_tracks(&[a, b]));
    commit(s);
    (a, b)
}

fn starts(s: &Session, t: TrackId) -> Vec<u64> {
    s.project
        .track(t)
        .unwrap()
        .arrangement
        .iter()
        .map(|c| c.start)
        .collect()
}

#[test]
fn moving_a_clip_on_a_linked_track_moves_its_partner() {
    let (_e, mut s) = session();
    let (a, b) = linked_pair(&mut s);
    let id = s.project.track(a).unwrap().arrangement[0].id;
    s.arr_move(
        &[ArrMove {
            track: a,
            id,
            to_track: a,
            start: BAR,
        }],
        false,
    );
    commit(&mut s);
    assert_eq!(starts(&s, a), vec![BAR]);
    assert_eq!(starts(&s, b), vec![BAR]);
    assert!(s.undo(), "one step");
    assert_eq!(starts(&s, a), vec![0]);
    assert_eq!(starts(&s, b), vec![0]);
}

#[test]
fn trimming_splitting_and_deleting_stay_in_step_across_linked_tracks() {
    let (_e, mut s) = session();
    let (a, b) = linked_pair(&mut s);
    let id = s.project.track(a).unwrap().arrangement[0].id;
    s.arr_trim(a, id, TrimEdge::Right(BAR));
    assert_eq!(s.project.track(b).unwrap().arrangement[0].length, BAR);
    s.arr_split(&[a], &[BAR / 2]);
    assert_eq!(starts(&s, b), vec![0, BAR / 2]);
    let second = s.project.track(a).unwrap().arrangement[1].id;
    s.arr_delete(&[(a, second)]);
    assert_eq!(starts(&s, b), vec![0]);
}

#[test]
fn a_comp_on_a_linked_track_comes_out_the_same_on_the_others() {
    let (_e, mut s) = session();
    let (a, b) = linked_pair(&mut s);
    assert!(s.comp_selection(&[(a, 1)], BAR / 2, BAR));
    let lane_b = &s.project.track(b).unwrap().arrangement;
    let spans: Vec<(u64, u64)> = lane_b.iter().map(|c| (c.start, c.length)).collect();
    assert_eq!(spans, vec![(0, BAR / 2), (BAR / 2, BAR / 2), (BAR, BAR)]);
    assert_eq!(file_of(&lane_b[1]), PathBuf::from("/tmp/b.wav"));
}

#[test]
fn stepping_a_linked_track_steps_the_others_to_the_same_lane() {
    let (_e, mut s) = session();
    let (a, b) = linked_pair(&mut s);
    s.comp_selection(&[(a, 0)], 0, BAR);
    assert_eq!(s.step_take(a, None, 0, BAR, TakeStep::Previous), Some(1));
    assert_eq!(
        file_of(&s.project.track(b).unwrap().arrangement[0]),
        PathBuf::from("/tmp/b.wav")
    );
}

#[test]
fn lane_management_and_audition_follow_linked_tracks() {
    let (_e, mut s) = session();
    let (a, b) = linked_pair(&mut s);
    let made = s.insert_take_lane(&[a], None);
    assert_eq!(made.len(), 2, "both tracks got a lane");
    let name =
        |s: &Session, t: TrackId, i: usize| s.project.track(t).unwrap().take_lanes[i].name.clone();
    s.rename_take_lane(a, 2, "Chorus".into());
    assert_eq!(name(&s, b, 2), "Chorus");
    s.set_audition(a, Some(1));
    assert_eq!(s.audition_of(b), Some(1));
    s.delete_take_lane(b, 2);
    assert_eq!(s.project.track(a).unwrap().take_lanes.len(), 2);
    s.set_audition(a, None);
    assert_eq!(s.audition_of(b), None);
}

#[test]
fn arming_a_linked_track_arms_its_partners_and_nothing_else() {
    let (_e, mut s) = session();
    let (a, b) = linked_pair(&mut s);
    let c = audio_track(&mut s);
    s.set_arm(a, true, true);
    assert!(s.project.track(a).unwrap().arm);
    assert!(s.project.track(b).unwrap().arm, "linked");
    assert!(!s.project.track(c).unwrap().arm);
    s.set_arm(b, false, false);
    assert!(!s.project.track(a).unwrap().arm);
}

#[test]
fn only_fades_that_start_together_move_together() {
    let (_e, mut s) = session();
    let (a, b) = linked_pair(&mut s);
    s.project.track_mut(b).unwrap().arrangement[0]
        .clip
        .fades
        .fade_out = 240;
    let id = s.project.track(a).unwrap().arrangement[0].id;
    s.arr_set_fade(a, id, FadeSide::In, 480, None);
    assert_eq!(
        s.project.track(b).unwrap().arrangement[0]
            .clip
            .fades
            .fade_in,
        480
    );
    // The fade outs don't start at the same place: this one starts at the clip's end.
    let end = s.project.track(a).unwrap().arrangement[0].length;
    s.arr_set_fade(a, id, FadeSide::Out, 960, None);
    let b_fades = s.project.track(b).unwrap().arrangement[0].clip.fades;
    assert_eq!(b_fades.fade_out, 240, "left alone");
    let a_fades = s.project.track(a).unwrap().arrangement[0].clip.fades;
    assert_eq!(a_fades.fade_out, 960, "{end}");
}

#[test]
fn linked_tracks_undo_and_unlink_as_one_step() {
    let (_e, mut s) = session();
    let (a, b) = linked_pair(&mut s);
    assert!(s.unlink_tracks(&[a]));
    commit(&mut s);
    assert!(s.project.arrangement.linked_tracks.is_empty());
    assert!(s.undo());
    assert_eq!(s.project.linked_with(b), vec![a, b]);
    // Deleting a linked track drops it from the instance, and undo puts it back in.
    s.delete_track(a);
    commit(&mut s);
    assert!(s.project.arrangement.linked_tracks.is_empty());
    assert!(s.undo());
    assert_eq!(s.project.linked_with(b).len(), 2);
}

#[test]
fn a_link_group_links_its_members() {
    let (_e, mut s) = session();
    let a = audio_track(&mut s);
    let b = audio_track(&mut s);
    let group = s.group_tracks(&[a, b]).unwrap();
    assert!(s.link_group_tracks(group));
    assert_eq!(s.project.linked_with(a), vec![a, b]);
}

#[test]
fn takes_lanes_and_links_round_trip_through_a_saved_project() {
    let (_e, mut s) = session();
    let (a, b) = linked_pair(&mut s);
    let dir = std::env::temp_dir().join(format!("rlive-comp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    s.project.track_mut(a).unwrap().ui.audition_lane = Some(1);
    s.project.track_mut(a).unwrap().ui.show_take_lanes = true;
    let path = dir.join("t.rlive");
    s.project.save(&path).unwrap();
    let loaded = Project::load(&path).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(loaded.linked_with(a), vec![a, b]);
    let t = loaded.track(a).unwrap();
    assert_eq!(t.take_lanes.len(), 2);
    assert_eq!(t.take_lanes[1].clips[0].length, 2 * BAR);
    assert_eq!(t.ui.audition_lane, Some(1));
    assert!(t.ui.show_take_lanes);
    assert_eq!(loaded.track(b).unwrap().take_lanes.len(), 2);
}

#[test]
fn a_project_without_takes_plays_the_main_lane_untouched() {
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    let main = install(&mut s, "m", 0.5);
    s.arr_place(t, placed(&main, 0, 1));
    let tr = s.project.track(t).unwrap();
    assert_eq!(tr.playing_lane().as_ptr(), tr.arrangement.as_ptr());
}

#[test]
fn a_midi_loop_recording_makes_a_take_for_each_pass() {
    let (mut engine, mut s) = session();
    let t = midi_track(&mut s);
    s.set_arm(t, true, true);
    s.set_loop(true, 0, BAR);
    commit(&mut s);
    assert!(s.start_recording());
    render(&mut engine, &mut s, 4_096);
    s.play_midi(t, on(60));
    render(&mut engine, &mut s, 4_096);
    s.play_midi(t, off(60));
    render(&mut engine, &mut s, BAR_FRAMES); // past the wrap
    s.play_midi(t, on(62));
    render(&mut engine, &mut s, 4_096);
    s.play_midi(t, off(62));
    render(&mut engine, &mut s, BAR_FRAMES); // and another wrap, with nothing played
    s.play_midi(t, on(64));
    render(&mut engine, &mut s, 4_096);
    s.play_midi(t, off(64));
    render(&mut engine, &mut s, 4_096);
    s.stop();
    render(&mut engine, &mut s, 1_024);
    let tr = s.project.track(t).unwrap();
    let keys = |c: &ArrangementClip| c.clip.notes().iter().map(|n| n.key).collect::<Vec<_>>();
    assert_eq!(tr.take_lanes.len(), 3, "one lane per pass with notes");
    assert_eq!(keys(&tr.take_lanes[0].clips[0]), vec![60]);
    assert_eq!(keys(&tr.take_lanes[1].clips[0]), vec![62]);
    assert_eq!(keys(&tr.take_lanes[2].clips[0]), vec![64]);
    for lane in &tr.take_lanes {
        assert_eq!(lane.clips[0].start, 0, "each takes its pass of the loop");
    }
    assert_eq!(tr.arrangement.len(), 1, "the last pass is on the main lane");
    assert_eq!(keys(&tr.arrangement[0]), vec![64]);
    assert_ne!(tr.arrangement[0].id, tr.take_lanes[2].clips[0].id);
    assert!(s.undo());
    assert_eq!(s.project.track(t).unwrap().take_lanes.len(), 2);
    let main_keys = keys(&s.project.track(t).unwrap().arrangement[0]);
    assert_eq!(main_keys, vec![62], "each pass one undo step");
    s.undo();
    s.undo();
    assert!(s.project.track(t).unwrap().take_lanes.is_empty());
    assert!(s.project.track(t).unwrap().arrangement.is_empty());
}

#[test]
fn recording_again_over_a_take_starts_a_new_lane_and_later_material_reuses_one() {
    let (mut engine, mut s) = session();
    let t = midi_track(&mut s);
    s.set_arm(t, true, true);
    let take = |s: &mut Session, engine: &mut Engine, at: f64, key: u8| {
        s.seek(at);
        render(engine, s, 512);
        assert!(s.start_recording());
        render(engine, s, 2_048);
        s.play_midi(t, on(key));
        render(engine, s, 2_048);
        s.play_midi(t, off(key));
        render(engine, s, 2_048);
        s.stop();
        render(engine, s, 2_048);
    };
    take(&mut s, &mut engine, 0.0, 60);
    take(&mut s, &mut engine, 0.0, 62);
    assert_eq!(s.project.track(t).unwrap().take_lanes.len(), 2, "over it");
    take(&mut s, &mut engine, (BAR * 4) as f64, 64);
    let tr = s.project.track(t).unwrap();
    assert_eq!(
        tr.take_lanes.len(),
        2,
        "nothing after there on Take 1: reused"
    );
    assert_eq!(tr.take_lanes[0].clips.len(), 2);
}

#[test]
fn overdub_still_merges_into_one_clip_and_makes_no_takes() {
    let (mut engine, mut s) = session();
    let t = midi_track(&mut s);
    s.set_arm(t, true, true);
    s.set_loop(true, 0, BAR);
    s.set_midi_arrangement_overdub(true);
    commit(&mut s);
    assert!(s.start_recording());
    render(&mut engine, &mut s, 4_096);
    s.play_midi(t, on(60));
    render(&mut engine, &mut s, 4_096);
    s.play_midi(t, off(60));
    render(&mut engine, &mut s, 4_096);
    s.stop();
    render(&mut engine, &mut s, 1_024);
    let tr = s.project.track(t).unwrap();
    assert!(tr.take_lanes.is_empty());
    assert_eq!(tr.arrangement.len(), 1);
}

#[test]
fn takes_colour_by_the_random_clip_color_setting() {
    let (mut engine, mut s) = session();
    s.colors.new_clip = crate::settings::ClipColor::Random;
    let t = midi_track(&mut s);
    s.set_arm(t, true, true);
    s.set_loop(true, 0, BAR);
    commit(&mut s);
    assert!(s.start_recording());
    render(&mut engine, &mut s, 2_048);
    s.play_midi(t, on(60));
    render(&mut engine, &mut s, 2_048);
    s.play_midi(t, off(60));
    render(&mut engine, &mut s, BAR_FRAMES);
    s.play_midi(t, on(62));
    render(&mut engine, &mut s, 2_048);
    s.play_midi(t, off(62));
    s.stop();
    render(&mut engine, &mut s, 1_024);
    let tr = s.project.track(t).unwrap();
    assert_ne!(
        tr.take_lanes[0].clips[0].clip.color, tr.take_lanes[1].clips[0].clip.color,
        "each take gets its own colour"
    );
}

fn midi_placed(key: u8, beats: u64) -> ArrangementClip {
    let mut clip = Clip::new_midi("m", [1, 2, 3], 1, Default::default());
    clip.notes_mut()
        .unwrap()
        .push(rlive_project::NoteData::new(0, PPQ / 2, key, 0.9));
    ArrangementClip {
        length: beats * PPQ,
        ..ArrangementClip::new(clip, 0)
    }
}

fn settle(engine: &mut Engine, s: &mut Session) {
    render(engine, s, BLOCK * 8);
    s.samples.wait_idle();
    render(engine, s, BLOCK * 8);
}

#[test]
fn a_take_lane_consolidates_in_place_and_linked_tracks_follow() {
    let (mut engine, mut s) = session();
    let (a, b) = (midi_track(&mut s), midi_track(&mut s));
    for (t, key) in [(a, 60), (b, 64)] {
        let tr = s.project.track_mut(t).unwrap();
        tr.insert_take_lane(None);
        let mut clip = midi_placed(key, 1);
        clip.start = PPQ;
        tr.take_lanes[0].clips.push(clip);
    }
    assert!(s.link_tracks(&[a, b]));
    s.consolidate_lanes(&[(a, Some(0))], 0, 4 * PPQ).unwrap();
    s.finish_render().unwrap();
    settle(&mut engine, &mut s);
    let tr = s.project.track(a).unwrap();
    let c = &tr.take_lanes[0].clips[0];
    assert_eq!((c.start, c.length), (0, 4 * PPQ), "a clip on the take lane");
    assert!(tr.arrangement.is_empty(), "nothing went on the main lane");
    let other = &s.project.track(b).unwrap().take_lanes[0].clips[0];
    assert_eq!(other.length, 4 * PPQ, "the linked track consolidated too");
}

#[test]
fn a_take_lane_clip_can_be_sent_to_the_session() {
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    comp_track(&mut s, t);
    let take = s.project.track(t).unwrap().take_lanes[1].clips[0].clone();
    let placed = s.paste_arrangement_clips(&[(t, take)], 0);
    assert_eq!(placed, vec![(t, 0)]);
    let slot = s.clip(t, 0).unwrap();
    match &slot.content {
        ClipContent::Audio { file, .. } => assert_eq!(file.path, PathBuf::from("/tmp/b.wav")),
        _ => panic!(),
    }
}

#[test]
fn copy_time_and_undo_cover_take_lanes() {
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    comp_track(&mut s, t);
    let clip = s.copy_time(0, BAR);
    assert_eq!(clip.takes.len(), 2);
    s.delete_time(0, BAR);
    commit(&mut s);
    s.paste_time(0, &clip);
    commit(&mut s);
    let tr = s.project.track(t).unwrap();
    assert_eq!(
        tr.take_lanes[0].clips.iter().map(|c| c.length).sum::<u64>(),
        2 * BAR
    );
    assert!(s.undo());
    assert_eq!(
        s.project.track(t).unwrap().take_lanes[0]
            .clips
            .iter()
            .map(|c| c.length)
            .sum::<u64>(),
        BAR
    );
}
