use rlive_core::{DeviceId, MidiMessage, ProcessContext, Processor, Quantize, TrackId};
use rlive_engine::*;
use std::sync::{Arc, Mutex};

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

/// Records note events with their absolute sample position.
struct Recorder {
    log: Arc<Mutex<Vec<(u64, MidiMessage)>>>,
}

impl Processor for Recorder {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        let mut log = self.log.lock().unwrap();
        for e in ctx.events {
            log.push((ctx.transport.sample_pos + e.offset as u64, e.message));
        }
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

/// Adds a constant value to the buffer (a "DC instrument").
struct Dc(f32);

impl Processor for Dc {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        for s in ctx.audio.left.iter_mut().chain(ctx.audio.right.iter_mut()) {
            *s += self.0;
        }
    }
    fn set_param(&mut self, id: u32, v: f64) {
        if id == 0 {
            self.0 = v as f32;
        }
    }
}

fn midi_clip(length: u64, notes: &[(u64, u64, u8)]) -> Arc<ClipData> {
    Arc::new(ClipData::Midi(MidiClip {
        length,
        looped: true,
        notes: notes
            .iter()
            .map(|&(start, length, key)| Note {
                start,
                length,
                key,
                velocity: 0.8,
            })
            .collect(),
    }))
}

fn run(engine: &mut Engine, frames: usize) -> (Vec<f32>, Vec<f32>) {
    let mut l = vec![0.0; frames];
    let mut r = vec![0.0; frames];
    // Use an odd host buffer size to exercise block splitting.
    for (cl, cr) in l.chunks_mut(300).zip(r.chunks_mut(300)) {
        engine.process(cl, cr);
    }
    (l, r)
}

fn track_with(device: Box<dyn Processor>) -> (TrackId, Box<TrackNode>) {
    let id = TrackId::new();
    let mut t = TrackNode::new(id, BLOCK);
    t.devices
        .push(Box::new(DeviceSlot::new(DeviceId::new(), device)));
    (id, Box::new(t))
}

#[test]
fn quantized_launch_switches_on_next_bar() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let log = Arc::new(Mutex::new(Vec::new()));
    let (tid, mut track) = track_with(Box::new(Recorder { log: log.clone() }));
    // Clip 0: one long note; clip 1: note at its start.
    track.set_slot(0, Some(midi_clip(3840, &[(0, 3840, 60)])));
    track.set_slot(1, Some(midi_clip(3840, &[(0, 480, 72)])));
    h.send(EngineCommand::AddTrack { index: 0, track })
        .ok()
        .unwrap();
    h.send(EngineCommand::SetQuantize(Quantize::Bar))
        .ok()
        .unwrap();
    h.send(EngineCommand::LaunchClip {
        track: tid,
        slot: 0,
    })
    .ok()
    .unwrap();
    run(&mut engine, 10_000);
    h.send(EngineCommand::LaunchClip {
        track: tid,
        slot: 1,
    })
    .ok()
    .unwrap();
    run(&mut engine, 100_000);

    let log = log.lock().unwrap();
    // 120 BPM, 4/4 → 1 bar = 2 s = 96000 samples.
    let on60 = log
        .iter()
        .find(|(_, m)| matches!(m, MidiMessage::NoteOn { key: 60, .. }));
    assert_eq!(on60.map(|e| e.0), Some(0));
    let off60 = log
        .iter()
        .find(|(_, m)| matches!(m, MidiMessage::NoteOff { key: 60, .. }))
        .expect("note-off for the first clip");
    let on72 = log
        .iter()
        .find(|(_, m)| matches!(m, MidiMessage::NoteOn { key: 72, .. }))
        .expect("second clip started");
    assert_eq!(on72.0, 96_000);
    assert_eq!(off60.0, 96_000);
}

