use super::*;
use crate::{Breakpoint, NoteExpression};
use rlive_core::PPQ;

const BAR: u64 = 4 * PPQ;
const SIXTEENTH: u64 = PPQ / 4;
const MAJOR: PitchScale = (0, 0b1010_1011_0101);

fn cx() -> ToolContext {
    ToolContext {
        range: (0, BAR),
        end: BAR,
        grid: SIXTEENTH,
        scale: None,
        seed: 7,
        bend_range: 48.0,
    }
}

fn note(start: u64, length: u64, key: u8) -> NoteData {
    NoteData::new(start, length, key, 0.8)
}

fn run(tool: Transform, notes: &[NoteData], cx: &ToolContext) -> Vec<NoteData> {
    let picks: Vec<usize> = (0..notes.len()).collect();
    let mut out = apply_transform(&tool, notes, &picks, cx).notes;
    out.sort_by_key(|n| (n.start, n.key));
    out
}

fn keys(notes: &[NoteData]) -> Vec<u8> {
    notes.iter().map(|n| n.key).collect()
}

fn starts(notes: &[NoteData]) -> Vec<u64> {
    notes.iter().map(|n| n.start).collect()
}

fn chord() -> Vec<NoteData> {
    vec![note(0, PPQ, 60), note(0, PPQ, 64), note(0, PPQ, 67)]
}

// ---- the framework

#[test]
fn a_seed_decides_randomness_and_nothing_else_does() {
    let tool = Transform::Span(Span {
        variation: 0.8,
        ..Span::default()
    });
    let notes = vec![note(0, PPQ, 60), note(PPQ, PPQ, 62), note(2 * PPQ, PPQ, 64)];
    assert_eq!(
        run(tool.clone(), &notes, &cx()),
        run(tool.clone(), &notes, &cx())
    );
    let other = ToolContext { seed: 8, ..cx() };
    assert_ne!(run(tool.clone(), &notes, &cx()), run(tool, &notes, &other));
}

#[test]
fn every_tool_comes_with_a_name_and_returns_to_its_defaults() {
    assert_eq!(Transform::all().len(), 12);
    assert_eq!(Generator::all().len(), 5);
    for t in Transform::all() {
        assert_eq!(t.defaults(), t);
    }
    let tuned = Transform::Chop(Chop {
        parts: 9,
        ..Chop::default()
    });
    assert_eq!(tuned.defaults(), Transform::Chop(Chop::default()));
}

#[test]
fn scope_is_the_selection_or_the_time_or_the_loop() {
    let notes = vec![note(0, 100, 60), note(PPQ, 100, 62), note(2 * BAR, 100, 64)];
    let lp = (0, BAR);
    let s = transform_scope(&notes, &[1], None, false, lp);
    assert_eq!(s.picks, vec![1]);
    assert_eq!(s.range, (PPQ, PPQ + 100));
    assert_eq!(s.end, BAR, "legato runs to the end of the loop");

    let time = Some((0, PPQ * 2));
    let s = transform_scope(&notes, &[1], time, false, lp);
    assert_eq!(
        s.range,
        (0, PPQ * 2),
        "a time selection around the notes bounds them"
    );
    assert_eq!(s.end, PPQ * 2);

    let s = transform_scope(&notes, &[], time, false, lp);
    assert_eq!(
        s.picks,
        vec![0, 1],
        "the notes starting in the time selection"
    );

    let s = transform_scope(&notes, &[2], time, true, lp);
    assert_eq!(
        s.picks,
        vec![0, 1],
        "the time selection has taken over from the notes"
    );

    let s = transform_scope(&notes, &[], None, false, lp);
    assert_eq!(
        s.picks,
        vec![0, 1],
        "the loop, and what's past it is left out"
    );
    assert_eq!(generate_range(time, lp), (0, PPQ * 2));
    assert_eq!(generate_range(None, lp), lp);
    assert_eq!(
        generate_range(Some((5, 5)), lp),
        lp,
        "an empty selection is none"
    );
}

#[test]
fn a_transformation_only_touches_what_it_was_given() {
    let notes = vec![note(0, PPQ, 60), note(0, PPQ, 72)];
    let tool = Transform::Chop(Chop::default());
    let out = apply_transform(&tool, &notes, &[0], &cx());
    assert_eq!(out.notes.iter().filter(|n| n.key == 72).count(), 1);
    assert_eq!(out.notes.iter().filter(|n| n.key == 60).count(), 4);
    assert_eq!(out.selected.len(), 4, "what it made is what's selected");
    assert!(out.selected.iter().all(|i| out.notes[*i].key == 60));
}

#[test]
fn generators_replace_only_what_they_land_on() {
    let tool = Generator::Rhythm(Rhythm {
        pitch: 36,
        steps: 4,
        density: 1,
        step_duration: PPQ,
        ..Rhythm::default()
    });
    let ctx = ToolContext {
        range: (0, PPQ * 4),
        ..cx()
    };
    let existing = vec![
        note(0, 100, 36),          // hit: same key, overlapped
        note(PPQ * 3 / 2, 10, 36), // same key, between generated notes
        note(0, 100, 60),          // other key
    ];
    let out = apply_generator(&tool, &existing, &ctx);
    assert!(out.notes.iter().any(|n| n.key == 60));
    assert!(out
        .notes
        .iter()
        .any(|n| n.start == PPQ * 3 / 2 && n.length == 10));
    assert!(
        !out.notes
            .iter()
            .any(|n| n.start == 0 && n.length == 100 && n.key == 36),
        "the note landed on goes"
    );
    assert_eq!(out.selected.len(), 1, "only what was generated is selected");
}

// ---- Arpeggiate

fn arp(style: ArpStyle) -> Arpeggiate {
    Arpeggiate {
        style,
        rate: PPQ / 4,
        gate: 1.0,
        distance: 12,
        steps: 0,
    }
}

#[test]
fn arpeggiate_plays_the_chord_note_by_note_in_style() {
    let out = run(Transform::Arpeggiate(arp(ArpStyle::Up)), &chord(), &cx());
    assert_eq!(out.len(), 4, "a beat of sixteenths");
    assert_eq!(keys(&out), vec![60, 64, 67, 60]);
    assert_eq!(starts(&out), [0, 1, 2, 3].map(|x| x * SIXTEENTH).to_vec());
    assert_eq!(out[0].length, SIXTEENTH);
    let down = run(Transform::Arpeggiate(arp(ArpStyle::Down)), &chord(), &cx());
    assert_eq!(keys(&down), vec![67, 64, 60, 67]);
}

#[test]
fn arpeggiate_has_all_eighteen_styles() {
    assert_eq!(ArpStyle::ALL.len(), 18);
    for style in ArpStyle::ALL {
        let out = run(Transform::Arpeggiate(arp(style)), &chord(), &cx());
        assert!(!out.is_empty(), "{}", style.label());
    }
}

