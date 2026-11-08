//! Launch settings, scenes and Arrangement Record, through a session.

use super::*;
use rlive_core::{FollowAction, LaunchMode, Quantize, TimeSignature, PPQ};
use rlive_engine::create_engine;
use rlive_engine::shared::NO_SLOT;
use rlive_project::{ArrangementClip, Clip, ClipContent};
use std::sync::atomic::{AtomicUsize, Ordering};

const BAR: u64 = 4 * PPQ;
/// 120 BPM at 48 kHz.
const BAR_FRAMES: usize = 96_000;
const BEAT_FRAMES: usize = 24_000;

fn session() -> (rlive_engine::Engine, Session) {
    let (engine, handle) = create_engine(48_000.0, 512);
    let mut s = Session::new(handle, Project::new_default(), PluginCache::default());
    // New scenes and tracks come and go in these tests; start from an empty grid.
    for t in s.project.tracks.iter_mut() {
        t.clips.iter_mut().for_each(|c| *c = None);
    }
    s.rebuild();
    (engine, s)
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

fn midi(name: &str, beats: u64) -> Clip {
    Clip {
        name: name.into(),
        color: [1, 2, 3],
        length: beats * PPQ,
        ..Clip::default()
    }
}

fn audio_track(s: &mut Session) -> TrackId {
    s.add_track(TrackKind::Audio, None)
}

fn track(s: &Session, i: usize) -> TrackId {
    s.project.tracks[i].id
}

fn playing(s: &Session, t: TrackId) -> i32 {
    s.track_shared[&t].playing_slot.load(Ordering::Relaxed)
}

fn slot(t: TrackId, s: usize) -> ClipRef {
    ClipRef::slot(t, s)
}

fn names(s: &Session, t: TrackId) -> Vec<String> {
    s.project
        .track(t)
        .unwrap()
        .clips
        .iter()
        .map(|c| c.as_ref().map_or(String::new(), |c| c.name.clone()))
        .collect()
}

// ---------------------------------------------------------------- launch settings

#[test]
fn launch_settings_reach_the_engine_and_undo() {
    let (mut engine, mut s) = session();
    let t = track(&s, 0);
    s.set_clip(t, 0, Some(midi("a", 4)));
    s.set_quantize(Quantize::None);
    commit(&mut s);
    s.edit_launch(&[slot(t, 0)], |l| l.mode = LaunchMode::Gate);
    commit(&mut s);

    s.clip_down(t, 0, 1.0);
    render(&mut engine, &mut s, 1024);
    assert_eq!(playing(&s, t), 0);
    s.clip_up(t, 0);
    render(&mut engine, &mut s, 1024);
    assert_eq!(playing(&s, t), NO_SLOT, "a Gate clip stops on release");

    assert!(s.undo());
    assert_eq!(s.clip(t, 0).unwrap().launch.mode, LaunchMode::Trigger);
    s.clip_down(t, 0, 1.0);
    render(&mut engine, &mut s, 1024);
    s.clip_up(t, 0);
    render(&mut engine, &mut s, 1024);
    assert_eq!(playing(&s, t), 0, "undo put it back to Trigger");
}

#[test]
fn one_edit_applies_to_every_selected_clip_as_one_undo_step() {
    let (_e, mut s) = session();
    let t = track(&s, 0);
    let u = track(&s, 1);
    s.set_clip(t, 0, Some(midi("a", 4)));
    s.set_clip(u, 2, Some(midi("b", 4)));
    commit(&mut s);
    s.edit_launch(&[slot(t, 0), slot(u, 2)], |l| l.legato = true);
    commit(&mut s);
    assert!(s.clip(t, 0).unwrap().launch.legato);
    assert!(s.clip(u, 2).unwrap().launch.legato);
    assert!(s.undo());
    assert!(!s.clip(t, 0).unwrap().launch.legato);
    assert!(!s.clip(u, 2).unwrap().launch.legato);
}

#[test]
fn new_audio_clips_take_the_default_launch_settings() {
    let (_e, mut s) = session();
    s.launch_defaults.mode = LaunchMode::Toggle;
    s.launch_defaults.quantize = Some(Quantize::Beat);
    assert_eq!(s.launch_defaults.settings().mode, LaunchMode::Toggle);
    assert_eq!(s.launch_defaults.settings().quantize, Some(Quantize::Beat));
}

#[test]
fn deactivating_a_clip_makes_it_unlaunchable() {
    let (mut engine, mut s) = session();
    let t = track(&s, 0);
    s.set_clip(t, 0, Some(midi("a", 4)));
    s.set_quantize(Quantize::None);
    s.toggle_clip_active(&[(t, 0)]);
    s.launch_clip(t, 0);
    render(&mut engine, &mut s, 1024);
    assert_eq!(playing(&s, t), NO_SLOT);
    s.toggle_clip_active(&[(t, 0)]);
    s.launch_clip(t, 0);
    render(&mut engine, &mut s, 1024);
    assert_eq!(playing(&s, t), 0);
}

// ---------------------------------------------------------------- Follow Actions

#[test]
fn the_button_turns_follow_actions_on_with_something_to_do() {
    let (_e, mut s) = session();
    let t = track(&s, 0);
    s.set_clip(t, 0, Some(midi("a", 4)));
    s.toggle_clip_follow(&[slot(t, 0)]);
    let f = s.clip(t, 0).unwrap().launch.follow;
    assert!(f.enabled);
    assert_eq!(f.action_a, FollowAction::Next);
    s.toggle_clip_follow(&[slot(t, 0)]);
    assert!(!s.clip(t, 0).unwrap().launch.follow.enabled);
    assert_eq!(
        s.clip(t, 0).unwrap().launch.follow.action_a,
        FollowAction::Next,
        "turning it off keeps the settings"
    );
}

#[test]
fn a_follow_chain_loops_through_a_non_contiguous_selection() {
    let (mut engine, mut s) = session();
    let t = track(&s, 0);
    for i in [0, 3, 5] {
        s.set_clip(t, i, Some(midi(&i.to_string(), 1)));
    }
    s.set_quantize(Quantize::None);
    s.create_follow_chain(&[(t, 5), (t, 0), (t, 3)]);
    let jump = |s: &Session, i: usize| s.clip(t, i).unwrap().launch.follow.action_a;
    assert_eq!(jump(&s, 0), FollowAction::Jump(3));
    assert_eq!(jump(&s, 3), FollowAction::Jump(5));
    assert_eq!(jump(&s, 5), FollowAction::Jump(0));
    commit(&mut s);

    s.launch_clip(t, 0);
    render(&mut engine, &mut s, BEAT_FRAMES + 1000);
    assert_eq!(playing(&s, t), 3);
    render(&mut engine, &mut s, BEAT_FRAMES);
    assert_eq!(playing(&s, t), 5);
    render(&mut engine, &mut s, BEAT_FRAMES);
    assert_eq!(playing(&s, t), 0, "and back round to the first");

    assert!(s.undo());
    assert_eq!(jump(&s, 0), FollowAction::None);
}

#[test]
fn the_global_button_is_for_the_set_and_greys_out_without_follow_actions() {
    let (mut engine, mut s) = session();
    let t = track(&s, 0);
    assert!(!s.has_follow_actions());
    s.set_clip(t, 0, Some(midi("a", 1)));
    s.set_clip(t, 1, Some(midi("b", 1)));
    s.toggle_clip_follow(&[slot(t, 0)]);
    assert!(s.has_follow_actions());
    s.set_quantize(Quantize::None);
    s.set_follow_actions_on(false);
    s.launch_clip(t, 0);
    render(&mut engine, &mut s, BEAT_FRAMES * 2);
    assert_eq!(playing(&s, t), 0, "held");
    commit(&mut s);
    assert!(s.undo());
    assert!(s.project.follow_actions_on);
}

#[test]
fn a_scene_follow_action_reaches_the_engine() {
    let (mut engine, mut s) = session();
    let t = track(&s, 0);
    s.set_clip(t, 0, Some(midi("a", 4)));
    s.set_clip(t, 1, Some(midi("b", 4)));
    s.set_quantize(Quantize::None);
    s.edit_scenes(&[0], "Follow", |sc| {
        sc.follow.enabled = true;
        sc.follow.action_a = FollowAction::Next;
        sc.follow.time = PPQ;
    });
    commit(&mut s);
    s.launch_scene(0);
    render(&mut engine, &mut s, BEAT_FRAMES + 1000);
    assert_eq!(playing(&s, t), 1);
}

// ---------------------------------------------------------------- scenes

#[test]
fn insert_scene_goes_below_and_moves_what_is_under_it() {
    let (mut engine, mut s) = session();
    let t = track(&s, 0);
    s.set_clip(t, 3, Some(midi("x", 4)));
    s.set_quantize(Quantize::None);
    s.launch_clip(t, 3);
    render(&mut engine, &mut s, 1024);
    assert_eq!(s.insert_scene(1), Some(2));
    assert_eq!(s.project.scenes.len(), 9);
    render(&mut engine, &mut s, 1024);
    assert_eq!(names(&s, t)[4], "x");
    assert_eq!(playing(&s, t), 4, "the clip goes on playing on its new row");
}

#[test]
fn deleting_a_scene_moves_what_is_under_it() {
    let (mut engine, mut s) = session();
    let t = track(&s, 0);
    s.set_clip(t, 3, Some(midi("x", 4)));
    s.set_quantize(Quantize::None);
    s.launch_clip(t, 3);
    render(&mut engine, &mut s, 1024);
    s.delete_scene(1);
    render(&mut engine, &mut s, 1024);
    assert_eq!(names(&s, t)[2], "x");
    assert_eq!(playing(&s, t), 2);
}

#[test]
fn capture_and_insert_scene_copies_what_is_running_and_takes_it_over() {
    let (mut engine, mut s) = session();
    let t = track(&s, 0);
    let u = track(&s, 1);
    let v = track(&s, 2);
    s.set_clip(t, 0, Some(midi("a", 4)));
    s.set_clip(u, 2, Some(midi("b", 4)));
    s.set_quantize(Quantize::None);
    s.launch_clip(t, 0);
    s.launch_clip(u, 2);
    render(&mut engine, &mut s, 2048);
    commit(&mut s);
    let at = s.capture_and_insert_scene(0).unwrap();
    assert_eq!(at, 1);
    render(&mut engine, &mut s, 1024);
    assert_eq!(names(&s, t)[1], "a");
    assert_eq!(names(&s, u)[1], "b");
    assert_eq!(names(&s, v)[1], "");
    assert_eq!(
        names(&s, u)[3],
        "b",
        "the running clip's own slot moved down"
    );
    assert_eq!(playing(&s, t), 1);
    assert_eq!(playing(&s, u), 1);
    commit(&mut s);
    assert!(s.undo());
    assert_eq!(s.project.scenes.len(), 8);
    assert_eq!(names(&s, u)[2], "b");
}

#[test]
fn capture_and_insert_scene_does_not_disturb_the_sound() {
    let build = || {
        let (mut engine, mut s) = session();
        let t = s.project.tracks[0].id;
        let notes = (0..8)
            .map(|i| rlive_project::NoteData::new(i * PPQ / 2, PPQ / 4, 60 + i as u8, 0.8))
            .collect();
        let clip = Clip {
            name: "riff".into(),
            length: 4 * PPQ,
            content: ClipContent::midi(notes),
            ..Clip::default()
        };
        s.set_clip(t, 0, Some(clip));
        s.set_quantize(Quantize::None);
        s.launch_clip(t, 0);
        render(&mut engine, &mut s, 512 * 8);
        (engine, s)
    };
    let (mut e1, mut s1) = build();
    let (mut e2, mut s2) = build();
    s2.capture_and_insert_scene(0);
    let a = render(&mut e1, &mut s1, 96_000);
    let b = render(&mut e2, &mut s2, 96_000);
    assert_eq!(a, b, "bit for bit");
}

#[test]
fn dragged_scenes_close_up_and_keyboard_moves_keep_their_gaps() {
    let (_e, mut s) = session();
    for (i, n) in ["a", "b", "c", "d", "e"].iter().enumerate() {
        s.project.scenes[i].name = n.to_string();
    }
    let order = |s: &Session| -> String {
        s.project
            .scenes
            .iter()
            .take(5)
            .map(|c| c.name.as_str())
            .collect()
    };
    // Scenes b and d dragged to before e, closing up.
    let moved = s.move_scenes(&[1, 3], 4);
    assert_eq!(order(&s), "acbde");
    assert_eq!(moved, vec![2, 3]);

    // Scenes a and c (0 and 1 after that) shifted down one, keeping their spacing.
    s.project.scenes[0].name = "a".into();
    let mut s2 = session().1;
    for (i, n) in ["a", "b", "c", "d", "e"].iter().enumerate() {
        s2.project.scenes[i].name = n.to_string();
    }
    let moved = s2.shift_scenes(&[1, 3], 1);
    assert_eq!(order(&s2), "acbed");
    assert_eq!(moved, vec![2, 4]);
    let moved = s2.shift_scenes(&[2, 4], -1);
    assert_eq!(order(&s2), "abcde");
    assert_eq!(moved, vec![1, 3]);
}

#[test]
fn scene_moves_stop_at_the_edges_and_keep_clips_with_their_scenes() {
    let (_e, mut s) = session();
    let t = track(&s, 0);
    s.set_clip(t, 0, Some(midi("top", 4)));
    s.set_clip(t, 7, Some(midi("bot", 4)));
    assert_eq!(s.shift_scenes(&[0], -1), vec![0], "already at the top");
    assert_eq!(s.shift_scenes(&[7], 1), vec![7], "already at the bottom");
    s.move_scenes(&[7], 0);
    assert_eq!(names(&s, t)[0], "bot");
    assert_eq!(names(&s, t)[1], "top");
}

#[test]
fn scene_tempo_is_clamped_and_a_signature_must_be_one_live_allows() {
    let (_e, mut s) = session();
    s.set_scene_tempo(&[0, 1], Some(5.0));
    assert_eq!(s.project.scenes[0].tempo, Some(20.0));
    assert_eq!(s.project.scenes[1].tempo, Some(20.0));
    s.set_scene_tempo(&[0], Some(5000.0));
    assert_eq!(s.project.scenes[0].tempo, Some(999.0));
    s.set_scene_tempo(&[0], None);
    assert_eq!(s.project.scenes[0].tempo, None, "the reset disables it");

    let sig = |n, d| TimeSignature {
        numerator: n,
        denominator: d,
    };
    s.set_scene_signature(&[0], Some(sig(7, 3)));
    assert_eq!(s.project.scenes[0].signature, None, "3 is not a beat value");
    s.set_scene_signature(&[0], Some(sig(100, 4)));
    assert_eq!(s.project.scenes[0].signature, None);
    s.set_scene_signature(&[0], Some(sig(7, 16)));
    assert_eq!(s.project.scenes[0].signature, Some(sig(7, 16)));
}

#[test]
fn launching_a_scene_sets_the_project_tempo_at_the_launch_point() {
    let (mut engine, mut s) = session();
    let t = track(&s, 0);
    s.set_clip(t, 1, Some(midi("a", 4)));
    s.set_scene_tempo(&[1], Some(90.0));
    s.play();
    render(&mut engine, &mut s, 1024);
    s.launch_scene(1);
    render(&mut engine, &mut s, 1024);
    assert_eq!(s.project.tempo, 120.0, "not before its launch point");
    render(&mut engine, &mut s, BAR_FRAMES);
    assert_eq!(s.project.tempo, 90.0);
    assert_eq!(s.engine.shared.tempo.load(), 90.0);
}

#[test]
fn a_scene_tempo_over_a_tempo_envelope_lasts_until_playback_stops() {
    let (mut engine, mut s) = session();
    let t = track(&s, 0);
    s.set_clip(t, 0, Some(midi("a", 4)));
    s.project.arrangement.tempo.points = vec![
        rlive_project::Breakpoint::new(0.0, 100.0),
        rlive_project::Breakpoint::new(BAR as f64 * 8.0, 100.0),
    ];
    s.rebuild();
    s.set_quantize(Quantize::None);
    s.set_scene_tempo(&[0], Some(77.0));
    s.launch_scene(0);
    render(&mut engine, &mut s, 2048);
    assert_eq!(s.engine.shared.tempo.load(), 77.0);
    s.stop();
    render(&mut engine, &mut s, 2048);
    assert_eq!(s.engine.shared.tempo.load(), 100.0, "the envelope is back");
}

#[test]
fn a_scene_signature_changes_the_project_when_not_recording() {
    let (mut engine, mut s) = session();
    let sig = TimeSignature {
        numerator: 6,
        denominator: 8,
    };
    s.set_quantize(Quantize::None);
    s.set_scene_signature(&[0], Some(sig));
    s.launch_scene(0);
    render(&mut engine, &mut s, 1024);
    assert_eq!(s.project.time_signature, sig);
}

// ---------------------------------------------------------------- moving clips

#[test]
fn clips_move_together_and_only_where_they_can_play() {
    let (_e, mut s) = session();
    let (a, b) = (track(&s, 0), track(&s, 1));
    s.set_clip(a, 0, Some(midi("x", 4)));
    s.set_clip(a, 1, Some(midi("y", 4)));
    let landed = s.move_slots(&[(a, 0), (a, 1)], 1, 2, false);
    assert_eq!(landed, vec![(b, 2), (b, 3)]);
    assert_eq!(names(&s, b)[2], "x");
    assert_eq!(names(&s, b)[3], "y");
    assert_eq!(names(&s, a)[0], "");

    // Copying leaves the original; an audio track doesn't take a MIDI clip.
    let audio = audio_track(&mut s);
    let copied = s.move_slots(&[(b, 2)], -1, 0, true);
    assert_eq!(copied, vec![(a, 2)]);
    assert_eq!(names(&s, b)[2], "x");
    let at = s.project.track_index(audio).unwrap();
    let from = s.project.track_index(b).unwrap();
    assert!(s
        .move_slots(&[(b, 2)], at as isize - from as isize, 0, false)
        .is_empty());
}

#[test]
fn clips_off_the_grid_stay_where_they_are() {
    let (_e, mut s) = session();
    let a = track(&s, 0);
    s.set_clip(a, 7, Some(midi("x", 4)));
    assert!(s.move_slots(&[(a, 7)], 0, 1, false).is_empty());
    assert_eq!(names(&s, a)[7], "x");
}

#[test]
fn pasting_from_the_arrangement_lays_clips_top_to_bottom() {
    let (_e, mut s) = session();
    let (a, b) = (track(&s, 0), track(&s, 1));
    let place = |name: &str, start: u64| ArrangementClip::new(midi(name, 4), start);
    let clips = vec![
        (a, place("a1", 0)),
        (b, place("b1", 0)),
        (a, place("a2", BAR)),
        (b, place("b2", 2 * BAR)),
    ];
    let placed = s.paste_arrangement_clips(&clips, 1);
    assert_eq!(placed.len(), 4);
    assert_eq!(names(&s, a)[1], "a1");
    assert_eq!(names(&s, b)[1], "b1");
    assert_eq!(names(&s, a)[2], "a2");
    assert_eq!(names(&s, b)[3], "b2");
}

#[test]
fn pasting_from_the_arrangement_adds_the_scenes_it_needs() {
    let (_e, mut s) = session();
    let a = track(&s, 0);
    let clips: Vec<_> = (0..10)
        .map(|i| (a, ArrangementClip::new(midi("x", 1), i * BAR)))
        .collect();
    let placed = s.paste_arrangement_clips(&clips, 0);
    assert_eq!(placed.len(), 10);
    assert_eq!(s.project.scenes.len(), 10);
}

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn wav_files(n: usize) -> (PathBuf, Vec<PathBuf>) {
    let at = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rlive-launch-{}-{at}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let silence = vec![0.0f32; 4800];
    let files = (0..n)
        .map(|i| {
            let path = dir.join(format!("{i}.wav"));
            rlive_devices::write_wav(&path, &silence, &silence, 48_000).unwrap();
            path
        })
        .collect();
    (dir, files)
}

#[test]
fn dropped_files_go_down_one_track_unless_spread() {
    let (_e, mut s) = session();
    let a = audio_track(&mut s);
    let b = audio_track(&mut s);
    let (dir, files) = wav_files(3);
    let placed = s.drop_files(&files, a, 1, false);
    assert_eq!(placed, vec![(a, 1), (a, 2), (a, 3)]);

    let placed = s.drop_files(&files[..2], a, 5, true);
    assert_eq!(placed, vec![(a, 5), (b, 5)]);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn only_audio_files_can_be_dropped() {
    let (_e, mut s) = session();
    let a = audio_track(&mut s);
    let placed = s.drop_files(&[PathBuf::from("notes.txt")], a, 0, false);
    assert!(placed.is_empty());
}

// ---------------------------------------------------------------- stop buttons

#[test]
fn a_stop_button_can_be_removed_and_undone() {
    let (mut engine, mut s) = session();
    let t = track(&s, 0);
    s.set_clip(t, 0, Some(midi("a", 4)));
    s.set_quantize(Quantize::None);
    commit(&mut s);
    s.toggle_stop_buttons(&[(t, 1)]);
    commit(&mut s);
    assert!(!s.has_stop_button(t, 1));
    s.launch_scene(0);
    render(&mut engine, &mut s, 1024);
    s.launch_scene(1);
    render(&mut engine, &mut s, 1024);
    assert_eq!(playing(&s, t), 0, "scene 2 doesn't touch this track");
    assert!(s.undo());
    assert!(s.has_stop_button(t, 1));
    s.launch_scene(1);
    render(&mut engine, &mut s, 1024);
    assert_eq!(playing(&s, t), NO_SLOT, "with its button back, it stops it");
}

// ---------------------------------------------------------------- Arrangement Record

fn lane(s: &Session, t: TrackId) -> Vec<(u64, u64, String)> {
    s.project
        .track(t)
        .unwrap()
        .arrangement
        .iter()
        .map(|c| (c.start, c.end(), c.clip.name.clone()))
        .collect()
}

#[test]
fn arrangement_record_logs_launches_as_clips_in_place() {
    let (mut engine, mut s) = session();
    let t = track(&s, 0);
    s.set_clip(t, 0, Some(midi("a", 4)));
    s.set_clip(t, 1, Some(midi("b", 4)));
    s.set_quantize(Quantize::Bar);
    assert!(s.start_recording());
    s.launch_clip(t, 0);
    render(&mut engine, &mut s, BAR_FRAMES + 1024);
    s.launch_clip(t, 1);
    render(&mut engine, &mut s, BAR_FRAMES);
    s.stop_track(t);
    render(&mut engine, &mut s, BAR_FRAMES);
    s.stop_recording();
    render(&mut engine, &mut s, 1024);
    assert_eq!(
        lane(&s, t),
        vec![
            (0, 2 * BAR, "a".to_string()),
            (2 * BAR, 3 * BAR, "b".to_string()),
        ]
    );
    let takes = s.project.track(t).unwrap().take_lanes.len();
    assert_eq!(takes, 0, "no new audio, only clips");
}

#[test]
fn arrangement_record_is_one_undo_step() {
    let (mut engine, mut s) = session();
    let t = track(&s, 0);
    s.set_clip(t, 0, Some(midi("a", 4)));
    s.set_quantize(Quantize::None);
    commit(&mut s);
    s.start_recording();
    s.launch_clip(t, 0);
    render(&mut engine, &mut s, BAR_FRAMES);
    s.stop_recording();
    render(&mut engine, &mut s, 1024);
    assert!(!lane(&s, t).is_empty());
    commit(&mut s);
    assert!(s.undo());
    assert!(lane(&s, t).is_empty());
}

#[test]
fn stopping_playback_closes_what_was_being_logged() {
    let (mut engine, mut s) = session();
    let t = track(&s, 0);
    s.set_clip(t, 0, Some(midi("a", 8)));
    s.set_quantize(Quantize::None);
    s.start_recording();
    s.launch_clip(t, 0);
    render(&mut engine, &mut s, BAR_FRAMES);
    s.stop();
    render(&mut engine, &mut s, 1024);
    let logged = lane(&s, t);
    assert_eq!(logged.len(), 1);
    assert_eq!(logged[0].0, 0);
    assert!(logged[0].1 >= BAR, "{logged:?}");
    assert!(!s.is_recording());
}

#[test]
fn a_clip_already_playing_is_logged_from_where_recording_starts() {
    let (mut engine, mut s) = session();
    let t = track(&s, 0);
    s.set_clip(t, 0, Some(midi("a", 16)));
    s.set_quantize(Quantize::None);
    s.launch_clip(t, 0);
    render(&mut engine, &mut s, BAR_FRAMES);
    let from = s.engine.shared.tick.load();
    s.start_recording();
    render(&mut engine, &mut s, BAR_FRAMES);
    s.stop_recording();
    render(&mut engine, &mut s, 1024);
    let track_data = s.project.track(t).unwrap();
    let c = &track_data.arrangement[0];
    assert_eq!(c.start, from.round() as u64);
    assert_eq!(
        c.offset, c.start,
        "picks up the clip at the point it had got to"
    );
}

#[test]
fn editing_a_logged_clip_starts_a_new_stretch_with_its_new_properties() {
    let (mut engine, mut s) = session();
    let t = track(&s, 0);
    s.set_clip(t, 0, Some(midi("a", 16)));
    s.set_quantize(Quantize::None);
    s.start_recording();
    s.launch_clip(t, 0);
    render(&mut engine, &mut s, BAR_FRAMES);
    s.clip_mut(t, 0).unwrap().name = "renamed".into();
    s.sync_clip(t, 0);
    render(&mut engine, &mut s, BAR_FRAMES);
    s.stop_recording();
    render(&mut engine, &mut s, 1024);
    let logged = lane(&s, t);
    assert_eq!(logged.len(), 2, "{logged:?}");
    assert_eq!(logged[0].2, "a");
    assert_eq!(logged[1].2, "renamed");
    assert_eq!(logged[0].1, logged[1].0, "with no gap");
    let c = &s.project.track(t).unwrap().arrangement[1];
    assert_eq!(
        c.offset, c.start,
        "and the music carries on from where it was"
    );
}

#[test]
fn arrangement_record_writes_scene_tempo_and_meter_into_the_arrangement() {
    let (mut engine, mut s) = session();
    let t = track(&s, 0);
    s.set_clip(t, 1, Some(midi("a", 4)));
    let sig = TimeSignature {
        numerator: 3,
        denominator: 4,
    };
    s.set_scene_tempo(&[1], Some(150.0));
    s.set_scene_signature(&[1], Some(sig));
    s.set_quantize(Quantize::Bar);
    s.start_recording();
    s.play();
    render(&mut engine, &mut s, 1024);
    s.launch_scene(1);
    render(&mut engine, &mut s, BAR_FRAMES + 2048);
    s.stop_recording();
    render(&mut engine, &mut s, 1024);

    let points = &s.project.arrangement.tempo.points;
    let last = points.last().unwrap();
    assert_eq!((last.time, last.value), (BAR as f64, 150.0));
    let before = points[points.len() - 2];
    assert_eq!((before.time, before.value), (BAR as f64, 120.0), "a step");
    assert_eq!(s.project.arrangement.signatures[0].tick, BAR);
    assert_eq!(s.project.arrangement.signatures[0].signature, sig);
    assert_eq!(
        s.project.time_signature,
        TimeSignature::default(),
        "what came before the launch stays as it was"
    );
    assert_eq!(s.engine.shared.tempo.load(), 150.0);
}

#[test]
fn arrangement_record_logs_launches_and_automation_together() {
    let (mut engine, mut s) = session();
    let t = track(&s, 0);
    s.set_clip(t, 0, Some(midi("a", 16)));
    s.set_quantize(Quantize::None);
    s.set_automation_arm(true);
    s.start_recording();
    s.launch_clip(t, 0);
    render(&mut engine, &mut s, BAR_FRAMES / 2);
    if let Some(track) = s.project.track_mut(t) {
        track.volume_db = -12.0;
    }
    s.sync_mix(t);
    render(&mut engine, &mut s, BAR_FRAMES / 2);
    s.stop_recording();
    render(&mut engine, &mut s, 1024);
    assert_eq!(lane(&s, t).len(), 1);
    assert!(
        !s.project.track(t).unwrap().automation.is_empty(),
        "the fader move went in as automation"
    );
}

#[test]
fn undoing_a_scene_tempo_takes_it_out_of_the_engine_too() {
    let (mut engine, mut s) = session();
    let t = track(&s, 0);
    s.set_clip(t, 0, Some(midi("a", 4)));
    s.set_quantize(Quantize::None);
    commit(&mut s);
    s.set_scene_tempo(&[0], Some(90.0));
    commit(&mut s);
    assert!(s.undo());
    assert_eq!(s.project.scenes[0].tempo, None);
    s.launch_scene(0);
    render(&mut engine, &mut s, 2048);
    assert_eq!(s.engine.shared.tempo.load(), 120.0);
}

#[test]
fn undoing_an_inserted_scene_puts_the_slots_back() {
    let (_e, mut s) = session();
    let t = track(&s, 0);
    s.set_clip(t, 3, Some(midi("x", 4)));
    commit(&mut s);
    s.insert_scene(1);
    commit(&mut s);
    assert_eq!(names(&s, t)[4], "x");
    assert!(s.undo());
    assert_eq!(s.project.scenes.len(), 8);
    assert_eq!(names(&s, t)[3], "x");
    assert!(s.redo());
    assert_eq!(names(&s, t)[4], "x");
}
