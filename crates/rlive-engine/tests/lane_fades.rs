//! Fades and crossfades as rendered, and the transport's quantized jumps.

use rlive_core::{
    fade_gain, MidiMessage, ParamRange, ProcessContext, Processor, Quantize, SampleBuffer, TrackId,
};
use rlive_engine::*;
use rlive_testkit::assert_no_alloc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;
/// At 120 BPM: 960 ticks a beat, 24 000 frames a beat.
const TPF: f64 = 0.04;

fn sample(value: f32, frames: usize) -> Arc<SampleBuffer> {
    Arc::new(SampleBuffer {
        name: "s".into(),
        left: vec![value; frames],
        right: vec![value; frames],
        sample_rate: SR,
    })
}

fn clip(value: f32, frames: usize, length: u64, looped: bool) -> Arc<ClipData> {
    Arc::new(ClipData::Audio(AudioClip {
        sample: sample(value, frames),
        length,
        looped,
        gain: 1.0,
        scale: None,
    }))
}

fn lane(clips: Vec<LaneClip>) -> Arc<ArrangementLane> {
    Arc::new(ArrangementLane { clips })
}

fn lane_clip(start: u64, end: u64, offset: u64, data: Arc<ClipData>, fades: LaneFades) -> LaneClip {
    LaneClip {
        start,
        end,
        offset,
        data,
        fades,
    }
}

fn engine_playing(l: Arc<ArrangementLane>) -> (Engine, EngineHandle, TrackId) {
    let (engine, mut h) = create_engine(SR, BLOCK);
    let tid = TrackId::new();
    let mut track = Box::new(TrackNode::new(tid, BLOCK));
    track.set_lane(l);
    send(
        &mut h,
        EngineCommand::graph(GraphOp::AddTrack { index: 0, track }),
    );
    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    (engine, h, tid)
}

fn send(h: &mut EngineHandle, cmd: EngineCommand) {
    assert!(h.send(cmd).is_ok(), "command queue full");
}

fn run(e: &mut Engine, frames: usize) -> (Vec<f32>, Vec<f32>) {
    let mut l = vec![0.0; frames];
    let mut r = vec![0.0; frames];
    e.process_stereo(&mut l, &mut r);
    (l, r)
}

fn frame_of(tick: f64) -> usize {
    (tick / TPF).round() as usize
}

#[test]
fn a_rendered_fade_in_matches_its_curve() {
    for curve in [0.0, 0.5, -0.7] {
        let fades = LaneFades {
            fade_in: 480,
            in_curve: curve,
            ..LaneFades::default()
        };
        let data = clip(0.5, 96_000, 3840, false);
        let (mut e, _h, _) = engine_playing(lane(vec![lane_clip(960, 3840, 0, data, fades)]));
        let (l, _) = run(&mut e, frame_of(1920.0));
        // Well past the 64-frame declick at the start of the clip.
        for tick in [120.0, 240.0, 360.0, 470.0] {
            let got = l[frame_of(960.0 + tick)];
            let want = 0.5 * fade_gain(tick / 480.0, curve);
            assert!(
                (got - want).abs() < 2e-3,
                "{curve} at {tick}: {got} vs {want}"
            );
        }
        assert!(
            (l[frame_of(960.0 + 600.0)] - 0.5).abs() < 1e-3,
            "full level after"
        );
    }
}

#[test]
fn a_rendered_fade_out_matches_its_curve_and_is_silent_at_the_end() {
    let fades = LaneFades {
        fade_out: 480,
        out_curve: 0.4,
        ..LaneFades::default()
    };
    let data = clip(0.5, 96_000, 3840, false);
    let (mut e, _h, _) = engine_playing(lane(vec![lane_clip(0, 1920, 0, data, fades)]));
    let (l, _) = run(&mut e, frame_of(2400.0));
    for tick in [100.0, 300.0, 460.0] {
        let got = l[frame_of(1920.0 - 480.0 + tick)];
        let want = 0.5 * fade_gain(1.0 - tick / 480.0, 0.4);
        assert!((got - want).abs() < 2e-3, "at {tick}: {got} vs {want}");
    }
    assert!(l[frame_of(1920.0) + 2].abs() < 1e-5, "silent after the end");
}

/// A and B of the same material meet at 1920, crossfading from 1680 to 2160.
fn crossfaded() -> Arc<ArrangementLane> {
    let (a, b) = (
        clip(0.5, 96_000, 3840, false),
        clip(0.5, 96_000, 3840, false),
    );
    lane(vec![
        lane_clip(
            0,
            1920,
            0,
            a,
            LaneFades {
                fade_out: 240,
                tail: 240,
                voice: 0,
                ..LaneFades::default()
            },
        ),
        lane_clip(
            1920,
            3840,
            1920,
            b,
            LaneFades {
                fade_in: 240,
                lead: 240,
                voice: 1,
                ..LaneFades::default()
            },
        ),
    ])
}

