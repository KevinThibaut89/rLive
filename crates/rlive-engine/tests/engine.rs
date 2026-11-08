use rlive_core::{
    DeviceId, MidiMessage, ProcessContext, Processor, Quantize, SampleBuffer, ScaleInfo, TrackId,
};
use rlive_engine::*;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

/// Records events and block starts by absolute frame, which keeps counting across jumps.
#[derive(Default)]
struct Timeline {
    notes: Vec<(u64, MidiMessage)>,
    /// (absolute frame, transport tick) at the start of each block.
    blocks: Vec<(u64, f64)>,
}

struct FrameRecorder {
    frames: u64,
    log: Arc<Mutex<Timeline>>,
}

impl FrameRecorder {
    fn new() -> (Self, Arc<Mutex<Timeline>>) {
        let log = Arc::new(Mutex::new(Timeline::default()));
        (
            Self {
                frames: 0,
                log: log.clone(),
            },
            log,
        )
    }
}

impl Processor for FrameRecorder {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        let mut log = self.log.lock().unwrap();
        log.blocks.push((self.frames, ctx.transport.tick));
        for e in ctx.events {
            log.notes.push((self.frames + e.offset as u64, e.message));
        }
        self.frames += ctx.audio.left.len() as u64;
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

fn ons(log: &Timeline, key: u8) -> Vec<u64> {
    log.notes
        .iter()
        .filter(|(_, m)| matches!(m, MidiMessage::NoteOn { key: k, .. } if *k == key))
        .map(|(f, _)| *f)
        .collect()
}

fn offs(log: &Timeline, key: u8) -> Vec<u64> {
    log.notes
        .iter()
        .filter(|(_, m)| matches!(m, MidiMessage::NoteOff { key: k, .. } if *k == key))
        .map(|(f, _)| *f)
        .collect()
}

/// Send a command, panicking if the queue is full.
fn send(h: &mut EngineHandle, cmd: EngineCommand) {
    assert!(h.send(cmd).is_ok(), "command queue full");
}

/// Set a mixer control or other cell.
fn set_cell(h: &mut EngineHandle, track: TrackId, cell: CellIx, value: f64) {
    send(
        h,
        EngineCommand::Param(ParamCmd::Set {
            track: TrackRef::Track(track),
            cell,
            value,
            gesture: true,
        }),
    );
}

fn tick(h: &EngineHandle) -> f64 {
    h.shared.tick.load()
}

/// Adds a constant value to the buffer (a "DC instrument").
struct Dc(f32);

impl Processor for Dc {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        if let Some(e) = ctx.params.iter().rfind(|e| e.id.0 == 0) {
            self.0 = e.value as f32;
        }
        for s in ctx.audio.left.iter_mut().chain(ctx.audio.right.iter_mut()) {
            *s += self.0;
        }
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

fn midi_clip(length: u64, notes: &[(u64, u64, u8)]) -> Arc<ClipData> {
    let notes = notes
        .iter()
        .map(|&(start, length, key)| Note {
            start,
            length,
            key,
            velocity: 0.8,
            ..Note::default()
        })
        .collect();
    Arc::new(ClipData::Midi(MidiClip::new(length, notes, true)))
}

fn sample(left: Vec<f32>) -> Arc<SampleBuffer> {
    Arc::new(SampleBuffer {
        name: "test".into(),
        right: left.clone(),
        left,
        sample_rate: SR,
    })
}

fn run(engine: &mut Engine, frames: usize) -> (Vec<f32>, Vec<f32>) {
    // Use an odd host buffer size to exercise block splitting.
    rlive_testkit::render_chunks(frames, 300, |l, r| engine.process_stereo(l, r))
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
    h.send(EngineCommand::graph(GraphOp::AddTrack { index: 0, track }))
        .ok()
        .unwrap();
    h.send(EngineCommand::Transport(TransportCmd::SetQuantize(
        Quantize::Bar,
    )))
    .ok()
    .unwrap();
    h.send(EngineCommand::Launch(LaunchCmd::LaunchClip {
        track: tid,
        slot: 0,
    }))
    .ok()
    .unwrap();
    run(&mut engine, 10_000);
    h.send(EngineCommand::Launch(LaunchCmd::LaunchClip {
        track: tid,
        slot: 1,
    }))
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
    h.send(EngineCommand::graph(GraphOp::AddTrack { index: 0, track }))
        .ok()
        .unwrap();
    h.send(EngineCommand::Launch(LaunchCmd::LaunchClip {
        track: tid,
        slot: 0,
    }))
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
    h.send(EngineCommand::graph(GraphOp::AddTrack { index: 0, track }))
        .ok()
        .unwrap();
    h.send(EngineCommand::Launch(LaunchCmd::LaunchClip {
        track: tid,
        slot: 0,
    }))
    .ok()
    .unwrap();
    run(&mut engine, 1000);
    h.send(EngineCommand::Transport(TransportCmd::Stop))
        .ok()
        .unwrap();
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
    h.send(EngineCommand::graph(GraphOp::AddTrack {
        index: 0,
        track: ta,
    }))
    .ok()
    .unwrap();
    h.send(EngineCommand::graph(GraphOp::AddTrack {
        index: 1,
        track: tb,
    }))
    .ok()
    .unwrap();
    let (l, _) = run(&mut engine, 4800);
    assert!((l[4799] - 0.75).abs() < 1e-3, "{}", l[4799]);

    set_cell(&mut h, b, mix_cell::SOLO, 1.0);
    let (l, _) = run(&mut engine, 4800);
    assert!((l[4799] - 0.5).abs() < 1e-3, "{}", l[4799]);

    set_cell(&mut h, b, mix_cell::MUTE, 1.0);
    set_cell(&mut h, b, mix_cell::SOLO, 0.0);
    let (l, r) = run(&mut engine, 4800);
    assert!((l[4799] - 0.25).abs() < 1e-3);
    assert!((r[4799] - 0.25).abs() < 1e-3);
    let _ = a;
}

#[test]
fn pan_hard_left() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (a, ta) = track_with(Box::new(Dc(0.5)));
    h.send(EngineCommand::graph(GraphOp::AddTrack {
        index: 0,
        track: ta,
    }))
    .ok()
    .unwrap();
    set_cell(&mut h, a, mix_cell::PAN, -1.0);
    let (l, r) = run(&mut engine, 4800);
    assert!((l[4799] - 0.5).abs() < 1e-3);
    assert!(r[4799].abs() < 1e-3);
}

#[test]
fn removed_devices_come_back_as_garbage() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let dev = DeviceId::new();
    let tid = TrackId::new();
    let mut track = Box::new(TrackNode::new(tid, BLOCK));
    let mut layout = BusLayout::for_track(&MixSettings::default());
    let level = rlive_core::ParamInfo::continuous(0, "Level", 0.0, 1.0, 0.1, "");
    layout.place_device(dev, true, std::slice::from_ref(&level), |p| p.default);
    let cell = layout.device_cell(dev, 0).unwrap();
    track.bus = layout.build();
    h.send(EngineCommand::graph(GraphOp::AddTrack { index: 0, track }))
        .ok()
        .unwrap();
    h.send(EngineCommand::graph(GraphOp::InsertDevice {
        target: ChainAddr::track(tid),
        index: 0,
        device: Box::new(DeviceSlot::new(dev, Box::new(Dc(0.1)))),
    }))
    .ok()
    .unwrap();
    let (l, _) = run(&mut engine, 2400);
    assert!(l[2399] > 0.09);
    set_cell(&mut h, tid, cell, 0.2);
    let (l, _) = run(&mut engine, 2400);
    assert!((l[2399] - 0.2).abs() < 1e-3);
    h.send(EngineCommand::graph(GraphOp::RemoveDevice {
        target: ChainAddr::track(tid),
        device: dev,
    }))
    .ok()
    .unwrap();
    let (l, _) = run(&mut engine, 2400);
    assert!(l[2399].abs() < 1e-6);
    assert_eq!(h.collect_garbage(), 1);
    h.send(EngineCommand::graph(GraphOp::RemoveTrack(tid)))
        .ok()
        .unwrap();
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
        sample: sample(vec![0.5; 24_000]),
        length: 960,
        looped: true,
        gain: 1.0,
        scale: None,
    };
    track.set_slot(0, Some(Arc::new(ClipData::Audio(clip))));
    h.send(EngineCommand::graph(GraphOp::AddTrack {
        index: 0,
        track: Box::new(track),
    }))
    .ok()
    .unwrap();
    h.send(EngineCommand::Launch(LaunchCmd::LaunchClip {
        track: tid,
        slot: 0,
    }))
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
    h.send(EngineCommand::Transport(TransportCmd::SetMetronome(true)))
        .ok()
        .unwrap();
    let (l, _) = run(&mut engine, 4800);
    assert!(l.iter().all(|s| *s == 0.0), "silent while stopped");
    h.send(EngineCommand::Transport(TransportCmd::Play))
        .ok()
        .unwrap();
    let (l, _) = run(&mut engine, 4800);
    assert!(l.iter().any(|s| s.abs() > 0.05));
}

