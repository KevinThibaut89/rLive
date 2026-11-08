//! Groups, return tracks and sends through the session: structure, undo, files and sound.

use super::*;
use rlive_core::PPQ;
use rlive_engine::{create_engine, mix_cell, Engine, Tap};
use rlive_project::{Clip, ClipContent, Route, Track};

fn session(project: Project) -> (Engine, Session) {
    let (engine, handle) = create_engine(48_000.0, 512);
    (
        engine,
        Session::new(handle, project, PluginCache::default()),
    )
}

fn midi(p: &Project, name: &str) -> Track {
    Track::new(name, TrackKind::Midi, [1, 2, 3], p.scenes.len())
}

fn project(n: usize) -> Project {
    let mut p = Project::empty();
    p.tracks = (0..n).map(|i| midi(&p, &format!("{i}"))).collect();
    p
}

fn ids(p: &Project) -> Vec<TrackId> {
    p.tracks.iter().map(|t| t.id).collect()
}

fn commit(s: &mut Session) {
    s.commit_edits(false);
}

fn group_of(p: &Project, id: TrackId) -> Option<TrackId> {
    p.track(id).unwrap().group
}

#[test]
fn grouping_puts_the_group_first_and_keeps_tracks_together() {
    let (_e, mut s) = session(project(4));
    let t = ids(&s.project);
    let g = s.group_tracks(&[t[3], t[1]]).unwrap();
    assert_eq!(ids(&s.project), [t[0], g, t[1], t[3], t[2]]);
    assert_eq!(group_of(&s.project, t[1]), Some(g));
    assert_eq!(group_of(&s.project, t[3]), Some(g));
    assert_eq!(group_of(&s.project, g), None);
    assert_eq!(s.project.track(g).unwrap().kind, TrackKind::Group);
}

#[test]
fn nesting_a_group_keeps_its_children_with_it() {
    let (_e, mut s) = session(project(3));
    let t = ids(&s.project);
    let inner = s.group_tracks(&[t[0], t[1]]).unwrap();
    let outer = s.group_tracks(&[inner, t[2]]).unwrap();
    assert_eq!(ids(&s.project), [outer, inner, t[0], t[1], t[2]]);
    assert_eq!(group_of(&s.project, inner), Some(outer));
    assert_eq!(group_of(&s.project, t[0]), Some(inner));
    assert_eq!(s.project.group_depth(t[0]), 2);
}

#[test]
fn grouping_inside_a_group_nests_in_that_group() {
    let (_e, mut s) = session(project(3));
    let t = ids(&s.project);
    let outer = s.group_tracks(&[t[0], t[1], t[2]]).unwrap();
    let inner = s.group_tracks(&[t[1], t[2]]).unwrap();
    assert_eq!(group_of(&s.project, inner), Some(outer));
    assert_eq!(group_of(&s.project, t[1]), Some(inner));
}

#[test]
fn a_custom_output_survives_grouping_and_ungrouping() {
    let (_e, mut s) = session(project(2));
    let t = ids(&s.project);
    s.project.track_mut(t[1]).unwrap().routing.output = Route::SendsOnly;
    let g = s.group_tracks(&t).unwrap();
    assert_eq!(
        s.project.track(t[0]).unwrap().routing.output,
        Route::Default
    );
    assert_eq!(
        s.project.track(t[1]).unwrap().routing.output,
        Route::SendsOnly
    );
    s.ungroup(g);
    assert_eq!(
        s.project.track(t[1]).unwrap().routing.output,
        Route::SendsOnly
    );
    assert_eq!(group_of(&s.project, t[0]), None);
}

#[test]
fn deleting_a_group_deletes_its_contents() {
    let (_e, mut s) = session(project(3));
    let t = ids(&s.project);
    let g = s.group_tracks(&[t[0], t[1]]).unwrap();
    s.delete_track(g);
    assert_eq!(ids(&s.project), [t[2]]);
}

