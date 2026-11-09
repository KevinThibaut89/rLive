use super::shown::{gather, set_foreground};
use super::*;

fn geo() -> Geo {
    Geo {
        grid: Rect::from_min_size(Pos2::new(100.0, 50.0), Vec2::new(800.0, 240.0)),
        ppb: 80.0,
        kh: 12.0,
        scroll: Vec2::new(0.0, 12.0 * 60.0),
        rows: (0..=127u8).rev().collect(),
    }
}

#[test]
fn positions_and_ticks_round_trip() {
    let g = geo();
    let x = g.x(PPQ as f64);
    assert_eq!(x, 180.0);
    assert!((g.tick(x) - PPQ as f64).abs() < 1e-3);
    assert_eq!(g.key_at(g.y(g.row_of(60).unwrap()) + 3.0), Some(60));
    assert_eq!(g.key_near(-1000.0), 127, "past the top is the top key");
}

#[test]
fn a_notes_edges_are_grabbed_as_its_edges() {
    let r = Rect::from_min_size(Pos2::new(10.0, 0.0), Vec2::new(100.0, 12.0));
    assert_eq!(zone(r, 12.0), Zone::Start);
    assert_eq!(zone(r, 60.0), Zone::Body);
    assert_eq!(zone(r, 108.0), Zone::End);
    // A short note's edges don't crowd out its body.
    let r = Rect::from_min_size(Pos2::new(10.0, 0.0), Vec2::new(10.0, 12.0));
    assert_eq!(zone(r, 15.0), Zone::Body);
}

#[test]
fn folding_leaves_rows_the_geometry_can_find() {
    let mut g = geo();
    g.rows = vec![72, 64, 60];
    g.scroll = Vec2::ZERO;
    assert_eq!(g.row_of(64), Some(1));
    assert_eq!(g.row_of(61), None);
    assert!(
        g.note_rect(0.0, PPQ, 61).is_none(),
        "a folded-away note isn't drawn"
    );
    assert_eq!(g.key_at(g.y(2) + 1.0), Some(60));
    assert_eq!(g.key_at(g.y(2) + 100.0), None);
}

#[test]
fn zooming_keeps_the_time_under_the_pointer() {
    let (ppb, scroll) = zoom_about(100.0, 300.0, 80.0, 2.0);
    assert_eq!(ppb, 160.0);
    // 400 px at 80 ppb is 5 beats, which is 800 px at 160.
    assert_eq!(scroll, 800.0 - 300.0);
}

struct Rig {
    _engine: rlive_engine::Engine,
    s: crate::session::Session,
    ui: UiState,
    ra: ClipRef,
    rb: ClipRef,
}

fn multi_session() -> Rig {
    let (engine, handle) = rlive_engine::create_engine(48_000.0, 512);
    let mut s = crate::session::Session::new(
        handle,
        rlive_project::Project::new_default(),
        rlive_plugin_host::scanner::PluginCache::default(),
    );
    let t = s.add_track(rlive_project::TrackKind::Midi, None);
    let clip = |bars| {
        let mut c = rlive_project::Clip::new_midi("c", [9, 9, 9], bars, Default::default());
        c.notes_mut().unwrap().push(NoteData::new(0, PPQ, 60, 0.8));
        c
    };
    let (a, b) = (clip(1), clip(2));
    s.set_clip(t, 0, Some(a));
    s.set_clip(t, 1, Some(b));
    let mut ui = UiState::initial(&s, &crate::settings::Settings::default(), String::new());
    let (ra, rb) = (ClipRef::slot(t, 0), ClipRef::slot(t, 1));
    ui.sel.slots = vec![(t, 0), (t, 1)];
    ui.selected_clip = Some(ra);
    Rig {
        _engine: engine,
        s,
        ui,
        ra,
        rb,
    }
}

#[test]
fn clips_of_different_lengths_repeat_until_they_realign() {
    let r = multi_session();
    let (s, ui) = (&r.s, &r.ui);
    let (shown, span) = gather(s, ui, ui.selected_clip.unwrap());
    assert_eq!(shown.len(), 2);
    let bar = 4 * PPQ;
    assert_eq!(span.1, 2 * bar);
    let mut at = Vec::new();
    shown[0].span.occurrences(0, &mut at);
    assert_eq!(at.len(), 2, "the one-bar clip shows twice");
    shown[1].span.occurrences(0, &mut at);
    assert_eq!(at.len(), 1);
}