#[test]
fn arpeggiate_styles_match_the_arpeggiators_patterns() {
    let long = vec![
        note(0, BAR, 60),
        note(0, BAR, 64),
        note(0, BAR, 67),
        note(0, BAR, 71),
    ];
    let seq = |style| {
        let mut a = arp(style);
        a.rate = BAR / 8;
        keys(&run(Transform::Arpeggiate(a), &long, &cx()))
    };
    assert_eq!(seq(ArpStyle::UpDown), [60, 64, 67, 71, 67, 64, 60, 64]);
    assert_eq!(seq(ArpStyle::Converge), [60, 71, 64, 67, 60, 71, 64, 67]);
    assert_eq!(seq(ArpStyle::ThumbUp), [60, 64, 60, 67, 60, 71, 60, 64]);
    assert_eq!(seq(ArpStyle::PinkyUp), [60, 71, 64, 71, 67, 71, 60, 71]);
}

#[test]
fn arpeggiate_gate_and_steps_and_chord_trigger() {
    let mut a = arp(ArpStyle::Up);
    a.gate = 0.5;
    assert_eq!(
        run(Transform::Arpeggiate(a), &chord(), &cx())[0].length,
        SIXTEENTH / 2
    );
    let mut a = arp(ArpStyle::Up);
    a.gate = 1.5;
    assert_eq!(
        run(Transform::Arpeggiate(a), &chord(), &cx())[0].length,
        SIXTEENTH * 3 / 2,
        "above 100% lengthens"
    );
    let mut a = arp(ArpStyle::Up);
    a.steps = 1;
    a.rate = PPQ / 8;
    let ks = keys(&run(Transform::Arpeggiate(a), &chord(), &cx()));
    assert_eq!(
        &ks[..6],
        &[60, 64, 67, 72, 76, 79],
        "the second pass is an octave up"
    );
    let a = arp(ArpStyle::ChordTrigger);
    let out = run(Transform::Arpeggiate(a), &chord(), &cx());
    assert_eq!(&starts(&out)[..3], &[0, 0, 0]);
}

#[test]
fn arpeggiate_distance_counts_degrees_with_a_scale() {
    let mut a = arp(ArpStyle::Up);
    a.distance = 1;
    a.steps = 1;
    a.rate = PPQ;
    let notes = vec![note(0, 2 * PPQ, 60)];
    let scaled = ToolContext {
        scale: Some(MAJOR),
        ..cx()
    };
    let ks = keys(&run(Transform::Arpeggiate(a.clone()), &notes, &scaled));
    assert_eq!(ks, [60, 62], "one degree up from C is D");
    let ks = keys(&run(Transform::Arpeggiate(a), &notes, &cx()));
    assert_eq!(ks, [60, 61], "one semitone without a scale");
}

// ---- Chop

#[test]
fn chop_divides_into_parts() {
    let t = Chop {
        parts: 8,
        ..Chop::default()
    };
    let out = run(Transform::Chop(t), &[note(0, 8 * 100, 60)], &cx());
    assert_eq!(out.len(), 8);
    assert_eq!(out[3].start, 300);
    assert_eq!(out[3].length, 100);
}

#[test]
fn chop_gaps_follow_the_manuals_examples() {
    let mut t = Chop {
        parts: 6,
        ..Chop::default()
    };
    // "When set to 2, a gap replaces a note in the sequence after two successive notes."
    t.set_gaps(2);
    assert_eq!(&t.pattern[..6], &[true, true, false, true, true, false]);
    // "When set to -2, two gaps are added after each note."
    t.set_gaps(-2);
    assert_eq!(&t.pattern[..6], &[true, false, false, true, false, false]);
    let out = run(Transform::Chop(t), &[note(0, 600, 60)], &cx());
    assert_eq!(starts(&out), vec![0, 300]);
}

#[test]
fn chop_pattern_toggles_clear_the_gaps_control_and_repeat_past_sixteen() {
    let mut t = Chop {
        parts: 32,
        ..Chop::default()
    };
    t.set_gaps(1);
    assert_eq!(t.gaps, Some(1));
    t.toggle_part(0);
    assert_eq!(t.gaps, None, "the control shows a dash");
    let out = run(Transform::Chop(t.clone()), &[note(0, 3200, 60)], &cx());
    let starts = starts(&out);
    assert!(!starts.contains(&0));
    assert!(
        !starts.contains(&1600),
        "part 17 is the pattern's first again"
    );
    t.set_gaps(0);
    assert!(
        t.pattern.iter().all(|p| *p),
        "the control replaces hand-made gaps"
    );
}

#[test]
fn chop_is_limited_to_sixty_four_parts() {
    let t = Chop {
        parts: 500,
        ..Chop::default()
    };
    assert_eq!(
        run(Transform::Chop(t), &[note(0, 6400, 60)], &cx()).len(),
        64
    );
}

#[test]
fn chop_emphasis_stretches_a_part() {
    let mut t = Chop {
        parts: 2,
        stretch: 3,
        ..Chop::default()
    };
    t.emphasis[0] = true;
    let out = run(Transform::Chop(t), &[note(0, 400, 60)], &cx());
    assert_eq!(out[0].length, 300);
    assert_eq!(out[1].start, 300);
    assert_eq!(out[1].length, 100);
}

#[test]
fn chop_variation_moves_parts_only_when_asked() {
    let t = Chop {
        parts: 4,
        variation: 1.0,
        ..Chop::default()
    };
    let out = run(Transform::Chop(t), &[note(0, 400, 60)], &cx());
    assert_ne!(starts(&out), vec![0, 100, 200, 300]);
    assert!(out.iter().all(|n| n.start < 400));
}

#[test]
fn chop_divides_expression_with_the_note() {
    let mut n = note(0, 400, 60);
    n.expr = Some(Box::new(NoteExpression {
        pitch: vec![Breakpoint::new(0.0, 0.0), Breakpoint::new(400.0, 4.0)],
        ..Default::default()
    }));
    let t = Chop {
        parts: 2,
        ..Chop::default()
    };
    let out = run(Transform::Chop(t), &[n], &cx());
    let second = out[1].expr.as_ref().unwrap();
    assert_eq!(second.pitch[0].value, 2.0, "it picks up where the note was");
    assert_eq!(second.pitch[0].time, 0.0);
}

// ---- Connect

#[test]
fn connect_fills_the_gap_at_full_density() {
    let t = Connect {
        spread: 0,
        density: 1.0,
        rate: 100,
        tie: 0.0,
    };
    let out = run(
        Transform::Connect(t),
        &[note(0, 100, 60), note(500, 100, 68)],
        &cx(),
    );
    let new: Vec<_> = out
        .iter()
        .filter(|n| n.start > 0 && n.start < 500)
        .collect();
    assert_eq!(new.len(), 4);
    assert_eq!(
        new.iter().map(|n| n.start).collect::<Vec<_>>(),
        [100, 200, 300, 400]
    );
    assert_eq!(new[0].length, 100);
    assert_eq!(
        new[1].key, 63,
        "the line from one note to the next, with no spread"
    );
}

