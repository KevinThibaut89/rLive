//! The MIDI effects of batch B1, checked against the behaviour the manual describes.

use rlive_core::{
    AudioBuffer, EventQueue, ExprKind, MidiEvent, MidiMessage, ParamEvent, ParamId, ParamOutQueue,
    ProcessContext, Processor, ScaleInfo, TransportInfo, ViewChannel, ViewWriter,
};
use rlive_devices::builtins;
use rlive_devices::devices::b1::arpeggiator::{self as arp, Arpeggiator};
use rlive_devices::devices::b1::cc_control::{self as cc, CcControl};
use rlive_devices::devices::b1::chord::{self as chord, Chord};
use rlive_devices::devices::b1::expression_control::{self as expr, ExpressionControl};
use rlive_devices::devices::b1::midi_monitor::{self as monitor, MidiMonitor};
use rlive_devices::devices::b1::mpe_control::{self as mpe, MpeControl};
use rlive_devices::devices::b1::note_echo::{self as echo, NoteEcho};
use rlive_devices::devices::b1::note_length::{self as length, NoteLength};
use rlive_devices::devices::b1::pitch::{self as pitch, Pitch};
use rlive_devices::devices::b1::random::{self as random, Random};
use rlive_devices::devices::b1::scale::{self as scale, Scale};
use rlive_devices::devices::b1::velocity::{self as velocity, Velocity};
use rlive_testkit::{assert_no_alloc, Harness};
use std::sync::Arc;

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;
/// A 16th note at 120 BPM and 48 kHz.
const SIXTEENTH: u32 = 6_000;

fn note_on(offset: u32, key: u8, velocity: f32) -> MidiEvent {
    note_on_id(offset, key, velocity, 0)
}

fn note_on_id(offset: u32, key: u8, velocity: f32, note_id: u32) -> MidiEvent {
    MidiEvent::new(
        offset,
        MidiMessage::NoteOn {
            channel: 0,
            key,
            velocity,
            note_id,
            tuning: 0.0,
        },
    )
}

fn note_off(offset: u32, key: u8) -> MidiEvent {
    note_off_vel(offset, key, 0.5)
}

fn note_off_vel(offset: u32, key: u8, velocity: f32) -> MidiEvent {
    MidiEvent::new(
        offset,
        MidiMessage::NoteOff {
            channel: 0,
            key,
            velocity,
            note_id: 0,
        },
    )
}

fn set(device: &mut dyn Processor, id: u32, value: f64) {
    device.set_param(id, value);
}

/// Everything that came out, with offsets counted from the start of the run.
#[derive(Default)]
struct Log(Vec<MidiEvent>);

impl Log {
    fn ons(&self) -> Vec<(u32, u8, f32)> {
        self.0
            .iter()
            .filter_map(|e| match e.message {
                MidiMessage::NoteOn { key, velocity, .. } => Some((e.offset, key, velocity)),
                _ => None,
            })
            .collect()
    }

    fn keys(&self) -> Vec<u8> {
        self.ons().iter().map(|(_, k, _)| *k).collect()
    }

    fn offs(&self) -> Vec<(u32, u8)> {
        self.0
            .iter()
            .filter_map(|e| match e.message {
                MidiMessage::NoteOff { key, .. } => Some((e.offset, key)),
                _ => None,
            })
            .collect()
    }

    /// Fails unless every note-on got exactly one matching note-off.
    fn assert_balanced(&self, context: &str) {
        let mut open = Vec::new();
        for e in &self.0 {
            match e.message {
                MidiMessage::NoteOn { key, note_id, .. } => open.push((key, note_id)),
                MidiMessage::NoteOff { key, note_id, .. } => {
                    let found = open
                        .iter()
                        .position(|(k, id)| *k == key && (*id == note_id || note_id == 0));
                    if let Some(found) = found {
                        open.swap_remove(found);
                    }
                }
                _ => {}
            }
        }
        assert!(open.is_empty(), "{context}: hung notes {open:?}");
    }
}

/// A device and its harness, moving along like the engine does: the transport advances, and the
/// allocation guard watches every block.
struct Rig<D: Processor> {
    device: D,
    harness: Harness,
    log: Log,
    frame: u32,
}

impl<D: Processor> Rig<D> {
    fn new(device: D) -> Self {
        Self {
            device,
            harness: Harness::new(SR),
            log: Log::default(),
            frame: 0,
        }
    }

    fn run_frames(&mut self, events: &[MidiEvent], params: &[ParamEvent], frames: u32) -> &Log {
        let start = self.frame;
        let end = start + frames;
        let mut scratch = vec![0.0f32; BLOCK];
        while self.frame < end {
            let block = (end - self.frame).min(BLOCK as u32);
            let at = self.frame;
            let events: Vec<MidiEvent> = events
                .iter()
                .filter(|e| e.offset >= at && e.offset < at + block)
                .map(|e| MidiEvent::new(e.offset - at, e.message))
                .collect();
            let params: Vec<ParamEvent> = params
                .iter()
                .filter(|e| e.offset >= at && e.offset < at + block)
                .map(|e| ParamEvent {
                    offset: e.offset - at,
                    id: e.id,
                    value: e.value,
                })
                .collect();
            let (left, right) = scratch.split_at_mut(block as usize / 2);
            let right = &mut right[..left.len()];
            let (mut l, mut r) = (left.to_vec(), right.to_vec());
            l.resize(block as usize, 0.0);
            r.resize(block as usize, 0.0);
            assert_no_alloc(|| {
                self.harness
                    .process(&mut self.device, &mut l, &mut r, &events, &params)
            });
            self.harness.events_out.sort();
            for e in self.harness.events_out.as_slice() {
                self.log.0.push(MidiEvent::new(e.offset + at, e.message));
            }
            self.harness.transport.tick += block as f64 * self.harness.transport.ticks_per_sample();
            self.harness.transport.sample_pos += block as u64;
            self.frame += block;
        }
        &self.log
    }

    fn run(&mut self, events: &[MidiEvent], params: &[ParamEvent], blocks: usize) -> &Log {
        self.run_frames(events, params, (blocks * BLOCK) as u32)
    }
}

fn param(offset: u32, id: u32, value: f64) -> ParamEvent {
    ParamEvent {
        offset,
        id: ParamId(id),
        value,
    }
}

fn major_c() -> ScaleInfo {
    ScaleInfo {
        root: 0,
        mask: 0b1010_1011_0101,
    }
}

// ---- Pitch ----

#[test]
fn pitch_transposes_and_its_note_off_follows_even_after_the_control_changes() {
    let mut rig = Rig::new(Pitch::default());
    set(&mut rig.device, pitch::PITCH, 7.0);
    let log = rig.run(
        &[note_on(0, 60, 0.8), note_off(600, 60)],
        &[param(300, pitch::PITCH, -3.0)],
        4,
    );
    assert_eq!(log.keys(), [67]);
    assert_eq!(log.offs(), [(600, 67)], "released where it sounded");
}

#[test]
fn pitch_block_fold_and_limit_handle_notes_outside_the_range() {
    let mut rig = Rig::new(Pitch::default());
    set(&mut rig.device, pitch::LOWEST, 60.0);
    set(&mut rig.device, pitch::RANGE, 12.0);
    let log = rig.run(
        &[note_on(0, 50, 0.5), note_on(0, 62, 0.5), note_off(10, 50)],
        &[],
        1,
    );
    assert_eq!(log.keys(), [62], "block drops the out-of-range note");
    assert!(log.offs().is_empty(), "and its note-off");

    let mut rig = Rig::new(Pitch::default());
    set(&mut rig.device, pitch::MODE, 1.0);
    set(&mut rig.device, pitch::LOWEST, 60.0);
    set(&mut rig.device, pitch::RANGE, 12.0);
    assert_eq!(
        rig.run(&[note_on(0, 50, 0.5), note_on(5, 80, 0.5)], &[], 1)
            .keys(),
        [62, 68],
        "fold moves by octaves into the range"
    );

    let mut rig = Rig::new(Pitch::default());
    set(&mut rig.device, pitch::MODE, 2.0);
    set(&mut rig.device, pitch::LOWEST, 60.0);
    set(&mut rig.device, pitch::RANGE, 12.0);
    assert_eq!(
        rig.run(&[note_on(0, 50, 0.5), note_on(5, 80, 0.5)], &[], 1)
            .keys(),
        [60, 72],
        "limit pins to the range"
    );
}

#[test]
fn pitch_step_buttons_move_the_pitch_by_the_step_width() {
    let mut rig = Rig::new(Pitch::default());
    set(&mut rig.device, pitch::STEP_WIDTH, 5.0);
    let log = rig.run(
        &[note_on(100, 60, 0.5)],
        &[param(0, pitch::STEP_UP, 1.0)],
        1,
    );
    assert_eq!(log.keys(), [65]);

    // The button springs back by itself, so the next press steps again.
    let at = rig.frame;
    let log = rig.run(
        &[note_on(at, 62, 0.5)],
        &[param(at, pitch::STEP_UP, 1.0)],
        1,
    );
    assert_eq!(log.keys().last(), Some(&72), "62 up two steps of 5 is 72");
    let at = rig.frame;
    let log = rig.run(
        &[note_on(at, 62, 0.5)],
        &[param(at, pitch::STEP_DOWN, 1.0)],
        1,
    );
    assert_eq!(log.keys().last(), Some(&67));
}

#[test]
fn pitch_works_in_scale_degrees_only_with_a_scale_present() {
    let mut rig = Rig::new(Pitch::default());
    set(&mut rig.device, pitch::PITCH, 2.0);
    set(&mut rig.device, pitch::USE_SCALE, 1.0);
    assert_eq!(
        rig.run(&[note_on(0, 60, 0.5)], &[], 1).keys(),
        [62],
        "no scale delivered: chromatic"
    );

    let mut rig = Rig::new(Pitch::default());
    set(&mut rig.device, pitch::PITCH, 2.0);
    set(&mut rig.device, pitch::USE_SCALE, 1.0);
    rig.harness.transport.scale = Some(major_c());
    assert_eq!(
        rig.run(&[note_on(0, 60, 0.5)], &[], 1).keys(),
        [64],
        "two scale degrees up from C is E"
    );
}

#[test]
fn pitch_expression_follows_the_transposed_note() {
    let mut rig = Rig::new(Pitch::default());
    set(&mut rig.device, pitch::PITCH, 12.0);
    let expr = MidiEvent::new(
        10,
        MidiMessage::NoteExpr {
            channel: 0,
            key: 60,
            note_id: 77,
            kind: ExprKind::Slide,
            value: 0.25,
        },
    );
    let log = rig.run(&[note_on_id(0, 60, 0.5, 77), expr], &[], 1);
    let seen = log.0.iter().find_map(|e| match e.message {
        MidiMessage::NoteExpr { key, note_id, .. } => Some((key, note_id)),
        _ => None,
    });
    assert_eq!(seen, Some((72, 77)));
}

// ---- Velocity ----

fn velocity_of(log: &Log) -> f32 {
    log.ons()[0].2 * 127.0
}

#[test]
fn velocity_defaults_leave_notes_alone() {
    let mut rig = Rig::new(Velocity::with_seed(1));
    assert!((velocity_of(rig.run(&[note_on(0, 60, 0.5)], &[], 1)) - 63.5).abs() < 0.01);
}

#[test]
fn velocity_out_low_and_high_rescale_the_output() {
    let mut rig = Rig::new(Velocity::with_seed(1));
    set(&mut rig.device, velocity::OUT_LOW, 80.0);
    set(&mut rig.device, velocity::OUT_HI, 127.0);
    let v = velocity_of(rig.run(&[note_on(0, 60, 0.0)], &[], 1));
    assert!((v - 80.0).abs() < 0.5, "soft notes hit the bottom: {v}");
}

