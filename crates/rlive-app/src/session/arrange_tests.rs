//! Arrangement editing through a session: what happens to the project and the undo history.

use super::*;
use rlive_core::param::{mixer, EnvTarget, ParamId, ParamOwner, ParamRef};
use rlive_core::{TimeSignature, PPQ};
use rlive_engine::create_engine;
use rlive_project::arrangement_edit::flat_range;
use rlive_project::{ArrangementClip, Breakpoint, Clip, ClipContent, Envelope, Track};

fn session() -> (rlive_engine::Engine, Session) {
    let (engine, handle) = create_engine(48_000.0, 512);
    (
        engine,
        Session::new(handle, Project::new_default(), PluginCache::default()),
    )
}

fn frame(s: &mut Session) {
    s.commit_edits(false);
}

fn audio_track(s: &mut Session) -> TrackId {
    s.add_track(TrackKind::Audio, None)
}

fn audio_clip(start: u64, length: u64, offset: u64, content: u64) -> ArrangementClip {
    ArrangementClip {
        id: rlive_core::ArrClipId::new(),
        start,
        length,
        offset,
        clip: Clip {
            name: "a".into(),
            color: [1, 2, 3],
            length: content,
            looped: false,
            content: ClipContent::audio("missing.wav", 0.0),
            ..Clip::default()
        },
    }
}

fn spans(s: &Session, id: TrackId) -> Vec<(u64, u64)> {
    s.project
        .track(id)
        .unwrap()
        .arrangement
        .iter()
        .map(|c| (c.start, c.end()))
        .collect()
}

fn volume(t: TrackId) -> EnvTarget {
    EnvTarget::Param(ParamRef {
        owner: ParamOwner::Track(t),
        id: mixer::VOLUME,
    })
}

#[test]
fn delete_time_moves_clips_automation_and_markers_as_one_undo_step() {
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    s.arr_place(t, audio_clip(0, 4 * PPQ, 0, 16 * PPQ));
    s.arr_place(t, audio_clip(8 * PPQ, 4 * PPQ, 0, 16 * PPQ));
    s.set_envelope(
        ChainRef::Track(t),
        volume(t),
        vec![
            Breakpoint::new(0.0, -10.0),
            Breakpoint::new((8 * PPQ) as f64, -3.0),
        ],
    );
    s.project.add_signature(10 * PPQ, TimeSignature::default());
    frame(&mut s);
    s.delete_time(4 * PPQ, 8 * PPQ);
    frame(&mut s);
    assert_eq!(spans(&s, t), vec![(0, 4 * PPQ), (4 * PPQ, 8 * PPQ)]);
    assert_eq!(s.project.arrangement.signatures[0].tick, 6 * PPQ);
    let envelope = s.env_points(ChainRef::Track(t), volume(t));
    assert_eq!(envelope.last().unwrap().time, (4 * PPQ) as f64);

    assert!(s.undo());
    assert_eq!(spans(&s, t), vec![(0, 4 * PPQ), (8 * PPQ, 12 * PPQ)]);
    assert_eq!(s.project.arrangement.signatures[0].tick, 10 * PPQ);
    assert_eq!(
        s.env_points(ChainRef::Track(t), volume(t))
            .last()
            .unwrap()
            .time,
        (8 * PPQ) as f64
    );
}

#[test]
fn split_at_a_point_and_around_a_selection() {
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    s.arr_place(t, audio_clip(0, 8 * PPQ, 0, 16 * PPQ));
    frame(&mut s);
    s.arr_split(&[t], &[PPQ]);
    assert_eq!(spans(&s, t), vec![(0, PPQ), (PPQ, 8 * PPQ)]);
    s.arr_split(&[t], &[2 * PPQ, 3 * PPQ]);
    assert_eq!(
        spans(&s, t),
        vec![
            (0, PPQ),
            (PPQ, 2 * PPQ),
            (2 * PPQ, 3 * PPQ),
            (3 * PPQ, 8 * PPQ)
        ]
    );
    frame(&mut s);
    assert!(s.undo());
    assert_eq!(
        spans(&s, t),
        vec![(0, 8 * PPQ)],
        "both splits were one edit"
    );
}