#[test]
fn ungrouping_hands_the_children_to_the_parent_group() {
    let (_e, mut s) = session(project(2));
    let t = ids(&s.project);
    let inner = s.group_tracks(&t).unwrap();
    let outer = s.group_tracks(&[inner]).unwrap();
    s.ungroup(inner);
    assert_eq!(group_of(&s.project, t[0]), Some(outer));
    assert_eq!(ids(&s.project), [outer, t[0], t[1]]);
}

#[test]
fn a_track_moved_into_a_group_and_out_again() {
    let (_e, mut s) = session(project(3));
    let t = ids(&s.project);
    let g = s.group_tracks(&[t[0]]).unwrap();
    s.move_into_group(t[2], g);
    assert_eq!(ids(&s.project), [g, t[0], t[2], t[1]]);
    assert_eq!(group_of(&s.project, t[2]), Some(g));
    // To the very end, past the group: out of it.
    s.move_track(t[2], 4);
    assert_eq!(ids(&s.project), [g, t[0], t[1], t[2]]);
    assert_eq!(group_of(&s.project, t[2]), None);
}

#[test]
fn moving_a_group_moves_its_block() {
    let (_e, mut s) = session(project(3));
    let t = ids(&s.project);
    let g = s.group_tracks(&[t[0], t[1]]).unwrap();
    s.move_track(g, 1);
    assert_eq!(ids(&s.project), [t[2], g, t[0], t[1]]);
}

#[test]
fn a_track_from_before_a_group_lands_inside_it() {
    let (_e, mut s) = session(project(4));
    let t = ids(&s.project);
    let g = s.group_tracks(&[t[2], t[3]]).unwrap();
    s.move_into_group(t[0], g);
    assert_eq!(ids(&s.project), [t[1], g, t[2], t[3], t[0]]);
    assert_eq!(group_of(&s.project, t[0]), Some(g));
}

#[test]
fn a_track_cant_be_put_inside_itself() {
    let (_e, mut s) = session(project(2));
    let t = ids(&s.project);
    let inner = s.group_tracks(&[t[0]]).unwrap();
    let outer = s.group_tracks(&[inner]).unwrap();
    s.move_into_group(outer, inner);
    assert_eq!(group_of(&s.project, outer), None);
    assert_eq!(ids(&s.project)[0], outer);
}

#[test]
fn a_track_added_among_a_groups_tracks_joins_it() {
    let (_e, mut s) = session(project(2));
    let t = ids(&s.project);
    let g = s.group_tracks(&t).unwrap();
    let at = s.project.track_index(t[1]).unwrap();
    let added = s.add_track(TrackKind::Midi, Some(at + 1));
    assert_eq!(group_of(&s.project, added), Some(g));
    let outside = s.add_track(TrackKind::Midi, Some(0));
    assert_eq!(group_of(&s.project, outside), None);
}

#[test]
fn assigning_a_groups_color_follows_the_view() {
    let (_e, mut s) = session(project(1));
    let t = ids(&s.project)[0];
    let g = s.group_tracks(&[t]).unwrap();
    s.project.track_mut(g).unwrap().color = [9, 9, 9];
    {
        let track = s.project.track_mut(t).unwrap();
        track.color = [1, 1, 1];
        track.clips[0] = Some(Clip {
            color: [1, 1, 1],
            ..Clip::default()
        });
        track.arrangement.push(rlive_project::ArrangementClip::new(
            Clip {
                color: [1, 1, 1],
                ..Clip::default()
            },
            0,
        ));
    }
    s.assign_group_color(g, true);
    let track = s.project.track(t).unwrap();
    assert_eq!(track.color, [9, 9, 9]);
    assert_eq!(track.clips[0].as_ref().unwrap().color, [9, 9, 9]);
    assert_eq!(
        track.arrangement[0].clip.color,
        [1, 1, 1],
        "Session view only"
    );
    s.assign_group_color(g, false);
    assert_eq!(
        s.project.track(t).unwrap().arrangement[0].clip.color,
        [9, 9, 9]
    );
}

