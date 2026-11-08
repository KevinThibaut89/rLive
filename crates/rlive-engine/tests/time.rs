//! The tempo and meter maps as the engine runs them.

use rlive_core::time::{BarMap, TempoMap, TempoPoint, TimeMaps};
use rlive_core::TimeSignature;
use rlive_engine::{create_engine, Engine, EngineCommand, EngineHandle, TransportCmd};
use rlive_testkit::assert_no_alloc;
use std::sync::Arc;

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

fn send(h: &mut EngineHandle, c: TransportCmd) {
    h.send(EngineCommand::Transport(c)).ok().unwrap();
}

fn sig(numerator: u8, denominator: u8) -> TimeSignature {
    TimeSignature {
        numerator,
        denominator,
    }
}

fn engine_with(maps: TimeMaps) -> (Engine, EngineHandle) {
    let (engine, mut h) = create_engine(SR, BLOCK);
    send(&mut h, TransportCmd::SetTimeMaps(Arc::new(maps)));
    (engine, h)
}

/// Runs `frames`, the left channel only.
fn run_blocks(engine: &mut Engine, frames: usize, chunk: usize) -> Vec<f32> {
    let mut l = vec![0.0; frames];
    let mut r = vec![0.0; frames];
    for (cl, cr) in l.chunks_mut(chunk).zip(r.chunks_mut(chunk)) {
        engine.process_stereo(cl, cr);
    }
    l
}

/// The frames where a run of sound starts, with its peak.
fn onsets(signal: &[f32]) -> Vec<(usize, f32)> {
    let mut out = Vec::new();
    let mut quiet = 256;
    let mut peak: Option<(usize, f32)> = None;
    for (i, s) in signal.iter().enumerate() {
        if *s == 0.0 {
            quiet += 1;
        } else {
            if quiet >= 256 {
                if let Some(p) = peak.take() {
                    out.push(p);
                }
                peak = Some((i, 0.0));
            }
            quiet = 0;
            if let Some(p) = &mut peak {
                p.1 = p.1.max(s.abs());
            }
        }
    }
    out.extend(peak);
    out
}

/// Runs with the metronome on and returns where each click started, at the frame it was due.
fn clicks(maps: TimeMaps, frames: usize, chunk: usize) -> Vec<(usize, f32)> {
    let (mut engine, mut h) = engine_with(maps);
    send(&mut h, TransportCmd::SetMetronome(true));
    send(&mut h, TransportCmd::Play);
    // The first sample of a click is sin(0): it shows one frame after it starts.
    onsets(&run_blocks(&mut engine, frames, chunk))
        .into_iter()
        .map(|(i, p)| (i - 1, p))
        .collect()
}

fn assert_clicks_at(got: &[(usize, f32)], want: &[f64]) {
    assert_eq!(got.len(), want.len(), "clicks {got:?} want {want:?}");
    for (g, w) in got.iter().zip(want) {
        assert!(
            (g.0 as f64 - w).abs() <= 1.0,
            "click at {} but due at {w}: {got:?}",
            g.0
        );
    }
}

#[test]
fn clicks_land_on_the_sample_across_a_tempo_step() {
    // 120 BPM for two beats (one second), then 240.
    let maps = TimeMaps {
        tempo: TempoMap::new([
            TempoPoint::new(0.0, 120.0),
            TempoPoint::new(1920.0, 120.0),
            TempoPoint::new(1920.0, 240.0),
        ]),
        bars: BarMap::default(),
    };
    for chunk in [256, 100, 37] {
        let got = clicks(maps.clone(), 84_000, chunk);
        assert_clicks_at(&got, &[0.0, 24_000.0, 48_000.0, 60_000.0, 72_000.0]);
    }
}

#[test]
fn clicks_follow_a_ramp_at_the_right_sample() {
    let tempo = TempoMap::new([TempoPoint::new(0.0, 80.0), TempoPoint::new(7680.0, 200.0)]);
    let maps = TimeMaps {
        tempo: tempo.clone(),
        bars: BarMap::default(),
    };
    let want: Vec<f64> = (0..8)
        .map(|b| tempo.tick_to_seconds(b as f64 * 960.0) * SR)
        .collect();
    for chunk in [256, 91] {
        let got = clicks(maps.clone(), want[7] as usize + 2000, chunk);
        assert_clicks_at(&got, &want);
    }
}