#[test]
fn connect_density_fills_a_share_and_tie_reaches_the_next_note() {
    let thin = Connect {
        spread: 0,
        density: 0.5,
        rate: 100,
        tie: 0.0,
    };
    let notes = [note(0, 100, 60), note(500, 100, 60)];
    assert_eq!(run(Transform::Connect(thin), &notes, &cx()).len(), 2 + 2);
    let tied = Connect {
        spread: 0,
        density: 1.0,
        rate: 100,
        tie: 1.0,
    };
    let out = run(Transform::Connect(tied), &notes, &cx());
    let new: Vec<_> = out
        .iter()
        .filter(|n| n.start > 0 && n.start < 500)
        .collect();
    assert_eq!(new.len(), 1);
    assert_eq!(new[0].start + new[0].length, 500);
}

#[test]
fn connect_spread_is_in_degrees_with_a_scale() {
    let t = Connect {
        spread: 2,
        density: 1.0,
        rate: 50,
        tie: 0.0,
    };
    let notes = [note(0, 100, 60), note(1000, 100, 60)];
    let scaled = ToolContext {
        scale: Some(MAJOR),
        ..cx()
    };
    let out = run(Transform::Connect(t), &notes, &scaled);
    for n in out {
        assert!(clip_edit::in_scale(i32::from(n.key), MAJOR), "{}", n.key);
        assert!(
            (57..=64).contains(&n.key),
            "at most two degrees away: {}",
            n.key
        );
    }
}

// ---- Glissando

fn pitch(n: &NoteData) -> Vec<(f64, f64)> {
    n.expr
        .as_ref()
        .map(|e| e.pitch.iter().map(|p| (p.time, p.value)).collect())
        .unwrap_or_default()
}

#[test]
fn glissando_bends_to_the_next_note_from_start() {
    let t = Glissando {
        start: 0.5,
        curve: 0.3,
    };
    let out = run(
        Transform::Glissando(t),
        &[note(0, 400, 60), note(400, 400, 67)],
        &cx(),
    );
    assert_eq!(pitch(&out[0]), vec![(0.0, 0.0), (200.0, 0.0), (400.0, 7.0)]);
    assert!((out[0].expr.as_ref().unwrap().pitch[1].curve - 0.3).abs() < 1e-5);
    assert!(out[1].expr.is_none(), "the last note has nowhere to go");
}

#[test]
fn glissando_needs_two_notes() {
    let t = Transform::Glissando(Glissando::default());
    assert!(run(t, &[note(0, 400, 60)], &cx())[0].expr.is_none());
}

#[test]
fn glissando_stays_in_the_bend_range() {
    let t = Glissando {
        start: 0.0,
        curve: 0.0,
    };
    let ctx = ToolContext {
        bend_range: 12.0,
        ..cx()
    };
    let out = run(
        Transform::Glissando(t),
        &[note(0, 400, 40), note(400, 400, 90)],
        &ctx,
    );
    assert_eq!(pitch(&out[0]), vec![(0.0, 0.0), (400.0, 12.0)]);
}

// ---- LFO

fn lfo(target: LfoTarget, rate: u64) -> Lfo {
    Lfo {
        target,
        rate,
        ..Lfo::default()
    }
}

#[test]
fn lfo_writes_a_period_of_sine_into_pitch() {
    let mut t = lfo(LfoTarget::Pitch, 480);
    t.amount = 2.0;
    let out = run(Transform::Lfo(t), &[note(0, 480, 60)], &cx());
    let p = &out[0].expr.as_ref().unwrap().pitch;
    let at = |x: f64| rlive_core::envelope::value_at(p, x, Default::default()).unwrap();
    assert!((at(0.0)).abs() < 1e-6);
    assert!((at(120.0) - 2.0).abs() < 1e-3, "{}", at(120.0));
    assert!((at(360.0) + 2.0).abs() < 1e-3);
}

#[test]
fn lfo_writes_only_its_target() {
    let t = lfo(LfoTarget::Slide, PPQ);
    let out = run(Transform::Lfo(t), &[note(0, PPQ, 60)], &cx());
    let e = out[0].expr.as_ref().unwrap();
    assert!(!e.slide.is_empty() && e.pitch.is_empty() && e.pressure.is_empty());
}

#[test]
fn lfo_amounts_are_in_seven_bit_units_for_slide_and_pressure() {
    let mut t = lfo(LfoTarget::Pressure, 480);
    t.amount = 127.0;
    t.base = 0.0;
    let out = run(Transform::Lfo(t), &[note(0, 480, 60)], &cx());
    let p = &out[0].expr.as_ref().unwrap().pressure;
    let top = p.iter().map(|x| x.value).fold(0.0, f64::max);
    assert!((top - 1.0).abs() < 1e-6);
    assert!(p.iter().all(|x| (0.0..=1.0).contains(&x.value)));
}

#[test]
fn lfo_attack_and_decay_share_the_oscillation() {
    assert_eq!(lfo_balance(1.0, 0.5, true), (1.0, 0.0));
    assert_eq!(lfo_balance(0.25, 0.5, true), (0.25, 0.5));
    assert_eq!(lfo_balance(0.75, 0.5, false), (0.5, 0.5));
    let mut t = lfo(LfoTarget::Pitch, 480);
    t.amount = 2.0;
    t.attack = 1.0;
    let out = run(Transform::Lfo(t), &[note(0, 480, 60)], &cx());
    let p = &out[0].expr.as_ref().unwrap().pitch;
    let first_peak = p.iter().take(9).map(|x| x.value.abs()).fold(0.0, f64::max);
    assert!(
        first_peak < 0.6,
        "the envelope starts at nothing: {first_peak}"
    );
}

#[test]
fn lfo_time_shift_delays_or_shifts_phase() {
    let mut delayed = lfo(LfoTarget::Pitch, 480);
    delayed.amount = 2.0;
    delayed.base = 0.0;
    delayed.time_shift = 0.5;
    let out = run(Transform::Lfo(delayed), &[note(0, 960, 60)], &cx());
    let p = &out[0].expr.as_ref().unwrap().pitch;
    assert_eq!(p[0].time, 0.0);
    assert_eq!(p[0].value, 0.0);
    assert_eq!(p[1].time, 240.0, "the oscillation starts half a period in");

    let mut phased = lfo(LfoTarget::Pitch, 480);
    phased.amount = 2.0;
    phased.time_shift = -0.25;
    let out = run(Transform::Lfo(phased), &[note(0, 480, 60)], &cx());
    let p = &out[0].expr.as_ref().unwrap().pitch;
    assert!(
        (p[0].value - 2.0).abs() < 1e-3,
        "a quarter of the way through a sine is its top"
    );
}