#[test]
fn replace_all_swaps_session() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (_, ta) = track_with(Box::new(Dc(0.25)));
    h.send(EngineCommand::graph(GraphOp::AddTrack {
        index: 0,
        track: ta,
    }))
    .ok()
    .unwrap();
    run(&mut engine, 2400);
    let (_, tb) = track_with(Box::new(Dc(0.5)));
    let mut tracks = Vec::with_capacity(MAX_TRACKS);
    tracks.push(tb);
    h.send(EngineCommand::graph(GraphOp::ReplaceAll {
        tracks: Box::new(tracks),
        master: Box::new(TrackNode::master(BLOCK)),
    }))
    .ok()
    .unwrap();
    let (l, _) = run(&mut engine, 2400);
    assert!((l[2399] - 0.5).abs() < 1e-3);
    assert_eq!(h.collect_garbage(), 2);
}

#[test]
fn tempo_change_during_audio_is_seamless() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let tid = TrackId::new();
    let mut track = TrackNode::new(tid, BLOCK);
    // A slow ramp: any jump of the read position shows up as a step between samples.
    let frames = 192_000;
    let step = 1.0 / frames as f32;
    let clip = AudioClip {
        sample: sample((0..frames).map(|i| i as f32 * step).collect()),
        length: 3840 * 2,
        looped: false,
        gain: 1.0,
        scale: None,
    };
    track.set_slot(0, Some(Arc::new(ClipData::Audio(clip))));
    h.send(EngineCommand::graph(GraphOp::AddTrack {
        index: 0,
        track: Box::new(track),
    }))
    .ok()
    .unwrap();
    h.send(EngineCommand::Transport(TransportCmd::SetQuantize(
        Quantize::None,
    )))
    .ok()
    .unwrap();
    h.send(EngineCommand::Launch(LaunchCmd::LaunchClip {
        track: tid,
        slot: 0,
    }))
    .ok()
    .unwrap();
    let mut out = run(&mut engine, 10_000).0;
    for tempo in [97.0, 143.5, 120.0] {
        h.send(EngineCommand::Transport(TransportCmd::SetTempo(tempo)))
            .ok()
            .unwrap();
        out.extend(run(&mut engine, 10_000).0);
    }
    assert!(out[0] < 1e-3);
    for pair in out.windows(2) {
        assert!(
            (pair[1] - pair[0] - step).abs() < step * 0.01,
            "read position jumped: {pair:?}"
        );
    }
}

/// 120 BPM at 48 kHz.
const TPS: f64 = 0.04;

/// A long note followed by many short ones: finding it needs the `max_len` lookback.
fn clip_with_long_note() -> Arc<ClipData> {
    let mut notes = vec![(0, 2000, 60)];
    notes.extend((1..100).map(|i| (i * 16, 8, 70)));
    midi_clip(3840, &notes)
}

#[test]
fn long_note_ends_past_later_short_notes() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (rec, log) = FrameRecorder::new();
    let (tid, mut track) = track_with(Box::new(rec));
    track.set_slot(0, Some(clip_with_long_note()));
    send(
        &mut h,
        EngineCommand::graph(GraphOp::AddTrack { index: 0, track }),
    );
    send(
        &mut h,
        EngineCommand::Transport(TransportCmd::SetQuantize(Quantize::None)),
    );
    send(
        &mut h,
        EngineCommand::Launch(LaunchCmd::LaunchClip {
            track: tid,
            slot: 0,
        }),
    );
    run(&mut engine, 60_000);
    let log = log.lock().unwrap();
    assert_eq!(ons(&log, 60), [0]);
    assert_eq!(offs(&log, 60), [50_000]);
}

