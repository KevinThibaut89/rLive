use crate::dsp::voice::*;
use rlive_core::{ExprKind, MidiMessage};

const fn on(key: u8) -> MidiMessage {
    MidiMessage::NoteOn {
        channel: 0,
        key,
        velocity: 0.8,
        note_id: 0,
        tuning: 0.0,
    }
}

const fn off(key: u8) -> MidiMessage {
    MidiMessage::NoteOff {
        channel: 0,
        key,
        velocity: 0.0,
        note_id: 0,
    }
}

fn run(a: &mut VoiceAllocator, m: MidiMessage) -> Vec<VoiceEvent> {
    let mut events = Vec::new();
    a.handle(m, |e| events.push(e));
    events
}

fn allocator(voices: usize, f: impl FnOnce(&mut VoiceSettings)) -> VoiceAllocator {
    let mut a = VoiceAllocator::new(voices);
    a.prepare(1_000.0);
    let mut s = VoiceSettings::default();
    f(&mut s);
    a.set(&s);
    a
}

fn started(events: &[VoiceEvent]) -> Vec<(usize, u8, bool)> {
    events
        .iter()
        .filter_map(|e| match e {
            VoiceEvent::Start {
                voice,
                note,
                stolen,
            } => Some((*voice, note.key, *stolen)),
            _ => None,
        })
        .collect()
}

#[test]
fn notes_take_free_voices_and_release_on_note_off() {
    let mut a = allocator(4, |_| {});
    assert_eq!(started(&run(&mut a, on(60))), [(0, 60, false)]);
    assert_eq!(started(&run(&mut a, on(64))), [(1, 64, false)]);
    assert_eq!(
        run(&mut a, off(60)),
        [VoiceEvent::Release { voice: 0 }],
        "only the matching note is released"
    );
    assert_eq!(a.voice(0).state, VoiceState::Released);
    a.finished(0);
    assert_eq!(a.voice(0).state, VoiceState::Free);
    assert_eq!(started(&run(&mut a, on(67))), [(0, 67, false)]);
}

#[test]
fn stealing_prefers_a_released_note() {
    let mut a = allocator(2, |_| {});
    run(&mut a, on(60));
    run(&mut a, on(62));
    run(&mut a, off(62));
    assert_eq!(started(&run(&mut a, on(64))), [(1, 64, true)]);
}

#[test]
fn stealing_takes_the_oldest_by_default() {
    let mut a = allocator(2, |_| {});
    run(&mut a, on(60));
    run(&mut a, on(62));
    assert_eq!(started(&run(&mut a, on(64))), [(0, 64, true)]);
    assert_eq!(started(&run(&mut a, on(65))), [(1, 65, true)]);
}

#[test]
fn stealing_can_go_by_pitch_or_level() {
    let mut low = allocator(3, |s| s.steal = StealMode::Lowest);
    let mut high = allocator(3, |s| s.steal = StealMode::Highest);
    let mut quiet = allocator(3, |s| s.steal = StealMode::Quietest);
    for a in [&mut low, &mut high, &mut quiet] {
        run(a, on(64));
        run(a, on(60));
        run(a, on(67));
    }
    assert_eq!(started(&run(&mut low, on(72))), [(1, 72, true)]);
    assert_eq!(started(&run(&mut high, on(72))), [(2, 72, true)]);
    quiet.set_level(0, 0.5);
    quiet.set_level(1, 0.9);
    quiet.set_level(2, 0.1);
    assert_eq!(started(&run(&mut quiet, on(72))), [(2, 72, true)]);
}

#[test]
fn steal_mode_none_drops_the_new_note() {
    let mut a = allocator(1, |s| s.steal = StealMode::None);
    run(&mut a, on(60));
    assert!(run(&mut a, on(62)).is_empty());
    assert_eq!(a.voice(0).note.key, 60);
}

#[test]
fn polyphony_limits_notes_below_the_voice_count() {
    let mut a = allocator(8, |s| s.polyphony = 2);
    run(&mut a, on(60));
    run(&mut a, on(62));
    assert_eq!(started(&run(&mut a, on(64))), [(0, 64, true)]);
}