#[test]
fn a_linear_crossfade_of_the_same_material_sums_to_unity() {
    let (mut e, _h, _) = engine_playing(crossfaded());
    let (l, _) = run(&mut e, frame_of(3000.0));
    for tick in [
        1500.0, 1700.0, 1750.0, 1800.0, 1900.0, 1920.0, 1950.0, 2050.0, 2150.0, 2400.0,
    ] {
        let got = l[frame_of(tick)];
        assert!((got - 0.5).abs() < 5e-3, "at {tick}: {got}");
    }
}

#[test]
fn a_clip_sounds_past_its_end_only_through_its_tail() {
    let silent_after = clip(0.5, 96_000, 3840, false);
    let lanes = lane(vec![
        lane_clip(
            0,
            1920,
            0,
            silent_after,
            LaneFades {
                fade_out: 0,
                tail: 240,
                voice: 0,
                ..LaneFades::default()
            },
        ),
        lane_clip(
            1920,
            3840,
            1920,
            clip(0.0, 96_000, 3840, false),
            LaneFades {
                voice: 1,
                ..LaneFades::default()
            },
        ),
    ]);
    let (mut e, _h, _) = engine_playing(lanes);
    let (l, _) = run(&mut e, frame_of(2400.0));
    assert!(
        l[frame_of(2100.0)].abs() > 0.05,
        "still fading out under the next clip"
    );
    assert!(l[frame_of(2300.0)].abs() < 1e-5, "and gone after its tail");
}

#[test]
fn fades_and_crossfades_never_allocate_on_the_audio_thread() {
    let (mut e, _h, _) = engine_playing(crossfaded());
    let mut l = vec![0.0f32; BLOCK];
    let mut r = vec![0.0f32; BLOCK];
    for _ in 0..800 {
        assert_no_alloc(|| e.process_stereo(&mut l, &mut r));
    }
}

#[test]
fn a_fade_edit_during_play_keeps_the_clip_going() {
    let data = clip(0.5, 96_000, 3840, false);
    let start = lane(vec![lane_clip(
        0,
        3840,
        0,
        data.clone(),
        LaneFades {
            fade_out: 240,
            ..LaneFades::default()
        },
    )]);
    let (mut e, mut h, track) = engine_playing(start);
    run(&mut e, frame_of(1000.0));
    let edited = lane(vec![lane_clip(
        0,
        3840,
        0,
        data,
        LaneFades {
            fade_out: 960,
            ..LaneFades::default()
        },
    )]);
    send(
        &mut h,
        EngineCommand::Clip(ClipCmd::SetLane {
            track,
            lane: edited,
        }),
    );
    // The new fade applies at the block after the swap, and nothing clicked: there was no reseek.
    let (l, _) = run(&mut e, frame_of(1000.0));
    assert!(
        (l[frame_of(100.0)] - 0.5).abs() < 2e-3,
        "{}",
        l[frame_of(100.0)]
    );
}

/// The same as [`clip`], with a gain envelope holding at `share` of the clip gain.
fn enveloped(value: f32, share: f64) -> Arc<ClipData> {
    let curve = EnvCurve::new(
        ParamRange {
            min: 0.0,
            max: 1.0,
            skew: 1.0,
            quantized: false,
        },
        [(0.0, share, 0.0)],
    )
    .unwrap();
    let inner = (*clip(value, 96_000, 3840, false)).clone();
    Arc::new(ClipData::Enveloped(Box::new(EnvelopedClip {
        clip: inner,
        envelopes: ClipEnvelopes {
            gain: Some(ClipCurve::new(
                curve,
                EnvTiming::Linked {
                    length: 3840.0,
                    looped: false,
                },
            )),
            ..ClipEnvelopes::default()
        },
    })))
}

#[test]
fn enveloped_clips_crossfade_through_both_voices() {
    let lanes = lane(vec![
        lane_clip(
            0,
            1920,
            0,
            enveloped(0.5, 0.5),
            LaneFades {
                fade_out: 240,
                tail: 240,
                voice: 0,
                ..LaneFades::default()
            },
        ),
        lane_clip(
            1920,
            3840,
            1920,
            enveloped(0.5, 0.5),
            LaneFades {
                fade_in: 240,
                lead: 240,
                voice: 1,
                ..LaneFades::default()
            },
        ),
    ]);
    let (mut e, _h, _) = engine_playing(lanes);
    let (l, _) = run(&mut e, frame_of(3000.0));
    // 0.5 signal at half the clip gain, unity across the crossfade, on both voices.
    for tick in [1000.0, 1700.0, 1800.0, 1920.0, 2050.0, 2500.0] {
        let got = l[frame_of(tick)];
        assert!((got - 0.25).abs() < 5e-3, "at {tick}: {got}");
    }
}

fn trigger(h: &mut EngineHandle, to: f64, hold: bool) {
    send(
        h,
        EngineCommand::Transport(TransportCmd::Trigger { to, hold }),
    );
}