#[test]
fn clicks_restart_at_a_signature_change_with_a_downbeat() {
    // 4/4 to tick 5760 (six beats), a fragmentary bar, then 3/8 eighths.
    let bars = BarMap::new(sig(4, 4), [(5760, sig(3, 8))]);
    let maps = TimeMaps {
        tempo: TempoMap::constant(120.0),
        bars,
    };
    let got = clicks(maps, (9600.0 / 0.04 + 2000.0) as usize, 256);
    let want_ticks = [
        0.0, 960.0, 1920.0, 2880.0, 3840.0, 4800.0, 5760.0, 6240.0, 6720.0, 7200.0, 7680.0, 8160.0,
        8640.0, 9120.0, 9600.0,
    ];
    let want: Vec<f64> = want_ticks.iter().map(|t| t / 0.04).collect();
    assert_clicks_at(&got, &want);
    // Downbeats are the loud clicks: the first of each bar, restarting at the change.
    let peaks: Vec<bool> = got.iter().map(|(_, p)| *p > 0.4).collect();
    let mut want_down = vec![false; want_ticks.len()];
    for i in [0, 4, 6, 9, 12] {
        want_down[i] = true;
    }
    assert_eq!(peaks, want_down);
}

#[test]
fn a_signature_change_mid_song_still_clicks_on_a_tempo_ramp() {
    let tempo = TempoMap::new([TempoPoint::new(0.0, 120.0), TempoPoint::new(3840.0, 180.0)]);
    let bars = BarMap::new(sig(4, 4), [(1920, sig(2, 4))]);
    let maps = TimeMaps {
        tempo: tempo.clone(),
        bars,
    };
    let want: Vec<f64> = [0.0, 960.0, 1920.0, 2880.0, 3840.0]
        .iter()
        .map(|t| tempo.tick_to_seconds(*t) * SR)
        .collect();
    let got = clicks(maps, want[4] as usize + 2000, 200);
    assert_clicks_at(&got, &want);
    assert!(got[2].1 > 0.4, "the change is a downbeat: {got:?}");
    assert!(got[3].1 < 0.4, "{got:?}");
}

fn loop_test(start: f64, end: f64, tempo: TempoMap, chunk: usize) {
    let maps = TimeMaps {
        tempo: tempo.clone(),
        bars: BarMap::default(),
    };
    let (mut engine, mut h) = engine_with(maps);
    send(&mut h, TransportCmd::SetLoop(Some((start, end))));
    send(&mut h, TransportCmd::Seek(start));
    send(&mut h, TransportCmd::Play);
    let period = tempo.tick_to_seconds(end) - tempo.tick_to_seconds(start);
    let period_frames = period * SR;
    assert!(
        period_frames.fract() > 0.05 && period_frames.fract() < 0.95,
        "the test only proves something with a fractional loop length: {period_frames}"
    );

    // Hundreds of passes, one block run at a time.
    let mut l = vec![0.0; 1 << 16];
    let mut r = vec![0.0; 1 << 16];
    let mut total = 0u64;
    let target = (period * SR * 2000.0) as u64;
    while total < target {
        let n = chunk.min((target - total) as usize);
        engine.process_stereo(&mut l[..n], &mut r[..n]);
        total += n as u64;
    }
    let elapsed = total as f64 / SR;
    let phase = elapsed.rem_euclid(period);
    let want = tempo.seconds_to_tick(tempo.tick_to_seconds(start) + phase);
    let tick = h.shared.tick.load();
    let got_seconds = tempo.tick_to_seconds(tick);
    let want_seconds = tempo.tick_to_seconds(want);
    let drift_frames = (got_seconds - want_seconds).abs() * SR;
    let around = (period - (got_seconds - want_seconds).abs()) * SR;
    assert!(
        drift_frames.min(around) < 1.5,
        "drifted {drift_frames} frames over 2000 passes (tick {tick}, want {want})"
    );
}

#[test]
fn a_loop_over_a_tempo_step_does_not_drift() {
    // A tempo step inside the loop. The remainder must survive each wrap in seconds.
    let tempo = TempoMap::new([
        TempoPoint::new(0.0, 97.3),
        TempoPoint::new(4001.0, 97.3),
        TempoPoint::new(4001.0, 151.7),
    ]);
    loop_test(1000.0, 6913.0, tempo, 256);
}

#[test]
fn a_loop_over_a_ramp_does_not_drift() {
    let tempo = TempoMap::new([TempoPoint::new(0.0, 83.0), TempoPoint::new(9611.0, 171.0)]);
    loop_test(1537.0, 7883.0, tempo, 256);
}

#[test]
fn a_loop_from_tick_zero_over_a_tempo_step_does_not_drift() {
    let tempo = TempoMap::new([
        TempoPoint::new(0.0, 133.0),
        TempoPoint::new(1500.0, 133.0),
        TempoPoint::new(1500.0, 71.0),
    ]);
    loop_test(0.0, 4007.0, tempo, 128);
}