#[test]
fn unison_takes_voices_per_note_and_spreads_them() {
    let mut a = allocator(6, |s| s.unison = 3);
    let events = run(&mut a, on(60));
    assert_eq!(events.len(), 3);
    let spreads: Vec<f32> = events
        .iter()
        .map(|e| match e {
            VoiceEvent::Start { note, .. } => note.spread,
            _ => panic!(),
        })
        .collect();
    assert_eq!(spreads, [-1.0, 0.0, 1.0]);

    // Two notes of three voices fill six, so a third note steals all of the oldest.
    run(&mut a, on(62));
    let stolen = started(&run(&mut a, on(64)));
    assert_eq!(stolen, [(0, 64, true), (1, 64, true), (2, 64, true)]);
    assert_eq!(
        run(&mut a, off(62)).len(),
        3,
        "a note off releases all three"
    );
}

#[test]
fn retrigger_restarts_the_voice_already_playing_the_key() {
    let mut plain = allocator(4, |_| {});
    run(&mut plain, on(60));
    assert_eq!(started(&run(&mut plain, on(60))), [(1, 60, false)]);

    let mut retrig = allocator(4, |s| s.retrigger = true);
    run(&mut retrig, on(60));
    assert_eq!(started(&run(&mut retrig, on(60))), [(0, 60, true)]);
}

#[test]
fn stacked_notes_of_one_key_are_released_oldest_first() {
    let mut a = allocator(4, |_| {});
    run(&mut a, on(60));
    run(&mut a, on(60));
    assert_eq!(run(&mut a, off(60)), [VoiceEvent::Release { voice: 0 }]);
    assert_eq!(run(&mut a, off(60)), [VoiceEvent::Release { voice: 1 }]);
}

#[test]
fn note_ids_target_one_note_among_equal_keys() {
    let mut a = allocator(4, |_| {});
    let note = |id, velocity| MidiMessage::NoteOn {
        channel: 0,
        key: 60,
        velocity,
        note_id: id,
        tuning: 0.0,
    };
    run(&mut a, note(7, 0.5));
    run(&mut a, note(8, 0.5));
    run(
        &mut a,
        MidiMessage::NoteExpr {
            channel: 0,
            key: 60,
            note_id: 8,
            kind: ExprKind::Pressure,
            value: 0.9,
        },
    );
    assert_eq!(a.voice(0).pressure, 0.0);
    assert_eq!(a.voice(1).pressure, 0.9);
    let events = run(
        &mut a,
        MidiMessage::NoteOff {
            channel: 0,
            key: 60,
            velocity: 0.0,
            note_id: 8,
        },
    );
    assert_eq!(events, [VoiceEvent::Release { voice: 1 }]);
}

#[test]
fn per_note_pitch_and_channel_bend_move_the_pitch() {
    let mut a = allocator(2, |s| s.bend_range = 12.0);
    run(&mut a, on(60));
    run(
        &mut a,
        MidiMessage::NoteExpr {
            channel: 0,
            key: 60,
            note_id: 0,
            kind: ExprKind::Pitch,
            value: 3.0,
        },
    );
    run(
        &mut a,
        MidiMessage::PitchBend {
            channel: 0,
            value: 0.5,
        },
    );
    assert_eq!(a.pitch(0), 60.0 + 3.0 + 6.0);
}

#[test]
fn note_tuning_is_part_of_the_pitch() {
    let mut a = allocator(2, |_| {});
    run(
        &mut a,
        MidiMessage::NoteOn {
            channel: 0,
            key: 60,
            velocity: 1.0,
            note_id: 0,
            tuning: -0.5,
        },
    );
    assert_eq!(a.pitch(0), 59.5);
    assert_eq!(a.note_bend(0), 0.0, "tuning isn't the player's bend");
    run(
        &mut a,
        MidiMessage::NoteExpr {
            channel: 0,
            key: 60,
            note_id: 0,
            kind: rlive_core::ExprKind::Pitch,
            value: 1.5,
        },
    );
    assert_eq!(a.pitch(0), 61.5);
    assert_eq!(a.note_bend(0), 2.0);
}