#[test]
fn velocity_clip_mode_holds_values_inside_the_range() {
    let mut rig = Rig::new(Velocity::with_seed(1));
    set(&mut rig.device, velocity::LOWEST, 40.0);
    set(&mut rig.device, velocity::RANGE, 40.0);
    let v = velocity_of(rig.run(&[note_on(0, 60, 100.0 / 127.0)], &[], 1));
    assert!((v - 127.0).abs() < 0.5, "over the top maps to the top: {v}");
}

#[test]
fn velocity_gate_removes_notes_outside_the_range() {
    let mut rig = Rig::new(Velocity::with_seed(1));
    set(&mut rig.device, velocity::MODE, 1.0);
    set(&mut rig.device, velocity::LOWEST, 40.0);
    set(&mut rig.device, velocity::RANGE, 40.0);
    let log = rig.run(&[note_on(0, 60, 100.0 / 127.0), note_off(10, 60)], &[], 1);
    assert!(log.ons().is_empty());
    assert!(log.offs().is_empty());
}

#[test]
fn velocity_fixed_uses_out_hi_for_everything() {
    let mut rig = Rig::new(Velocity::with_seed(1));
    set(&mut rig.device, velocity::MODE, 2.0);
    set(&mut rig.device, velocity::OUT_HI, 90.0);
    let v = velocity_of(rig.run(&[note_on(0, 60, 0.1)], &[], 1));
    assert!((v - 90.0).abs() < 0.01);
}

#[test]
fn velocity_operation_chooses_note_on_or_release_velocity() {
    let mut rig = Rig::new(Velocity::with_seed(1));
    set(&mut rig.device, velocity::OUT_HI, 10.0);
    set(&mut rig.device, velocity::OPERATION, 1.0);
    let log = rig.run(&[note_on(0, 60, 1.0), note_off_vel(10, 60, 1.0)], &[], 1);
    assert!((velocity_of(log) - 127.0).abs() < 0.01, "note-on untouched");
    let off = log.0.iter().find_map(|e| match e.message {
        MidiMessage::NoteOff { velocity, .. } => Some(velocity),
        _ => None,
    });
    assert!((off.unwrap() * 127.0 - 10.0).abs() < 0.01);
}

#[test]
fn velocity_random_stays_within_the_output_range_and_repeats_under_a_seed() {
    let run = |seed| {
        let mut rig = Rig::new(Velocity::with_seed(seed));
        set(&mut rig.device, velocity::OUT_LOW, 60.0);
        set(&mut rig.device, velocity::RANDOM, 50.0);
        let notes: Vec<_> = (0..40).map(|i| note_on(i * 8, 60, 0.75)).collect();
        rig.run(&notes, &[], 1)
            .ons()
            .iter()
            .map(|(_, _, v)| *v)
            .collect::<Vec<_>>()
    };
    let (a, b, c) = (run(5), run(5), run(6));
    assert_eq!(a, b);
    assert_ne!(a, c);
    assert!(a.iter().all(|v| *v * 127.0 >= 60.0 - 0.01 && *v <= 1.0));
}

#[test]
fn velocity_drive_lifts_and_compand_spreads_the_curve() {
    let middle = |drive: f64, compand: f64, input: f32| {
        let mut rig = Rig::new(Velocity::with_seed(1));
        set(&mut rig.device, velocity::DRIVE, drive);
        set(&mut rig.device, velocity::COMPAND, compand);
        velocity_of(rig.run(&[note_on(0, 60, input)], &[], 1))
    };
    assert!(middle(50.0, 0.0, 0.25) > middle(0.0, 0.0, 0.25));
    assert!(middle(-50.0, 0.0, 0.25) < middle(0.0, 0.0, 0.25));
    assert!(
        middle(0.0, 80.0, 0.8) > middle(0.0, 0.0, 0.8),
        "compand expands"
    );
    assert!(
        middle(0.0, -80.0, 0.8) < middle(0.0, 0.0, 0.8),
        "and compresses"
    );
}

// ---- Scale ----

#[test]
fn scale_named_scale_snaps_chromatic_notes_into_the_scale() {
    let mut rig = Rig::new(Scale::default());
    set(&mut rig.device, scale::SCALE_NAME, 0.0); // Major
    let notes: Vec<_> = (60..72).map(|k| note_on(k as u32, k, 0.5)).collect();
    let log = rig.run(&notes, &[], 1);
    assert_eq!(log.keys(), [60, 62, 62, 64, 64, 65, 67, 67, 69, 69, 71, 71]);
}

#[test]
fn scale_base_rotates_the_matrix() {
    let mut rig = Rig::new(Scale::default());
    set(&mut rig.device, scale::SCALE_NAME, 0.0); // Major
    set(&mut rig.device, scale::BASE, 2.0); // D
    assert_eq!(
        rig.run(&[note_on(0, 65, 0.5)], &[], 1).keys(),
        [66],
        "F# is in D major"
    );
}

#[test]
fn scale_user_matrix_remaps_and_removes_notes() {
    let mut rig = Rig::new(Scale::default());
    set(&mut rig.device, scale::FIRST_MAP, 2.0); // C goes to D
    set(&mut rig.device, scale::FIRST_MAP + 4, scale::REMOVED as f64); // E is gone
    let log = rig.run(&[note_on(0, 60, 0.5), note_on(1, 64, 0.5)], &[], 1);
    assert_eq!(log.keys(), [62]);
}

#[test]
fn scale_transpose_applies_after_the_mapping() {
    let mut rig = Rig::new(Scale::default());
    set(&mut rig.device, scale::SCALE_NAME, 0.0);
    set(&mut rig.device, scale::TRANSPOSE, 7.0);
    assert_eq!(rig.run(&[note_on(0, 64, 0.5)], &[], 1).keys(), [71]);
}

#[test]
fn scale_range_limits_which_notes_are_remapped() {
    // The manual's example: C2 with a range of 36 remaps C2 through B4.
    let mut rig = Rig::new(Scale::default());
    set(&mut rig.device, scale::SCALE_NAME, 0.0);
    set(&mut rig.device, scale::LOWEST, 48.0);
    set(&mut rig.device, scale::RANGE, 36.0);
    set(&mut rig.device, scale::TRANSPOSE, 7.0);
    let log = rig.run(
        &[
            note_on(0, 47, 0.5),
            note_on(1, 48, 0.5),
            note_on(2, 83, 0.5),
            note_on(3, 84, 0.5),
        ],
        &[],
        1,
    );
    assert_eq!(log.keys(), [47, 55, 90, 84]);
}

#[test]
fn scale_fold_keeps_user_mappings_within_six_semitones() {
    let mut rig = Rig::new(Scale::default());
    set(&mut rig.device, scale::FIRST_MAP, 9.0); // C goes to A
    set(&mut rig.device, scale::FOLD, 1.0);
    assert_eq!(
        rig.run(&[note_on(0, 60, 0.5)], &[], 1).keys(),
        [57],
        "A below, not above"
    );
}

#[test]
fn scale_uses_the_clip_scale_when_asked_and_its_own_otherwise() {
    let mut rig = Rig::new(Scale::default());
    set(&mut rig.device, scale::SCALE_NAME, 1.0); // Minor
    set(&mut rig.device, scale::USE_SCALE, 1.0);
    rig.harness.transport.scale = Some(major_c());
    assert_eq!(
        rig.run(&[note_on(0, 63, 0.5)], &[], 1).keys(),
        [64],
        "the clip's C major overrides its own C minor"
    );
}

// ---- Note Length ----

#[test]
fn note_length_note_on_mode_sets_the_length_from_gate_and_length() {
    let mut rig = Rig::new(NoteLength::default());
    set(&mut rig.device, length::TIME_MODE, 0.0);
    set(&mut rig.device, length::LENGTH_MS, 100.0);
    set(&mut rig.device, length::GATE, 200.0);
    let log = rig.run_frames(&[note_on(0, 60, 0.5), note_off(1, 60)], &[], 12_000);
    assert_eq!(log.ons(), [(0, 60, 0.5)]);
    assert_eq!(log.offs(), [(9_600, 60)], "100 ms doubled");
}

#[test]
fn note_length_synced_length_follows_the_tempo() {
    let mut rig = Rig::new(NoteLength::default());
    set(&mut rig.device, length::LENGTH_SYNC, 12.0); // 1/4
    let log = rig.run_frames(&[note_on(0, 60, 0.5)], &[], 26_000);
    assert_eq!(log.offs(), [(24_000, 60)]);
}

#[test]
fn note_length_note_off_mode_starts_at_the_release() {
    let mut rig = Rig::new(NoteLength::default());
    set(&mut rig.device, length::TRIGGER, 1.0);
    set(&mut rig.device, length::TIME_MODE, 0.0);
    set(&mut rig.device, length::LENGTH_MS, 50.0);
    let log = rig.run_frames(&[note_on(0, 60, 0.5), note_off(5_000, 60)], &[], 9_000);
    let ons = log.ons();
    assert_eq!(ons.len(), 1);
    assert_eq!(ons[0].0, 5_000);
    assert_eq!(log.offs()[0].0, 5_000 + 2_400);
}

#[test]
fn note_length_release_velocity_blends_on_and_off_velocities() {
    let mut rig = Rig::new(NoteLength::default());
    set(&mut rig.device, length::TRIGGER, 1.0);
    set(&mut rig.device, length::RELEASE_VELOCITY, 50.0);
    let log = rig.run_frames(
        &[note_on(0, 60, 0.8), note_off_vel(100, 60, 0.2)],
        &[],
        1_000,
    );
    assert!(
        (log.ons()[0].2 - 0.5).abs() < 1e-4,
        "halfway between: {}",
        log.ons()[0].2
    );
}

#[test]
fn note_length_decay_time_fades_the_velocity_while_the_key_is_held() {
    let mut rig = Rig::new(NoteLength::default());
    set(&mut rig.device, length::TRIGGER, 1.0);
    set(&mut rig.device, length::DECAY_TIME, 1000.0);
    let log = rig.run_frames(
        &[note_on(0, 60, 0.8), note_off_vel(24_000, 60, 0.2)],
        &[],
        25_000,
    );
    assert!(
        (log.ons()[0].2 - 0.4).abs() < 1e-3,
        "half the decay: {}",
        log.ons()[0].2
    );
}

#[test]
fn note_length_key_scale_stretches_low_notes_and_shortens_high_ones() {
    let run = |key| {
        let mut rig = Rig::new(NoteLength::default());
        set(&mut rig.device, length::TRIGGER, 1.0);
        set(&mut rig.device, length::TIME_MODE, 0.0);
        set(&mut rig.device, length::LENGTH_MS, 100.0);
        set(&mut rig.device, length::KEY_SCALE, 100.0);
        let log = rig.run_frames(&[note_on(0, key, 0.5), note_off(10, key)], &[], 20_000);
        log.offs()[0].0 - log.ons()[0].0
    };
    let c3 = run(60);
    assert_eq!(c3, 4_800);
    assert!(run(48) > c3 + 1_000, "an octave below is longer");
    assert!(run(72) < c3 - 1_000, "an octave above is shorter");
}

#[test]
fn note_length_latch_on_note_on_holds_until_the_next_note() {
    let mut rig = Rig::new(NoteLength::default());
    set(&mut rig.device, length::LATCH, 1.0);
    let log = rig.run_frames(
        &[
            note_on(0, 60, 0.5),
            note_off(100, 60),
            note_on(3_000, 64, 0.5),
        ],
        &[],
        5_000,
    );
    assert_eq!(log.offs(), [(3_000, 60)]);
    assert_eq!(log.keys(), [60, 64]);
}