#[test]
fn chase_finds_a_long_note_past_later_short_notes() {
    let lane = lane_of(vec![lane_clip(0, 3840, 0, clip_with_long_note())]);
    let (mut engine, mut h, _, log) = engine_with_lane(lane);
    send(&mut h, EngineCommand::Transport(TransportCmd::Seek(1900.0)));
    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    run(&mut engine, 2_560);
    let log = log.lock().unwrap();
    assert_eq!(ons(&log, 60), [0], "chased at the play position");
    assert_eq!(offs(&log, 60), [2_500], "and released at its own end");
}

#[test]
fn seek_while_stopped_moves_marker_and_playhead() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    send(&mut h, EngineCommand::Transport(TransportCmd::Seek(1920.0)));
    run(&mut engine, 256);
    assert_eq!(tick(&h), 1920.0);
    assert_eq!(h.shared.insert_marker.load(), 1920.0);

    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    run(&mut engine, 2400);
    assert!((tick(&h) - (1920.0 + 2400.0 * TPS)).abs() < 1e-6);
    assert_eq!(h.shared.insert_marker.load(), 1920.0);
}

#[test]
fn stop_returns_to_marker_then_to_zero() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    send(&mut h, EngineCommand::Transport(TransportCmd::Seek(960.0)));
    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    run(&mut engine, 4800);
    send(&mut h, EngineCommand::Transport(TransportCmd::Stop));
    run(&mut engine, 256);
    assert_eq!(tick(&h), 960.0, "first stop returns to the marker");
    assert_eq!(h.shared.insert_marker.load(), 960.0);
    assert!(!h.shared.playing.load(std::sync::atomic::Ordering::Relaxed));

    send(&mut h, EngineCommand::Transport(TransportCmd::Stop));
    run(&mut engine, 256);
    assert_eq!(tick(&h), 0.0, "second stop rewinds");
    assert_eq!(h.shared.insert_marker.load(), 0.0);
}

#[test]
fn seek_while_playing_moves_tick_and_marker() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    run(&mut engine, 2400);
    send(&mut h, EngineCommand::Transport(TransportCmd::Seek(5000.0)));
    run(&mut engine, 256);
    assert!((tick(&h) - (5000.0 + 256.0 * TPS)).abs() < 1e-6);
    assert_eq!(h.shared.insert_marker.load(), 5000.0);
}

#[test]
fn loop_wraps_sample_accurately() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (rec, log) = FrameRecorder::new();
    let (_, track) = track_with(Box::new(rec));
    send(
        &mut h,
        EngineCommand::graph(GraphOp::AddTrack { index: 0, track }),
    );
    send(
        &mut h,
        EngineCommand::Transport(TransportCmd::SetLoop(Some((960.0, 1920.0)))),
    );
    send(&mut h, EngineCommand::Transport(TransportCmd::Seek(960.0)));
    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    run(&mut engine, 100_000);

    let log = log.lock().unwrap();
    // One beat = 24000 frames per pass, starting at frame 0.
    for pass in 1..=4u64 {
        let frame = pass * 24_000;
        let block = log.blocks.iter().find(|(f, _)| *f == frame);
        let (_, t) = block.unwrap_or_else(|| panic!("no block starts at frame {frame}"));
        assert!((t - 960.0).abs() < TPS, "pass {pass} began at tick {t}");
    }
    assert!(log.blocks.iter().all(|(_, t)| *t < 1920.0));
    assert!(log.blocks.iter().all(|(_, t)| *t > 960.0 - TPS));
}

#[test]
fn seeking_past_the_loop_does_not_snap_back() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    send(
        &mut h,
        EngineCommand::Transport(TransportCmd::SetLoop(Some((0.0, 960.0)))),
    );
    send(&mut h, EngineCommand::Transport(TransportCmd::Seek(5000.0)));
    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    run(&mut engine, 50_000);
    assert!((tick(&h) - (5000.0 + 50_000.0 * TPS)).abs() < 1e-6);
}

#[test]
fn seeking_exactly_onto_the_loop_end_does_not_snap_back() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    send(
        &mut h,
        EngineCommand::Transport(TransportCmd::SetLoop(Some((0.0, 960.0)))),
    );
    send(&mut h, EngineCommand::Transport(TransportCmd::Seek(960.0)));
    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    run(&mut engine, 1000);
    assert!((tick(&h) - (960.0 + 1000.0 * TPS)).abs() < 1e-6);
}

#[test]
fn playing_into_a_loop_from_before_it_wraps() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    send(
        &mut h,
        EngineCommand::Transport(TransportCmd::SetLoop(Some((960.0, 1000.0)))),
    );
    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    // Ends mid-pass: 960 ticks to reach the loop, then 40-tick passes of 1000 frames.
    run(&mut engine, 30_100);
    assert!(
        (tick(&h) - (960.0 + 100.0 * TPS)).abs() < 1e-6,
        "{}",
        tick(&h)
    );
}

#[test]
fn session_clip_phase_survives_a_wrap() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (rec, log) = FrameRecorder::new();
    let (tid, mut track) = track_with(Box::new(rec));
    track.set_slot(0, Some(midi_clip(960, &[(0, 480, 64)])));
    send(
        &mut h,
        EngineCommand::graph(GraphOp::AddTrack { index: 0, track }),
    );
    send(
        &mut h,
        EngineCommand::Transport(TransportCmd::SetQuantize(Quantize::None)),
    );
    // 1500 ticks is not a multiple of the clip: a phase reset would be obvious.
    send(
        &mut h,
        EngineCommand::Transport(TransportCmd::SetLoop(Some((0.0, 1500.0)))),
    );
    send(
        &mut h,
        EngineCommand::Launch(LaunchCmd::LaunchClip {
            track: tid,
            slot: 0,
        }),
    );
    run(&mut engine, 100_000);

    let log = log.lock().unwrap();
    assert_eq!(ons(&log, 64), [0, 24_000, 48_000, 72_000, 96_000]);
    assert_eq!(offs(&log, 64), [12_000, 36_000, 60_000, 84_000]);
}