#[test]
fn only_clip_tracks_take_instruments() {
    let (_e, mut s) = session(project(1));
    let t = ids(&s.project)[0];
    let g = s.group_tracks(&[t]).unwrap();
    let r = s.add_return().unwrap();
    s.add_builtin(ChainRef::Track(g), "rlive.poly");
    s.add_builtin(ChainRef::Track(r), "rlive.poly");
    assert!(s.project.track(g).unwrap().devices.is_empty());
    assert!(s.project.track(r).unwrap().devices.is_empty());
    s.add_builtin(ChainRef::Track(g), "rlive.delay");
    s.add_builtin(ChainRef::Track(r), "rlive.delay");
    assert_eq!(s.project.track(g).unwrap().devices.len(), 1);
    assert_eq!(s.project.track(r).unwrap().devices.len(), 1);
}

// ------------------------------------------------------------------ plan compilation

fn steps(plan: &rlive_engine::RenderPlan) -> Vec<u32> {
    plan.steps.iter().map(|s| s.track).collect()
}

fn inputs_of(plan: &rlive_engine::RenderPlan, track: u32) -> Vec<u32> {
    let step = plan.steps.iter().find(|s| s.track == track).unwrap();
    step.inputs.iter().map(|i| i.from).collect()
}

/// `[g a b] [c]` and two returns.
fn grouped() -> Project {
    let mut p = project(3);
    let (a, b) = (p.tracks[0].id, p.tracks[1].id);
    let mut g = Track::new("g", TrackKind::Group, [1, 1, 1], p.scenes.len());
    g.group = None;
    let gid = g.id;
    p.tracks[0].group = Some(gid);
    p.tracks[1].group = Some(gid);
    p.tracks.insert(0, g);
    let _ = (a, b);
    p.returns = vec![
        Track::new("A", TrackKind::Return, [2, 2, 2], 0),
        Track::new("B", TrackKind::Return, [3, 3, 3], 0),
    ];
    p
}

#[test]
fn a_flat_project_compiles_the_flat_plan_exactly() {
    let plan = compile_plan(&project(3));
    assert_eq!(steps(&plan), [0, 1, 2, 3]);
    assert_eq!(plan.levels, [0..3, 3..4]);
    assert_eq!(inputs_of(&plan, 3), [0, 1, 2]);
    assert!(plan.returns.is_empty());
    assert!(plan.groups.iter().all(|g| g.is_none()));
}

#[test]
fn a_group_compiles_to_its_own_level_and_takes_its_children() {
    let plan = compile_plan(&grouped());
    // 0=g 1=a 2=b 3=c 4=A 5=B 6=Master. a, b, c run first, then g and the returns, then Main.
    assert_eq!(plan.levels.len(), 4);
    assert_eq!(steps(&plan)[..3], [1, 2, 3]);
    assert_eq!(inputs_of(&plan, 0), [1, 2]);
    assert_eq!(
        inputs_of(&plan, 6),
        [0, 3, 4, 5],
        "Main hears the top level only"
    );
    assert_eq!(plan.groups[1], Some(0));
    assert_eq!(plan.returns, 4..6);
}

#[test]
fn nested_groups_each_get_a_level() {
    let mut p = project(2);
    let scenes = p.scenes.len();
    let (mut outer, mut inner) = (
        Track::new("o", TrackKind::Group, [1, 1, 1], scenes),
        Track::new("i", TrackKind::Group, [1, 1, 1], scenes),
    );
    outer.group = None;
    inner.group = Some(outer.id);
    p.tracks[0].group = Some(inner.id);
    p.tracks[1].group = Some(outer.id);
    let (a, b) = (p.tracks.remove(0), p.tracks.remove(0));
    p.tracks = vec![outer, inner, a, b];
    let plan = compile_plan(&p);
    // Steps: a b / i / o / master.
    assert_eq!(plan.levels.len(), 4);
    assert_eq!(steps(&plan), [2, 3, 1, 0, 4]);
    assert_eq!(inputs_of(&plan, 1), [2]);
    assert_eq!(inputs_of(&plan, 0), [1, 3]);
}