#[test]
fn note_length_latch_on_note_off_fires_when_all_keys_are_up() {
    let mut rig = Rig::new(NoteLength::default());
    set(&mut rig.device, length::TRIGGER, 1.0);
    set(&mut rig.device, length::LATCH, 1.0);
    let log = rig.run_frames(
        &[
            note_on(0, 60, 0.5),
            note_on(10, 64, 0.5),
            note_off(100, 60),
            note_off(200, 64),
            note_on(2_000, 67, 0.5),
            note_off(3_000, 67),
        ],
        &[],
        4_000,
    );
    assert_eq!(
        log.ons().iter().map(|o| o.0).collect::<Vec<_>>(),
        [200, 200, 3_000]
    );
    let mut offs = log.offs();
    offs.sort();
    assert_eq!(
        offs,
        [(3_000, 60), (3_000, 64)],
        "the next trigger ends the last"
    );
}

// ---- Chord ----

#[test]
fn chord_adds_the_shifted_pitches_and_removes_them_on_release() {
    let mut rig = Rig::new(Chord::with_seed(1));
    set(&mut rig.device, chord::FIRST_SHIFT, 4.0);
    set(&mut rig.device, chord::FIRST_SHIFT + 1, 7.0);
    let log = rig.run(&[note_on(0, 60, 0.5), note_off(100, 60)], &[], 1);
    let mut keys = log.keys();
    keys.sort();
    assert_eq!(keys, [60, 64, 67]);
    let mut offs: Vec<u8> = log.offs().iter().map(|o| o.1).collect();
    offs.sort();
    assert_eq!(offs, [60, 64, 67]);
}

#[test]
fn chord_a_pitch_can_only_be_used_once_unless_strumming_with_duplicates() {
    let mut rig = Rig::new(Chord::with_seed(1));
    set(&mut rig.device, chord::FIRST_SHIFT, 8.0);
    set(&mut rig.device, chord::FIRST_SHIFT + 1, 8.0);
    assert_eq!(rig.run(&[note_on(0, 60, 0.5)], &[], 1).keys().len(), 2);

    let mut rig = Rig::new(Chord::with_seed(1));
    set(&mut rig.device, chord::FIRST_SHIFT, 8.0);
    set(&mut rig.device, chord::FIRST_SHIFT + 1, 8.0);
    set(&mut rig.device, chord::STRUM, 10.0);
    set(&mut rig.device, chord::PLAY_DUPLICATES, 1.0);
    let log = rig.run_frames(&[note_on(0, 60, 0.5)], &[], 2_000);
    assert_eq!(log.keys(), [60, 68, 68]);
}

#[test]
fn chord_velocity_sliders_scale_generated_notes_when_on() {
    let mut rig = Rig::new(Chord::with_seed(1));
    set(&mut rig.device, chord::FIRST_SHIFT, 7.0);
    set(&mut rig.device, chord::VELOCITY_ON, 1.0);
    set(&mut rig.device, chord::FIRST_VELOCITY, 50.0);
    let log = rig.run(&[note_on(0, 60, 0.8)], &[], 1);
    let shifted = log.ons().into_iter().find(|o| o.1 == 67).unwrap();
    assert!((shifted.2 - 0.4).abs() < 1e-4);
}

#[test]
fn chord_chance_sets_the_probability_of_each_generated_note() {
    let run = |chance: f64| {
        let mut rig = Rig::new(Chord::with_seed(9));
        set(&mut rig.device, chord::FIRST_SHIFT, 7.0);
        set(&mut rig.device, chord::CHANCE_ON, 1.0);
        set(&mut rig.device, chord::FIRST_CHANCE, chance);
        let notes: Vec<_> = (0..200)
            .flat_map(|i| [note_on(i * 8, 60, 0.5), note_off(i * 8 + 4, 60)])
            .collect();
        rig.run(&notes, &[], 4)
            .keys()
            .iter()
            .filter(|k| **k == 67)
            .count()
    };
    assert_eq!(run(0.0), 0);
    assert_eq!(run(100.0), 200);
    let half = run(50.0);
    assert!((70..130).contains(&half), "{half}");
}

#[test]
fn chord_strum_delays_each_note_in_shift_order_and_reversed_when_negative() {
    let ms = |ms: u32| ms * 48;
    let mut rig = Rig::new(Chord::with_seed(1));
    set(&mut rig.device, chord::FIRST_SHIFT, 4.0);
    set(&mut rig.device, chord::FIRST_SHIFT + 1, 7.0);
    set(&mut rig.device, chord::STRUM, 10.0);
    let log = rig.run_frames(&[note_on(0, 60, 0.5)], &[], 2_000);
    assert_eq!(
        log.ons().iter().map(|o| (o.0, o.1)).collect::<Vec<_>>(),
        [(0, 60), (ms(10), 64), (ms(20), 67)]
    );

    let mut rig = Rig::new(Chord::with_seed(1));
    set(&mut rig.device, chord::FIRST_SHIFT, 4.0);
    set(&mut rig.device, chord::FIRST_SHIFT + 1, 7.0);
    set(&mut rig.device, chord::STRUM, -10.0);
    let log = rig.run_frames(&[note_on(0, 60, 0.5)], &[], 2_000);
    assert_eq!(
        log.ons().iter().map(|o| (o.0, o.1)).collect::<Vec<_>>(),
        [(0, 67), (ms(10), 64), (ms(20), 60)]
    );
}

#[test]
fn chord_tension_speeds_up_or_slows_down_the_strum() {
    let gaps = |tension: f64| {
        let mut rig = Rig::new(Chord::with_seed(1));
        for i in 0..3 {
            set(&mut rig.device, chord::FIRST_SHIFT + i, (i * 2 + 2) as f64);
        }
        set(&mut rig.device, chord::STRUM, 20.0);
        set(&mut rig.device, chord::TENSION, tension);
        let log = rig.run_frames(&[note_on(0, 60, 0.5)], &[], 4_000);
        let times: Vec<u32> = log.ons().iter().map(|o| o.0).collect();
        (times[1] - times[0], times[3] - times[2], times[3])
    };
    let (first, last, total) = gaps(80.0);
    assert!(first > last, "positive tension accelerates");
    let (first2, last2, total2) = gaps(-80.0);
    assert!(first2 < last2, "negative tension slows down");
    assert_eq!(total, total2, "tension keeps the overall length");
}

#[test]
fn chord_crescendo_ramps_strummed_velocities() {
    let mut rig = Rig::new(Chord::with_seed(1));
    set(&mut rig.device, chord::FIRST_SHIFT, 4.0);
    set(&mut rig.device, chord::FIRST_SHIFT + 1, 7.0);
    set(&mut rig.device, chord::STRUM, 5.0);
    set(&mut rig.device, chord::CRESCENDO, 100.0);
    let log = rig.run_frames(&[note_on(0, 60, 0.8)], &[], 1_500);
    let v: Vec<f32> = log.ons().iter().map(|o| o.2).collect();
    assert!(v[0] < v[1] && v[1] < v[2], "{v:?}");
}

#[test]
fn chord_in_scale_degrees_and_with_a_late_release_still_ends_everything() {
    let mut rig = Rig::new(Chord::with_seed(1));
    set(&mut rig.device, chord::FIRST_SHIFT, 2.0);
    set(&mut rig.device, chord::USE_SCALE, 1.0);
    rig.harness.transport.scale = Some(major_c());
    let log = rig.run(&[note_on(0, 60, 0.5), note_off(200, 60)], &[], 1);
    let mut keys = log.keys();
    keys.sort();
    assert_eq!(keys, [60, 64], "two degrees above C is E");
    log.assert_balanced("scale chord");
}

#[test]
fn chord_strummed_notes_that_outlast_the_key_still_end() {
    let mut rig = Rig::new(Chord::with_seed(1));
    set(&mut rig.device, chord::FIRST_SHIFT, 4.0);
    set(&mut rig.device, chord::FIRST_SHIFT + 1, 7.0);
    set(&mut rig.device, chord::STRUM, 100.0);
    let log = rig.run_frames(&[note_on(0, 60, 0.5), note_off(10, 60)], &[], 20_000);
    log.assert_balanced("quick release");
    assert_eq!(log.ons().len(), 3);
}

#[test]
fn chord_bends_generated_notes_within_the_scale_when_asked() {
    let mut rig = Rig::new(Chord::with_seed(1));
    set(&mut rig.device, chord::FIRST_SHIFT, 2.0); // Two degrees up.
    set(&mut rig.device, chord::USE_SCALE, 1.0);
    set(&mut rig.device, chord::PER_NOTE_EVENTS, 1.0);
    rig.harness.transport.scale = Some(major_c());
    let bend = MidiEvent::new(
        5,
        MidiMessage::NoteExpr {
            channel: 0,
            key: 60,
            note_id: 9,
            kind: ExprKind::Pitch,
            value: 2.0,
        },
    );
    let log = rig.run(&[note_on_id(0, 60, 0.5, 9), bend], &[], 1);
    let generated = log.0.iter().find_map(|e| match e.message {
        MidiMessage::NoteExpr { key: 64, value, .. } => Some(value),
        _ => None,
    });
    // A whole tone bend from C reaches D. The note two degrees up then bends from E to F,
    // only a semitone, because that is what the next scale step above E is.
    let v = generated.expect("the generated note hears the bend");
    assert!((v - 1.0).abs() < 1e-3, "{v}");
}

#[test]
fn chord_expression_reaches_generated_notes_only_when_asked() {
    let expr_keys = |per_note: f64| {
        let mut rig = Rig::new(Chord::with_seed(1));
        set(&mut rig.device, chord::FIRST_SHIFT, 7.0);
        set(&mut rig.device, chord::PER_NOTE_EVENTS, per_note);
        let e = MidiEvent::new(
            50,
            MidiMessage::NoteExpr {
                channel: 0,
                key: 60,
                note_id: 5,
                kind: ExprKind::Pressure,
                value: 0.5,
            },
        );
        let log = rig.run(&[note_on_id(0, 60, 0.5, 5), e], &[], 1);
        let mut keys: Vec<u8> = log
            .0
            .iter()
            .filter_map(|ev| match ev.message {
                MidiMessage::NoteExpr { key, .. } => Some(key),
                _ => None,
            })
            .collect();
        keys.sort();
        keys
    };
    assert_eq!(expr_keys(0.0), [60]);
    assert_eq!(expr_keys(1.0), [60, 67]);
}

#[test]
fn chord_learns_shifts_from_held_keys() {
    let mut rig = Rig::new(Chord::with_seed(1));
    set(&mut rig.device, chord::LEARN, 1.0);
    let log = rig.run(
        &[
            note_on(0, 60, 0.5),
            note_on(1, 64, 0.5),
            note_on(2, 67, 0.5),
        ],
        &[],
        1,
    );
    let _ = log;
    let reports: Vec<(u32, f64)> = rig.harness.param_out.drain().collect();
    assert!(reports.contains(&(chord::FIRST_SHIFT, 4.0)), "{reports:?}");
    assert!(
        reports.contains(&(chord::FIRST_SHIFT + 1, 7.0)),
        "{reports:?}"
    );
}

// ---- Arpeggiator ----