fn position(h: &EngineHandle) -> f64 {
    h.shared.tick.load()
}

#[test]
fn a_trigger_while_playing_waits_for_the_next_launch_point() {
    let (mut e, mut h, _) = engine_playing(lane(vec![]));
    send(
        &mut h,
        EngineCommand::Transport(TransportCmd::SetQuantize(Quantize::Bar)),
    );
    run(&mut e, frame_of(100.0));
    trigger(&mut h, 10_000.0, false);
    run(&mut e, frame_of(3000.0));
    assert!(position(&h) < 3840.0, "still playing up to the bar");
    run(&mut e, frame_of(1000.0));
    assert!(position(&h) >= 10_000.0, "jumped at the bar");
}

#[test]
fn a_trigger_while_stopped_starts_from_there() {
    let (mut e, mut h) = create_engine(SR, BLOCK);
    send(
        &mut h,
        EngineCommand::Transport(TransportCmd::SetQuantize(Quantize::Bar)),
    );
    trigger(&mut h, 2000.0, false);
    run(&mut e, frame_of(100.0));
    assert!(h.shared.playing.load(std::sync::atomic::Ordering::Relaxed));
    assert!(position(&h) >= 2000.0, "{}", position(&h));
}

#[test]
fn holding_repeats_the_quantized_region_until_released() {
    let (mut e, mut h) = create_engine(SR, BLOCK);
    send(
        &mut h,
        EngineCommand::Transport(TransportCmd::SetQuantize(Quantize::Beat)),
    );
    trigger(&mut h, 3840.0, true);
    run(&mut e, frame_of(2000.0));
    let looping = position(&h);
    assert!(
        (3840.0..3840.0 + 961.0).contains(&looping),
        "held inside the beat: {looping}"
    );
    send(&mut h, EngineCommand::Transport(TransportCmd::ReleaseHold));
    run(&mut e, frame_of(1200.0));
    assert!(position(&h) > 4800.0, "released, it plays on");
}

#[test]
fn stopping_and_continuing_picks_up_at_the_stop_point() {
    let (mut e, mut h, _) = engine_playing(lane(vec![]));
    run(&mut e, frame_of(1000.0));
    send(&mut h, EngineCommand::Transport(TransportCmd::Stop));
    run(&mut e, frame_of(100.0));
    assert_eq!(position(&h), 0.0, "stop returns to the insert marker");
    send(&mut h, EngineCommand::Transport(TransportCmd::Continue));
    run(&mut e, frame_of(10.0));
    assert!(position(&h) >= 1000.0, "{}", position(&h));
}

#[test]
fn stop_at_stops_on_the_tick() {
    let (mut e, mut h, _) = engine_playing(lane(vec![]));
    send(
        &mut h,
        EngineCommand::Transport(TransportCmd::StopAt(Some(500.0))),
    );
    run(&mut e, frame_of(2000.0));
    assert!(!h.shared.playing.load(std::sync::atomic::Ordering::Relaxed));
}

/// Counts the notes that reach the track's first device.
struct NoteCounter(Arc<AtomicUsize>);

impl Processor for NoteCounter {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        let ons = ctx
            .events
            .iter()
            .filter(|e| matches!(e.message, MidiMessage::NoteOn { .. }))
            .count();
        self.0.fetch_add(ons, Ordering::Relaxed);
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

fn note_clip(length: u64) -> Arc<ClipData> {
    Arc::new(ClipData::Midi(MidiClip::new(
        length,
        vec![Note {
            start: 0,
            length,
            key: 60,
            velocity: 1.0,
            ..Note::default()
        }],
        false,
    )))
}

#[test]
fn chase_off_skips_a_note_already_in_progress() {
    // The clip's only note starts before the playhead does.
    for (chase, expected_on) in [(true, true), (false, false)] {
        let (mut e, mut h) = create_engine(SR, BLOCK);
        let track = TrackId::new();
        let count = Arc::new(AtomicUsize::new(0));
        let mut node = TrackNode::new(track, BLOCK);
        node.devices.push(Box::new(DeviceSlot::new(
            rlive_core::DeviceId::new(),
            Box::new(NoteCounter(count.clone())),
        )));
        node.set_lane(lane(vec![LaneClip {
            start: 0,
            end: 3840,
            offset: 0,
            data: note_clip(3840),
            fades: LaneFades::default(),
        }]));
        send(
            &mut h,
            EngineCommand::graph(GraphOp::AddTrack {
                index: 0,
                track: Box::new(node),
            }),
        );
        send(
            &mut h,
            EngineCommand::Transport(TransportCmd::SetChase(chase)),
        );
        send(&mut h, EngineCommand::Transport(TransportCmd::Seek(960.0)));
        send(&mut h, EngineCommand::Transport(TransportCmd::Play));
        run(&mut e, 512);
        let on = count.load(Ordering::Relaxed) > 0;
        assert_eq!(on, expected_on, "chase {chase}");
    }
}