#[test]
fn lfo_random_is_seeded_and_reseed_changes_it() {
    let mut t = lfo(LfoTarget::Pitch, 100);
    t.shape_type = LfoShape::Random;
    t.amount = 1.0;
    let notes = [note(0, 400, 60)];
    let a = run(Transform::Lfo(t.clone()), &notes, &cx());
    assert_eq!(a, run(Transform::Lfo(t.clone()), &notes, &cx()));
    t.reseed = 3;
    assert_ne!(a, run(Transform::Lfo(t), &notes, &cx()));
}

#[test]
fn lfo_shapes_have_their_own_character() {
    let mut t = lfo(LfoTarget::Pitch, 480);
    t.amount = 1.0;
    t.shape_type = LfoShape::Square;
    let out = run(Transform::Lfo(t.clone()), &[note(0, 480, 60)], &cx());
    let p = &out[0].expr.as_ref().unwrap().pitch;
    assert!(p.iter().all(|x| x.value.abs() == 1.0));
    t.shape_type = LfoShape::Triangle;
    let out = run(Transform::Lfo(t), &[note(0, 480, 60)], &cx());
    let p = &out[0].expr.as_ref().unwrap().pitch;
    assert!(
        (p[8].value).abs() < 1e-6,
        "half way up from the bottom is the middle: {}",
        p[8].value
    );
}

// ---- Ornament

#[test]
fn flam_before_the_note_is_a_share_of_the_grid() {
    let t = Ornament {
        kind: OrnamentKind::Flam,
        flam_position: -1.0,
        flam_velocity: 0.5,
        ..Ornament::default()
    };
    let out = run(Transform::Ornament(t), &[note(960, 480, 60)], &cx());
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].start, 960 - SIXTEENTH);
    assert_eq!(out[0].length, SIXTEENTH);
    assert!((out[0].velocity - 0.4).abs() < 1e-5);
    assert_eq!(out[1].start, 960, "the note itself stays");
    assert_eq!(out[1].length, 480);
}

#[test]
fn flam_at_a_positive_position_replaces_the_notes_start() {
    let t = Ornament {
        kind: OrnamentKind::Flam,
        flam_position: 0.5,
        ..Ornament::default()
    };
    let out = run(Transform::Ornament(t), &[note(960, 480, 60)], &cx());
    assert_eq!(out[0].start, 960);
    assert_eq!(out[0].length, SIXTEENTH / 2);
    assert_eq!(out[1].start, 960 + SIXTEENTH / 2);
    assert_eq!(
        out[1].start + out[1].length,
        960 + 480,
        "its end hasn't moved"
    );
}

#[test]
fn reapplying_ornament_adds_more() {
    let t = Transform::Ornament(Ornament {
        kind: OrnamentKind::Flam,
        flam_position: -1.0,
        ..Ornament::default()
    });
    let notes = vec![note(960, 480, 60)];
    let first = apply_transform(&t, &notes, &[0], &cx());
    let second = apply_transform(&t, &first.notes, &first.selected, &cx());
    assert_eq!(second.notes.len(), 3);
    let mut s = starts(&second.notes);
    s.sort_unstable();
    assert_eq!(s, vec![960 - 2 * SIXTEENTH, 960 - SIXTEENTH, 960]);
}

#[test]
fn grace_notes_alternate_pitch_are_even_and_take_chance() {
    let t = Ornament {
        kind: OrnamentKind::Grace,
        grace_pitch: GracePitch::High,
        grace_position: -1.0,
        grace_velocity: 0.5,
        grace_chance: 0.5,
        grace_amount: 3,
        ..Ornament::default()
    };
    let mut n = note(960, 480, 60);
    n.probability = 0.8;
    let out = run(Transform::Ornament(t.clone()), &[n], &cx());
    assert_eq!(out.len(), 4);
    assert_eq!(
        keys(&out[..3]),
        vec![61, 60, 61],
        "every other one is a semitone up"
    );
    let each = SIXTEENTH / 3;
    assert_eq!(out[0].length, each);
    assert_eq!(out[1].start - out[0].start, each);
    assert!((out[0].probability - 0.4).abs() < 1e-5);
    let low = Ornament {
        grace_pitch: GracePitch::Low,
        ..t
    };
    let scaled = ToolContext {
        scale: Some(MAJOR),
        ..cx()
    };
    let out = run(Transform::Ornament(low), &[note(960, 480, 60)], &scaled);
    assert_eq!(
        keys(&out[..3]),
        vec![59, 60, 59],
        "or a degree down with a scale"
    );
}

// ---- Quantize

#[test]
fn quantize_tool_moves_by_the_value_or_grid_and_can_do_ends() {
    let mut t = QuantizeTool::default();
    t.settings.grid = PPQ;
    let out = run(
        Transform::Quantize(t.clone()),
        &[note(PPQ / 4, 100, 60)],
        &cx(),
    );
    assert_eq!(out[0].start, 0);
    t.use_grid = true;
    let out = run(
        Transform::Quantize(t.clone()),
        &[note(PPQ / 4 + 3, 100, 60)],
        &cx(),
    );
    assert_eq!(out[0].start, SIXTEENTH, "the current grid when asked");
    t.use_grid = false;
    t.settings.amount = 0.5;
    let out = run(
        Transform::Quantize(t.clone()),
        &[note(PPQ / 4, 100, 60)],
        &cx(),
    );
    assert_eq!(out[0].start, PPQ / 8, "half way there");
    t.settings.amount = 1.0;
    t.settings.starts = false;
    t.settings.ends = true;
    let out = run(
        Transform::Quantize(t.clone()),
        &[note(0, PPQ * 3 / 2 + 3, 60)],
        &cx(),
    );
    assert_eq!(out[0].length, PPQ * 2, "the end to the next value");
    t.settings.grid = PPQ / 3;
    t.settings.starts = true;
    t.settings.ends = false;
    let out = run(Transform::Quantize(t), &[note(PPQ / 3 + 5, 10, 60)], &cx());
    assert_eq!(out[0].start, PPQ / 3, "triplets are a value like any other");
}

// ---- Recombine

fn four() -> Vec<NoteData> {
    let mut v = vec![
        note(0, 100, 60),
        note(100, 200, 61),
        note(300, 300, 62),
        note(600, 400, 63),
    ];
    for (i, n) in v.iter_mut().enumerate() {
        n.velocity = 0.2 * (i + 1) as f32;
    }
    v
}