#[test]
fn note_on_the_loop_boundary_fires_once() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (rec, log) = FrameRecorder::new();
    let (tid, mut track) = track_with(Box::new(rec));
    // The note sits exactly where the song loop ends.
    track.set_slot(0, Some(midi_clip(1920, &[(960, 240, 70)])));
    send(
        &mut h,
        EngineCommand::graph(GraphOp::AddTrack { index: 0, track }),
    );
    send(
        &mut h,
        EngineCommand::Transport(TransportCmd::SetQuantize(Quantize::None)),
    );
    send(
        &mut h,
        EngineCommand::Transport(TransportCmd::SetLoop(Some((0.0, 960.0)))),
    );
    send(
        &mut h,
        EngineCommand::Launch(LaunchCmd::LaunchClip {
            track: tid,
            slot: 0,
        }),
    );
    run(&mut engine, 30_000);
    assert_eq!(ons(&log.lock().unwrap(), 70), [24_000]);
}

#[test]
fn wrap_keeps_a_queued_launch_the_same_distance_away() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (rec, log) = FrameRecorder::new();
    let (tid, mut track) = track_with(Box::new(rec));
    track.set_slot(0, Some(midi_clip(3840, &[(0, 480, 72)])));
    send(
        &mut h,
        EngineCommand::graph(GraphOp::AddTrack { index: 0, track }),
    );
    send(
        &mut h,
        EngineCommand::Transport(TransportCmd::SetLoop(Some((0.0, 1920.0)))),
    );
    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    run(&mut engine, 10_000);
    // Bar quantize: queued for tick 3840, which is one wrap beyond the loop end.
    send(
        &mut h,
        EngineCommand::Launch(LaunchCmd::LaunchClip {
            track: tid,
            slot: 0,
        }),
    );
    run(&mut engine, 200_000);
    assert_eq!(ons(&log.lock().unwrap(), 72)[0], 96_000);
}

#[test]
fn seek_requantizes_a_queued_launch() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (rec, log) = FrameRecorder::new();
    let (tid, mut track) = track_with(Box::new(rec));
    track.set_slot(0, Some(midi_clip(3840, &[(0, 480, 72)])));
    send(
        &mut h,
        EngineCommand::graph(GraphOp::AddTrack { index: 0, track }),
    );
    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    run(&mut engine, 2560);
    send(
        &mut h,
        EngineCommand::Launch(LaunchCmd::LaunchClip {
            track: tid,
            slot: 0,
        }),
    );
    send(&mut h, EngineCommand::Transport(TransportCmd::Seek(5000.0)));
    run(&mut engine, 256_000);
    // From tick 5000 the next bar is 7680: 2680 ticks = 67000 frames after the seek.
    assert_eq!(ons(&log.lock().unwrap(), 72)[0], 2560 + 67_000);
}

fn lane_of(clips: Vec<LaneClip>) -> Arc<ArrangementLane> {
    Arc::new(ArrangementLane { clips })
}

fn lane_clip(start: u64, end: u64, offset: u64, data: Arc<ClipData>) -> LaneClip {
    LaneClip {
        start,
        end,
        offset,
        data,
    }
}

/// A track playing `lane`, already added to a stopped engine.
fn engine_with_lane(
    lane: Arc<ArrangementLane>,
) -> (Engine, EngineHandle, TrackId, Arc<Mutex<Timeline>>) {
    let (engine, mut h) = create_engine(SR, BLOCK);
    let (rec, log) = FrameRecorder::new();
    let (tid, mut track) = track_with(Box::new(rec));
    track.set_lane(lane);
    send(
        &mut h,
        EngineCommand::graph(GraphOp::AddTrack { index: 0, track }),
    );
    (engine, h, tid, log)
}

#[test]
fn lane_clip_plays_at_its_position() {
    let lane = lane_of(vec![lane_clip(
        960,
        1920,
        0,
        midi_clip(960, &[(0, 240, 60)]),
    )]);
    let (mut engine, mut h, _, log) = engine_with_lane(lane);
    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    run(&mut engine, 60_000);
    let log = log.lock().unwrap();
    assert_eq!(ons(&log, 60), [24_000]);
    assert_eq!(offs(&log, 60), [30_000]);
}

#[test]
fn lane_clip_offset_and_looped_content() {
    // A 480-tick loop entered 120 ticks in: origin 840, so notes land on 1320, 1800, 2280.
    let lane = lane_of(vec![lane_clip(
        960,
        2400,
        120,
        midi_clip(480, &[(0, 120, 60)]),
    )]);
    let (mut engine, mut h, _, log) = engine_with_lane(lane);
    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    run(&mut engine, 80_000);
    let log = log.lock().unwrap();
    assert_eq!(ons(&log, 60), [33_000, 45_000, 57_000]);
    assert_eq!(offs(&log, 60), [36_000, 48_000, 60_000]);
}

#[test]
fn entering_a_clip_inside_a_note_chases_it() {
    // The note spans the region start, so it has to start with the region.
    let lane = lane_of(vec![lane_clip(
        960,
        1500,
        120,
        midi_clip(960, &[(0, 480, 60)]),
    )]);
    let (mut engine, mut h, _, log) = engine_with_lane(lane);
    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    run(&mut engine, 60_000);
    let log = log.lock().unwrap();
    assert_eq!(ons(&log, 60), [24_000]);
    assert_eq!(offs(&log, 60), [33_000]);
}

#[test]
fn seek_chases_into_a_held_note() {
    let lane = lane_of(vec![lane_clip(
        0,
        3840,
        0,
        midi_clip(3840, &[(0, 3840, 60)]),
    )]);
    let (mut engine, mut h, _, log) = engine_with_lane(lane);
    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    run(&mut engine, 9_600);
    send(&mut h, EngineCommand::Transport(TransportCmd::Seek(1000.0)));
    run(&mut engine, 2_400);
    let log = log.lock().unwrap();
    assert_eq!(ons(&log, 60), [0, 9_600]);
    assert_eq!(offs(&log, 60), [9_600]);
    let position = |m: fn(&MidiMessage) -> bool| {
        log.notes
            .iter()
            .rposition(|(f, msg)| *f == 9_600 && m(msg))
            .unwrap()
    };
    assert!(
        position(|m| matches!(m, MidiMessage::NoteOff { .. }))
            < position(|m| matches!(m, MidiMessage::NoteOn { .. })),
        "the old voice ends before the chased one starts"
    );
}