fn arp_keys(style: f64, held: &[u8], steps: usize) -> Vec<u8> {
    let mut rig = Rig::new(Arpeggiator::with_seed(3));
    set(&mut rig.device, arp::STYLE, style);
    let notes: Vec<_> = held.iter().map(|k| note_on(0, *k, 0.5)).collect();
    rig.run_frames(&notes, &[], SIXTEENTH * steps as u32 + 100)
        .keys()
}

#[test]
fn arpeggiator_styles_play_their_patterns() {
    let chord = [60u8, 64, 67];
    let run = |style: arp::Style, n: usize| {
        let index = arp::Style::LABELS
            .iter()
            .position(|l| {
                *l == match style {
                    arp::Style::Up => "Up",
                    arp::Style::Down => "Down",
                    arp::Style::UpDown => "UpDown",
                    arp::Style::DownUp => "DownUp",
                    arp::Style::UpAndDown => "Up & Down",
                    arp::Style::DownAndUp => "Down & Up",
                    arp::Style::Converge => "Converge",
                    arp::Style::Diverge => "Diverge",
                    arp::Style::ConAndDiverge => "Con & Diverge",
                    arp::Style::PinkyUp => "Pinky Up",
                    arp::Style::PinkyUpDown => "Pinky UpDown",
                    arp::Style::ThumbUp => "Thumb Up",
                    arp::Style::ThumbUpDown => "Thumb UpDown",
                    _ => unreachable!(),
                }
            })
            .unwrap();
        arp_keys(index as f64, &chord, n)
    };
    use arp::Style::*;
    assert_eq!(run(Up, 3), [60, 64, 67, 60]);
    assert_eq!(run(Down, 3), [67, 64, 60, 67]);
    assert_eq!(run(UpDown, 4), [60, 64, 67, 64, 60]);
    assert_eq!(run(DownUp, 4), [67, 64, 60, 64, 67]);
    assert_eq!(run(UpAndDown, 6), [60, 64, 67, 67, 64, 60, 60]);
    assert_eq!(run(DownAndUp, 6), [67, 64, 60, 60, 64, 67, 67]);
    assert_eq!(run(Converge, 3), [60, 67, 64, 60]);
    assert_eq!(run(Diverge, 3), [64, 67, 60, 64]);
    assert_eq!(run(ConAndDiverge, 5), [60, 67, 64, 67, 60, 60]);
    assert_eq!(run(PinkyUp, 4), [60, 67, 64, 67, 60]);
    assert_eq!(run(PinkyUpDown, 6), [60, 67, 64, 67, 64, 67, 60]);
    assert_eq!(run(ThumbUp, 4), [60, 64, 60, 67, 60]);
    assert_eq!(run(ThumbUpDown, 6), [60, 64, 60, 67, 60, 64, 60]);
}

#[test]
fn arpeggiator_converge_handles_any_number_of_notes() {
    let index = arp::Style::LABELS
        .iter()
        .position(|l| *l == "Converge")
        .unwrap() as f64;
    assert_eq!(arp_keys(index, &[60, 62, 64, 65], 4), [60, 65, 62, 64, 60]);
    assert_eq!(arp_keys(index, &[60], 2), [60, 60, 60]);
}

#[test]
fn arpeggiator_play_order_follows_the_order_notes_were_played() {
    let index = arp::Style::LABELS
        .iter()
        .position(|l| *l == "Play Order")
        .unwrap() as f64;
    assert_eq!(arp_keys(index, &[67, 60, 64], 3), [67, 60, 64, 67]);
}

#[test]
fn arpeggiator_notes_of_a_hand_played_chord_join_the_pattern_it_opened() {
    // Keys of a chord played by hand land a few frames apart.
    let mut rig = Rig::new(Arpeggiator::with_seed(3));
    let notes: Vec<_> = [60u8, 64, 67]
        .iter()
        .enumerate()
        .map(|(i, k)| note_on(i as u32 * 40, *k, 0.5))
        .collect();
    let keys = rig.run_frames(&notes, &[], SIXTEENTH * 3 + 100).keys();
    assert_eq!(keys, [60, 64, 67, 60], "no doubled first note");
}

#[test]
fn arpeggiator_chord_trigger_repeats_a_block_chord() {
    let index = arp::Style::LABELS
        .iter()
        .position(|l| *l == "Chord Trigger")
        .unwrap() as f64;
    let mut rig = Rig::new(Arpeggiator::with_seed(3));
    set(&mut rig.device, arp::STYLE, index);
    let log = rig.run_frames(
        &[note_on(0, 60, 0.5), note_on(0, 64, 0.5)],
        &[],
        SIXTEENTH + 10,
    );
    let ons = log.ons();
    assert_eq!(ons.len(), 4);
    assert_eq!(ons[0].0, ons[1].0);
    assert_eq!(ons[2].0, SIXTEENTH);
}

#[test]
fn arpeggiator_random_other_visits_each_note_before_repeating() {
    let index = arp::Style::LABELS
        .iter()
        .position(|l| *l == "Random Other")
        .unwrap() as f64;
    let notes = [60u8, 62, 64, 65];
    let keys = arp_keys(index, &notes, 20);
    for chunk in keys.chunks(4).take(5) {
        let mut sorted = chunk.to_vec();
        sorted.sort();
        assert_eq!(sorted, notes, "{keys:?}");
    }
}

#[test]
fn arpeggiator_random_once_repeats_one_shuffled_pattern() {
    let index = arp::Style::LABELS
        .iter()
        .position(|l| *l == "Random Once")
        .unwrap() as f64;
    let notes = [60u8, 62, 64, 65];
    let keys = arp_keys(index, &notes, 12);
    assert_eq!(&keys[0..4], &keys[4..8]);
    assert_eq!(&keys[0..4], &keys[8..12]);
    let mut sorted = keys[0..4].to_vec();
    sorted.sort();
    assert_eq!(sorted, notes);
}

#[test]
fn arpeggiator_random_is_repeatable_under_a_seed() {
    let index = arp::Style::LABELS
        .iter()
        .position(|l| *l == "Random")
        .unwrap() as f64;
    let run = |seed| {
        let mut rig = Rig::new(Arpeggiator::with_seed(seed));
        set(&mut rig.device, arp::STYLE, index);
        let notes = [
            note_on(0, 60, 0.5),
            note_on(0, 64, 0.5),
            note_on(0, 67, 0.5),
        ];
        rig.run_frames(&notes, &[], SIXTEENTH * 30).keys()
    };
    assert_eq!(run(11), run(11));
    assert_ne!(run(11), run(12));
}

#[test]
fn arpeggiator_rate_follows_the_tempo_or_milliseconds() {
    let mut rig = Rig::new(Arpeggiator::with_seed(3));
    let log = rig.run_frames(&[note_on(0, 60, 0.5)], &[], SIXTEENTH * 3);
    assert_eq!(
        log.ons().iter().map(|o| o.0).collect::<Vec<_>>(),
        [0, SIXTEENTH, 2 * SIXTEENTH]
    );

    let mut rig = Rig::new(Arpeggiator::with_seed(3));
    set(&mut rig.device, arp::SYNC, 0.0);
    set(&mut rig.device, arp::RATE_MS, 100.0);
    let log = rig.run_frames(&[note_on(0, 60, 0.5)], &[], 10_000);
    assert_eq!(
        log.ons().iter().map(|o| o.0).collect::<Vec<_>>(),
        [0, 4_800, 9_600]
    );
}

#[test]
fn arpeggiator_gate_sets_note_length_and_overlaps_past_100_percent() {
    let mut rig = Rig::new(Arpeggiator::with_seed(3));
    set(&mut rig.device, arp::GATE, 150.0);
    let log = rig.run_frames(&[note_on(0, 60, 0.5)], &[], SIXTEENTH * 2 - 1);
    assert_eq!(
        log.offs()[0].0,
        SIXTEENTH + SIXTEENTH / 2,
        "a step and a half"
    );
    assert_eq!(
        log.ons().len(),
        2,
        "the next step starts before the first ends"
    );
}

#[test]
fn arpeggiator_distance_and_steps_transpose_each_pass() {
    // The manual's example: +12 and Steps 2 plays C3, C4, C5.
    let mut rig = Rig::new(Arpeggiator::with_seed(3));
    set(&mut rig.device, arp::DISTANCE, 12.0);
    set(&mut rig.device, arp::STEPS, 2.0);
    assert_eq!(
        rig.run_frames(&[note_on(0, 60, 0.5)], &[], SIXTEENTH * 4)
            .keys(),
        [60, 72, 84, 60]
    );
}

#[test]
fn arpeggiator_transposes_in_scale_degrees_with_a_scale() {
    let mut rig = Rig::new(Arpeggiator::with_seed(3));
    set(&mut rig.device, arp::DISTANCE, 2.0);
    set(&mut rig.device, arp::STEPS, 1.0);
    set(&mut rig.device, arp::USE_SCALE, 1.0);
    rig.harness.transport.scale = Some(major_c());
    assert_eq!(
        rig.run_frames(&[note_on(0, 60, 0.5)], &[], SIXTEENTH * 2)
            .keys(),
        [60, 64],
        "two degrees above C is E"
    );

    let mut rig = Rig::new(Arpeggiator::with_seed(3));
    set(&mut rig.device, arp::DISTANCE, 2.0);
    set(&mut rig.device, arp::STEPS, 1.0);
    set(&mut rig.device, arp::SCALE, 2.0); // Minor
    set(&mut rig.device, arp::ROOT, 0.0);
    assert_eq!(
        rig.run_frames(&[note_on(0, 60, 0.5)], &[], SIXTEENTH * 2)
            .keys(),
        [60, 63],
        "its own Root and Scale work without a clip scale"
    );
}

#[test]
fn arpeggiator_offset_rotates_the_pattern() {
    // The manual: at offset 1 the second note plays first and the first plays last.
    let mut rig = Rig::new(Arpeggiator::with_seed(3));
    set(&mut rig.device, arp::OFFSET, 1.0);
    let notes = [
        note_on(0, 60, 0.5),
        note_on(0, 64, 0.5),
        note_on(0, 67, 0.5),
    ];
    assert_eq!(
        rig.run_frames(&notes, &[], SIXTEENTH * 3 + 10).keys(),
        [64, 67, 60, 64]
    );
}

#[test]
fn arpeggiator_repeats_stop_the_pattern_until_retriggered() {
    let mut rig = Rig::new(Arpeggiator::with_seed(3));
    set(&mut rig.device, arp::REPEATS, 1.0); // Two repeats.
    let log = rig.run_frames(
        &[note_on(0, 60, 0.5), note_on(0, 64, 0.5)],
        &[],
        SIXTEENTH * 10,
    );
    assert_eq!(log.keys(), [60, 64, 60, 64]);
}

#[test]
fn arpeggiator_note_retrigger_restarts_the_pattern_on_a_new_note() {
    let mut rig = Rig::new(Arpeggiator::with_seed(3));
    set(&mut rig.device, arp::RETRIGGER, 1.0);
    let log = rig.run_frames(
        &[
            note_on(0, 60, 0.5),
            note_on(0, 62, 0.5),
            note_on(0, 64, 0.5),
            note_on(SIXTEENTH + 100, 67, 0.5),
        ],
        &[],
        SIXTEENTH * 4,
    );
    assert_eq!(
        log.keys(),
        [60, 62, 60, 62],
        "starts over from the first note"
    );
}

#[test]
fn arpeggiator_off_retrigger_never_restarts() {
    let mut rig = Rig::new(Arpeggiator::with_seed(3));
    let log = rig.run_frames(
        &[
            note_on(0, 60, 0.5),
            note_on(0, 62, 0.5),
            note_on(0, 64, 0.5),
            note_on(SIXTEENTH + 100, 65, 0.5),
        ],
        &[],
        SIXTEENTH * 4,
    );
    assert_eq!(
        log.keys(),
        [60, 62, 64, 65],
        "carries on through the new note"
    );
}