#[test]
fn moving_a_clip_away_drops_the_crossfade_it_was_in() {
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    let a = audio_clip(0, 2 * PPQ, 0, 8 * PPQ);
    let b = audio_clip(2 * PPQ, 2 * PPQ, 2 * PPQ, 8 * PPQ);
    let (ida, idb) = (a.id, b.id);
    s.arr_place(t, a);
    s.arr_place(t, b);
    s.arr_crossfade(t, ida, 100, 200, 0.0);
    let fades = |s: &Session, id| s.project.track(t).unwrap().arr_clip(id).unwrap().clip.fades;
    assert_eq!(fades(&s, ida).out_tail, 200);
    s.arr_move(
        &[ArrMove {
            track: t,
            id: idb,
            to_track: t,
            start: 10 * PPQ,
        }],
        false,
    );
    assert_eq!(fades(&s, ida).out_tail, 0);
    assert_eq!(fades(&s, idb).in_tail, 0);
}

#[test]
fn a_fade_never_passes_its_limit_through_the_session() {
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    let c = audio_clip(0, 1000, 0, 2000);
    let id = c.id;
    s.arr_place(t, c);
    s.arr_set_fade(t, id, FadeSide::In, 700, None);
    s.arr_set_fade(t, id, FadeSide::Out, 700, None);
    let f = s.project.track(t).unwrap().arr_clip(id).unwrap().clip.fades;
    assert_eq!((f.fade_in, f.fade_out), (700, 300));
    s.arr_delete_fade(t, id, FadeSide::In);
    let f = s.project.track(t).unwrap().arr_clip(id).unwrap().clip.fades;
    assert_eq!(f.fade_in, 0);
}

#[test]
fn a_time_selection_makes_a_fade_or_a_crossfade() {
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    let a = audio_clip(0, 2 * PPQ, 0, 8 * PPQ);
    let b = audio_clip(2 * PPQ, 2 * PPQ, 2 * PPQ, 8 * PPQ);
    let (ida, idb) = (a.id, b.id);
    s.arr_place(t, a);
    s.arr_place(t, b);
    s.arr_fade_from_span(&[t], 2 * PPQ - 240, 2 * PPQ + 120);
    let fades = |s: &Session, id| s.project.track(t).unwrap().arr_clip(id).unwrap().clip.fades;
    assert_eq!(
        (fades(&s, ida).fade_out, fades(&s, ida).out_tail),
        (240, 120)
    );
    assert_eq!((fades(&s, idb).fade_in, fades(&s, idb).in_tail), (120, 240));

    let lone = audio_track(&mut s);
    let c = audio_clip(0, 4 * PPQ, 0, 8 * PPQ);
    let id = c.id;
    s.arr_place(lone, c);
    s.arr_fade_from_span(&[lone], 0, 480);
    let f = s
        .project
        .track(lone)
        .unwrap()
        .arr_clip(id)
        .unwrap()
        .clip
        .fades;
    assert_eq!(f.fade_in, 480);
}

#[test]
fn a_deactivated_clip_is_left_out_of_the_lane_and_back_on_toggle() {
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    let c = audio_clip(0, 4 * PPQ, 0, 8 * PPQ);
    let id = c.id;
    s.arr_place(t, c);
    s.arr_toggle_active(&[(t, id)]);
    assert!(
        !s.project
            .track(t)
            .unwrap()
            .arr_clip(id)
            .unwrap()
            .clip
            .active
    );
    s.arr_toggle_active(&[(t, id)]);
    assert!(
        s.project
            .track(t)
            .unwrap()
            .arr_clip(id)
            .unwrap()
            .clip
            .active
    );
}

#[test]
fn deactivating_a_selection_splits_at_its_edges_and_keeps_the_rest_on() {
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    s.arr_place(t, audio_clip(0, 4 * PPQ, 0, 8 * PPQ));
    s.arr_toggle_span(&[t], PPQ, 2 * PPQ);
    let lane = &s.project.track(t).unwrap().arrangement;
    assert_eq!(lane.len(), 3);
    assert!(lane[0].clip.active && !lane[1].clip.active && lane[2].clip.active);
}

#[test]
fn locators_sort_rename_and_loop_to_the_next_one() {
    let (_e, mut s) = session();
    let second = s.add_locator(2000);
    let first = s.add_locator(1000);
    assert_eq!((first, second), (0, 0));
    assert_eq!(s.project.arrangement.locators[0].tick, 1000);
    s.rename_locator(0, "Verse".into());
    assert_eq!(s.project.arrangement.locators[0].name, "Verse");
    s.loop_to_next_locator(0);
    let a = &s.project.arrangement;
    assert!(a.loop_enabled);
    assert_eq!((a.loop_start, a.loop_end), (1000, 2000));
    s.delete_locator(0);
    assert_eq!(s.project.arrangement.locators.len(), 1);
}