#[test]
fn stop_then_play_chases_once_from_the_marker() {
    let lane = lane_of(vec![lane_clip(
        0,
        3840,
        0,
        midi_clip(3840, &[(0, 3840, 60)]),
    )]);
    let (mut engine, mut h, _, log) = engine_with_lane(lane);
    send(&mut h, EngineCommand::Transport(TransportCmd::Seek(480.0)));
    run(&mut engine, 300);
    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    run(&mut engine, 2_400);
    send(&mut h, EngineCommand::Transport(TransportCmd::Stop));
    run(&mut engine, 300);
    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    run(&mut engine, 2_400);
    let log = log.lock().unwrap();
    assert_eq!(ons(&log, 60).len(), 2);
    assert_eq!(offs(&log, 60).len(), 1);
}

#[test]
fn lane_releases_at_clip_end() {
    let lane = lane_of(vec![lane_clip(
        0,
        960,
        0,
        midi_clip(3840, &[(0, 3840, 60)]),
    )]);
    let (mut engine, mut h, _, log) = engine_with_lane(lane);
    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    run(&mut engine, 48_000);
    assert_eq!(offs(&log.lock().unwrap(), 60), [24_000]);
}

#[test]
fn lane_release_lands_on_a_block_boundary() {
    // 256 frames per block at 0.04 ticks per frame: 1024 ticks ends exactly on a block.
    let lane = lane_of(vec![lane_clip(
        0,
        1024,
        0,
        midi_clip(3840, &[(0, 3840, 60)]),
    )]);
    let (mut engine, mut h, _, log) = engine_with_lane(lane);
    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    let mut l = vec![0.0; 256];
    let mut r = vec![0.0; 256];
    for _ in 0..120 {
        engine.process_stereo(&mut l, &mut r);
    }
    assert_eq!(offs(&log.lock().unwrap(), 60), [25_600]);
}

#[test]
fn adjacent_lane_clips_hand_over_cleanly() {
    let a = midi_clip(960, &[(0, 960, 60)]);
    let b = midi_clip(960, &[(0, 480, 60)]);
    let lane = lane_of(vec![lane_clip(0, 960, 0, a), lane_clip(960, 1920, 0, b)]);
    let (mut engine, mut h, _, log) = engine_with_lane(lane);
    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    run(&mut engine, 48_000);
    let log = log.lock().unwrap();
    assert_eq!(ons(&log, 60), [0, 24_000]);
    assert_eq!(offs(&log, 60), [24_000, 36_000]);
}

#[test]
fn lane_note_on_the_loop_boundaries_fires_once_per_pass() {
    // One note on the loop start, one exactly on the loop end.
    let data = midi_clip(1920, &[(0, 240, 60), (960, 240, 61)]);
    let lane = lane_of(vec![lane_clip(0, 1920, 0, data)]);
    let (mut engine, mut h, _, log) = engine_with_lane(lane);
    send(
        &mut h,
        EngineCommand::Transport(TransportCmd::SetLoop(Some((0.0, 960.0)))),
    );
    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    run(&mut engine, 60_000);
    let log = log.lock().unwrap();
    assert_eq!(ons(&log, 60), [0, 24_000, 48_000]);
    assert!(ons(&log, 61).is_empty(), "the note at loop end never plays");
}

#[test]
fn launch_overrides_the_lane_at_the_quantize_point() {
    let lane = lane_of(vec![lane_clip(
        0,
        7680,
        0,
        midi_clip(7680, &[(0, 7680, 60)]),
    )]);
    let (mut engine, mut h, tid, log) = engine_with_lane(lane);
    send(
        &mut h,
        EngineCommand::Clip(ClipCmd::SetClip {
            track: tid,
            slot: 0,
            clip: Some(midi_clip(3840, &[(0, 480, 72)])),
        }),
    );
    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    run(&mut engine, 10_000);
    send(
        &mut h,
        EngineCommand::Launch(LaunchCmd::LaunchClip {
            track: tid,
            slot: 0,
        }),
    );
    run(&mut engine, 200_000);
    let log = log.lock().unwrap();
    assert_eq!(ons(&log, 60), [0], "the lane does not come back by itself");
    assert_eq!(offs(&log, 60), [96_000]);
    assert_eq!(ons(&log, 72)[0], 96_000);
}

#[test]
fn back_to_arrangement_chases_the_lane() {
    let lane = lane_of(vec![lane_clip(
        0,
        38_400,
        0,
        midi_clip(38_400, &[(0, 38_400, 60)]),
    )]);
    let (mut engine, mut h, tid, log) = engine_with_lane(lane);
    send(
        &mut h,
        EngineCommand::Clip(ClipCmd::SetClip {
            track: tid,
            slot: 0,
            clip: Some(midi_clip(3840, &[(0, 3840, 72)])),
        }),
    );
    send(
        &mut h,
        EngineCommand::Transport(TransportCmd::SetQuantize(Quantize::None)),
    );
    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    run(&mut engine, 2_400);
    send(
        &mut h,
        EngineCommand::Launch(LaunchCmd::LaunchClip {
            track: tid,
            slot: 0,
        }),
    );
    run(&mut engine, 2_400);
    h.collect_garbage();
    send(&mut h, EngineCommand::Launch(LaunchCmd::BackToArrangement));
    run(&mut engine, 2_400);
    assert_eq!(h.collect_garbage(), 1, "the session clip is retired");

    let log = log.lock().unwrap();
    assert_eq!(ons(&log, 60), [0, 4_800]);
    assert_eq!(offs(&log, 60), [2_400]);
    assert_eq!(ons(&log, 72), [2_400]);
    assert_eq!(offs(&log, 72), [4_800]);
}

