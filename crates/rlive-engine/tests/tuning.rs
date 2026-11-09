//! Tuning systems in the engine: what instruments hear, what MIDI effects don't, controller
//! layouts, and MPE on the way out of a track.
#![allow(clippy::vec_box)]

use rlive_core::{
    DeviceId, ExprKind, KeyLayout, MidiMessage, MpeEncoder, MpeOutput, MpeZone, ProcessContext,
    Processor, TrackId, TuningTable,
};
use rlive_engine::*;
use rlive_testkit::assert_no_alloc;
use std::sync::{Arc, Mutex};
use std::time::Instant;

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

type Log = Arc<Mutex<Vec<MidiMessage>>>;

struct Recorder(Log);

impl Processor for Recorder {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        self.0
            .lock()
            .unwrap()
            .extend(ctx.events.iter().map(|e| e.message));
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

/// Passes MIDI on, one note up.
struct Transposer;

impl Processor for Transposer {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        for e in ctx.events {
            let mut e = *e;
            if let MidiMessage::NoteOn { key, .. } = &mut e.message {
                *key += 1;
            }
            ctx.events_out.push(e);
        }
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

fn tuned_track() -> (TrackId, Box<TrackNode>, Log) {
    let id = TrackId::new();
    let mut t = TrackNode::new(id, BLOCK);
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    t.devices.push(Box::new(DeviceSlot::new(
        DeviceId::new(),
        Box::new(Recorder(log.clone())),
    )));
    (id, Box::new(t), log)
}

/// Hears everything and keeps none of it.
struct Quiet;

impl Processor for Quiet {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, _: &mut ProcessContext) {}
    fn set_param(&mut self, _: u32, _: f64) {}
}

fn engine_with(track: Box<TrackNode>) -> (Engine, EngineHandle) {
    let (engine, mut h) = create_engine(SR, BLOCK);
    assert!(h
        .send(EngineCommand::graph(GraphOp::AddTrack { index: 0, track }))
        .is_ok());
    (engine, h)
}

fn run(engine: &mut Engine, frames: usize) {
    rlive_testkit::render_chunks(frames, 128, |l, r| engine.process_stereo(l, r));
}

/// Each key sits a semitone sharp.
fn sharp() -> Arc<TuningTable> {
    Arc::new(TuningTable::from_hz(|k| {
        440.0 * 2f64.powf((f64::from(k) - 68.0) / 12.0)
    }))
}

fn on(key: u8) -> MidiMessage {
    MidiMessage::NoteOn {
        channel: 0,
        key,
        velocity: 1.0,
        note_id: 1,
        tuning: 0.0,
    }
}

fn pitch(key: u8, value: f32) -> MidiMessage {
    MidiMessage::NoteExpr {
        channel: 0,
        key,
        note_id: 1,
        kind: ExprKind::Pitch,
        value,
    }
}

fn set_tuning(
    h: &mut EngineHandle,
    track: TrackId,
    table: Arc<TuningTable>,
    active: bool,
    layout: Option<Arc<KeyLayout>>,
) {
    assert!(h
        .send(EngineCommand::Tuning(TuningCmd::SetTrack {
            track,
            table,
            active,
            layout,
        }))
        .is_ok());
}

fn send_midi(h: &mut EngineHandle, track: TrackId, message: MidiMessage) {
    assert!(h
        .send(EngineCommand::Record(RecordCmd::PlayedMidi {
            track,
            message,
            at: Instant::now(),
        }))
        .is_ok());
}

#[test]
fn an_instrument_hears_each_notes_tuning_in_notes_and_pitch() {
    let (track_id, track, log) = tuned_track();
    let (mut e, mut h) = engine_with(track);
    set_tuning(&mut h, track_id, sharp(), true, None);
    send_midi(&mut h, track_id, on(60));
    send_midi(&mut h, track_id, pitch(60, 2.0));
    run(&mut e, 512);
    let got = log.lock().unwrap().clone();
    assert!(
        matches!(got[0], MidiMessage::NoteOn { tuning, .. } if (tuning - 1.0).abs() < 1e-3),
        "{got:?}"
    );
    assert!(
        matches!(got[1], MidiMessage::NoteExpr { value, .. } if (value - 3.0).abs() < 1e-3),
        "{got:?}"
    );
}

#[test]
fn a_bypassed_track_hears_nothing_of_the_tuning() {
    let (track_id, track, log) = tuned_track();
    let (mut e, mut h) = engine_with(track);
    set_tuning(&mut h, track_id, sharp(), false, None);
    send_midi(&mut h, track_id, on(60));
    run(&mut e, 512);
    assert_eq!(log.lock().unwrap()[0], on(60));
}

#[test]
fn midi_effects_work_in_notes_and_the_instrument_is_tuned_after_them() {
    let (track_id, mut track, log) = tuned_track();
    let mut effect = DeviceSlot::new(DeviceId::new(), Box::new(Transposer));
    effect.midi_effect = true;
    track.devices.insert(0, Box::new(effect));
    let (mut e, mut h) = engine_with(track);
    // Only the key the effect sends is sharp: the key that went in is not.
    let table = Arc::new(TuningTable::from_hz(|k| {
        let up = if k == 61 { 1.0 } else { 0.0 };
        440.0 * 2f64.powf((f64::from(k) + up - 69.0) / 12.0)
    }));
    set_tuning(&mut h, track_id, table, true, None);
    send_midi(&mut h, track_id, on(60));
    run(&mut e, 512);
    let got = log.lock().unwrap().clone();
    assert!(
        matches!(got[0], MidiMessage::NoteOn { key: 61, tuning, .. } if (tuning - 1.0).abs() < 1e-3),
        "{got:?}"
    );
}

#[test]
fn a_controller_layout_moves_keys_before_they_are_recorded() {
    let (track_id, track, log) = tuned_track();
    let (mut e, mut h) = engine_with(track);
    let layout = Arc::new(KeyLayout::new(|k| (k != 40).then_some(k + 2)));
    set_tuning(
        &mut h,
        track_id,
        Arc::new(TuningTable::default()),
        false,
        Some(layout),
    );
    send_midi(&mut h, track_id, on(60));
    send_midi(&mut h, track_id, on(40));
    run(&mut e, 512);
    let got = log.lock().unwrap().clone();
    assert_eq!(got.len(), 1, "key 40 plays nothing");
    assert!(matches!(got[0], MidiMessage::NoteOn { key: 62, .. }));
}

#[test]
fn played_expression_is_reported_for_recording() {
    let (track_id, track, _) = tuned_track();
    let (mut e, mut h) = engine_with(track);
    assert!(h
        .send(EngineCommand::Record(RecordCmd::SetRecording(true)))
        .is_ok());
    assert!(h.send(EngineCommand::Transport(TransportCmd::Play)).is_ok());
    run(&mut e, 512);
    send_midi(&mut h, track_id, on(60));
    send_midi(&mut h, track_id, pitch(60, 3.0));
    run(&mut e, 512);
    let mut seen_pitch = false;
    while let Some(ev) = h.poll_event() {
        if let EngineEvent::RecordedMidi { message, .. } = ev {
            seen_pitch |= matches!(message, MidiMessage::NoteExpr { .. });
        }
    }
    assert!(seen_pitch);
}

#[test]
fn hardware_output_gets_zoned_channels_and_tuning_as_bend() {
    let (track_id, track, _) = tuned_track();
    let (mut e, mut h) = engine_with(track);
    let mut hw = h.take_hw_midi().unwrap();
    assert!(h
        .send(EngineCommand::Tuning(TuningCmd::SetOutput {
            track: track_id,
            port: Some(3),
            encoder: Some(Box::new(MpeEncoder::configured(MpeOutput::Zone {
                zone: MpeZone::Upper,
                members: 4,
            }))),
        }))
        .is_ok());
    set_tuning(&mut h, track_id, sharp(), true, None);
    run(&mut e, 256);
    send_midi(&mut h, track_id, on(60));
    run(&mut e, 512);
    let mut out = Vec::new();
    while let Ok(m) = hw.pop() {
        assert_eq!(m.port, 3);
        out.push(m.message);
    }
    assert!(
        matches!(
            out[2],
            MidiMessage::ControlChange {
                channel: 15,
                cc: 6,
                value: 4
            }
        ),
        "{out:?}"
    );
    assert!(
        matches!(out[3], MidiMessage::PitchBend { channel: 14, value } if (value - 1.0 / 48.0).abs() < 1e-3),
        "{out:?}"
    );
    assert!(matches!(out[4], MidiMessage::NoteOn { channel: 14, .. }));
}

#[test]
fn tuned_tracks_never_allocate() {
    let track_id = TrackId::new();
    let mut track = Box::new(TrackNode::new(track_id, BLOCK));
    track
        .devices
        .push(Box::new(DeviceSlot::new(DeviceId::new(), Box::new(Quiet))));
    let (mut e, mut h) = engine_with(track);
    set_tuning(&mut h, track_id, sharp(), true, None);
    run(&mut e, 512);
    assert_no_alloc(|| {
        for _ in 0..200 {
            let (mut l, mut r) = ([0.0f32; 128], [0.0f32; 128]);
            send_midi(&mut h, track_id, on(60));
            send_midi(&mut h, track_id, pitch(60, 1.0));
            e.process_stereo(&mut l, &mut r);
        }
    });
}