#[test]
fn arpeggiator_beat_retrigger_restarts_on_the_interval() {
    let mut rig = Rig::new(Arpeggiator::with_seed(3));
    set(&mut rig.device, arp::RETRIGGER, 2.0);
    set(&mut rig.device, arp::INTERVAL, 2.0); // 1/4
    let notes: Vec<_> = [60u8, 62, 64, 65, 67, 69]
        .iter()
        .map(|k| note_on(0, *k, 0.5))
        .collect();
    let log = rig.run_frames(&notes, &[], SIXTEENTH * 6);
    assert_eq!(log.keys(), [60, 62, 64, 65, 60, 62]);
}

#[test]
fn arpeggiator_hold_keeps_playing_and_replaces_on_a_fresh_press() {
    let mut rig = Rig::new(Arpeggiator::with_seed(3));
    set(&mut rig.device, arp::HOLD, 1.0);
    let log = rig.run_frames(
        &[
            note_on(0, 60, 0.5),
            note_off(100, 60),
            note_on(SIXTEENTH * 2 + 50, 72, 0.5),
        ],
        &[],
        SIXTEENTH * 3 + 10,
    );
    assert_eq!(
        log.keys(),
        [60, 60, 60, 72],
        "steps carry on, then the new note takes over"
    );
}

#[test]
fn arpeggiator_hold_adds_while_a_key_is_down_and_removes_on_a_second_press() {
    let mut rig = Rig::new(Arpeggiator::with_seed(3));
    set(&mut rig.device, arp::HOLD, 1.0);
    let log = rig.run_frames(
        &[
            note_on(0, 60, 0.5),
            note_on(0, 64, 0.5),
            note_off(100, 64),
            // 60 is still down, so 64 was held by Hold: playing it again takes it out.
            note_on(SIXTEENTH + 50, 64, 0.5),
        ],
        &[],
        SIXTEENTH * 3,
    );
    assert_eq!(log.keys(), [60, 64, 60], "gone after the second press");
}

#[test]
fn arpeggiator_hold_off_drops_keys_no_longer_down() {
    let mut rig = Rig::new(Arpeggiator::with_seed(3));
    set(&mut rig.device, arp::HOLD, 1.0);
    let log = rig.run_frames(
        &[note_on(0, 60, 0.5), note_off(100, 60)],
        &[param(SIXTEENTH * 2 + 10, arp::HOLD, 0.0)],
        SIXTEENTH * 5,
    );
    assert_eq!(log.keys().len(), 3, "stops once Hold is released");
    log.assert_balanced("hold off");
}

#[test]
fn arpeggiator_velocity_decays_to_the_target() {
    let mut rig = Rig::new(Arpeggiator::with_seed(3));
    set(&mut rig.device, arp::VELOCITY_ON, 1.0);
    set(&mut rig.device, arp::DECAY, 500.0);
    set(&mut rig.device, arp::TARGET, 0.0);
    let log = rig.run_frames(&[note_on(0, 60, 1.0)], &[], SIXTEENTH * 8);
    let v: Vec<f32> = log.ons().iter().map(|o| o.2).collect();
    assert!((v[0] - 1.0).abs() < 1e-3);
    assert!(v[1] < v[0] && v[2] < v[1], "{v:?}");
    assert!(v[4] < 0.05, "reaches the target after the decay: {v:?}");
}

#[test]
fn arpeggiator_velocity_retrigger_restarts_the_slope() {
    let mut rig = Rig::new(Arpeggiator::with_seed(3));
    set(&mut rig.device, arp::VELOCITY_ON, 1.0);
    set(&mut rig.device, arp::DECAY, 1000.0);
    set(&mut rig.device, arp::TARGET, 0.0);
    set(&mut rig.device, arp::VELOCITY_RETRIGGER, 1.0);
    set(&mut rig.device, arp::RETRIGGER, 2.0);
    set(&mut rig.device, arp::INTERVAL, 2.0); // Every beat, every 4 steps.
    let log = rig.run_frames(&[note_on(0, 60, 1.0)], &[], SIXTEENTH * 4 + 10);
    let v: Vec<f32> = log.ons().iter().map(|o| o.2).collect();
    assert!(v[3] < v[2]);
    assert!((v[4] - 1.0).abs() < 1e-3, "the slope restarts: {v:?}");
}

#[test]
fn arpeggiator_swing_groove_delays_off_grid_steps() {
    let mut rig = Rig::new(Arpeggiator::with_seed(3));
    set(&mut rig.device, arp::GROOVE, 2.0); // Swing 16.
    let log = rig.run_frames(&[note_on(0, 60, 0.5)], &[], SIXTEENTH * 3);
    let times: Vec<u32> = log.ons().iter().map(|o| o.0).collect();
    assert_eq!(times[0], 0);
    assert!(times[1] > SIXTEENTH, "the off-beat 16th is pushed late");
    assert_eq!(times[2], 2 * SIXTEENTH);
}

#[test]
fn arpeggiator_notes_have_their_own_ids_and_never_repeat_one() {
    let mut rig = Rig::new(Arpeggiator::with_seed(3));
    let log = rig.run_frames(&[note_on_id(0, 60, 0.5, 42)], &[], SIXTEENTH * 4);
    let mut ids: Vec<u32> = log
        .0
        .iter()
        .filter_map(|e| match e.message {
            MidiMessage::NoteOn { note_id, .. } => Some(note_id),
            _ => None,
        })
        .collect();
    assert!(ids.iter().all(|id| *id != 0 && *id != 42));
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 4);
}

#[test]
fn arpeggiator_expression_goes_to_the_note_playing_for_its_source() {
    let mut rig = Rig::new(Arpeggiator::with_seed(3));
    let e = MidiEvent::new(
        50,
        MidiMessage::NoteExpr {
            channel: 0,
            key: 60,
            note_id: 42,
            kind: ExprKind::Slide,
            value: 0.3,
        },
    );
    let log = rig.run_frames(&[note_on_id(0, 60, 0.5, 42), e], &[], 200);
    let forwarded = log.0.iter().find_map(|ev| match ev.message {
        MidiMessage::NoteExpr { note_id, .. } => Some(note_id),
        _ => None,
    });
    let sounding = log.0.iter().find_map(|ev| match ev.message {
        MidiMessage::NoteOn { note_id, .. } => Some(note_id),
        _ => None,
    });
    assert!(sounding.is_some());
    assert_eq!(forwarded, sounding);
}

// ---- Random ----

#[test]
fn random_chance_zero_changes_nothing() {
    let mut rig = Rig::new(Random::with_seed(1));
    set(&mut rig.device, random::CHANCE, 0.0);
    assert_eq!(rig.run(&[note_on(0, 60, 0.5)], &[], 1).keys(), [60]);
}

#[test]
fn random_the_manual_example_half_and_half_an_octave_up() {
    let mut rig = Rig::new(Random::with_seed(7));
    set(&mut rig.device, random::CHANCE, 50.0);
    set(&mut rig.device, random::CHOICES, 1.0);
    set(&mut rig.device, random::INTERVAL, 12.0);
    let notes: Vec<_> = (0..400)
        .flat_map(|i| [note_on(i * 8, 60, 0.5), note_off(i * 8 + 4, 60)])
        .collect();
    let log = rig.run(&notes, &[], 8);
    let keys = log.keys();
    let high = keys.iter().filter(|k| **k == 72).count();
    let low = keys.iter().filter(|k| **k == 60).count();
    assert_eq!(high + low, keys.len(), "only C3 or C4");
    assert!((130..270).contains(&high), "{high}");
    log.assert_balanced("random");
}

#[test]
fn random_choices_span_a_range_of_pitches() {
    let mut rig = Rig::new(Random::with_seed(7));
    set(&mut rig.device, random::CHANCE, 100.0);
    set(&mut rig.device, random::CHOICES, 12.0);
    set(&mut rig.device, random::INTERVAL, 1.0);
    let notes: Vec<_> = (0..300)
        .flat_map(|i| [note_on(i * 8, 60, 0.5), note_off(i * 8 + 4, 60)])
        .collect();
    let keys = rig.run(&notes, &[], 6).keys();
    assert!(keys.iter().all(|k| (61..=72).contains(k)));
    for k in 61..=72u8 {
        assert!(keys.contains(&k), "{k} should turn up");
    }
}

#[test]
fn random_sub_and_bi_go_the_other_way() {
    let spread = |sign: f64| {
        let mut rig = Rig::new(Random::with_seed(7));
        set(&mut rig.device, random::CHANCE, 100.0);
        set(&mut rig.device, random::CHOICES, 3.0);
        set(&mut rig.device, random::INTERVAL, 1.0);
        set(&mut rig.device, random::SIGN, sign);
        let notes: Vec<_> = (0..200)
            .flat_map(|i| [note_on(i * 8, 60, 0.5), note_off(i * 8 + 4, 60)])
            .collect();
        rig.run(&notes, &[], 4).keys()
    };
    assert!(spread(1.0).iter().all(|k| (57..=59).contains(k)));
    let bi = spread(2.0);
    assert!(bi.iter().any(|k| *k < 60) && bi.iter().any(|k| *k > 60));
}

#[test]
fn random_alt_cycles_through_pitches_in_a_fixed_order() {
    // The manual's examples.
    let run = |choices: f64, interval: f64, notes: usize| {
        let mut rig = Rig::new(Random::with_seed(1));
        set(&mut rig.device, random::MODE, 1.0);
        set(&mut rig.device, random::CHANCE, 100.0);
        set(&mut rig.device, random::CHOICES, choices);
        set(&mut rig.device, random::INTERVAL, interval);
        let events: Vec<_> = (0..notes as u32)
            .flat_map(|i| [note_on(i * 8, 60, 0.5), note_off(i * 8 + 4, 60)])
            .collect();
        rig.run(&events, &[], 2).keys()
    };
    assert_eq!(run(2.0, 2.0, 4), [60, 62, 60, 62]);
    let keys = run(12.0, 1.0, 14);
    assert_eq!(&keys[..12], &(60..=71).collect::<Vec<u8>>()[..]);
    assert_eq!(keys[12], 60, "the cycle restarts after the twelfth");
}

#[test]
fn random_alt_chance_zero_plays_the_incoming_note() {
    let mut rig = Rig::new(Random::with_seed(1));
    set(&mut rig.device, random::MODE, 1.0);
    set(&mut rig.device, random::CHANCE, 0.0);
    set(&mut rig.device, random::CHOICES, 4.0);
    let events: Vec<_> = (0..4)
        .flat_map(|i| [note_on(i * 8, 60, 0.5), note_off(i * 8 + 4, 60)])
        .collect();
    assert_eq!(rig.run(&events, &[], 1).keys(), [60, 60, 60, 60]);
}

#[test]
fn random_uses_scale_degrees_with_a_clip_scale() {
    let mut rig = Rig::new(Random::with_seed(7));
    set(&mut rig.device, random::CHANCE, 100.0);
    set(&mut rig.device, random::CHOICES, 1.0);
    set(&mut rig.device, random::INTERVAL, 1.0);
    set(&mut rig.device, random::USE_SCALE, 1.0);
    rig.harness.transport.scale = Some(major_c());
    assert_eq!(
        rig.run(&[note_on(0, 64, 0.5)], &[], 1).keys(),
        [65],
        "a degree up from E is F"
    );
}

// ---- CC Control ----