#[test]
fn every_track_and_group_sends_to_every_return_by_default() {
    let mut p = grouped();
    p.returns[1].pre_fader_sends = true;
    let plan = compile_plan(&p);
    let a = plan.steps.iter().find(|s| s.track == 4).unwrap();
    let from: Vec<u32> = a.inputs.iter().map(|i| i.from).collect();
    assert_eq!(from, [0, 1, 2, 3], "clip and group tracks, no feedback");
    assert!(a.inputs.iter().all(|i| i.gain == Some(mix_cell::send(0))));
    assert!(a
        .inputs
        .iter()
        .all(|i| i.tap == Tap::PostMixer && !i.feedback));
    let b = plan.steps.iter().find(|s| s.track == 5).unwrap();
    assert!(b.inputs.iter().all(|i| i.tap == Tap::PostFx), "pre fader");
    assert!(b.inputs.iter().all(|i| i.gain == Some(mix_cell::send(1))));
}

#[test]
fn enabled_feedback_sends_read_the_past_and_forward_ones_do_not() {
    let mut p = grouped();
    let (r0, r1) = (p.returns[0].id, p.returns[1].id);
    let enable = |t: &mut Track, to| {
        t.sends.push(rlive_project::TrackSend {
            to,
            level_db: -6.0,
            enabled: true,
        })
    };
    let source = &mut p.returns[1];
    enable(source, r0); // back: B into A
    let source = &mut p.returns[0];
    enable(source, r0); // to itself
    enable(source, r1); // forward: A into B
    let plan = compile_plan(&p);
    let a = plan.steps.iter().find(|s| s.track == 4).unwrap();
    let into_a: Vec<(u32, bool)> = a.inputs.iter().map(|i| (i.from, i.feedback)).collect();
    assert!(into_a.contains(&(4, true)), "A hears itself from the past");
    assert!(into_a.contains(&(5, true)), "and B, which runs later");
    let b = plan.steps.iter().find(|s| s.track == 5).unwrap();
    assert!(
        b.inputs.iter().any(|i| i.from == 4 && !i.feedback),
        "A→B is forward"
    );
    // Forward edges put B a level after A.
    let level = |t: u32| {
        plan.levels
            .iter()
            .position(|l| {
                plan.steps[l.start as usize..l.end as usize]
                    .iter()
                    .any(|s| s.track == t)
            })
            .unwrap()
    };
    assert!(level(4) < level(5));
    assert!(level(5) < level(6));
}

#[test]
fn feedback_sends_are_left_out_until_enabled() {
    let p = grouped();
    let plan = compile_plan(&p);
    for ret in [4, 5] {
        let step = plan.steps.iter().find(|s| s.track == ret).unwrap();
        assert!(step.inputs.iter().all(|i| i.from < 4), "no return→return");
    }
}

#[test]
fn outputs_without_a_destination_reach_nothing_but_their_sends() {
    let mut p = grouped();
    p.tracks[3].routing.output = Route::SendsOnly;
    p.tracks[1].routing.output = Route::None;
    let plan = compile_plan(&p);
    assert_eq!(
        inputs_of(&plan, 0),
        [2],
        "a is off, b still feeds the group"
    );
    assert_eq!(inputs_of(&plan, 6), [0, 4, 5], "c is sends only");
}