#[test]
fn recombine_mirror_and_rotate() {
    let t = Recombine {
        dimension: Dimension::Pitch,
        mirror: true,
        ..Recombine::default()
    };
    assert_eq!(
        keys(&run(Transform::Recombine(t), &four(), &cx())),
        [63, 62, 61, 60]
    );
    let t = Recombine {
        dimension: Dimension::Pitch,
        rotate: 1,
        ..Recombine::default()
    };
    assert_eq!(
        keys(&run(Transform::Recombine(t), &four(), &cx())),
        [63, 60, 61, 62]
    );
    let t = Recombine {
        dimension: Dimension::Pitch,
        rotate: -1,
        ..Recombine::default()
    };
    assert_eq!(
        keys(&run(Transform::Recombine(t), &four(), &cx())),
        [61, 62, 63, 60]
    );
}

#[test]
fn recombine_runs_in_shuffle_mirror_rotate_order() {
    let both = Recombine {
        dimension: Dimension::Pitch,
        mirror: true,
        rotate: 1,
        ..Recombine::default()
    };
    let out = keys(&run(Transform::Recombine(both), &four(), &cx()));
    assert_eq!(out, [60, 63, 62, 61], "mirrored, then rotated");
    let shuffled = Recombine {
        dimension: Dimension::Pitch,
        shuffle: true,
        ..Recombine::default()
    };
    let a = keys(&run(Transform::Recombine(shuffled.clone()), &four(), &cx()));
    let mut sorted = a.clone();
    sorted.sort_unstable();
    assert_eq!(
        sorted,
        vec![60, 61, 62, 63],
        "a permutation, not new values"
    );
    let other = ToolContext { seed: 99, ..cx() };
    assert_ne!(
        a,
        keys(&run(Transform::Recombine(shuffled), &four(), &other))
    );
}

#[test]
fn recombine_permutes_each_dimension() {
    let d = |dimension| Recombine {
        dimension,
        mirror: true,
        ..Recombine::default()
    };
    let out = run(Transform::Recombine(d(Dimension::Duration)), &four(), &cx());
    assert_eq!(
        out.iter().map(|n| n.length).collect::<Vec<_>>(),
        [400, 300, 200, 100]
    );
    let out = run(Transform::Recombine(d(Dimension::Velocity)), &four(), &cx());
    assert!((out[0].velocity - 0.8).abs() < 1e-5);
    let out = run(Transform::Recombine(d(Dimension::Position)), &four(), &cx());
    let k63 = out.iter().find(|n| n.key == 63).unwrap();
    assert_eq!(k63.start, 0, "the last note takes the first note's place");
}

#[test]
fn recombine_rotate_on_grid_wraps_around_the_range_by_cells() {
    let t = Recombine {
        dimension: Dimension::Position,
        rotate: 1,
        rotate_on_grid: true,
        ..Recombine::default()
    };
    let ctx = ToolContext {
        range: (0, 400),
        grid: 100,
        ..cx()
    };
    let out = run(
        Transform::Recombine(t.clone()),
        &[note(0, 50, 60), note(300, 50, 61)],
        &ctx,
    );
    let s = |key| out.iter().find(|n| n.key == key).unwrap().start;
    assert_eq!(s(60), 100);
    assert_eq!(s(61), 0, "past the end comes round to the start");
    assert_eq!(t.most_steps(2, 400, 100), 3, "one fewer than the cells");
    let by_notes = Recombine {
        rotate_on_grid: false,
        ..t
    };
    assert_eq!(
        by_notes.most_steps(4, 400, 100),
        3,
        "one fewer than the notes"
    );
}

// ---- Span

#[test]
fn span_legato_runs_to_the_next_note_and_the_last_to_the_end() {
    let t = Span {
        articulation: Articulation::Legato,
        ..Span::default()
    };
    let ctx = ToolContext { end: 1000, ..cx() };
    let notes = [
        note(0, 10, 60),
        note(0, 10, 64),
        note(300, 10, 62),
        note(500, 10, 65),
    ];
    let out = run(Transform::Span(t), &notes, &ctx);
    assert_eq!(
        out.iter().map(|n| n.length).collect::<Vec<_>>(),
        [300, 300, 200, 500]
    );
}

#[test]
fn span_tenuto_keeps_and_staccato_halves_the_smallest_gap() {
    let notes = [note(0, 77, 60), note(300, 88, 62), note(500, 99, 65)];
    let tenuto = Span {
        articulation: Articulation::Tenuto,
        ..Span::default()
    };
    let out = run(Transform::Span(tenuto), &notes, &cx());
    assert_eq!(
        out.iter().map(|n| n.length).collect::<Vec<_>>(),
        [77, 88, 99]
    );
    let stacc = Span {
        articulation: Articulation::Staccato,
        ..Span::default()
    };
    let out = run(Transform::Span(stacc), &notes, &cx());
    assert!(
        out.iter().all(|n| n.length == 100),
        "half of the 200 between starts"
    );
}

#[test]
fn span_offset_and_variation_change_length_in_the_right_direction() {
    let notes = [note(0, 400, 60), note(500, 400, 62)];
    let t = Span {
        articulation: Articulation::Tenuto,
        offset: 0.5,
        variation: 0.0,
    };
    let out = run(Transform::Span(t), &notes, &cx());
    assert_eq!(out[0].length, 400 + SIXTEENTH / 2);
    let shorter = Span {
        articulation: Articulation::Tenuto,
        offset: 0.0,
        variation: 1.0,
    };
    assert!(run(Transform::Span(shorter), &notes, &cx())
        .iter()
        .all(|n| n.length < 400));
    let longer = Span {
        articulation: Articulation::Tenuto,
        offset: 0.0,
        variation: -1.0,
    };
    assert!(run(Transform::Span(longer), &notes, &cx())
        .iter()
        .all(|n| n.length > 400));
}

// ---- Strum

#[test]
fn strum_low_moves_the_lowest_note_and_high_the_highest() {
    let t = Strum {
        low: 1.0,
        high: 0.0,
        tension: 0.0,
    };
    let out = run(Transform::Strum(t), &chord(), &cx());
    let s = |key| out.iter().find(|n| n.key == key).unwrap().start;
    assert_eq!(s(60), SIXTEENTH);
    assert_eq!(s(67), 0);
    assert_eq!(s(64), SIXTEENTH / 2, "evenly between at no tension");
    let up = Strum {
        low: 0.0,
        high: 1.0,
        tension: 0.0,
    };
    let out = run(Transform::Strum(up), &chord(), &cx());
    assert_eq!(out.iter().find(|n| n.key == 67).unwrap().start, SIXTEENTH);
}

#[test]
fn strum_keeps_the_end_of_a_note_and_stays_at_rest_with_nothing_set() {
    let t = Strum {
        low: 1.0,
        high: 0.0,
        tension: 0.0,
    };
    let out = run(Transform::Strum(t), &chord(), &cx());
    let low = out.iter().find(|n| n.key == 60).unwrap();
    assert_eq!(low.start + low.length, PPQ);
    assert_eq!(run(Transform::Strum(Strum::default()), &chord(), &cx()), {
        let mut c = chord();
        c.sort_by_key(|n| (n.start, n.key));
        c
    });
}

