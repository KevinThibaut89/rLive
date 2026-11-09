use super::*;
use rlive_core::PPQ;
use rlive_project::midi_tools::{Chop, Recombine, Rhythm};

const BAR: u64 = 4 * PPQ;

struct Rig {
    _engine: rlive_engine::Engine,
    s: Session,
    ui: UiState,
    r: ClipRef,
}

fn rig(notes: Vec<NoteData>) -> Rig {
    let (engine, handle) = rlive_engine::create_engine(48_000.0, 512);
    let mut s = Session::new(
        handle,
        rlive_project::Project::new_default(),
        rlive_plugin_host::scanner::PluginCache::default(),
    );
    let t = s.add_track(rlive_project::TrackKind::Midi, None);
    let mut c = rlive_project::Clip::new_midi("c", [9, 9, 9], 1, Default::default());
    *c.notes_mut().unwrap() = notes;
    s.set_clip(t, 0, Some(c));
    s.commit_edits(false);
    let mut ui = UiState::initial(&s, &crate::settings::Settings::default(), String::new());
    let r = ClipRef::slot(t, 0);
    ui.sel.slots = vec![(t, 0)];
    ui.selected_clip = Some(r);
    Rig {
        _engine: engine,
        s,
        ui,
        r,
    }
}

fn note(start: u64, length: u64, key: u8) -> NoteData {
    NoteData::new(start, length, key, 0.8)
}

fn notes(rig: &Rig) -> Vec<NoteData> {
    rig.s.clip_at(rig.r).unwrap().notes().to_vec()
}

fn set_parts(rig: &mut Rig, parts: u32) {
    let tools = &mut rig.ui.clip_view.midi_tools;
    tools.transform = 1;
    tools.transforms[1] = Transform::Chop(Chop {
        parts,
        ..Chop::default()
    });
}

fn change(rig: &mut Rig, side: Side) {
    settings_changed(&mut rig.s, &mut rig.ui, side);
}

fn finish_gesture(rig: &mut Rig) {
    rig.s.commit_edits(false);
}

#[test]
fn auto_apply_is_on_by_default_for_both_panels() {
    let tools = ToolsState::default();
    assert_eq!(tools.auto_apply, [true, true]);
}

#[test]
fn each_setting_change_reworks_the_notes_from_where_they_began() {
    let mut rig = rig(vec![note(0, 800, 60)]);
    rig.ui.piano.selected = vec![0];
    set_parts(&mut rig, 2);
    change(&mut rig, Side::Transform);
    assert_eq!(notes(&rig).len(), 2);
    set_parts(&mut rig, 4);
    change(&mut rig, Side::Transform);
    assert_eq!(
        notes(&rig).len(),
        4,
        "not eight: it starts again from one note"
    );
    assert_eq!(rig.ui.piano.selected.len(), 4);
}

#[test]
fn turning_auto_apply_off_puts_the_notes_back() {
    let original = vec![note(0, 800, 60)];
    let mut rig = rig(original.clone());
    rig.ui.piano.selected = vec![0];
    set_parts(&mut rig, 8);
    change(&mut rig, Side::Transform);
    assert_eq!(notes(&rig).len(), 8);
    set_auto_apply(&mut rig.s, &mut rig.ui, Side::Transform, false);
    assert_eq!(notes(&rig), original);
    assert_eq!(
        rig.ui.piano.selected,
        vec![0],
        "the selection comes back too"
    );
    assert!(!rig.ui.clip_view.midi_tools.auto_apply[0]);
}

#[test]
fn turning_it_off_after_the_notes_were_edited_leaves_them_alone() {
    let mut rig = rig(vec![note(0, 800, 60)]);
    rig.ui.piano.selected = vec![0];
    set_parts(&mut rig, 2);
    change(&mut rig, Side::Transform);
    rig.s.edit_notes(rig.r, "Other", |n| n[0].key = 70);
    let edited = notes(&rig);
    set_auto_apply(&mut rig.s, &mut rig.ui, Side::Transform, false);
    assert_eq!(
        notes(&rig),
        edited,
        "somebody else's edit is not ours to undo"
    );
}