#[test]
fn a_loop_of_custom_routing_compiles_to_something_runnable() {
    let mut p = grouped();
    let g2 = Track::new("g2", TrackKind::Group, [1, 1, 1], p.scenes.len());
    let g2_id = g2.id;
    p.tracks.push(g2);
    let g = p.tracks[0].id;
    // g2 feeds g, and g feeds g2. One of the two has to be cut.
    p.tracks.last_mut().unwrap().group = Some(g);
    p.tracks[0].routing.output = Route::Track {
        track: g2_id,
        device: None,
    };
    let plan = compile_plan(&p);
    assert_eq!(plan.steps.len(), p.tracks.len() + p.returns.len() + 1);
    let (mut engine, mut s) = session(p);
    render_some(&mut engine, &mut s);
}

#[test]
fn the_engine_keeps_a_compiled_plan_through_every_structural_edit() {
    let (mut engine, handle) = create_engine(48_000.0, 512);
    let mut s = Session::new(handle, project(3), PluginCache::default());
    let t = ids(&s.project);
    let mut block = |s: &mut Session| {
        let (mut l, mut r) = ([0.0; 512], [0.0; 512]);
        engine.process_stereo(&mut l, &mut r);
        s.idle();
        engine.has_compiled_plan()
    };
    assert!(block(&mut s));
    let g = s.group_tracks(&[t[0], t[1]]).unwrap();
    assert!(block(&mut s), "after grouping");
    s.set_folded(g, true);
    assert!(block(&mut s));
    let r = s.add_return().unwrap();
    assert!(block(&mut s), "after a return");
    s.set_pre_fader_sends(r, true);
    assert!(block(&mut s));
    s.set_send_enabled(r, r, true);
    assert!(block(&mut s));
    s.move_into_group(t[2], g);
    assert!(block(&mut s));
    s.ungroup(g);
    assert!(block(&mut s));
    s.delete_return(r);
    assert!(block(&mut s));
    assert!(s.project.returns.is_empty());
}

// ------------------------------------------------------------------ undo

/// Undoes the edit made since the last commit, checks the project went back exactly, then redoes.
fn round_trips(s: &mut Session, engine: &mut Engine, before: &Project) {
    commit(s);
    let after = s.project.clone();
    assert!(s.undo(), "undo");
    assert_eq!(s.project.tracks, before.tracks, "undo tracks");
    assert_eq!(s.project.returns, before.returns, "undo returns");
    render_some(engine, s);
    assert!(s.redo(), "redo");
    assert_eq!(s.project.tracks, after.tracks, "redo tracks");
    assert_eq!(s.project.returns, after.returns, "redo returns");
    render_some(engine, s);
}

fn render_some(engine: &mut Engine, s: &mut Session) {
    let (mut l, mut r) = ([0.0; 512], [0.0; 512]);
    engine.process_stereo(&mut l, &mut r);
    s.idle();
    assert!(engine.has_compiled_plan());
}

#[test]
fn undo_round_trips_every_group_and_return_edit() {
    let (mut engine, mut s) = session(project(3));
    commit(&mut s);
    let t = ids(&s.project);
    let mut before = s.project.clone();

    let g = s.group_tracks(&[t[0], t[1]]).unwrap();
    round_trips(&mut s, &mut engine, &before);
    before = s.project.clone();

    let r = s.add_return().unwrap();
    round_trips(&mut s, &mut engine, &before);
    before = s.project.clone();

    s.set_send(g, r, -9.0);
    round_trips(&mut s, &mut engine, &before);
    before = s.project.clone();

    s.set_pre_fader_sends(r, true);
    round_trips(&mut s, &mut engine, &before);
    before = s.project.clone();

    s.add_return();
    round_trips(&mut s, &mut engine, &before);
    before = s.project.clone();

    s.set_send_enabled(r, r, true);
    s.set_send(r, r, -20.0);
    round_trips(&mut s, &mut engine, &before);
    before = s.project.clone();

    s.ungroup(g);
    round_trips(&mut s, &mut engine, &before);
    before = s.project.clone();

    let g2 = s.group_tracks(&t).unwrap();
    commit(&mut s);
    before.clone_from(&s.project);
    s.delete_track(g2);
    round_trips(&mut s, &mut engine, &before);
    before = s.project.clone();

    s.delete_return(r);
    round_trips(&mut s, &mut engine, &before);
}