#[test]
fn retiring_every_kind_of_object_stays_off_the_heap_on_the_audio_thread() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (tid, mut track) = track_with(Box::new(Dc(0.0)));
    let device = track.devices[0].id;
    track.set_slot(0, Some(midi_clip(480, &[(0, 240, 60)])));
    let (other, other_track) = track_with(Box::new(Dc(0.0)));
    send(
        &mut h,
        EngineCommand::graph(GraphOp::AddTrack { index: 0, track }),
    );
    send(
        &mut h,
        EngineCommand::graph(GraphOp::AddTrack {
            index: 1,
            track: other_track,
        }),
    );
    let mut l = vec![0.0; BLOCK];
    let mut r = vec![0.0; BLOCK];
    engine.process_stereo(&mut l, &mut r);

    send(
        &mut h,
        EngineCommand::Clip(ClipCmd::SetClip {
            track: tid,
            slot: 0,
            clip: Some(midi_clip(480, &[(0, 240, 62)])),
        }),
    );
    send(
        &mut h,
        EngineCommand::Clip(ClipCmd::SetLane {
            track: tid,
            lane: lane_of(Vec::new()),
        }),
    );
    send(
        &mut h,
        EngineCommand::graph(GraphOp::RemoveDevice {
            target: ChainAddr::track(tid),
            device,
        }),
    );
    send(&mut h, EngineCommand::graph(GraphOp::RemoveTrack(other)));
    send(
        &mut h,
        EngineCommand::graph(GraphOp::ReplaceAll {
            tracks: Box::new(Vec::with_capacity(MAX_TRACKS)),
            master: Box::new(TrackNode::master(BLOCK)),
        }),
    );
    rlive_testkit::assert_no_alloc(|| engine.process_stereo(&mut l, &mut r));

    // Clip, lane, device, track, then the replaced track list and master.
    assert_eq!(h.collect_garbage(), 6);
}

#[test]
fn follow_flag_tracks_launch_and_back_to_arrangement() {
    use std::sync::atomic::Ordering::Relaxed;
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let shared = Arc::new(rlive_engine::shared::TrackShared::default());
    let tid = TrackId::new();
    let mut track = TrackNode::new(tid, BLOCK).with_shared(shared.clone());
    track.set_slot(0, Some(midi_clip(960, &[(0, 240, 72)])));
    send(
        &mut h,
        EngineCommand::graph(GraphOp::AddTrack {
            index: 0,
            track: Box::new(track),
        }),
    );
    send(
        &mut h,
        EngineCommand::Transport(TransportCmd::SetQuantize(Quantize::None)),
    );
    run(&mut engine, 256);
    assert!(shared.follow_arrangement.load(Relaxed));

    send(
        &mut h,
        EngineCommand::Launch(LaunchCmd::LaunchClip {
            track: tid,
            slot: 0,
        }),
    );
    run(&mut engine, 256);
    assert!(!shared.follow_arrangement.load(Relaxed));

    send(&mut h, EngineCommand::Launch(LaunchCmd::BackToArrangement));
    run(&mut engine, 256);
    assert!(shared.follow_arrangement.load(Relaxed));
    assert_eq!(
        shared.playing_slot.load(Relaxed),
        rlive_engine::shared::NO_SLOT
    );
}

#[test]
fn set_lane_retires_the_old_lane() {
    let (mut engine, mut h, tid, _) = engine_with_lane(lane_of(vec![]));
    send(
        &mut h,
        EngineCommand::Clip(ClipCmd::SetLane {
            track: tid,
            lane: lane_of(vec![]),
        }),
    );
    send(
        &mut h,
        EngineCommand::Clip(ClipCmd::SetLane {
            track: tid,
            lane: lane_of(vec![]),
        }),
    );
    run(&mut engine, 256);
    assert_eq!(h.collect_garbage(), 2);
}

#[test]
fn set_lane_for_an_unknown_track_comes_back_as_garbage() {
    let (mut engine, mut h, _, _) = engine_with_lane(lane_of(vec![]));
    send(
        &mut h,
        EngineCommand::Clip(ClipCmd::SetLane {
            track: TrackId::new(),
            lane: lane_of(vec![]),
        }),
    );
    run(&mut engine, 256);
    assert_eq!(h.collect_garbage(), 1);
}

#[test]
fn value_equal_lane_swap_keeps_voices() {
    let data = midi_clip(3840, &[(0, 3840, 60)]);
    let lane = lane_of(vec![lane_clip(0, 3840, 0, data.clone())]);
    let (mut engine, mut h, tid, log) = engine_with_lane(lane);
    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    run(&mut engine, 4_800);
    // Equal but not identical: the same content in new allocations, with a clip added later.
    let copy = Arc::new((*data).clone());
    send(
        &mut h,
        EngineCommand::Clip(ClipCmd::SetLane {
            track: tid,
            lane: lane_of(vec![
                lane_clip(0, 3840, 0, copy),
                lane_clip(7680, 9600, 0, midi_clip(960, &[(0, 240, 80)])),
            ]),
        }),
    );
    run(&mut engine, 4_800);
    let log = log.lock().unwrap();
    assert_eq!(ons(&log, 60), [0], "no retrigger");
    assert!(offs(&log, 60).is_empty(), "no release");
}

#[test]
fn changed_lane_under_the_playhead_releases_and_chases() {
    let lane = lane_of(vec![lane_clip(
        0,
        3840,
        0,
        midi_clip(3840, &[(0, 3840, 60)]),
    )]);
    let (mut engine, mut h, tid, log) = engine_with_lane(lane);
    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    run(&mut engine, 4_800);
    send(
        &mut h,
        EngineCommand::Clip(ClipCmd::SetLane {
            track: tid,
            lane: lane_of(vec![lane_clip(
                0,
                3840,
                0,
                midi_clip(3840, &[(0, 3840, 62)]),
            )]),
        }),
    );
    run(&mut engine, 4_800);
    let log = log.lock().unwrap();
    assert_eq!(offs(&log, 60), [4_800]);
    assert_eq!(ons(&log, 62), [4_800]);
}

fn audio_clip(value: f32, frames: usize, length: u64, looped: bool) -> Arc<ClipData> {
    Arc::new(ClipData::Audio(AudioClip {
        sample: sample(vec![value; frames]),
        length,
        looped,
        gain: 1.0,
        scale: None,
    }))
}

#[test]
fn lane_audio_ramps_at_its_region_edges() {
    let data = audio_clip(0.5, 96_000, 3840, true);
    let lane = lane_of(vec![lane_clip(480, 1440, 0, data)]);
    let (mut engine, mut h, _, _) = engine_with_lane(lane);
    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    let (l, _) = run(&mut engine, 40_000);

    // Silent outside the region, 0.04 ticks per frame.
    assert!(l[..12_000].iter().all(|s| *s == 0.0));
    assert!(l[36_000..].iter().all(|s| *s == 0.0));
    // Ramps up over 64 frames, without a step.
    assert!(l[12_000].abs() < 1e-3, "{}", l[12_000]);
    for i in 12_000..12_064 {
        assert!(l[i + 1] - l[i] < 0.5 / 32.0, "step at {i}");
    }
    assert!((l[12_064] - 0.5).abs() < 1e-3);
    // Full level in the middle, ramps back down at the end.
    assert!((l[24_000] - 0.5).abs() < 1e-3);
    assert!(l[35_999] < 0.02, "{}", l[35_999]);
    assert!((l[35_936] - 0.5).abs() < 1e-3);
}