#[test]
fn apply_with_auto_off_is_the_only_thing_that_changes_notes() {
    let mut rig = rig(vec![note(0, 800, 60)]);
    rig.ui.piano.selected = vec![0];
    set_auto_apply(&mut rig.s, &mut rig.ui, Side::Transform, false);
    set_parts(&mut rig, 2);
    let tools = &mut rig.ui.clip_view.midi_tools;
    assert!(!tools.auto_apply[0]);
    assert_eq!(notes(&rig).len(), 1, "nothing yet");
    apply(&mut rig.s, &mut rig.ui, Side::Transform);
    assert_eq!(notes(&rig).len(), 2);
}

#[test]
fn reapplying_builds_on_the_result_with_a_new_seed() {
    let mut rig = rig((0..8).map(|i| note(i * 100, 50, 60 + i as u8)).collect());
    rig.ui.piano.selected = (0..8).collect();
    let tools = &mut rig.ui.clip_view.midi_tools;
    tools.transform = 7;
    tools.transforms[7] = Transform::Recombine(Recombine {
        dimension: rlive_project::midi_tools::Dimension::Pitch,
        shuffle: true,
        ..Recombine::default()
    });
    apply(&mut rig.s, &mut rig.ui, Side::Transform);
    let first = notes(&rig);
    apply(&mut rig.s, &mut rig.ui, Side::Transform);
    assert_ne!(notes(&rig), first, "Shuffle: a new permutation each time");
}

#[test]
fn reset_changes_the_settings_and_not_the_notes() {
    let mut rig = rig(vec![note(0, 800, 60)]);
    rig.ui.piano.selected = vec![0];
    set_parts(&mut rig, 8);
    change(&mut rig, Side::Transform);
    let before = notes(&rig);
    rig.ui.clip_view.midi_tools.reset(Side::Transform);
    assert!(rig.ui.clip_view.midi_tools.at_defaults(Side::Transform));
    notice_changes(&mut rig.s, &mut rig.ui, Side::Transform);
    assert_eq!(notes(&rig), before, "not a change to run Auto Apply for");
}