#[test]
fn legato_only_changes_pitch_while_a_note_is_held() {
    let mut a = allocator(4, |s| s.mode = VoiceMode::Legato);
    assert_eq!(started(&run(&mut a, on(60))), [(0, 60, false)]);
    let events = run(&mut a, on(64));
    assert!(matches!(events[..], [VoiceEvent::Legato { voice: 0, note }] if note.key == 64));
    assert_eq!(a.pitch(0), 64.0);

    // Back to the note still held, without a restart.
    let events = run(&mut a, off(64));
    assert!(matches!(events[..], [VoiceEvent::Legato { voice: 0, note }] if note.key == 60));
    assert_eq!(run(&mut a, off(60)), [VoiceEvent::Release { voice: 0 }]);

    // Detached notes start over; the voice is still releasing, so that's a steal.
    assert_eq!(started(&run(&mut a, on(62))), [(0, 62, true)]);
}

#[test]
fn mono_restarts_on_every_note() {
    let mut a = allocator(4, |s| s.mode = VoiceMode::Mono);
    run(&mut a, on(60));
    assert_eq!(started(&run(&mut a, on(64))), [(0, 64, true)]);
    assert_eq!(a.voice(0).note.key, 64);
    assert_eq!(a.voice(0).state, VoiceState::Held);
}

#[test]
fn mono_priority_decides_which_key_sounds() {
    let mut a = allocator(4, |s| {
        s.mode = VoiceMode::Legato;
        s.priority = NotePriority::Low;
    });
    run(&mut a, on(60));
    assert!(run(&mut a, on(64)).is_empty(), "a higher note is ignored");
    assert_eq!(a.voice(0).note.key, 60);
    assert!(run(&mut a, off(64)).is_empty());
    let events = run(&mut a, on(55));
    assert!(matches!(events[..], [VoiceEvent::Legato { voice: 0, note }] if note.key == 55));
}

#[test]
fn glide_moves_at_a_constant_time_or_rate() {
    let mut a = allocator(2, |s| {
        s.mode = VoiceMode::Legato;
        s.glide = GlideMode::Always;
        s.glide_time = 0.1;
    });
    run(&mut a, on(60));
    run(&mut a, on(72));
    assert_eq!(a.pitch(0), 60.0, "the glide starts at the old pitch");
    a.advance(50);
    assert!((a.pitch(0) - 66.0).abs() < 1e-3, "halfway through");
    a.advance(50);
    assert_eq!(a.pitch(0), 72.0);

    let mut a = allocator(2, |s| {
        s.mode = VoiceMode::Legato;
        s.glide = GlideMode::Always;
        s.glide_time = 0.1;
        s.glide_curve = GlideCurve::ConstantRate;
    });
    run(&mut a, on(60));
    run(&mut a, on(84));
    a.advance(100);
    assert!(
        (a.pitch(0) - 72.0).abs() < 1e-3,
        "one octave per glide time"
    );
}

#[test]
fn legato_only_glide_snaps_detached_notes() {
    let mut a = allocator(4, |s| {
        s.glide = GlideMode::LegatoOnly;
        s.glide_time = 0.1;
    });
    run(&mut a, on(60));
    run(&mut a, off(60));
    a.finished(0);
    run(&mut a, on(72));
    assert_eq!(a.pitch(0), 72.0);
    run(&mut a, on(60));
    assert_eq!(
        a.pitch(1),
        72.0,
        "overlapped notes glide from the last note"
    );
}

#[test]
fn the_sustain_pedal_holds_notes_until_released() {
    let pedal = |value| MidiMessage::ControlChange {
        channel: 0,
        cc: 64,
        value,
    };
    let mut a = allocator(4, |_| {});
    run(&mut a, pedal(127));
    run(&mut a, on(60));
    assert!(run(&mut a, off(60)).is_empty());
    assert_eq!(a.voice(0).state, VoiceState::Held);
    assert_eq!(run(&mut a, pedal(0)), [VoiceEvent::Release { voice: 0 }]);
}

#[test]
fn all_notes_off_releases_everything() {
    let mut a = allocator(4, |_| {});
    run(&mut a, on(60));
    run(&mut a, on(62));
    assert_eq!(run(&mut a, MidiMessage::AllNotesOff).len(), 2);
}

#[test]
fn velocity_zero_is_a_note_off() {
    let mut a = allocator(4, |_| {});
    run(&mut a, on(60));
    let events = run(
        &mut a,
        MidiMessage::NoteOn {
            channel: 0,
            key: 60,
            velocity: 0.0,
            note_id: 0,
            tuning: 0.0,
        },
    );
    assert_eq!(events, [VoiceEvent::Release { voice: 0 }]);
}