#[test]
fn a_loop_from_tick_zero_over_a_ramp_does_not_drift() {
    let tempo = TempoMap::new([TempoPoint::new(0.0, 83.0), TempoPoint::new(9611.0, 171.0)]);
    loop_test(0.0, 7901.0, tempo, 256);
}

#[test]
fn tempo_points_are_crossed_without_losing_time() {
    // A chain of tempo steps, each landing mid-frame. A block cut short at each one must not
    // shed the fraction of a frame.
    let points: Vec<TempoPoint> = (0..400)
        .flat_map(|i| {
            let bpm = 90.0 + (i % 37) as f64 * 3.3;
            let tick = 13.37 * i as f64;
            [TempoPoint::new(tick, bpm), TempoPoint::new(tick, bpm + 1.5)]
        })
        .collect();
    let tempo = TempoMap::new(points);
    let (mut engine, mut h) = engine_with(TimeMaps {
        tempo: tempo.clone(),
        bars: BarMap::default(),
    });
    send(&mut h, TransportCmd::Play);
    let frames = 240_000;
    run_blocks(&mut engine, frames, 256);
    let want = tempo.seconds_to_tick(frames as f64 / SR);
    let got = h.shared.tick.load();
    let err = (tempo.tick_to_seconds(got) - tempo.tick_to_seconds(want)).abs() * SR;
    assert!(err < 1e-3, "lost {err} frames (tick {got}, want {want})");
}

#[test]
fn swapping_maps_is_allocation_free_and_retires_the_old_ones() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    send(&mut h, TransportCmd::Play);
    assert_no_alloc(|| engine.process_stereo(&mut l, &mut r));
    assert_eq!(h.collect_garbage(), 0);

    let maps = Arc::new(TimeMaps {
        tempo: TempoMap::new([TempoPoint::new(0.0, 100.0), TempoPoint::new(1920.0, 150.0)]),
        bars: BarMap::new(sig(3, 4), [(2880, sig(7, 8))]),
    });
    send(&mut h, TransportCmd::SetTimeMaps(maps.clone()));
    send(&mut h, TransportCmd::Seek(0.0));
    assert_no_alloc(|| engine.process_stereo(&mut l, &mut r));
    let bpm = h.shared.tempo.load();
    assert!((100.0..101.0).contains(&bpm), "the new map plays: {bpm}");
    assert_eq!(h.collect_garbage(), 1, "the old maps come back");
    drop(maps);

    // Playing across the swapped-in points allocates nothing, ramps and block splits included.
    send(&mut h, TransportCmd::SetLoop(Some((1000.0, 4000.0))));
    send(&mut h, TransportCmd::Seek(900.0));
    for _ in 0..400 {
        assert_no_alloc(|| engine.process_stereo(&mut l, &mut r));
    }
    h.collect_garbage();

    // In-place edits of a constant map allocate nothing either.
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    send(&mut h, TransportCmd::SetTempo(90.0));
    send(&mut h, TransportCmd::SetTimeSignature(sig(5, 4)));
    assert_no_alloc(|| engine.process_stereo(&mut l, &mut r));
    assert_eq!(h.collect_garbage(), 0);
    assert_eq!(h.shared.tempo.load(), 90.0);
}

#[test]
fn set_tempo_does_not_move_an_automated_map() {
    let (mut engine, mut h) = engine_with(TimeMaps {
        tempo: TempoMap::new([TempoPoint::new(0.0, 110.0), TempoPoint::new(960.0, 90.0)]),
        bars: BarMap::default(),
    });
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    engine.process_stereo(&mut l, &mut r);
    send(&mut h, TransportCmd::SetTempo(60.0));
    engine.process_stereo(&mut l, &mut r);
    assert_eq!(h.shared.tempo.load(), 110.0);
}

/// A constant map starts at the same place the engine's old transport fields did.
#[test]
fn a_fresh_engine_runs_at_120_in_four_four() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    send(&mut h, TransportCmd::SetMetronome(true));
    send(&mut h, TransportCmd::Play);
    let got = onsets(&run_blocks(&mut engine, 200_000, 256));
    let frames: Vec<usize> = got.iter().map(|(i, _)| i - 1).collect();
    let spaced = frames.iter().map(|i| *i as f64).collect::<Vec<_>>();
    assert_eq!(spaced[..4], [0.0, 24_000.0, 48_000.0, 72_000.0]);
}