#[test]
fn song_start_toggles_on_and_off() {
    let (_e, mut s) = session();
    s.toggle_song_start(960);
    assert_eq!(s.project.arrangement.song_start, Some(960));
    s.toggle_song_start(960);
    assert_eq!(s.project.arrangement.song_start, None);
}

#[test]
fn signature_markers_edit_move_and_delete_with_the_undo() {
    let (_e, mut s) = session();
    let sig = TimeSignature {
        numerator: 3,
        denominator: 4,
    };
    s.add_signature_marker(960, sig);
    frame(&mut s);
    assert_eq!(s.project.arrangement.signatures.len(), 1);
    let at = s.move_signature_marker(0, 1920);
    assert_eq!(at, 0);
    frame(&mut s);
    assert_eq!(s.project.arrangement.signatures[0].tick, 1920);
    assert!(s.undo());
    assert_eq!(s.project.arrangement.signatures[0].tick, 960);
    s.delete_signature_marker(0);
    assert!(s.project.arrangement.signatures.is_empty());
}

#[test]
fn the_control_bar_signature_edits_the_marker_in_force() {
    let (_e, mut s) = session();
    s.add_signature_marker(960, TimeSignature::default());
    s.set_signature_at(
        2000.0,
        TimeSignature {
            numerator: 7,
            denominator: 8,
        },
    );
    assert_eq!(s.project.arrangement.signatures[0].signature.numerator, 7);
    assert_eq!(s.project.time_signature, TimeSignature::default());
    s.set_signature_at(
        100.0,
        TimeSignature {
            numerator: 5,
            denominator: 4,
        },
    );
    assert_eq!(s.project.time_signature.numerator, 5);
}

#[test]
fn fragments_correct_themselves_through_the_session() {
    let (_e, mut s) = session();
    s.add_signature_marker(6 * PPQ, TimeSignature::default());
    let f = s.fragments()[0];
    let before = s.project.arrangement.signatures[0].tick;
    s.delete_fragment(f);
    assert_eq!(s.project.arrangement.signatures[0].tick, 4 * PPQ);
    assert!(s.fragments().is_empty());
    assert_ne!(before, 4 * PPQ);
}

#[test]
fn loop_editing_goes_through_the_session() {
    let (_e, mut s) = session();
    s.set_loop(false, 960, 1920);
    s.loop_edit(LoopEdit::Double);
    let a = &s.project.arrangement;
    assert_eq!((a.loop_start, a.loop_end), (960, 2880));
    s.loop_selection(100, 200);
    let a = &s.project.arrangement;
    assert!(a.loop_enabled && a.loop_start == 100 && a.loop_end == 200);
    s.loop_selection(100, 200);
    assert!(
        !s.project.arrangement.loop_enabled,
        "the same selection toggles it off"
    );
}

#[test]
fn time_commands_keep_a_tracks_automation_at_flat_range_for_unknown_targets() {
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    let target = EnvTarget::Param(ParamRef {
        owner: ParamOwner::Device(rlive_core::DeviceId::new()),
        id: ParamId(9),
    });
    if let Some(track) = s.project.track_mut(t) {
        track.automation.push(Envelope {
            target,
            points: vec![Breakpoint::new(0.0, 0.0), Breakpoint::new(1000.0, 1.0)],
        });
    }
    s.insert_silence(500, 100);
    let t0: &Track = s.project.track(t).unwrap();
    assert_eq!(t0.automation[0].points.last().unwrap().time, 1100.0);
    let _ = flat_range;
}

#[test]
fn paste_time_opens_room_and_returns_what_landed() {
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    s.arr_place(t, audio_clip(0, PPQ, 0, 8 * PPQ));
    s.arr_place(t, audio_clip(PPQ, PPQ, 0, 8 * PPQ));
    let clip = s.copy_time(0, PPQ);
    let placed = s.paste_time(PPQ, &clip);
    assert_eq!(placed.len(), 1);
    assert_eq!(
        spans(&s, t),
        vec![(0, PPQ), (PPQ, 2 * PPQ), (2 * PPQ, 3 * PPQ)]
    );
}

#[test]
fn edge_fades_reach_the_lane_only_when_the_option_is_on() {
    let (_e, mut s) = session();
    assert!(!s.edge_fades, "a bare session plays the project exactly");
    s.set_edge_fades(true);
    let tempo = s.project.tempo;
    assert_eq!(edge_fade_ticks(tempo), 8);
    s.set_edge_fades(false);
    assert!(!s.edge_fades);
}