#[test]
fn cc_control_sends_each_kind_of_message_when_a_control_moves() {
    let mut rig = Rig::new(CcControl::default());
    let log = rig.run(
        &[],
        &[
            param(0, cc::MOD_WHEEL, 100.0),
            param(0, cc::PITCH_BEND, 4096.0),
            param(0, cc::PRESSURE, 64.0),
            param(0, cc::CUSTOM_A, 1.0),
            param(0, cc::FIRST_CUSTOM, 33.0),
        ],
        1,
    );
    let m: Vec<MidiMessage> = log.0.iter().map(|e| e.message).collect();
    assert!(m.contains(&MidiMessage::ControlChange {
        channel: 0,
        cc: 1,
        value: 100
    }));
    assert!(m.contains(&MidiMessage::PitchBend {
        channel: 0,
        value: 0.5
    }));
    assert!(m.contains(&MidiMessage::ChannelPressure {
        channel: 0,
        value: 64.0 / 127.0
    }));
    assert!(m.contains(&MidiMessage::ControlChange {
        channel: 0,
        cc: 64,
        value: 127
    }));
    assert!(m.contains(&MidiMessage::ControlChange {
        channel: 0,
        cc: 20,
        value: 33
    }));
}

#[test]
fn cc_control_custom_controls_send_the_controller_they_are_set_to() {
    let mut rig = Rig::new(CcControl::default());
    set(&mut rig.device, cc::FIRST_CUSTOM_CC + 3, 77.0);
    let log = rig.run(&[], &[param(0, cc::FIRST_CUSTOM + 3, 9.0)], 1);
    assert!(log.0.iter().any(|e| e.message
        == MidiMessage::ControlChange {
            channel: 0,
            cc: 77,
            value: 9
        }));
}

#[test]
fn cc_control_merges_with_midi_already_there() {
    let mut rig = Rig::new(CcControl::default());
    let existing = MidiEvent::new(
        5,
        MidiMessage::ControlChange {
            channel: 0,
            cc: 7,
            value: 50,
        },
    );
    let log = rig.run(&[existing], &[param(0, cc::MOD_WHEEL, 1.0)], 1);
    assert!(log.0.iter().any(|e| e.message
        == MidiMessage::ControlChange {
            channel: 0,
            cc: 7,
            value: 50
        }));
}

#[test]
fn cc_control_send_button_resends_everything_and_lights_back_off() {
    let mut rig = Rig::new(CcControl::default());
    set(&mut rig.device, cc::MOD_WHEEL, 11.0);
    let log = rig.run(&[], &[param(0, cc::SEND, 1.0)], 1);
    assert!(log.0.iter().any(|e| e.message
        == MidiMessage::ControlChange {
            channel: 0,
            cc: 1,
            value: 11
        }));
    let reports: Vec<(u32, f64)> = rig.harness.param_out.drain().collect();
    assert_eq!(reports, [(cc::SEND, 0.0)], "the button springs back");
}

#[test]
fn cc_control_learn_assigns_the_incoming_controller() {
    let mut rig = Rig::new(CcControl::default());
    set(&mut rig.device, cc::LEARN, 1.0);
    set(&mut rig.device, cc::LEARN_TARGET, 2.0); // Custom C.
    let ev = MidiEvent::new(
        0,
        MidiMessage::ControlChange {
            channel: 0,
            cc: 44,
            value: 21,
        },
    );
    rig.run(&[ev], &[], 1);
    let reports: Vec<(u32, f64)> = rig.harness.param_out.drain().collect();
    assert!(
        reports.contains(&(cc::FIRST_CUSTOM_CC + 1, 44.0)),
        "{reports:?}"
    );
    assert!(
        reports.contains(&(cc::FIRST_CUSTOM + 1, 21.0)),
        "{reports:?}"
    );
}

// ---- Expression Control ----

/// Runs a block with a modulation output, which the harness doesn't provide.
fn run_with_mods(
    device: &mut dyn Processor,
    events: &[MidiEvent],
    transport: &TransportInfo,
    out: &mut [f32],
) {
    let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
    let mut events_out = EventQueue::with_capacity(1024);
    let mut param_out = ParamOutQueue::with_capacity(16);
    let view = ViewWriter::none();
    let mut ctx = ProcessContext {
        audio: AudioBuffer {
            left: &mut l,
            right: &mut r,
        },
        sidechain: None,
        aux_out: &mut [],
        events,
        events_out: &mut events_out,
        params: &[],
        transport,
        param_out: &mut param_out,
        mod_out: out,
        view: &view,
    };
    device.process(&mut ctx);
}

fn expression() -> ExpressionControl {
    ExpressionControl::with_seed(1)
}

fn playing() -> TransportInfo {
    TransportInfo {
        playing: true,
        sample_rate: SR,
        ..Default::default()
    }
}

#[test]
fn expression_control_follows_each_midi_source() {
    let source = |index: f64, events: &[MidiEvent]| {
        let mut d = expression();
        set(&mut d, expr::slot_param(0, expr::SOURCE), index);
        let mut out = [0.0; 5];
        run_with_mods(&mut d, events, &playing(), &mut out);
        out[0]
    };
    let cc = |n: u8, v: u8| {
        MidiEvent::new(
            0,
            MidiMessage::ControlChange {
                channel: 0,
                cc: n,
                value: v,
            },
        )
    };
    assert!((source(0.0, &[note_on(0, 60, 0.4)]) - 0.4).abs() < 1e-3);
    assert!((source(1.0, &[cc(1, 127)]) - 1.0).abs() < 1e-3);
    assert!((source(4.0, &[note_on(0, 127, 0.5)]) - 1.0).abs() < 1e-3);
    assert!((source(5.0, &[cc(11, 127)]) - 1.0).abs() < 1e-3);
    assert!((source(8.0, &[cc(74, 127)]) - 1.0).abs() < 1e-3);
    assert!((source(9.0, &[cc(64, 127)]) - 1.0).abs() < 1e-3);
    let bend = MidiEvent::new(
        0,
        MidiMessage::PitchBend {
            channel: 0,
            value: 1.0,
        },
    );
    assert!((source(2.0, &[bend]) - 1.0).abs() < 1e-3);
    let pressure = MidiEvent::new(
        0,
        MidiMessage::ChannelPressure {
            channel: 0,
            value: 0.75,
        },
    );
    assert!((source(3.0, &[pressure]) - 0.75).abs() < 1e-3);
}

#[test]
fn expression_control_increment_counts_note_triggers_and_resets_on_stop() {
    let mut d = expression();
    set(&mut d, expr::slot_param(0, expr::SOURCE), 7.0);
    set(&mut d, expr::slot_param(0, expr::INCREMENT_STEPS), 4.0);
    let t = playing();
    let mut out = [0.0; 5];
    let trigger = [note_on(0, 60, 0.5)];
    let mut values = Vec::new();
    for _ in 0..5 {
        run_with_mods(&mut d, &trigger, &t, &mut out);
        values.push((out[0] * 3.0).round() as i32);
    }
    assert_eq!(
        values,
        [0, 1, 2, 3, 0],
        "cycles through the whole range in 4 notes"
    );

    // Stopping the transport resets the count.
    run_with_mods(
        &mut d,
        &[],
        &TransportInfo {
            playing: false,
            ..playing()
        },
        &mut out,
    );
    run_with_mods(&mut d, &trigger, &t, &mut out);
    assert!(
        out[0].abs() < 1e-6,
        "the first note after a stop is the minimum"
    );
}

#[test]
fn expression_control_random_varies_only_on_a_new_note_and_scales_with_amount() {
    let run = |amount: f64| {
        let mut d = expression();
        set(&mut d, expr::slot_param(0, expr::SOURCE), 6.0);
        set(&mut d, expr::slot_param(0, expr::RANDOM_AMOUNT), amount);
        let t = playing();
        let mut out = [0.0; 5];
        let mut values = Vec::new();
        for _ in 0..50 {
            run_with_mods(&mut d, &[note_on(0, 60, 0.5)], &t, &mut out);
            values.push(out[0]);
        }
        values
    };
    let full = run(100.0);
    let spread = full.iter().cloned().fold(f32::MIN, f32::max)
        - full.iter().cloned().fold(f32::MAX, f32::min);
    assert!(spread > 0.5, "{spread}");
    assert!(
        run(0.0).iter().all(|v| (*v - 0.5).abs() < 1e-3),
        "no amount, no deviation"
    );
}

#[test]
fn expression_control_curve_min_max_and_output_range_shape_the_result() {
    let run = |configure: &dyn Fn(&mut ExpressionControl)| {
        let mut d = expression();
        configure(&mut d);
        let mut out = [0.0; 5];
        run_with_mods(&mut d, &[note_on(0, 60, 0.5)], &playing(), &mut out);
        out[0]
    };
    let id = |p| expr::slot_param(0, p);
    assert!(
        (run(&|d| set(d, id(expr::MIN), 50.0)) - 0.75).abs() < 1e-3,
        "min lifts the floor"
    );
    let bipolar = run(&|d| set(d, id(expr::POLARITY), 0.0));
    assert!(
        bipolar.abs() < 1e-3,
        "bipolar centres the middle of the range"
    );
    let remote = run(&|d| {
        set(d, id(expr::MODULATION), 1.0);
        set(d, id(expr::OUT_MIN), 20.0);
        set(d, id(expr::OUT_MAX), 40.0);
    });
    assert!(
        (remote - 0.3).abs() < 1e-3,
        "remote control spans min to max: {remote}"
    );
    let unipolar_scaled = run(&|d| set(d, id(expr::OUT_MAX), 50.0));
    assert!((unipolar_scaled - 0.25).abs() < 1e-3);
    let lifted = run(&|d| set(d, id(expr::CURVE_A), 80.0));
    assert!(lifted > 0.6, "positive curve lifts the middle: {lifted}");
}

#[test]
fn expression_control_s_curve_uses_its_breakpoint() {
    let mut d = expression();
    let id = |p| expr::slot_param(0, p);
    set(&mut d, id(expr::CURVE_TYPE), 1.0);
    set(&mut d, id(expr::X), 50.0);
    set(&mut d, id(expr::Y), 80.0);
    let mut out = [0.0; 5];
    run_with_mods(&mut d, &[note_on(0, 60, 0.5)], &playing(), &mut out);
    assert!(
        (out[0] - 0.8).abs() < 1e-2,
        "the middle breakpoint: {}",
        out[0]
    );
}

#[test]
fn expression_control_smoothing_takes_the_rise_time_to_get_there() {
    let mut d = expression();
    set(&mut d, expr::slot_param(0, expr::RISE), 100.0);
    let t = playing();
    let mut out = [0.0; 5];
    run_with_mods(&mut d, &[note_on(0, 60, 1.0)], &t, &mut out);
    assert!(out[0] < 0.1, "not there yet: {}", out[0]);
    for _ in 0..4 {
        run_with_mods(&mut d, &[], &t, &mut out);
    }
    assert!((0.3..0.8).contains(&out[0]), "on its way: {}", out[0]);
    for _ in 0..12 {
        run_with_mods(&mut d, &[], &t, &mut out);
    }
    assert!(out[0] > 0.98, "arrives: {}", out[0]);
}

#[test]
fn expression_control_passes_midi_through() {
    let mut d = expression();
    let t = playing();
    let (mut l, mut r) = (vec![0.0f32; 64], vec![0.0f32; 64]);
    let mut events_out = EventQueue::with_capacity(16);
    let mut param_out = ParamOutQueue::with_capacity(4);
    let events = [note_on(0, 60, 0.5)];
    let view = ViewWriter::none();
    let mut mods = [0.0; 5];
    d.process(&mut ProcessContext {
        audio: AudioBuffer {
            left: &mut l,
            right: &mut r,
        },
        sidechain: None,
        aux_out: &mut [],
        events: &events,
        events_out: &mut events_out,
        params: &[],
        transport: &t,
        param_out: &mut param_out,
        mod_out: &mut mods,
        view: &view,
    });
    assert_eq!(events_out.as_slice(), events);
}

