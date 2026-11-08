//! Per-note events from clips: note ids, release velocity and expression, with the same chase
//! and looping behaviour as the notes themselves.
#![allow(clippy::vec_box)]

use rlive_core::{DeviceId, ExprKind, MidiMessage, NoteId, ProcessContext, Processor, TrackId};
use rlive_engine::*;
use rlive_testkit::assert_no_alloc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

/// Records every event with its absolute frame.
struct Recorder {
    frames: u64,
    log: Arc<Mutex<Vec<(u64, MidiMessage)>>>,
}

impl Processor for Recorder {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        let mut log = self.log.lock().unwrap();
        for e in ctx.events {
            log.push((self.frames + e.offset as u64, e.message));
        }
        self.frames += ctx.audio.left.len() as u64;
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

type Log = Arc<Mutex<Vec<(u64, MidiMessage)>>>;

fn recording_track() -> (TrackId, Box<TrackNode>, Log) {
    let id = TrackId::new();
    let mut t = TrackNode::new(id, BLOCK);
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    t.devices.push(Box::new(DeviceSlot::new(
        DeviceId::new(),
        Box::new(Recorder {
            frames: 0,
            log: log.clone(),
        }),
    )));
    (id, Box::new(t), log)
}

fn note(start: u64, length: u64, key: u8) -> Note {
    Note {
        start,
        length,
        key,
        velocity: 0.8,
        ..Note::default()
    }
}

fn points(points: &[(f64, f32)]) -> Vec<ExprPoint> {
    points
        .iter()
        .map(|&(time, value)| ExprPoint { time, value })
        .collect()
}

fn with_expr(mut n: Note, pitch: &[(f64, f32)], slide: &[(f64, f32)]) -> Note {
    n.expr = Some(Box::new(NoteExpression {
        pitch: points(pitch),
        slide: points(slide),
        pressure: Vec::new(),
    }));
    n
}

fn send(h: &mut EngineHandle, cmd: EngineCommand) {
    assert!(h.send(cmd).is_ok(), "command queue full");
}

fn run(engine: &mut Engine, frames: usize) {
    rlive_testkit::render_chunks(frames, 300, |l, r| engine.process_stereo(l, r));
}

/// An engine playing `clip` from its first slot.
fn launched(clip: Arc<ClipData>) -> (Engine, EngineHandle, Log) {
    let (engine, mut h) = create_engine(SR, BLOCK);
    let (tid, mut track, log) = recording_track();
    track.set_slot(0, Some(clip));
    send(
        &mut h,
        EngineCommand::graph(GraphOp::AddTrack { index: 0, track }),
    );
    send(
        &mut h,
        EngineCommand::Launch(LaunchCmd::LaunchClip {
            track: tid,
            slot: 0,
        }),
    );
    (engine, h, log)
}

fn midi(length: u64, notes: Vec<Note>, looped: bool) -> Arc<ClipData> {
    Arc::new(ClipData::Midi(MidiClip::new(length, notes, looped)))
}

fn note_ons(log: &[(u64, MidiMessage)]) -> Vec<(u64, NoteId)> {
    log.iter()
        .filter_map(|(f, m)| match m {
            MidiMessage::NoteOn { note_id, .. } => Some((*f, *note_id)),
            _ => None,
        })
        .collect()
}

fn note_offs(log: &[(u64, MidiMessage)]) -> Vec<(u64, NoteId, f32)> {
    log.iter()
        .filter_map(|(f, m)| match m {
            MidiMessage::NoteOff {
                note_id, velocity, ..
            } => Some((*f, *note_id, *velocity)),
            _ => None,
        })
        .collect()
}

fn expression(log: &[(u64, MidiMessage)], kind: ExprKind) -> Vec<(u64, NoteId, f32)> {
    log.iter()
        .filter_map(|(f, m)| match *m {
            MidiMessage::NoteExpr {
                note_id,
                kind: k,
                value,
                ..
            } if k == kind => Some((*f, note_id, value)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_note_off_names_the_note_it_ends_when_same_key_notes_overlap() {
    // Note A is 960 ticks and note B starts 240 ticks later on the same key.
    let (mut engine, _h, log) = launched(midi(
        9600,
        vec![note(0, 960, 60), note(240, 960, 60)],
        false,
    ));
    run(&mut engine, 60_000);
    let log = log.lock().unwrap();
    let ons = note_ons(&log);
    let offs = note_offs(&log);
    assert_eq!(ons.len(), 2);
    assert_ne!(ons[0].1, ons[1].1, "each note is its own instance");
    assert_ne!(ons[0].1, 0);
    assert_eq!(offs[0].1, ons[0].1, "the earlier note ends first");
    assert_eq!(offs[1].1, ons[1].1);
}

#[test]
fn notes_shorter_than_a_block_still_get_their_note_off() {
    // At 120 BPM a block of 256 frames is about 10 ticks.
    let (mut engine, _h, log) = launched(midi(960, vec![note(3, 2, 60)], true));
    run(&mut engine, 2_000);
    let log = log.lock().unwrap();
    assert_eq!(note_ons(&log).len(), 1);
    assert_eq!(note_offs(&log).len(), 1, "{log:?}");
}

#[test]
fn a_note_off_carries_the_note_release_velocity() {
    let mut n = note(0, 240, 60);
    n.release = 0.25;
    let (mut engine, _h, log) = launched(midi(9600, vec![n], false));
    run(&mut engine, 20_000);
    let log = log.lock().unwrap();
    assert_eq!(note_offs(&log)[0].2, 0.25);
}

#[test]
fn stopping_releases_with_each_note_s_id_and_release_velocity() {
    let mut n = note(0, 9600, 60);
    n.release = 0.25;
    let (mut engine, mut h, log) = launched(midi(9600, vec![n], false));
    run(&mut engine, 1_000);
    send(&mut h, EngineCommand::Transport(TransportCmd::Stop));
    run(&mut engine, 1_000);
    let log = log.lock().unwrap();
    let offs = note_offs(&log);
    assert_eq!(offs.len(), 1);
    assert_eq!(offs[0].1, note_ons(&log)[0].1);
    assert_eq!(offs[0].2, 0.25);
}

#[test]
fn expression_ramps_on_a_fixed_grid_and_follows_the_note() {
    let n = with_expr(
        note(0, 960, 60),
        &[(0.0, 0.0), (960.0, 12.0)],
        &[(0.0, 0.5)],
    );
    let (mut engine, _h, log) = launched(midi(9600, vec![n], false));
    run(&mut engine, 24_000);
    let log = log.lock().unwrap();
    let id = note_ons(&log)[0].1;

    let pitch = expression(&log, ExprKind::Pitch);
    assert_eq!(pitch.len(), 48, "start plus one value every 20 ticks");
    assert!(pitch.iter().all(|e| e.1 == id));
    assert_eq!(pitch[0].0, 0, "starts with the note");
    assert_eq!(pitch[0].2, 0.0);
    // Halfway through the note the bend is half its range.
    let at_half = pitch.iter().find(|e| e.0 >= 12_000).unwrap();
    assert!((at_half.2 - 6.0).abs() < 0.1, "{at_half:?}");

    let slide = expression(&log, ExprKind::Slide);
    assert_eq!(slide, vec![(0, id, 0.5)], "a flat envelope sends one value");
}

#[test]
fn expression_comes_after_its_note_on() {
    let n = with_expr(note(0, 480, 60), &[(0.0, 1.0)], &[]);
    let (mut engine, _h, log) = launched(midi(9600, vec![n], false));
    run(&mut engine, 300);
    let log = log.lock().unwrap();
    let on = log
        .iter()
        .position(|(_, m)| matches!(m, MidiMessage::NoteOn { .. }))
        .unwrap();
    let expr = log
        .iter()
        .position(|(_, m)| matches!(m, MidiMessage::NoteExpr { .. }))
        .unwrap();
    assert!(on < expr);
}

#[test]
fn looping_restarts_expression_for_each_pass_under_a_new_id() {
    let n = with_expr(note(0, 960, 60), &[(0.0, 0.0), (960.0, 12.0)], &[]);
    let (mut engine, _h, log) = launched(midi(1920, vec![n], true));
    run(&mut engine, 100_000);
    let log = log.lock().unwrap();
    let ons = note_ons(&log);
    assert!(ons.len() >= 2);
    assert_ne!(ons[0].1, ons[1].1, "each pass is a new note");
    let pitch = expression(&log, ExprKind::Pitch);
    let of = |id| pitch.iter().filter(|e| e.1 == id).collect::<Vec<_>>();
    let (a, b) = (of(ons[0].1), of(ons[1].1));
    assert_eq!(a.len(), b.len());
    assert_eq!(a[0].2, 0.0);
    assert_eq!(b[0].2, 0.0, "the second pass starts from zero again");
    assert!(b.last().unwrap().0 > ons[1].0);
}

/// An engine playing `clip` from the Arrangement, not started.
fn on_timeline(clip: Arc<ClipData>) -> (Engine, EngineHandle, Log) {
    let (engine, mut h) = create_engine(SR, BLOCK);
    let (_, mut track, log) = recording_track();
    track.set_lane(Arc::new(ArrangementLane {
        clips: vec![LaneClip {
            start: 0,
            end: 9600,
            offset: 0,
            data: clip,
        }],
    }));
    send(
        &mut h,
        EngineCommand::graph(GraphOp::AddTrack { index: 0, track }),
    );
    (engine, h, log)
}

#[test]
fn chasing_into_a_note_starts_its_expression_where_it_stands() {
    let n = with_expr(
        note(0, 960, 60),
        &[(0.0, 0.0), (960.0, 12.0)],
        &[(0.0, 0.5)],
    );
    let (mut engine, mut h, log) = on_timeline(midi(9600, vec![n], true));
    send(&mut h, EngineCommand::Transport(TransportCmd::Seek(480.0)));
    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    run(&mut engine, 2_000);
    let log = log.lock().unwrap();
    let ons = note_ons(&log);
    assert_eq!(ons.len(), 1);
    let pitch = expression(&log, ExprKind::Pitch);
    assert_eq!(pitch[0].0, ons[0].0, "arrives with the chased note");
    assert_eq!(pitch[0].1, ons[0].1);
    assert!((pitch[0].2 - 6.0).abs() < 0.1, "{:?}", pitch[0]);
    assert_eq!(expression(&log, ExprKind::Slide)[0].2, 0.5);
}

/// Counts expression events without touching the heap.
struct ExprCounter(Arc<AtomicUsize>);

impl Processor for ExprCounter {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        let n = ctx
            .events
            .iter()
            .filter(|e| matches!(e.message, MidiMessage::NoteExpr { .. }))
            .count();
        self.0.fetch_add(n, Ordering::Relaxed);
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

#[test]
fn clip_expression_never_allocates_on_the_audio_thread() {
    let notes = (0..32u8)
        .map(|i| {
            with_expr(
                note(i as u64 * 20, 5000, 60 + i),
                &[(0.0, 0.0), (5000.0, 2.0)],
                &[(0.0, 0.2), (5000.0, 0.9)],
            )
        })
        .collect();
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let id = TrackId::new();
    let mut track = TrackNode::new(id, BLOCK);
    let count = Arc::new(AtomicUsize::new(0));
    track.devices.push(Box::new(DeviceSlot::new(
        DeviceId::new(),
        Box::new(ExprCounter(count.clone())),
    )));
    track.set_slot(0, Some(midi(9600, notes, true)));
    send(
        &mut h,
        EngineCommand::graph(GraphOp::AddTrack {
            index: 0,
            track: Box::new(track),
        }),
    );
    send(
        &mut h,
        EngineCommand::Launch(LaunchCmd::LaunchClip { track: id, slot: 0 }),
    );
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    engine.process_stereo(&mut l, &mut r);
    assert_no_alloc(|| {
        for _ in 0..200 {
            engine.process_stereo(&mut l, &mut r);
        }
    });
    assert!(
        count.load(Ordering::Relaxed) > 0,
        "expression reached the device"
    );
}