#[test]
fn undo_brings_a_deleted_return_back_with_its_devices_and_cells() {
    let (mut engine, mut s) = session(project(1));
    let r = s.add_return().unwrap();
    s.add_builtin(ChainRef::Track(r), "rlive.delay");
    let t = ids(&s.project)[0];
    s.set_send(t, r, -3.0);
    commit(&mut s);
    render_some(&mut engine, &mut s);

    s.delete_return(r);
    commit(&mut s);
    assert!(s.undo());
    render_some(&mut engine, &mut s);
    assert_eq!(s.devices(ChainRef::Track(r)).len(), 1, "its devices");
    assert_eq!(s.send_level(t, r), -3.0);
    assert_eq!(
        s.layouts[&ChainRef::Track(t)].base(mix_cell::send(0)),
        Some(-3.0),
        "the cell got the value back"
    );
    assert!(s.track_shared.contains_key(&r));
}

#[test]
fn deleting_a_return_drops_sends_to_it_and_shifts_the_rest_down() {
    let (mut engine, mut s) = session(project(1));
    let t = ids(&s.project)[0];
    let (a, b) = (s.add_return().unwrap(), s.add_return().unwrap());
    s.set_send(t, a, -12.0);
    s.set_send(t, b, -3.0);
    s.delete_return(a);
    render_some(&mut engine, &mut s);
    assert_eq!(s.project.returns[0].id, b);
    assert_eq!(s.send_level(t, b), -3.0);
    assert_eq!(
        s.layouts[&ChainRef::Track(t)].base(mix_cell::send(0)),
        Some(-3.0),
        "send 0 is now return B's"
    );
    assert!(s.project.track(t).unwrap().sends.iter().all(|x| x.to != a));
}

#[test]
fn a_saved_project_keeps_groups_returns_and_sends() {
    let (_e, mut s) = session(project(2));
    let t = ids(&s.project);
    let g = s.group_tracks(&t).unwrap();
    s.set_folded(g, true);
    let r = s.add_return().unwrap();
    s.set_send(t[0], r, -6.0);
    s.set_pre_fader_sends(r, true);
    s.set_send_enabled(r, r, true);
    s.set_solo_in_place(false);
    let text = s.project.to_ron().unwrap();
    let loaded = Project::from_ron(&text).unwrap();
    assert_eq!(loaded, s.project);
    assert!(loaded.track(g).unwrap().ui.folded);
    assert_eq!(loaded.track(t[0]).unwrap().send_level(r), -6.0);
    assert!(!loaded.solo_in_place);
    let (_e2, s2) = session(loaded);
    assert_eq!(s2.project.returns.len(), 1);
}

#[test]
fn a_new_project_saves_nothing_extra() {
    let p = project(1);
    assert!(!p.to_ron().unwrap().contains("solo_in_place"));
}

// ------------------------------------------------------------------ sound

fn dc_wav(dir: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rlive-routing-{}-{dir}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("dc.wav");
    let frames = vec![0.5f32; 48_000 * 2];
    rlive_devices::write_wav(&path, &frames, &frames, 48_000).unwrap();
    path
}