#[test]
fn an_unlooped_clip_stops_session_multi_clip_view() {
    let Rig {
        mut s,
        ui,
        ra,
        rb,
        _engine,
    } = multi_session();
    s.edit_clips(&[rb], "t", |c| c.looped = false);
    let (shown, _) = gather(&s, &ui, ra);
    assert_eq!(shown.len(), 1, "Session clips must all be looped");
    assert_eq!(shown[0].r, ra);
}

#[test]
fn edits_reach_every_clip_unless_focused() {
    let Rig {
        mut s,
        mut ui,
        ra,
        rb,
        _engine,
    } = multi_session();
    ui.piano.selected = vec![0];
    ui.piano.others.insert(rb, vec![0]);
    edit_selected(&mut s, &mut ui, "Move", |n, sel| {
        for &i in sel.iter() {
            n[i].key += 1;
        }
    });
    assert_eq!(s.clip_at(ra).unwrap().notes()[0].key, 61);
    assert_eq!(s.clip_at(rb).unwrap().notes()[0].key, 61);
    ui.piano.focus_mode = true;
    edit_selected(&mut s, &mut ui, "Move", |n, sel| {
        for &i in sel.iter() {
            n[i].key += 1;
        }
    });
    assert_eq!(s.clip_at(ra).unwrap().notes()[0].key, 62);
    assert_eq!(
        s.clip_at(rb).unwrap().notes()[0].key,
        61,
        "Focus Mode edits one clip"
    );
}

#[test]
fn velocity_and_chance_only_touch_the_foreground() {
    let Rig {
        mut s,
        mut ui,
        ra,
        rb,
        _engine,
    } = multi_session();
    ui.piano.selected = vec![0];
    ui.piano.others.insert(rb, vec![0]);
    edit_foreground(&mut s, &mut ui, "Chance", |n, sel| {
        note_edit::set_value(n, sel, note_edit::LaneValue::Chance, 0.25);
    });
    assert_eq!(s.clip_at(ra).unwrap().notes()[0].probability, 0.25);
    assert_eq!(s.clip_at(rb).unwrap().notes()[0].probability, 1.0);
}

#[test]
fn copied_notes_go_back_where_they_were_unless_the_foreground_changed() {
    let Rig {
        mut s,
        mut ui,
        ra,
        rb,
        _engine,
    } = multi_session();
    s.edit_notes(rb, "t", |n| n[0].key = 64);
    ui.piano.selected = vec![0];
    ui.piano.others.insert(rb, vec![0]);
    let copied = copy_selection(&s, &mut ui).unwrap();
    assert_eq!(copied.len(), 2);
    ui.piano.insert_tick = Some(PPQ);
    paste_notes(&mut s, &mut ui, &copied);
    assert_eq!(s.clip_at(ra).unwrap().notes().len(), 2);
    assert_eq!(s.clip_at(rb).unwrap().notes().len(), 2);
    set_foreground(&mut ui, rb);
    ui.piano.insert_tick = Some(2 * PPQ);
    paste_notes(&mut s, &mut ui, &copied);
    assert_eq!(
        s.clip_at(rb).unwrap().notes().len(),
        4,
        "all into the clip in front"
    );
    assert_eq!(s.clip_at(ra).unwrap().notes().len(), 2);
}

#[test]
fn switching_the_foreground_keeps_each_clips_selection() {
    let Rig {
        mut ui,
        rb,
        _engine,
        ..
    } = multi_session();
    ui.piano.selected = vec![0];
    set_foreground(&mut ui, rb);
    assert_eq!(ui.piano.selected, Vec::<usize>::new());
    assert!(ui.piano.others.values().any(|s| s == &vec![0usize]));
}

