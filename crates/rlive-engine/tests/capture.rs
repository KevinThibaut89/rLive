//! Recording in the engine: the metronome and count-in, slots opening and closing on launch
//! points, and resampling. Fed through `AudioIo`, with no device anywhere.

use rlive_core::{TimeSignature, TrackId, PPQ};
use rlive_engine::capture::CaptureMode;
use rlive_engine::*;
use rlive_testkit::assert_no_alloc;

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;
/// One beat at 120 BPM.
const BEAT: usize = 24_000;

fn engine_with(track: TrackId) -> (Engine, EngineHandle) {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let node = Box::new(TrackNode::new(track, BLOCK));
    h.send(EngineCommand::graph(GraphOp::AddTrack {
        index: 0,
        track: node,
    }))
    .ok()
    .unwrap();
    engine.process_stereo(&mut [0.0; 64], &mut [0.0; 64]);
    (engine, h)
}

fn cmd(h: &mut EngineHandle, c: EngineCommand) {
    assert!(h.send(c).is_ok());
}

fn transport(h: &mut EngineHandle, c: TransportCmd) {
    cmd(h, EngineCommand::Transport(c));
}

fn render(engine: &mut Engine, frames: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(frames);
    let mut done = 0;
    while done < frames {
        let n = (frames - done).min(BLOCK);
        let (mut l, mut r) = (vec![0.0; n], vec![0.0; n]);
        engine.process_stereo(&mut l, &mut r);
        out.extend_from_slice(&l);
        done += n;
    }
    out
}

/// Where sound begins after silence, every time.
fn onsets(audio: &[f32]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut quiet = true;
    let mut silent_for = 0;
    for (i, s) in audio.iter().enumerate() {
        if s.abs() > 1e-4 {
            if quiet {
                out.push(i);
            }
            quiet = false;
            silent_for = 0;
        } else {
            silent_for += 1;
            if silent_for > 400 {
                quiet = true;
            }
        }
    }
    out
}

fn near(a: usize, b: usize, tolerance: usize) -> bool {
    a.abs_diff(b) <= tolerance
}

#[test]
fn a_count_in_clicks_every_beat_for_its_bars_then_starts_at_the_marker() {
    let (mut engine, mut h) = engine_with(TrackId::new());
    cmd(&mut h, EngineCommand::Record(RecordCmd::SetCountIn(1)));
    cmd(&mut h, EngineCommand::Record(RecordCmd::SetRecording(true)));
    transport(&mut h, TransportCmd::Play);
    // One bar of count-in at 120 BPM: four clicks, a beat apart.
    let audio = render(&mut engine, 4 * BEAT);
    let clicks = onsets(&audio);
    assert_eq!(clicks.len(), 4, "{clicks:?}");
    for (n, at) in clicks.iter().enumerate() {
        assert!(near(*at, n * BEAT, 2), "click {n} at {at}");
    }
    assert!(!h.shared.playing.load(std::sync::atomic::Ordering::Relaxed));
    render(&mut engine, 2_000);
    assert_eq!(h.shared.count_in_left.load(), 0.0);
    assert!(h.shared.playing.load(std::sync::atomic::Ordering::Relaxed));
}

#[test]
fn a_count_in_stopped_early_never_starts_the_transport() {
    let (mut engine, mut h) = engine_with(TrackId::new());
    cmd(&mut h, EngineCommand::Record(RecordCmd::SetCountIn(2)));
    cmd(&mut h, EngineCommand::Record(RecordCmd::SetRecording(true)));
    transport(&mut h, TransportCmd::Play);
    render(&mut engine, BEAT);
    transport(&mut h, TransportCmd::Stop);
    render(&mut engine, 2 * BEAT);
    assert!(!h.shared.playing.load(std::sync::atomic::Ordering::Relaxed));
    assert_eq!(h.shared.count_in_left.load(), 0.0);
}

#[test]
fn the_metronome_division_changes_how_often_it_ticks() {
    let (mut engine, mut h) = engine_with(TrackId::new());
    transport(&mut h, TransportCmd::SetMetronome(true));
    transport(
        &mut h,
        TransportCmd::SetMetronomeConfig(MetronomeConfig {
            division: Some(PPQ as u32 / 2),
            ..Default::default()
        }),
    );
    transport(&mut h, TransportCmd::Play);
    let audio = render(&mut engine, BEAT * 2);
    assert_eq!(onsets(&audio).len(), 4, "eighths");
}

#[test]
fn a_division_that_stops_fitting_falls_back_to_the_denominator() {
    let (mut engine, mut h) = engine_with(TrackId::new());
    transport(&mut h, TransportCmd::SetMetronome(true));
    transport(
        &mut h,
        TransportCmd::SetTimeSignature(TimeSignature {
            numerator: 3,
            denominator: 4,
        }),
    );
    transport(
        &mut h,
        TransportCmd::SetMetronomeConfig(MetronomeConfig {
            division: Some(PPQ as u32 * 2),
            ..Default::default()
        }),
    );
    transport(&mut h, TransportCmd::Play);
    let audio = render(&mut engine, BEAT * 3);
    assert_eq!(
        onsets(&audio).len(),
        3,
        "a half note doesn't fit 3/4: quarters"
    );
}