#[test]
fn lane_audio_reseeds_on_a_seek() {
    // The sample value equals its position, so the read position is visible in the output.
    let frames = 192_000;
    let data = Arc::new(ClipData::Audio(AudioClip {
        sample: sample((0..frames).map(|i| i as f32 / frames as f32).collect()),
        length: 7680,
        looped: true,
        gain: 1.0,
        scale: None,
    }));
    let lane = lane_of(vec![lane_clip(0, 7680, 0, data)]);
    let (mut engine, mut h, _, _) = engine_with_lane(lane);
    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    run(&mut engine, 4_800);
    send(&mut h, EngineCommand::Transport(TransportCmd::Seek(3840.0)));
    let (l, _) = run(&mut engine, 4_800);
    // After the ramp-in we are reading half way through the sample.
    assert!((l[1_000] - 0.505).abs() < 1e-3, "{}", l[1_000]);
}

fn note_on(key: u8) -> MidiMessage {
    MidiMessage::NoteOn {
        channel: 0,
        key,
        velocity: 0.5,
        note_id: 0,
        tuning: 0.0,
    }
}

fn events(h: &mut EngineHandle) -> Vec<EngineEvent> {
    std::iter::from_fn(|| h.poll_event()).collect()
}

fn recorded_ticks(events: &[EngineEvent]) -> Vec<f64> {
    events
        .iter()
        .filter_map(|e| match e {
            EngineEvent::RecordedMidi { tick, .. } => Some(*tick),
            _ => None,
        })
        .collect()
}

fn engine_recording() -> (Engine, EngineHandle, TrackId, Arc<Mutex<Timeline>>) {
    let (mut engine, mut h, tid, log) = engine_with_lane(lane_of(vec![]));
    send(&mut h, EngineCommand::Record(RecordCmd::SetRecording(true)));
    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    run(&mut engine, 24_000); // one beat
    (engine, h, tid, log)
}

fn played(track: TrackId, message: MidiMessage, age: Duration) -> EngineCommand {
    EngineCommand::Record(RecordCmd::PlayedMidi {
        track,
        message,
        at: Instant::now() - age,
    })
}

#[test]
fn recorded_tick_is_backdated_by_the_message_age() {
    let (mut engine, mut h, tid, log) = engine_recording();
    send(&mut h, played(tid, note_on(60), Duration::from_millis(100)));
    run(&mut engine, 256);

    // 100 ms at 120 BPM is 192 ticks; scheduling can only make the message older.
    let ticks = recorded_ticks(&events(&mut h));
    assert_eq!(ticks.len(), 1, "{ticks:?}");
    assert!(ticks[0] <= 768.0 + 1e-3, "{}", ticks[0]);
    assert!(ticks[0] > 768.0 - 192.0, "{}", ticks[0]);
    assert!(
        !ons(&log.lock().unwrap(), 60).is_empty(),
        "played notes are audible"
    );
}

#[test]
fn recorded_tick_is_floored_at_the_last_jump() {
    let (mut engine, mut h, tid, _) = engine_recording();
    send(&mut h, EngineCommand::Transport(TransportCmd::Seek(2000.0)));
    send(&mut h, played(tid, note_on(60), Duration::from_millis(100)));
    run(&mut engine, 256);
    assert_eq!(recorded_ticks(&events(&mut h)), [2000.0]);
}

#[test]
fn recording_starts_no_earlier_than_the_play_position() {
    let (mut engine, mut h, tid, _) = engine_with_lane(lane_of(vec![]));
    send(&mut h, EngineCommand::Transport(TransportCmd::Seek(480.0)));
    send(&mut h, EngineCommand::Record(RecordCmd::SetRecording(true)));
    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    send(&mut h, played(tid, note_on(60), Duration::from_millis(500)));
    run(&mut engine, 256);
    assert_eq!(recorded_ticks(&events(&mut h)), [480.0]);
}

#[test]
fn midi_sender_stamps_and_records() {
    let (mut engine, mut h, tid, _) = engine_recording();
    let mut midi = h.take_midi_sender().unwrap();
    assert!(midi.send(tid, note_on(60)));
    run(&mut engine, 256);
    let ticks = recorded_ticks(&events(&mut h));
    assert_eq!(ticks.len(), 1, "{ticks:?}");
    assert!((960.0 - ticks[0]).abs() < 100.0, "{}", ticks[0]);
}

#[test]
fn preview_midi_is_not_recorded() {
    let (mut engine, mut h, tid, log) = engine_recording();
    send(
        &mut h,
        EngineCommand::Record(RecordCmd::Midi {
            track: tid,
            message: note_on(60),
        }),
    );
    run(&mut engine, 256);
    assert!(recorded_ticks(&events(&mut h)).is_empty());
    assert_eq!(ons(&log.lock().unwrap(), 60).len(), 1, "still audible");
}

#[test]
fn only_notes_are_recorded() {
    let (mut engine, mut h, tid, _) = engine_recording();
    let cc = MidiMessage::ControlChange {
        channel: 0,
        cc: 1,
        value: 64,
    };
    let bend = MidiMessage::PitchBend {
        channel: 0,
        value: 0.5,
    };
    send(&mut h, played(tid, cc, Duration::ZERO));
    send(&mut h, played(tid, bend, Duration::ZERO));
    send(&mut h, played(tid, note_on(60), Duration::ZERO));
    let off = MidiMessage::NoteOff {
        channel: 0,
        key: 60,
        velocity: 0.5,
        note_id: 0,
    };
    send(&mut h, played(tid, off, Duration::ZERO));
    run(&mut engine, 256);
    assert_eq!(recorded_ticks(&events(&mut h)).len(), 2);
}

#[test]
fn nothing_is_recorded_unless_recording_and_playing() {
    let (mut engine, mut h, tid, _) = engine_with_lane(lane_of(vec![]));
    // Recording armed but stopped.
    send(&mut h, EngineCommand::Record(RecordCmd::SetRecording(true)));
    send(&mut h, played(tid, note_on(60), Duration::ZERO));
    run(&mut engine, 256);
    assert!(recorded_ticks(&events(&mut h)).is_empty());

    // Playing but not recording.
    send(
        &mut h,
        EngineCommand::Record(RecordCmd::SetRecording(false)),
    );
    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    send(&mut h, played(tid, note_on(60), Duration::ZERO));
    run(&mut engine, 256);
    assert!(recorded_ticks(&events(&mut h)).is_empty());
}