#[test]
fn strum_tension_bunches_the_notes_toward_one_end() {
    let notes: Vec<NoteData> = (0..5).map(|i| note(0, PPQ * 4, 60 + i)).collect();
    let mids = |tension| {
        let t = Strum {
            low: 0.0,
            high: 4.0,
            tension,
        };
        let out = run(Transform::Strum(t), &notes, &cx());
        let s = |key: u8| out.iter().find(|n| n.key == key).unwrap().start as f64;
        (s(61) - s(60), s(64) - s(63))
    };
    let (first, last) = mids(0.8);
    assert!(
        first > last,
        "positive tension: wider apart at the start: {first} {last}"
    );
    let (first, last) = mids(-0.8);
    assert!(
        first < last,
        "negative tension: wider apart at the end: {first} {last}"
    );
}

// ---- Time Warp

fn warp(speed: f64) -> TimeWarp {
    let mut t = TimeWarp::default();
    t.points[0].speed = speed;
    t.preserve_range = false;
    t
}

#[test]
fn time_warp_faster_brings_notes_closer() {
    let ctx = ToolContext {
        range: (0, 1000),
        ..cx()
    };
    let notes = [note(0, 10, 60), note(1000, 10, 62)];
    let out = run(Transform::TimeWarp(warp(2.0)), &notes, &ctx);
    assert_eq!(out[1].start, 500, "twice the speed, half the time");
    let out = run(Transform::TimeWarp(warp(0.5)), &notes, &ctx);
    assert_eq!(out[1].start, 2000);
    assert_eq!(
        out[1].length, 10,
        "note length alone without Include Note End"
    );
}

#[test]
fn time_warp_preserves_the_range_when_asked() {
    let ctx = ToolContext {
        range: (0, 1000),
        ..cx()
    };
    let mut t = warp(2.0);
    t.preserve_range = true;
    let notes = [note(0, 10, 60), note(500, 10, 61), note(1000, 10, 62)];
    let out = run(Transform::TimeWarp(t), &notes, &ctx);
    assert_eq!(out[2].start, 1000);
}

#[test]
fn time_warp_makes_accelerando_with_a_rising_curve() {
    let ctx = ToolContext {
        range: (0, 1000),
        ..cx()
    };
    let t = TimeWarp {
        points: [
            WarpPoint {
                on: true,
                time: 0.0,
                speed: 0.5,
            },
            WarpPoint {
                on: true,
                time: 1.0,
                speed: 3.0,
            },
            WarpPoint {
                on: false,
                time: 0.5,
                speed: 1.0,
            },
        ],
        ..TimeWarp::default()
    };
    let notes: Vec<NoteData> = (0..=4).map(|i| note(i * 250, 10, 60)).collect();
    let out = run(Transform::TimeWarp(t), &notes, &ctx);
    let s = starts(&out);
    assert!(s[1] - s[0] > s[4] - s[3], "the gaps get shorter: {s:?}");
    assert_eq!(s[4], 1000);
}

#[test]
fn time_warp_can_include_note_ends_and_quantize() {
    let ctx = ToolContext {
        range: (0, 1000),
        grid: 100,
        ..cx()
    };
    let mut t = warp(2.0);
    t.include_note_end = true;
    let mut n = note(0, 500, 60);
    n.expr = Some(Box::new(NoteExpression {
        slide: vec![Breakpoint::new(500.0, 1.0)],
        ..Default::default()
    }));
    let out = run(Transform::TimeWarp(t.clone()), &[n], &ctx);
    assert_eq!(out[0].length, 250);
    assert_eq!(
        out[0].expr.as_ref().unwrap().slide[0].time,
        250.0,
        "expression goes too"
    );
    t.include_note_end = false;
    t.quantize = true;
    t.points[0].speed = 3.0;
    let out = run(Transform::TimeWarp(t), &[note(210, 5, 60)], &ctx);
    assert_eq!(out[0].start % 100, 0);
}

// ---- Velocity Shaper

#[test]
fn velocity_shaper_follows_the_envelope_within_min_and_max() {
    let t = VelocityShaper {
        envelope: vec![(0.0, 0.0), (1.0, 1.0)],
        min: 0.2,
        max: 1.0,
        loops: 1,
        rotate: 0,
        division: None,
    };
    let ctx = ToolContext {
        range: (0, 1000),
        ..cx()
    };
    let notes = [note(0, 10, 60), note(500, 10, 61), note(999, 10, 62)];
    let out = run(Transform::VelocityShaper(t), &notes, &ctx);
    assert!((out[0].velocity - 0.2).abs() < 1e-3);
    assert!((out[1].velocity - 0.6).abs() < 1e-3);
    assert!(out[2].velocity > 0.99);
}

#[test]
fn velocity_shaper_loops_and_rotates() {
    let ramp = VelocityShaper {
        envelope: vec![(0.0, 0.0), (1.0, 1.0)],
        min: 0.0,
        max: 1.0,
        loops: 2,
        rotate: 0,
        division: Some(100),
    };
    let ctx = ToolContext {
        range: (0, 1000),
        ..cx()
    };
    let out = run(
        Transform::VelocityShaper(ramp.clone()),
        &[note(250, 10, 60), note(750, 10, 61)],
        &ctx,
    );
    assert!(
        (out[0].velocity - 0.5).abs() < 1e-3,
        "half way along the first pass"
    );
    assert!(
        (out[1].velocity - 0.5).abs() < 1e-3,
        "and half way along the second"
    );
    let moved = VelocityShaper {
        loops: 1,
        rotate: 2,
        ..ramp
    };
    let out = run(Transform::VelocityShaper(moved), &[note(200, 10, 60)], &ctx);
    assert!(
        out[0].velocity.abs() < 1e-3,
        "two steps right: the start of the shape is here"
    );
}

// ---- Rhythm

#[test]
fn rhythm_lays_a_pattern_repeated_to_fill_the_range() {
    let t = Rhythm {
        steps: 8,
        density: 3,
        pattern: 0,
        step_duration: BAR / 8,
        ..Rhythm::default()
    };
    let ctx = ToolContext {
        range: (0, BAR * 2),
        ..cx()
    };
    let out = t.run(&ctx);
    assert_eq!(out.len(), 6, "twice over");
    assert_eq!(
        starts(&out),
        vec![0, BAR / 8, BAR / 4, BAR, BAR + BAR / 8, BAR + BAR / 4]
    );
    assert!(out.iter().all(|n| n.key == 36 && n.length == BAR / 8));
}

#[test]
fn rhythm_step_duration_sets_how_many_times_it_repeats() {
    // A bar: a 1/8 step and 8 steps repeats once; at 1/16 it repeats twice.
    let t = |step_duration| Rhythm {
        steps: 8,
        density: 1,
        step_duration,
        ..Rhythm::default()
    };
    assert_eq!(t(BAR / 8).run(&cx()).len(), 1);
    assert_eq!(t(BAR / 16).run(&cx()).len(), 2);
    assert_eq!(
        t(BAR / 2).most_steps(BAR),
        2,
        "a long step leaves room for fewer"
    );
}