// ---- MIDI Monitor ----

#[test]
fn midi_monitor_shows_notes_and_keeps_a_log_it_can_freeze() {
    let channel = Arc::new(ViewChannel::new(monitor::VIEW_LEN));
    let reader = channel.attach();
    let mut d = MidiMonitor::default();
    let transport = playing();
    let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
    let mut events_out = EventQueue::with_capacity(16);
    let mut param_out = ParamOutQueue::with_capacity(4);
    let view = ViewWriter::new(&channel);
    let mut process = |d: &mut MidiMonitor, events: &[MidiEvent]| {
        events_out.clear();
        d.process(&mut ProcessContext {
            audio: AudioBuffer {
                left: &mut l,
                right: &mut r,
            },
            sidechain: None,
            aux_out: &mut [],
            events,
            events_out: &mut events_out,
            params: &[],
            transport: &transport,
            param_out: &mut param_out,
            mod_out: &mut [],
            view: &view,
        });
        events_out.as_slice().to_vec()
    };
    let bend = MidiEvent::new(
        0,
        MidiMessage::PitchBend {
            channel: 0,
            value: 0.25,
        },
    );
    let out = process(&mut d, &[note_on(0, 60, 0.5), bend]);
    assert_eq!(out.len(), 2, "passes everything through");

    let mut notes = [0.0f32; monitor::NOTE_COUNT];
    assert!(reader.read(monitor::NOTES, &mut notes));
    assert_eq!(notes[60], 0.5);

    let mut count = [0.0f32];
    assert!(reader.read(monitor::LOG_COUNT, &mut count));
    assert_eq!(count[0], 2.0);

    process(&mut d, &[note_off(0, 60)]);
    assert!(reader.read(monitor::NOTES, &mut notes));
    assert_eq!(notes[60], 0.0);

    set(&mut d, monitor::FREEZE, 1.0);
    process(&mut d, &[note_on(0, 62, 0.5)]);
    assert!(reader.read(monitor::LOG_COUNT, &mut count));
    assert_eq!(count[0], 3.0, "frozen log stops growing");
}

#[test]
fn midi_monitor_names_common_chords() {
    assert_eq!(monitor::chord_name(&[60, 64, 67]).as_deref(), Some("maj"));
    assert_eq!(monitor::chord_name(&[60, 63, 67]).as_deref(), Some("min"));
    assert_eq!(
        monitor::chord_name(&[62, 65, 69, 72]).as_deref(),
        Some("min7")
    );
    assert_eq!(monitor::chord_name(&[60, 61]), None);
}

// ---- MPE Control ----

fn expr_event(offset: u32, kind: ExprKind, value: f32) -> MidiEvent {
    MidiEvent::new(
        offset,
        MidiMessage::NoteExpr {
            channel: 0,
            key: 60,
            note_id: 7,
            kind,
            value,
        },
    )
}

fn expr_values(log: &Log, kind: ExprKind) -> Vec<f32> {
    log.0
        .iter()
        .filter_map(|e| match e.message {
            MidiMessage::NoteExpr { kind: k, value, .. } if k == kind => Some(value),
            _ => None,
        })
        .collect()
}

fn note_with_expr(kind: ExprKind, value: f32) -> [MidiEvent; 2] {
    [note_on_id(0, 60, 0.5, 7), expr_event(1, kind, value)]
}

#[test]
fn mpe_control_shapes_each_source_through_its_curve() {
    let mut rig = Rig::new(MpeControl::default());
    set(&mut rig.device, mpe::PRESS + mpe::MIN, 50.0);
    let log = rig.run(&note_with_expr(ExprKind::Pressure, 0.5), &[], 1);
    let v = expr_values(log, ExprKind::Pressure);
    assert!((v[0] - 0.75).abs() < 1e-3, "{v:?}");
}

#[test]
fn mpe_control_switching_a_source_off_leaves_it_unshaped() {
    let mut rig = Rig::new(MpeControl::default());
    set(&mut rig.device, mpe::SLIDE + mpe::ON, 0.0);
    set(&mut rig.device, mpe::SLIDE + mpe::MIN, 50.0);
    let log = rig.run(&note_with_expr(ExprKind::Slide, 0.25), &[], 1);
    assert_eq!(expr_values(log, ExprKind::Slide), [0.25]);
}

#[test]
fn mpe_control_press_can_become_slide_or_channel_aftertouch() {
    let mut rig = Rig::new(MpeControl::default());
    set(&mut rig.device, mpe::PRESS + mpe::SWAP_TO_SLIDE, 1.0);
    let log = rig.run(&note_with_expr(ExprKind::Pressure, 0.5), &[], 1);
    assert_eq!(expr_values(log, ExprKind::Slide), [0.5]);
    assert!(expr_values(log, ExprKind::Pressure).is_empty());

    let mut rig = Rig::new(MpeControl::default());
    set(&mut rig.device, mpe::PRESS + mpe::PRESS_TO_AT, 1.0);
    let log = rig.run(&note_with_expr(ExprKind::Pressure, 0.5), &[], 1);
    assert!(log.0.iter().any(|e| e.message
        == MidiMessage::ChannelPressure {
            channel: 0,
            value: 0.5
        }));
}

#[test]
fn mpe_control_slide_can_become_mod_wheel_and_centred_measures_from_the_middle() {
    let mut rig = Rig::new(MpeControl::default());
    set(&mut rig.device, mpe::SLIDE + mpe::SLIDE_TO_MOD, 1.0);
    let log = rig.run(&note_with_expr(ExprKind::Slide, 1.0), &[], 1);
    assert!(log.0.iter().any(|e| e.message
        == MidiMessage::ControlChange {
            channel: 0,
            cc: 1,
            value: 127
        }));

    let mut rig = Rig::new(MpeControl::default());
    set(&mut rig.device, mpe::SLIDE + mpe::CENTERED, 1.0);
    let log = rig.run(&note_with_expr(ExprKind::Slide, 0.5), &[], 1);
    assert!(
        expr_values(log, ExprKind::Slide)[0].abs() < 1e-3,
        "the centre is zero"
    );
}

#[test]
fn mpe_control_slide_relative_starts_in_the_middle_and_onset_never_moves() {
    let mut rig = Rig::new(MpeControl::default());
    set(&mut rig.device, mpe::SLIDE + mpe::SLIDE_MODE, 1.0);
    let log = rig.run(
        &[
            note_on_id(0, 60, 0.5, 7),
            expr_event(1, ExprKind::Slide, 0.8),
            expr_event(2, ExprKind::Slide, 1.0),
        ],
        &[],
        1,
    );
    let v = expr_values(log, ExprKind::Slide);
    assert!(
        (v[0] - 0.5).abs() < 1e-3,
        "first value is the middle: {v:?}"
    );
    assert!((v[1] - 0.7).abs() < 1e-3, "then moves by the change: {v:?}");

    let mut rig = Rig::new(MpeControl::default());
    set(&mut rig.device, mpe::SLIDE + mpe::SLIDE_MODE, 2.0);
    let log = rig.run(
        &[
            note_on_id(0, 60, 0.5, 7),
            expr_event(1, ExprKind::Slide, 0.2),
            expr_event(2, ExprKind::Slide, 0.9),
        ],
        &[],
        1,
    );
    assert_eq!(
        expr_values(log, ExprKind::Slide),
        [0.2],
        "only the note-on's value"
    );
}

#[test]
fn mpe_control_default_gives_notes_a_value_without_mpe_data() {
    let mut rig = Rig::new(MpeControl::default());
    set(&mut rig.device, mpe::PRESS + mpe::PRESS_DEFAULT, 1.0);
    set(&mut rig.device, mpe::PRESS + mpe::PRESS_DEFAULT_VALUE, 25.0);
    let log = rig.run(&[note_on_id(0, 60, 0.5, 7)], &[], 1);
    assert_eq!(expr_values(log, ExprKind::Pressure), [0.25]);
}

#[test]
fn mpe_control_pitch_range_and_translation_to_channel_bend() {
    let mut rig = Rig::new(MpeControl::default());
    set(&mut rig.device, mpe::NOTE_PB + mpe::PITCH_RANGE, 1.0); // 2x
    let log = rig.run(&note_with_expr(ExprKind::Pitch, 2.0), &[], 1);
    assert_eq!(expr_values(log, ExprKind::Pitch), [4.0]);

    let mut rig = Rig::new(MpeControl::default());
    set(&mut rig.device, mpe::NOTE_PB + mpe::NOTE_PB_TO_PB, 1.0);
    let log = rig.run(&note_with_expr(ExprKind::Pitch, -24.0), &[], 1);
    assert!(log.0.iter().any(|e| e.message
        == MidiMessage::PitchBend {
            channel: 0,
            value: -0.5
        }));
    assert!(expr_values(log, ExprKind::Pitch).is_empty());
}

#[test]
fn mpe_control_smoothing_glides_to_new_values() {
    let mut rig = Rig::new(MpeControl::default());
    set(&mut rig.device, mpe::SLIDE + mpe::SMOOTH, 1.0);
    set(&mut rig.device, mpe::SLIDE + mpe::RISE, 50.0);
    let log = rig.run(&note_with_expr(ExprKind::Slide, 1.0), &[], 30);
    let v = expr_values(log, ExprKind::Slide);
    assert!(v.len() > 5, "emits a series of values: {}", v.len());
    assert!(v.windows(2).all(|w| w[1] >= w[0]), "rises steadily: {v:?}");
    assert!(*v.last().unwrap() > 0.98, "reaches the target: {v:?}");
}

// ---- Note Echo ----

#[test]
fn note_echo_repeats_with_falling_velocity() {
    let mut rig = Rig::new(NoteEcho::default());
    set(&mut rig.device, echo::DELAY_SIXTEENTHS, 1.0);
    set(&mut rig.device, echo::DELAY, 50.0);
    set(&mut rig.device, echo::FEEDBACK, 50.0);
    let log = rig.run_frames(
        &[note_on(0, 60, 0.8), note_off(100, 60)],
        &[],
        SIXTEENTH * 4,
    );
    let ons = log.ons();
    assert_eq!(ons[0], (0, 60, 0.8));
    assert_eq!(ons[1].0, SIXTEENTH);
    assert!((ons[1].2 - 0.4).abs() < 1e-3);
    assert_eq!(ons[2].0, 2 * SIXTEENTH);
    assert!((ons[2].2 - 0.2).abs() < 1e-3);
    log.assert_balanced("echo");
}