#[test]
fn stop_reports_recording_stopped_after_the_notes() {
    let (mut engine, mut h, tid, _) = engine_recording();
    let mut midi = h.take_midi_sender().unwrap();
    // Queued by the MIDI thread before the stop arrives: still has to be recorded first.
    assert!(midi.send(tid, note_on(62)));
    send(&mut h, EngineCommand::Transport(TransportCmd::Stop));
    run(&mut engine, 256);

    let ev = events(&mut h);
    let kinds: Vec<_> = ev
        .iter()
        .map(|e| matches!(e, EngineEvent::RecordingStopped { .. }))
        .collect();
    assert_eq!(kinds, [false, true], "{ev:?}");
    let EngineEvent::RecordingStopped { tick: stopped_at } = ev[1] else {
        unreachable!()
    };
    assert!(
        (stopped_at - 960.0).abs() < 1e-6,
        "reported before the tick resets: {stopped_at}"
    );
    assert_eq!(tick(&h), 0.0);

    // Recording ended with the stop.
    send(&mut h, played(tid, note_on(60), Duration::ZERO));
    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    run(&mut engine, 256);
    assert!(recorded_ticks(&events(&mut h)).is_empty());
}

#[test]
fn stop_while_stopped_keeps_recording_armed() {
    let (mut engine, mut h, tid, _) = engine_with_lane(lane_of(vec![]));
    send(&mut h, EngineCommand::Record(RecordCmd::SetRecording(true)));
    send(&mut h, EngineCommand::Transport(TransportCmd::Stop));
    run(&mut engine, 256);
    assert!(events(&mut h).is_empty(), "nothing was being recorded");

    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    send(&mut h, played(tid, note_on(60), Duration::ZERO));
    run(&mut engine, 256);
    assert_eq!(recorded_ticks(&events(&mut h)).len(), 1);
}

#[test]
fn jumps_are_reported_while_recording() {
    let (mut engine, mut h, _, _) = engine_recording();
    send(
        &mut h,
        EngineCommand::Transport(TransportCmd::SetLoop(Some((0.0, 1200.0)))),
    );
    send(&mut h, EngineCommand::Transport(TransportCmd::Seek(1100.0)));
    run(&mut engine, 12_000);
    let jumps: Vec<_> = events(&mut h)
        .into_iter()
        .filter_map(|e| match e {
            EngineEvent::Jumped { from, to } => Some((from, to)),
            _ => None,
        })
        .collect();
    assert_eq!(jumps.len(), 2, "{jumps:?}");
    assert_eq!(jumps[0].1, 1100.0, "the seek");
    assert!(
        (jumps[1].0 - 1200.0).abs() < 1e-6,
        "the wrap: {:?}",
        jumps[1]
    );
    assert!(jumps[1].1.abs() < 1e-6, "{:?}", jumps[1]);
}

#[test]
fn jumps_are_not_reported_when_not_recording() {
    let (mut engine, mut h, _, _) = engine_with_lane(lane_of(vec![]));
    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    send(&mut h, EngineCommand::Transport(TransportCmd::Seek(1100.0)));
    run(&mut engine, 256);
    assert!(events(&mut h).is_empty());
}

#[test]
fn midi_sender_reaches_track() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let log = Arc::new(Mutex::new(Vec::new()));
    let (tid, track) = track_with(Box::new(Recorder { log: log.clone() }));
    h.send(EngineCommand::graph(GraphOp::AddTrack { index: 0, track }))
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
            velocity: 0.5,
            note_id: 0,
            tuning: 0.0,
        }
    ));
    run(&mut engine, 256);
    assert!(log
        .lock()
        .unwrap()
        .iter()
        .any(|(_, m)| matches!(m, MidiMessage::NoteOn { key: 61, .. })));
}

/// Records the scale devices are told about, each block.
struct ScaleSpy(Arc<Mutex<Vec<Option<ScaleInfo>>>>);

impl Processor for ScaleSpy {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        self.0.lock().unwrap().push(ctx.transport.scale);
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

const D_MAJOR: ScaleInfo = ScaleInfo {
    root: 2,
    mask: 0b1010_1011_0101,
};

fn scaled_clip() -> Arc<ClipData> {
    let notes = vec![Note {
        start: 0,
        length: 480,
        key: 60,
        velocity: 0.8,
        ..Note::default()
    }];
    Arc::new(ClipData::Midi(
        MidiClip::new(3840, notes, true).with_scale(Some(D_MAJOR)),
    ))
}

#[test]
fn a_launched_clips_scale_reaches_the_devices_on_its_track() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (tid, mut track) = track_with(Box::new(ScaleSpy(seen.clone())));
    track.set_slot(0, Some(scaled_clip()));
    h.send(EngineCommand::graph(GraphOp::AddTrack { index: 0, track }))
        .ok()
        .unwrap();
    run(&mut engine, 1_000);
    assert_eq!(
        seen.lock().unwrap().last(),
        Some(&None),
        "nothing playing yet"
    );
    h.send(EngineCommand::Launch(LaunchCmd::LaunchClip {
        track: tid,
        slot: 0,
    }))
    .ok()
    .unwrap();
    run(&mut engine, 5_000);
    assert_eq!(seen.lock().unwrap().last(), Some(&Some(D_MAJOR)));
}

#[test]
fn a_timeline_clips_scale_reaches_the_devices_on_its_track() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (_tid, mut track) = track_with(Box::new(ScaleSpy(seen.clone())));
    track.set_lane(Arc::new(ArrangementLane {
        clips: vec![LaneClip {
            start: 0,
            end: 3840,
            offset: 0,
            data: scaled_clip(),
        }],
    }));
    h.send(EngineCommand::graph(GraphOp::AddTrack { index: 0, track }))
        .ok()
        .unwrap();
    h.send(EngineCommand::Transport(TransportCmd::Play))
        .ok()
        .unwrap();
    run(&mut engine, 5_000);
    assert_eq!(seen.lock().unwrap().last(), Some(&Some(D_MAJOR)));
}