#[test]
fn rhythm_pattern_picks_among_every_arrangement() {
    assert_eq!(pattern_count(4, 2), 6);
    assert_eq!(pattern_count(16, 8), 12_870);
    let t = |pattern| Rhythm {
        steps: 4,
        density: 2,
        pattern,
        step_duration: BAR / 4,
        ..Rhythm::default()
    };
    let cells: Vec<Vec<bool>> = (0..6).map(|p| t(p).steps_on(BAR)).collect();
    for (i, a) in cells.iter().enumerate() {
        assert_eq!(a.iter().filter(|x| **x).count(), 2);
        for b in &cells[i + 1..] {
            assert_ne!(a, b);
        }
    }
}

#[test]
fn rhythm_shift_moves_the_pattern_either_way() {
    let t = Rhythm {
        steps: 4,
        density: 1,
        step_duration: BAR / 4,
        shift: 1,
        ..Rhythm::default()
    };
    assert_eq!(t.steps_on(BAR), vec![false, true, false, false]);
    let t = Rhythm { shift: -1, ..t };
    assert_eq!(t.steps_on(BAR), vec![false, false, false, true]);
}

#[test]
fn rhythm_accents_count_notes_and_can_be_offset() {
    let t = Rhythm {
        steps: 4,
        density: 4,
        step_duration: BAR / 4,
        velocity: 0.5,
        accent: 1.0,
        accent_frequency: 1,
        ..Rhythm::default()
    };
    assert!(
        t.run(&cx()).iter().all(|n| n.velocity == 1.0),
        "a frequency of 1 accents every note"
    );
    let t = Rhythm {
        accent_frequency: 2,
        ..t
    };
    let v: Vec<f32> = t.run(&cx()).iter().map(|n| n.velocity).collect();
    assert_eq!(v, [1.0, 0.5, 1.0, 0.5]);
    let t = Rhythm {
        accent_offset: 1,
        ..t
    };
    let v: Vec<f32> = t.run(&cx()).iter().map(|n| n.velocity).collect();
    assert_eq!(v, [0.5, 1.0, 0.5, 1.0]);
    let t = Rhythm {
        accent_frequency: 0,
        ..t
    };
    assert!(t.run(&cx()).iter().all(|n| n.velocity == 0.5));
}

#[test]
fn rhythm_split_halves_a_step() {
    let t = Rhythm {
        steps: 4,
        density: 4,
        step_duration: BAR / 4,
        split: 1.0,
        ..Rhythm::default()
    };
    let out = t.run(&cx());
    assert_eq!(out.len(), 8);
    assert_eq!(out[1].start, PPQ / 2);
    assert_eq!(out[0].length, PPQ / 2);
}

#[test]
fn rhythm_pitch_fits_the_scale() {
    let t = Rhythm {
        pitch: 61,
        ..Rhythm::default()
    };
    let scaled = ToolContext {
        scale: Some(MAJOR),
        ..cx()
    };
    assert!(t.run(&scaled).iter().all(|n| n.key == 60));
}

// ---- Seed

#[test]
fn seed_stays_in_its_ranges_and_voices() {
    let t = Seed {
        pitch: (60, 72),
        duration: (PPQ / 2, PPQ),
        velocity: (0.3, 0.6),
        voices: 2,
        density: 1.0,
    };
    let out = t.run(&cx());
    assert!(!out.is_empty());
    for n in &out {
        assert!((60..=72).contains(&n.key));
        assert!(n.length >= 1 && n.length <= PPQ);
        assert!((0.3..=0.6).contains(&n.velocity));
        assert!(n.start >= cx().range.0 && n.start + n.length <= cx().range.1);
        // The Voices limit: at the moment each note starts, no more than two are sounding.
        let sounding = out
            .iter()
            .filter(|o| o.start <= n.start && n.start < o.start + o.length)
            .count();
        assert!(sounding <= 2);
    }
}

#[test]
fn seed_density_is_a_share_of_the_pitch_range() {
    let t = |density| Seed {
        pitch: (60, 71),
        voices: 12,
        density,
        ..Seed::default()
    };
    assert_eq!(t(1.0).run(&cx()).len(), 12);
    assert_eq!(t(0.5).run(&cx()).len(), 6);
    assert!(t(0.0).run(&cx()).is_empty());
}

#[test]
fn seed_with_a_scale_only_uses_the_scale_and_with_merged_handles_one_key() {
    let t = Seed {
        pitch: (60, 72),
        voices: 12,
        ..Seed::default()
    };
    let scaled = ToolContext {
        scale: Some(MAJOR),
        ..cx()
    };
    for n in t.run(&scaled) {
        assert!(clip_edit::in_scale(i32::from(n.key), MAJOR));
    }
    let one = Seed {
        pitch: (65, 65),
        voices: 3,
        ..Seed::default()
    };
    assert!(one.run(&cx()).iter().all(|n| n.key == 65));
}

// ---- Shape

#[test]
fn shape_follows_its_levels_across_the_range() {
    let t = Shape {
        levels: ShapePreset::Rise.levels(),
        pitch: (60, 72),
        rate: BAR / 12,
        ..Shape::default()
    };
    let out = t.run(&cx());
    assert_eq!(out.len(), 12);
    assert_eq!(out[0].key, 60);
    assert_eq!(out[11].key, 71);
    assert!(out.windows(2).all(|w| w[1].key >= w[0].key));
    let fall = Shape {
        levels: ShapePreset::Fall.levels(),
        ..t
    };
    let out = fall.run(&cx());
    assert!(out.windows(2).all(|w| w[1].key <= w[0].key));
}

#[test]
fn shape_density_tie_and_jitter() {
    let base = Shape {
        rate: BAR / 8,
        ..Shape::default()
    };
    let half = Shape {
        density: 0.5,
        ..base.clone()
    };
    assert_eq!(half.run(&cx()).len(), 4);
    let tied = Shape {
        tie: 1.0,
        density: 0.5,
        ..base.clone()
    };
    let out = tied.run(&cx());
    for pair in out.windows(2) {
        assert_eq!(
            pair[0].start + pair[0].length,
            pair[1].start,
            "held to the next note"
        );
    }
    let smooth = base.run(&cx());
    let jittery = Shape {
        jitter: 1.0,
        ..base.clone()
    }
    .run(&cx());
    assert_ne!(keys(&smooth), keys(&jittery));
    assert!(
        jittery.iter().all(|n| (60..=72).contains(&n.key)),
        "never out of range"
    );
}

#[test]
fn shape_fits_the_scale() {
    let t = Shape::default();
    let scaled = ToolContext {
        scale: Some(MAJOR),
        ..cx()
    };
    assert!(t
        .run(&scaled)
        .iter()
        .all(|n| clip_edit::in_scale(i32::from(n.key), MAJOR)));
}