/// `n` tracks, each playing a clip of constant 0.5 in slot 0, launched and left to settle.
fn playing(n: usize, path: &Path) -> (Engine, Session, Vec<TrackId>) {
    let mut p = Project::empty();
    p.quantize = rlive_core::Quantize::None;
    for i in 0..n {
        let mut t = Track::new(&format!("{i}"), TrackKind::Audio, [1, 1, 1], p.scenes.len());
        t.clips[0] = Some(Clip {
            name: "dc".into(),
            color: [1, 1, 1],
            length: 8 * PPQ,
            looped: true,
            content: ClipContent::audio(path, 0.0),
            ..Clip::default()
        });
        p.tracks.push(t);
    }
    let (engine, mut s) = session(p);
    let t = ids(&s.project);
    s.play();
    for id in &t {
        s.launch_clip(*id, 0);
    }
    (engine, s, t)
}

fn level(engine: &mut Engine, s: &mut Session) -> f32 {
    let (l, _) = rlive_testkit::render_chunks(512 * 32, 512, |l, r| {
        engine.process_stereo(l, r);
        s.idle();
    });
    *l.last().unwrap()
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() <= 2e-3 * (1.0 + b.abs())
}

#[test]
fn a_group_is_the_sum_of_what_is_in_it() {
    let path = dc_wav("sum");
    let (mut engine, mut s, t) = playing(2, &path);
    let g = s.group_tracks(&t).unwrap();
    s.project.track_mut(g).unwrap().volume_db = -6.0;
    s.sync_mix(g);
    let got = level(&mut engine, &mut s);
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
    assert!(close(got, 1.0 * 0.5012), "{got}");
}

#[test]
fn a_send_reaches_a_return_and_undo_takes_it_back() {
    let path = dc_wav("send");
    let (mut engine, mut s, t) = playing(1, &path);
    let r = s.add_return().unwrap();
    commit(&mut s);
    let dry = level(&mut engine, &mut s);
    s.set_send(t[0], r, -6.0);
    commit(&mut s);
    let wet = level(&mut engine, &mut s);
    assert!(close(dry, 0.5), "{dry}");
    assert!(close(wet, 0.5 + 0.5 * 0.5012), "{wet}");
    assert!(s.undo());
    let back = level(&mut engine, &mut s);
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
    assert!(close(back, 0.5), "{back}");
}

#[test]
fn solo_in_place_decides_if_a_soloed_tracks_return_is_heard() {
    let path = dc_wav("solo");
    let (mut engine, mut s, t) = playing(2, &path);
    let r = s.add_return().unwrap();
    s.set_send(t[0], r, 0.0);
    s.set_send(t[1], r, 0.0);
    s.project.track_mut(t[0]).unwrap().solo = true;
    s.sync_mix(t[0]);
    let got = level(&mut engine, &mut s);
    assert!(close(got, 0.5 + 0.5), "soloed track plus its send: {got}");
    s.set_solo_in_place(false);
    let got = level(&mut engine, &mut s);
    assert!(close(got, 0.5), "the return stays out of it: {got}");
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn soloing_a_return_keeps_what_every_track_sends_it() {
    let path = dc_wav("retsolo");
    let (mut engine, mut s, t) = playing(2, &path);
    let r = s.add_return().unwrap();
    s.set_send(t[0], r, 0.0);
    s.set_send(t[1], r, 0.0);
    s.project.track_mut(r).unwrap().solo = true;
    s.sync_mix(r);
    let got = level(&mut engine, &mut s);
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
    assert!(close(got, 1.0), "both sends, neither main: {got}");
}

#[test]
fn a_group_slot_launches_and_stops_what_it_holds() {
    let path = dc_wav("slot");
    let (mut engine, mut s, t) = playing(2, &path);
    s.stop_all_clips();
    let g = s.group_tracks(&t).unwrap();
    level(&mut engine, &mut s);
    assert!(s.group_slot_has_clip(g, 0));
    assert!(!s.group_slot_has_clip(g, 1));
    s.launch_group_slot(g, 0);
    let got = level(&mut engine, &mut s);
    assert!(close(got, 1.0), "both children playing: {got}");
    s.stop_group(g);
    let got = level(&mut engine, &mut s);
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
    assert!(close(got, 0.0), "both stopped: {got}");
}