#[test]
fn filters_select_as_their_settings_change() {
    let Rig {
        s,
        mut ui,
        ra,
        rb,
        _engine,
    } = multi_session();
    let (shown, _) = gather(&s, &ui, ra);
    ui.piano.filters.rows[0].applied = true; // Pitch: C, in every octave.
    let fg = shown.iter().position(|x| x.r == ra).unwrap();
    filters::apply(&s, &mut ui, &shown, fg);
    assert_eq!(ui.piano.selected, vec![0]);
    assert_eq!(ui.piano.others.get(&rb), Some(&vec![0]));
    ui.piano.filters.rows[0].invert = true;
    filters::apply(&s, &mut ui, &shown, fg);
    assert!(ui.piano.selected.is_empty(), "Invert finds the rest");
}

#[test]
fn a_gesture_is_one_undo_step() {
    let Rig {
        mut s,
        mut ui,
        ra,
        _engine,
        ..
    } = multi_session();
    s.commit_edits(false);
    let before = s.history.undo_label();
    let _ = before;
    // Several frames of a drag land as one step, as the app holds the commit while the pointer's
    // down.
    for key in 61..64 {
        s.clip_at_mut(ra).unwrap().notes_mut().unwrap()[0].key = key;
        s.history.pending = Some("Move Notes");
        s.sync_ref(ra);
        s.commit_edits(true);
    }
    s.commit_edits(false);
    assert!(s.undo());
    assert_eq!(
        s.clip_at(ra).unwrap().notes()[0].key,
        60,
        "all of it undone at once"
    );
    let _ = &mut ui;
}

#[test]
fn a_note_dragged_past_the_loop_comes_round_again() {
    use rlive_project::note_view::ClipSpan;
    let bar = 4 * PPQ;
    let pick = gestures::Pick {
        r: ClipRef::Slot {
            track: rlive_core::TrackId::new(),
            slot: 0,
        },
        base: Vec::new(),
        picks: Vec::new(),
        span: ClipSpan::Placed {
            region: rlive_core::ClipRegion::whole(bar as f64, true),
            origin: 0.0,
            from: 0.0,
            to: 8.0 * bar as f64,
        },
        limit: bar,
    };
    assert_eq!(gestures::moved_start(&pick, PPQ, bar as i64), PPQ);
    assert_eq!(gestures::moved_start(&pick, 0, -(PPQ as i64)), bar - PPQ);
}

#[test]
fn arrangement_clips_are_seen_on_the_timeline_from_first_to_last() {
    use rlive_project::ArrangementClip;
    let (engine, handle) = rlive_engine::create_engine(48_000.0, 512);
    let mut s = crate::session::Session::new(
        handle,
        rlive_project::Project::new_default(),
        rlive_plugin_host::scanner::PluginCache::default(),
    );
    let t = s.add_track(rlive_project::TrackKind::Midi, None);
    let bar = 4 * PPQ;
    let make = |start| {
        let mut c = rlive_project::Clip::new_midi("c", [9, 9, 9], 1, Default::default());
        c.looped = false;
        c.notes_mut().unwrap().push(NoteData::new(0, PPQ, 60, 0.8));
        ArrangementClip::new(c, start)
    };
    let (a, b) = (make(bar), make(3 * bar));
    let (ra, rb) = (a.id, b.id);
    s.arr_place(t, a);
    s.arr_place(t, b);
    let mut ui = UiState::initial(&s, &crate::settings::Settings::default(), String::new());
    let r = ClipRef::Arr { track: t, id: ra };
    ui.selected_clip = Some(r);
    ui.arr.selection = vec![(t, ra), (t, rb)];
    let (shown, span) = gather(&s, &ui, r);
    assert_eq!(shown.len(), 2);
    assert_eq!(span, (bar, 4 * bar), "no silence before the first clip");
    let mut at = Vec::new();
    shown[1].span.occurrences(0, &mut at);
    assert_eq!(at, vec![(3 * bar) as f64]);
    drop(engine);
}

#[test]
fn a_new_clip_takes_the_last_shown_scale_and_its_state() {
    let r = multi_session();
    let mut s = r.s;
    let minor = rlive_project::ClipScale {
        enabled: true,
        root: 9,
        name: "Minor".into(),
    };
    s.note_recent_scale(&minor);
    assert_eq!(s.new_midi_clip([1, 2, 3], Default::default()).scale, minor);
    let off = rlive_project::ClipScale {
        enabled: false,
        ..minor
    };
    s.note_recent_scale(&off);
    assert!(!s.new_midi_clip([1, 2, 3], Default::default()).scale.enabled);
}