// ---- Stacks

#[test]
fn stacks_makes_a_chord_across_the_range() {
    let t = Stacks::default();
    let out = t.run(&cx());
    assert_eq!(keys(&out), vec![60, 64, 67]);
    assert!(out.iter().all(|n| n.start == 0 && n.length == BAR));
}

#[test]
fn stacks_progressions_share_the_range_and_can_change() {
    let mut t = Stacks::default();
    t.chords[0].chord = 1; // Minor
    t.add_chord();
    assert_eq!(t.chords.len(), 2);
    assert_eq!(t.current, 1);
    t.chords[1].chord = 0;
    t.chords[1].root = 65;
    let out = t.run(&cx());
    assert_eq!(starts(&out), vec![0, 0, 0, BAR / 2, BAR / 2, BAR / 2]);
    assert_eq!(keys(&out), vec![60, 63, 67, 65, 69, 72]);
    t.remove_chord();
    assert_eq!(t.chords.len(), 1);
    t.remove_chord();
    assert_eq!(t.chords.len(), 1, "one is always left");
}

#[test]
fn stacks_inverts_up_and_an_octave_down() {
    let mut t = Stacks::default();
    t.chords[0].inversion = 1;
    assert_eq!(keys(&t.run(&cx())), vec![64, 67, 72]);
    t.chords[0].inversion = 2;
    assert_eq!(keys(&t.run(&cx())), vec![67, 72, 76]);
    t.chords[0].inversion = -1;
    assert_eq!(keys(&t.run(&cx())), vec![52, 55, 60]);
    assert_eq!(t.most_inversions(0), 2);
}

#[test]
fn stacks_duration_and_offset_are_eighths() {
    let mut t = Stacks::default();
    t.chords[0].duration = 4;
    t.chords[0].offset = 2;
    let out = t.run(&cx());
    assert_eq!(out[0].length, BAR / 2);
    assert_eq!(out[0].start, BAR / 4);
}

#[test]
fn stacks_roots_follow_the_scale() {
    let roots = Stacks::roots(Some(MAJOR));
    assert!(roots.contains(&60) && !roots.contains(&61));
    assert_eq!(Stacks::roots(None).len(), 128);
    let mut t = Stacks::default();
    t.chords[0].root = 63;
    t.follow_scale(Some((2, MAJOR.1)));
    assert_eq!(
        t.chords[0].root % 12,
        2,
        "out of the scale: it goes to the scale's root"
    );
    t.chords[0].root = 66;
    t.follow_scale(Some((2, MAJOR.1)));
    assert_eq!(t.chords[0].root, 66, "already in it: it stays");
    t.chords[0].chord = 1; // Minor triad on a scale's root is outside C Major's third
    let scaled = ToolContext {
        scale: Some(MAJOR),
        ..cx()
    };
    t.chords[0].root = 60;
    assert!(t
        .run(&scaled)
        .iter()
        .all(|n| clip_edit::in_scale(i32::from(n.key), MAJOR)));
}

#[test]
fn stacks_loads_a_custom_bank() {
    let bank = parse_bank(
        r#"{"name":"Mine","chords":[{"name":"Fifth","intervals":[0,7]},
            {"name":"Octave","intervals":[0,12]}]}"#,
    )
    .unwrap();
    assert_eq!(bank.chords.len(), 2);
    let t = Stacks::with_bank(bank.clone());
    assert_eq!(keys(&t.run(&cx())), vec![60, 67]);
    let tool = Generator::Stacks(t);
    match tool.defaults() {
        Generator::Stacks(s) => assert_eq!(s.bank, bank, "Reset keeps the loaded bank"),
        other => panic!("{other:?}"),
    }
    assert!(parse_bank("not json").is_err());
    assert!(parse_bank(r#"{"chords":[]}"#).is_err());
    assert!(parse_bank(r#"{"chords":[{"name":"x","intervals":[]}]}"#).is_err());
}

#[test]
fn tonnetz_places_intervals_near_the_root() {
    assert_eq!(tonnetz(7), (1, 0), "a fifth is one step across");
    assert_eq!(tonnetz(4), (0, 1), "a major third one step up");
    assert_eq!(tonnetz(0), (0, 0));
    // A minor third is reached a fifth up and a major third down.
    let (a, b) = tonnetz(3);
    assert_eq!((7 * a + 4 * b).rem_euclid(12), 3);
}

// ---- Euclidean

#[test]
fn euclidean_spreads_pulses_evenly() {
    assert_eq!(
        euclidean::euclid(8, 3),
        vec![true, false, false, true, false, false, true, false]
    );
    assert_eq!(euclidean::euclid(4, 4), vec![true; 4]);
}

fn one_voice() -> Euclidean {
    let mut t = Euclidean {
        steps: 8,
        division: BAR / 8,
        ..Euclidean::default()
    };
    t.voices[0].pulses = 2;
    t.voices[1].on = false;
    t
}

#[test]
fn euclidean_voices_rotate_and_have_their_own_pitch_and_velocity() {
    let mut t = one_voice();
    t.voices[0].pitch = 48;
    t.voices[0].velocity = 0.5;
    let out = t.run(&cx());
    assert_eq!(starts(&out), vec![0, BAR / 2]);
    assert!(out.iter().all(|n| n.key == 48 && n.velocity == 0.5));
    t.voices[0].rotation = 1;
    let out = t.run(&cx());
    let mut s = starts(&out);
    s.sort_unstable();
    assert_eq!(s, vec![BAR / 8, BAR * 5 / 8]);
    t.voices[0].on = false;
    assert!(t.run(&cx()).is_empty());
}

#[test]
fn euclidean_repeats_and_wraps_around_the_span() {
    let mut t = one_voice();
    t.steps = 4;
    t.voices[0].pulses = 1;
    t.density = 0;
    let out = t.run(&cx());
    assert_eq!(
        starts(&out),
        vec![0, BAR / 2],
        "a short pattern repeats to fill the span"
    );
    t.density = 1;
    assert_eq!(
        t.run(&cx()).len(),
        1,
        "Density sets how many times it repeats"
    );
    t.steps = 16;
    t.division = BAR / 8;
    t.voices[0].rotation = 15;
    let out = t.run(&cx());
    assert!(out[0].start < BAR, "what doesn't fit wraps around");
}

#[test]
fn euclidean_randomize_is_seeded() {
    let mut a = Euclidean::default();
    let mut b = Euclidean::default();
    a.randomize_rotations(5);
    b.randomize_rotations(5);
    assert_eq!(a, b);
    let mut c = Euclidean::default();
    c.randomize_rotations(6);
    assert_ne!(a, c);
    assert!(a.voices.iter().all(|v| (0..16).contains(&v.rotation)));
}