#[test]
fn loop_wrap_sends_off_before_on() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let log = Arc::new(Mutex::new(Vec::new()));
    let (tid, mut track) = track_with(Box::new(Recorder { log: log.clone() }));
    // One-beat clip, note fills the whole beat.
    track.set_slot(0, Some(midi_clip(960, &[(0, 960, 64)])));
    h.send(EngineCommand::AddTrack { index: 0, track })
        .ok()
        .unwrap();
    h.send(EngineCommand::LaunchClip {
        track: tid,
        slot: 0,
    })
    .ok()
    .unwrap();
    run(&mut engine, 50_000);
    let log = log.lock().unwrap();
    // One beat at 120 BPM = 24000 samples.
    let at_24k: Vec<_> = log.iter().filter(|(t, _)| *t == 24_000).collect();
    assert_eq!(at_24k.len(), 2, "{log:?}");
    assert!(matches!(at_24k[0].1, MidiMessage::NoteOff { key: 64, .. }));
    assert!(matches!(at_24k[1].1, MidiMessage::NoteOn { key: 64, .. }));
    let ons = log
        .iter()
        .filter(|(_, m)| matches!(m, MidiMessage::NoteOn { .. }))
        .count();
    assert_eq!(ons, 3); // at 0, 24000, 48000
}

#[test]
fn stop_releases_notes() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let log = Arc::new(Mutex::new(Vec::new()));
    let (tid, mut track) = track_with(Box::new(Recorder { log: log.clone() }));
    track.set_slot(0, Some(midi_clip(3840, &[(0, 3840, 60)])));
    h.send(EngineCommand::AddTrack { index: 0, track })
        .ok()
        .unwrap();
    h.send(EngineCommand::LaunchClip {
        track: tid,
        slot: 0,
    })
    .ok()
    .unwrap();
    run(&mut engine, 1000);
    h.send(EngineCommand::Stop).ok().unwrap();
    run(&mut engine, 1000);
    let log = log.lock().unwrap();
    assert!(log
        .iter()
        .any(|(_, m)| matches!(m, MidiMessage::NoteOff { key: 60, .. })));
}

#[test]
fn mute_and_solo() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (a, ta) = track_with(Box::new(Dc(0.25)));
    let (b, tb) = track_with(Box::new(Dc(0.5)));
    h.send(EngineCommand::AddTrack {
        index: 0,
        track: ta,
    })
    .ok()
    .unwrap();
    h.send(EngineCommand::AddTrack {
        index: 1,
        track: tb,
    })
    .ok()
    .unwrap();
    let (l, _) = run(&mut engine, 4800);
    assert!((l[4799] - 0.75).abs() < 1e-3, "{}", l[4799]);

    h.send(EngineCommand::SetMix {
        track: b,
        mix: MixSettings {
            solo: true,
            ..Default::default()
        },
    })
    .ok()
    .unwrap();
    let (l, _) = run(&mut engine, 4800);
    assert!((l[4799] - 0.5).abs() < 1e-3, "{}", l[4799]);

    h.send(EngineCommand::SetMix {
        track: b,
        mix: MixSettings {
            mute: true,
            solo: false,
            ..Default::default()
        },
    })
    .ok()
    .unwrap();
    let (l, r) = run(&mut engine, 4800);
    assert!((l[4799] - 0.25).abs() < 1e-3);
    assert!((r[4799] - 0.25).abs() < 1e-3);
    let _ = a;
}

#[test]
fn pan_hard_left() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (a, ta) = track_with(Box::new(Dc(0.5)));
    h.send(EngineCommand::AddTrack {
        index: 0,
        track: ta,
    })
    .ok()
    .unwrap();
    h.send(EngineCommand::SetMix {
        track: a,
        mix: MixSettings {
            pan: -1.0,
            ..Default::default()
        },
    })
    .ok()
    .unwrap();
    let (l, r) = run(&mut engine, 4800);
    assert!((l[4799] - 0.5).abs() < 1e-3);
    assert!(r[4799].abs() < 1e-3);
}