#[test]
fn undo_takes_back_the_notes_and_not_the_settings() {
    let original = vec![note(0, 800, 60)];
    let mut rig = rig(original.clone());
    rig.ui.piano.selected = vec![0];
    set_parts(&mut rig, 4);
    change(&mut rig, Side::Transform);
    finish_gesture(&mut rig);
    assert!(rig.s.undo());
    assert_eq!(notes(&rig), original);
    match &rig.ui.clip_view.midi_tools.transforms[1] {
        Transform::Chop(c) => assert_eq!(c.parts, 4, "settings are not undone"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_whole_drag_of_a_setting_is_one_undo_step() {
    let original = vec![note(0, 800, 60)];
    let mut rig = rig(original.clone());
    rig.ui.piano.selected = vec![0];
    for parts in 2..8 {
        set_parts(&mut rig, parts);
        change(&mut rig, Side::Transform);
        rig.s.commit_edits(true); // the mouse is still down
    }
    finish_gesture(&mut rig);
    assert_eq!(notes(&rig).len(), 7);
    assert!(rig.s.undo());
    assert_eq!(notes(&rig), original, "one undo goes all the way back");
}

#[test]
fn a_time_selection_narrows_what_a_transformation_and_a_generator_reach() {
    let mut rig = rig(vec![note(0, 100, 60), note(PPQ * 2, 100, 62)]);
    rig.ui.piano.time = Some((0, PPQ));
    set_parts(&mut rig, 2);
    change(&mut rig, Side::Transform);
    let n = notes(&rig);
    assert_eq!(n.iter().filter(|x| x.key == 60).count(), 2);
    assert_eq!(
        n.iter().filter(|x| x.key == 62).count(),
        1,
        "outside the time selection"
    );

    let mut rig = self::rig(vec![]);
    rig.ui.piano.time = Some((PPQ, PPQ * 2));
    let tools = &mut rig.ui.clip_view.midi_tools;
    tools.side = Side::Generate;
    tools.generators[0] = Generator::Rhythm(Rhythm {
        steps: 4,
        density: 4,
        step_duration: PPQ / 4,
        ..Rhythm::default()
    });
    change(&mut rig, Side::Generate);
    let n = notes(&rig);
    assert_eq!(n.len(), 4);
    assert!(n.iter().all(|x| x.start >= PPQ && x.start < PPQ * 2));
}

#[test]
fn a_generator_fills_the_loop_keeps_clear_notes_and_replaces_what_it_lands_on() {
    let mut rig = rig(vec![note(0, 100, 36), note(0, 100, 72)]);
    let tools = &mut rig.ui.clip_view.midi_tools;
    tools.generators[0] = Generator::Rhythm(Rhythm {
        pitch: 36,
        steps: 4,
        density: 4,
        step_duration: BAR / 4,
        ..Rhythm::default()
    });
    change(&mut rig, Side::Generate);
    let n = notes(&rig);
    assert_eq!(
        n.iter().filter(|x| x.key == 36).count(),
        4,
        "the old note is gone"
    );
    assert_eq!(
        n.iter().filter(|x| x.key == 72).count(),
        1,
        "the other pitch stays"
    );
    assert_eq!(
        rig.ui.piano.selected.len(),
        4,
        "what was generated is selected"
    );
}

#[test]
fn a_new_selection_starts_a_new_run() {
    let mut rig = rig(vec![note(0, 800, 60), note(800, 800, 62)]);
    rig.ui.piano.selected = vec![0];
    set_parts(&mut rig, 2);
    change(&mut rig, Side::Transform);
    assert_eq!(notes(&rig).len(), 3);
    // Selecting the other note now: the first one's chop has been left behind.
    let other = notes(&rig).iter().position(|n| n.key == 62).unwrap();
    rig.ui.piano.selected = vec![other];
    set_parts(&mut rig, 3);
    change(&mut rig, Side::Transform);
    let n = notes(&rig);
    assert_eq!(n.iter().filter(|x| x.key == 60).count(), 2);
    assert_eq!(n.iter().filter(|x| x.key == 62).count(), 3);
}

#[test]
fn transformations_reach_the_selection_in_every_shown_clip_unless_focused() {
    let mut rig = rig(vec![note(0, 800, 60)]);
    let t = rig.r.track();
    let mut c = rlive_project::Clip::new_midi("d", [9, 9, 9], 1, Default::default());
    *c.notes_mut().unwrap() = vec![note(0, 800, 72)];
    rig.s.set_clip(t, 1, Some(c));
    let b = ClipRef::slot(t, 1);
    rig.ui.sel.slots = vec![(t, 0), (t, 1)];
    rig.ui.piano.selected = vec![0];
    rig.ui.piano.others.insert(b, vec![0]);
    set_parts(&mut rig, 2);
    change(&mut rig, Side::Transform);
    assert_eq!(rig.s.clip_at(b).unwrap().notes().len(), 2);
    assert_eq!(
        rig.ui.piano.others[&b].len(),
        2,
        "the other clip's selection follows"
    );

    let mut rig2 = self::rig(vec![note(0, 800, 60)]);
    let t = rig2.r.track();
    let mut c = rlive_project::Clip::new_midi("d", [9, 9, 9], 1, Default::default());
    *c.notes_mut().unwrap() = vec![note(0, 800, 72)];
    rig2.s.set_clip(t, 1, Some(c));
    let b = ClipRef::slot(t, 1);
    rig2.ui.sel.slots = vec![(t, 0), (t, 1)];
    rig2.ui.piano.focus_mode = true;
    rig2.ui.piano.selected = vec![0];
    rig2.ui.piano.others.insert(b, vec![0]);
    set_parts(&mut rig2, 2);
    change(&mut rig2, Side::Transform);
    assert_eq!(
        rig2.s.clip_at(b).unwrap().notes().len(),
        1,
        "Focus Mode confines it to the foreground clip"
    );
}

#[test]
fn the_piano_ruler_picks_a_generators_pitch_or_range() {
    let mut tools = ToolsState {
        picking: true,
        ..ToolsState::default()
    };
    assert!(tools.picks_keys(EditorMode::Content));
    assert!(!tools.picks_keys(EditorMode::Mpe));
    tools.pick_keys(40, 43);
    match &tools.generators[0] {
        Generator::Rhythm(r) => assert_eq!(r.pitch, 43),
        other => panic!("{other:?}"),
    }
    tools.choose(Side::Generate, 1); // Seed
    tools.pick_keys(64, 60);
    match &tools.generators[1] {
        Generator::Seed(s) => assert_eq!(s.pitch, (60, 64), "dragging across makes a range"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn nothing_happens_with_nothing_to_work_on() {
    let mut rig = rig(vec![]);
    set_parts(&mut rig, 4);
    change(&mut rig, Side::Transform);
    assert!(notes(&rig).is_empty());
    assert!(rig.ui.clip_view.midi_tools.run.is_none());
}