#[test]
fn note_echo_delay_time_is_in_sixteenths_or_milliseconds_and_offsets_stretch_it() {
    let mut rig = Rig::new(NoteEcho::default());
    set(&mut rig.device, echo::DELAY_SIXTEENTHS, 4.0);
    set(&mut rig.device, echo::DELAY, 100.0);
    set(&mut rig.device, echo::FEEDBACK, 0.0);
    let log = rig.run_frames(&[note_on(0, 60, 0.5), note_off(100, 60)], &[], 40_000);
    assert_eq!(log.ons()[1].0, 24_000, "4 sixteenths are a beat");

    let mut rig = Rig::new(NoteEcho::default());
    set(&mut rig.device, echo::SYNC, 0.0);
    set(&mut rig.device, echo::DELAY_TIME, 10.0);
    set(&mut rig.device, echo::DELAY, 100.0);
    set(&mut rig.device, echo::FEEDBACK, 0.0);
    let log = rig.run_frames(&[note_on(0, 60, 0.5), note_off(100, 60)], &[], 2_000);
    assert_eq!(log.ons()[1].0, 480);

    let mut rig = Rig::new(NoteEcho::default());
    set(&mut rig.device, echo::DELAY_SIXTEENTHS, 4.0);
    set(&mut rig.device, echo::DELAY_OFFSET, 25.0);
    set(&mut rig.device, echo::DELAY, 100.0);
    set(&mut rig.device, echo::FEEDBACK, 0.0);
    let log = rig.run_frames(&[note_on(0, 60, 0.5), note_off(100, 60)], &[], 40_000);
    assert_eq!(log.ons()[1].0, 30_000, "a quarter longer");
}

#[test]
fn note_echo_mute_input_keeps_only_the_echo_and_pitch_climbs_each_repeat() {
    let mut rig = Rig::new(NoteEcho::default());
    set(&mut rig.device, echo::DELAY_SIXTEENTHS, 1.0);
    set(&mut rig.device, echo::INPUT, 1.0);
    set(&mut rig.device, echo::PITCH, 12.0);
    set(&mut rig.device, echo::DELAY, 100.0);
    set(&mut rig.device, echo::FEEDBACK, 100.0);
    let log = rig.run_frames(
        &[note_on(0, 60, 0.5), note_off(100, 60)],
        &[],
        SIXTEENTH * 3 + 500,
    );
    assert_eq!(log.keys(), [72, 84, 96]);
    log.assert_balanced("mute");
}

#[test]
fn note_echo_mpe_data_is_echoed_and_decays_when_enabled() {
    let run = |mpe_on: f64| {
        let mut rig = Rig::new(NoteEcho::default());
        set(&mut rig.device, echo::DELAY_SIXTEENTHS, 1.0);
        set(&mut rig.device, echo::DELAY, 100.0);
        set(&mut rig.device, echo::FEEDBACK, 100.0);
        set(&mut rig.device, echo::MPE, mpe_on);
        set(&mut rig.device, echo::PRESS, 50.0);
        let events = [
            note_on_id(0, 60, 0.5, 7),
            expr_event(10, ExprKind::Pressure, 0.8),
            note_off(100, 60),
        ];
        let log = rig.run_frames(&events, &[], SIXTEENTH * 3 + 100);
        expr_values(log, ExprKind::Pressure)
    };
    assert_eq!(run(0.0), [0.8], "only the dry note");
    let echoed = run(1.0);
    assert!(echoed.len() >= 3, "{echoed:?}");
    assert!((echoed[1] - 0.4).abs() < 1e-3, "echo 1 at half: {echoed:?}");
    assert!(
        (echoed[2] - 0.2).abs() < 1e-3,
        "echo 2 at a quarter: {echoed:?}"
    );
}

#[test]
fn note_echo_a_note_held_past_many_repeats_still_ends_them_all() {
    let mut rig = Rig::new(NoteEcho::default());
    set(&mut rig.device, echo::DELAY_SIXTEENTHS, 1.0);
    set(&mut rig.device, echo::DELAY, 100.0);
    set(&mut rig.device, echo::FEEDBACK, 100.0);
    let log = rig.run_frames(
        &[note_on(0, 60, 0.5), note_off(SIXTEENTH * 2 + 3, 60)],
        &[],
        SIXTEENTH * 40,
    );
    log.assert_balanced("long note");
}

// ---- Every device: note safety, determinism, allocation ----

fn all_devices() -> Vec<(&'static str, Box<dyn Processor>)> {
    builtins()
        .iter()
        .filter(|b| b.role == rlive_devices::DeviceRole::MidiEffect && b.visible())
        .map(|b| {
            let mut p = (b.create)(&rlive_devices::DeviceInit::default());
            p.activate(SR, BLOCK);
            (b.name, p)
        })
        .collect()
}

/// Sets a device up so that it generates notes: nothing that would swallow everything.
fn configure_to_generate(name: &str, device: &mut dyn Processor) {
    match name {
        "Chord" => {
            device.set_param(chord::FIRST_SHIFT, 4.0);
            device.set_param(chord::FIRST_SHIFT + 1, 7.0);
            device.set_param(chord::STRUM, 20.0);
        }
        "Arpeggiator" => device.set_param(arp::GATE, 150.0),
        "Pitch" => device.set_param(pitch::PITCH, 7.0),
        "Random" => device.set_param(random::CHANCE, 100.0),
        "Scale" => device.set_param(scale::SCALE_NAME, 0.0),
        "Note Echo" => {
            device.set_param(echo::DELAY_SIXTEENTHS, 1.0);
            device.set_param(echo::DELAY, 100.0);
            device.set_param(echo::FEEDBACK, 100.0);
        }
        _ => {}
    }
}

fn process(
    device: &mut dyn Processor,
    h: &mut Harness,
    log: &mut Log,
    events: &[MidiEvent],
    params: &[ParamEvent],
    blocks: usize,
) {
    let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
    for b in 0..blocks {
        let (ev, pa): (&[MidiEvent], &[ParamEvent]) =
            if b == 0 { (events, params) } else { (&[], &[]) };
        assert_no_alloc(|| h.process(device, &mut l, &mut r, ev, pa));
        h.events_out.sort();
        log.0.extend_from_slice(h.events_out.as_slice());
        h.transport.tick += BLOCK as f64 * h.transport.ticks_per_sample();
        h.transport.sample_pos += BLOCK as u64;
    }
}

fn notes_with_ids() -> Vec<MidiEvent> {
    vec![
        note_on_id(0, 60, 0.7, 101),
        note_on_id(10, 64, 0.7, 102),
        note_on_id(20, 67, 0.7, 103),
    ]
}

fn releases() -> Vec<MidiEvent> {
    vec![
        MidiEvent::new(
            0,
            MidiMessage::NoteOff {
                channel: 0,
                key: 60,
                velocity: 0.5,
                note_id: 101,
            },
        ),
        MidiEvent::new(
            0,
            MidiMessage::NoteOff {
                channel: 0,
                key: 64,
                velocity: 0.5,
                note_id: 102,
            },
        ),
        MidiEvent::new(
            0,
            MidiMessage::NoteOff {
                channel: 0,
                key: 67,
                velocity: 0.5,
                note_id: 103,
            },
        ),
    ]
}

/// Every parameter set to the value it has at the given knob position.
fn all_params(name: &str, position: f64) -> Vec<ParamEvent> {
    let info = builtins().iter().find(|b| b.name == name).unwrap();
    (info.desc)()
        .params
        .iter()
        .map(|p| ParamEvent {
            offset: 7,
            id: p.param_id(),
            value: p.from_normalized(position),
        })
        .collect()
}

#[test]
fn no_device_leaves_a_note_hung_when_keys_are_released() {
    for (name, mut device) in all_devices() {
        configure_to_generate(name, device.as_mut());
        let mut h = Harness::new(SR);
        let mut log = Log::default();
        process(
            device.as_mut(),
            &mut h,
            &mut log,
            &notes_with_ids(),
            &[],
            40,
        );
        process(device.as_mut(), &mut h, &mut log, &releases(), &[], 800);
        device.reset();
        process(device.as_mut(), &mut h, &mut log, &[], &[], 2);
        log.assert_balanced(name);
    }
}

#[test]
fn no_device_leaves_a_note_hung_when_every_parameter_moves_mid_note() {
    for (name, mut device) in all_devices() {
        configure_to_generate(name, device.as_mut());
        let mut h = Harness::new(SR);
        let mut log = Log::default();
        process(
            device.as_mut(),
            &mut h,
            &mut log,
            &notes_with_ids(),
            &[],
            20,
        );
        for position in [0.0, 1.0, 0.5, 0.25] {
            process(
                device.as_mut(),
                &mut h,
                &mut log,
                &[],
                &all_params(name, position),
                10,
            );
        }
        process(device.as_mut(), &mut h, &mut log, &releases(), &[], 1500);
        device.reset();
        process(device.as_mut(), &mut h, &mut log, &[], &[], 2);
        log.assert_balanced(name);
    }
}

#[test]
fn no_device_leaves_a_note_hung_after_reset_in_mid_play() {
    for (name, mut device) in all_devices() {
        configure_to_generate(name, device.as_mut());
        let mut h = Harness::new(SR);
        let mut log = Log::default();
        process(
            device.as_mut(),
            &mut h,
            &mut log,
            &notes_with_ids(),
            &[],
            40,
        );
        device.reset();
        process(device.as_mut(), &mut h, &mut log, &[], &[], 3);
        log.assert_balanced(name);
        // A second reset with nothing sounding must not make anything up.
        device.reset();
        log.0.clear();
        process(device.as_mut(), &mut h, &mut log, &[], &[], 2);
        assert!(log.ons().is_empty(), "{name} played something after reset");
    }
}

#[test]
fn no_device_leaves_a_note_hung_when_the_transport_stops() {
    for (name, mut device) in all_devices() {
        configure_to_generate(name, device.as_mut());
        let mut h = Harness::new(SR);
        let mut log = Log::default();
        process(
            device.as_mut(),
            &mut h,
            &mut log,
            &notes_with_ids(),
            &[],
            40,
        );
        h.transport.playing = false;
        process(device.as_mut(), &mut h, &mut log, &[], &[], 3);
        log.assert_balanced(name);
    }
}

#[test]
fn every_device_handles_note_off_for_a_note_it_never_saw() {
    for (name, mut device) in all_devices() {
        let mut h = Harness::new(SR);
        let mut log = Log::default();
        process(device.as_mut(), &mut h, &mut log, &releases(), &[], 2);
        // No panics and nothing invented: only the note-off (if anything) comes through.
        assert!(log.ons().is_empty(), "{name} made up a note");
    }
}

#[test]
fn generated_note_ids_are_unique_across_chained_devices() {
    let mut a = Chord::with_seed(1);
    let mut b = Chord::with_seed(2);
    set(&mut a, chord::FIRST_SHIFT, 4.0);
    set(&mut b, chord::FIRST_SHIFT, 7.0);
    let mut ha = Harness::new(SR);
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    let input = [note_on_id(0, 60, 0.5, 1)];
    ha.process(&mut a, &mut l, &mut r, &input, &[]);
    let from_a: Vec<MidiEvent> = ha.events_out.as_slice().to_vec();
    let mut hb = Harness::new(SR);
    hb.process(&mut b, &mut l, &mut r, &from_a, &[]);
    let ids: Vec<u32> = hb
        .events_out
        .as_slice()
        .iter()
        .filter_map(|e| match e.message {
            MidiMessage::NoteOn { note_id, .. } => Some(note_id),
            _ => None,
        })
        .collect();
    let mut unique = ids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), ids.len(), "{ids:?}");
}

#[test]
fn every_device_processes_without_allocating_under_heavy_note_traffic() {
    for (name, mut device) in all_devices() {
        configure_to_generate(name, device.as_mut());
        let mut h = Harness::new(SR);
        let mut log = Log::default();
        for i in 0..60u32 {
            let key = (i * 7 % 60 + 40) as u8;
            let events = [note_on_id(i, key, 0.6, 1000 + i)];
            process(
                device.as_mut(),
                &mut h,
                &mut log,
                &events,
                &all_params(name, 0.75),
                2,
            );
            if i % 3 == 0 {
                process(
                    device.as_mut(),
                    &mut h,
                    &mut log,
                    &[note_off(0, key)],
                    &[],
                    1,
                );
            }
        }
        let _ = name;
    }
}