#[test]
fn removed_devices_come_back_as_garbage() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let dev = DeviceId::new();
    let tid = TrackId::new();
    let track = Box::new(TrackNode::new(tid, BLOCK));
    h.send(EngineCommand::AddTrack { index: 0, track })
        .ok()
        .unwrap();
    h.send(EngineCommand::InsertDevice {
        target: ChainTarget::Track(tid),
        index: 0,
        device: Box::new(DeviceSlot::new(dev, Box::new(Dc(0.1)))),
    })
    .ok()
    .unwrap();
    let (l, _) = run(&mut engine, 2400);
    assert!(l[2399] > 0.09);
    h.send(EngineCommand::SetParam {
        target: ChainTarget::Track(tid),
        device: dev,
        param: 0,
        value: 0.2,
    })
    .ok()
    .unwrap();
    let (l, _) = run(&mut engine, 2400);
    assert!((l[2399] - 0.2).abs() < 1e-3);
    h.send(EngineCommand::RemoveDevice {
        target: ChainTarget::Track(tid),
        device: dev,
    })
    .ok()
    .unwrap();
    let (l, _) = run(&mut engine, 2400);
    assert!(l[2399].abs() < 1e-6);
    assert_eq!(h.collect_garbage(), 1);
    h.send(EngineCommand::RemoveTrack(tid)).ok().unwrap();
    run(&mut engine, 256);
    assert_eq!(h.collect_garbage(), 1);
}

#[test]
fn audio_clip_plays_and_loops() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let tid = TrackId::new();
    let mut track = TrackNode::new(tid, BLOCK);
    // 1 beat of constant 0.5 at 48 kHz, 120 BPM.
    let clip = AudioClip {
        left: vec![0.5; 24_000],
        right: vec![0.5; 24_000],
        sample_rate: SR,
        length: 960,
        looped: true,
        gain: 1.0,
    };
    track.set_slot(0, Some(Arc::new(ClipData::Audio(clip))));
    h.send(EngineCommand::AddTrack {
        index: 0,
        track: Box::new(track),
    })
    .ok()
    .unwrap();
    h.send(EngineCommand::LaunchClip {
        track: tid,
        slot: 0,
    })
    .ok()
    .unwrap();
    let (l, _) = run(&mut engine, 60_000);
    assert!((l[10_000] - 0.5).abs() < 1e-3);
    // Still playing after the loop point.
    assert!((l[30_000] - 0.5).abs() < 1e-3);
    assert!(h.shared.playing.load(std::sync::atomic::Ordering::Relaxed));
}

#[test]
fn metronome_clicks_when_playing() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    h.send(EngineCommand::SetMetronome(true)).ok().unwrap();
    let (l, _) = run(&mut engine, 4800);
    assert!(l.iter().all(|s| *s == 0.0), "silent while stopped");
    h.send(EngineCommand::Play).ok().unwrap();
    let (l, _) = run(&mut engine, 4800);
    assert!(l.iter().any(|s| s.abs() > 0.05));
}

#[test]
fn replace_all_swaps_session() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (_, ta) = track_with(Box::new(Dc(0.25)));
    h.send(EngineCommand::AddTrack {
        index: 0,
        track: ta,
    })
    .ok()
    .unwrap();
    run(&mut engine, 2400);
    let (_, tb) = track_with(Box::new(Dc(0.5)));
    let mut tracks = Vec::with_capacity(MAX_TRACKS);
    tracks.push(tb);
    h.send(EngineCommand::ReplaceAll {
        tracks: Box::new(tracks),
        master: Box::new(MasterNode::new(BLOCK)),
    })
    .ok()
    .unwrap();
    let (l, _) = run(&mut engine, 2400);
    assert!((l[2399] - 0.5).abs() < 1e-3);
    assert_eq!(h.collect_garbage(), 2);
}

#[test]
fn midi_sender_reaches_track() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let log = Arc::new(Mutex::new(Vec::new()));
    let (tid, track) = track_with(Box::new(Recorder { log: log.clone() }));
    h.send(EngineCommand::AddTrack { index: 0, track })
        .ok()
        .unwrap();
    let mut midi = h.take_midi_sender().unwrap();
    assert!(h.take_midi_sender().is_none());
    run(&mut engine, 256);
    assert!(midi.send(
        tid,
        MidiMessage::NoteOn {
            channel: 0,
            key: 61,
            velocity: 0.5
        }
    ));
    run(&mut engine, 256);
    assert!(log
        .lock()
        .unwrap()
        .iter()
        .any(|(_, m)| matches!(m, MidiMessage::NoteOn { key: 61, .. })));
}