#[test]
fn enable_only_while_recording_is_silent_until_recording_and_after_punch_in() {
    let (mut engine, mut h) = engine_with(TrackId::new());
    transport(&mut h, TransportCmd::SetMetronome(true));
    transport(
        &mut h,
        TransportCmd::SetMetronomeConfig(MetronomeConfig {
            only_while_recording: true,
            ..Default::default()
        }),
    );
    transport(&mut h, TransportCmd::Play);
    assert!(
        onsets(&render(&mut engine, BEAT)).is_empty(),
        "not recording"
    );
    cmd(
        &mut h,
        EngineCommand::Record(RecordCmd::SetPunch {
            punch_in: Some(3.0 * PPQ as f64),
            punch_out: None,
        }),
    );
    cmd(&mut h, EngineCommand::Record(RecordCmd::SetRecording(true)));
    let before = render(&mut engine, BEAT);
    assert!(onsets(&before).is_empty(), "before the punch-in");
    let after = render(&mut engine, 3 * BEAT);
    let clicks = onsets(&after);
    assert_eq!(clicks.len(), 2, "{clicks:?}");
    assert!(
        near(clicks[0], BEAT, 2),
        "the first is at punch-in: {clicks:?}"
    );
}

/// What the engine said happened, in order, until the queue runs dry.
fn events(h: &mut EngineHandle) -> Vec<EngineEvent> {
    let mut out = Vec::new();
    while let Some(e) = h.poll_event() {
        out.push(e);
    }
    out
}

#[test]
fn a_slot_starts_and_closes_on_launch_points_and_reports_them() {
    let track = TrackId::new();
    let (mut engine, mut h) = engine_with(track);
    let (sink, tail) = capture_pair(1, 192_000, CaptureMode::Slot, 0);
    cmd(
        &mut h,
        EngineCommand::Record(RecordCmd::InstallCapture {
            track,
            sink: Box::new(sink),
        }),
    );
    transport(&mut h, TransportCmd::SetQuantize(rlive_core::Quantize::Bar));
    transport(&mut h, TransportCmd::Play);
    render(&mut engine, BEAT);
    // Playing, a quarter into bar 1: the take waits for bar 2.
    cmd(
        &mut h,
        EngineCommand::Launch(LaunchCmd::RecordSlot { track, slot: 3 }),
    );
    let input = vec![0.5f32; BLOCK];
    let feed = |engine: &mut Engine, frames: usize| {
        let mut done = 0;
        while done < frames {
            let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
            engine.process(AudioIo {
                inputs: &[&input],
                outputs: &mut [&mut l, &mut r],
            });
            done += BLOCK;
        }
    };
    feed(&mut engine, 3 * BEAT - BLOCK);
    assert_eq!(tail.dropped(), 0);
    assert!(events(&mut h)
        .iter()
        .all(|e| !matches!(e, EngineEvent::SlotRecordStarted { .. })));
    feed(&mut engine, BLOCK * 2);
    let started = events(&mut h);
    let start = started.iter().find_map(|e| match e {
        EngineEvent::SlotRecordStarted { slot, tick, .. } => Some((*slot, *tick)),
        _ => None,
    });
    assert_eq!(start, Some((3, 4.0 * PPQ as f64)), "at bar 2");
    cmd(
        &mut h,
        EngineCommand::Launch(LaunchCmd::CloseRecording { track }),
    );
    feed(&mut engine, BEAT);
    let closing = events(&mut h).into_iter().find_map(|e| match e {
        EngineEvent::SlotClosing { at, launch, .. } => Some((at, launch)),
        _ => None,
    });
    assert_eq!(
        closing,
        Some((8.0 * PPQ as f64, true)),
        "at the next bar line"
    );
    feed(&mut engine, 4 * BEAT);
    let end = events(&mut h).into_iter().find_map(|e| match e {
        EngineEvent::CaptureClosed { frames, .. } => Some(frames),
        _ => None,
    });
    assert_eq!(end, Some(4 * BEAT as u64), "exactly one bar");
}

#[test]
fn a_full_engine_recording_block_does_not_allocate() {
    let track = TrackId::new();
    let (mut engine, mut h) = engine_with(track);
    let (sink, _tail) = capture_pair(2, 192_000, CaptureMode::Arrangement, 0);
    cmd(
        &mut h,
        EngineCommand::Record(RecordCmd::InstallCapture {
            track,
            sink: Box::new(sink),
        }),
    );
    cmd(&mut h, EngineCommand::Record(RecordCmd::SetRecording(true)));
    transport(&mut h, TransportCmd::Play);
    render(&mut engine, BLOCK);
    let input = vec![0.5f32; BLOCK];
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    assert_no_alloc(|| {
        for _ in 0..8 {
            engine.process(AudioIo {
                inputs: &[&input, &input],
                outputs: &mut [&mut l, &mut r],
            });
        }
    });
}
