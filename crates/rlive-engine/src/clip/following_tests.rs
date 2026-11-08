//! Warped clips against the timeline: Re-Pitch and Beats through the real stretchers.

use super::audio::{render_audio, AudioCursor};
use super::data::{AudioClip, WarpedClip};
use super::BlockTime;
use rlive_core::time::ticks_per_sample;
use rlive_core::warp::{BeatsLoop, BeatsPreserve, StretchBank, WarpMap, WarpMarker, WarpMode};
use rlive_core::{SampleBuffer, PPQ};
use rlive_stretch::bank;
use rlive_testkit::{assert_no_alloc, max_step, peak, rms};
use std::sync::Arc;

const SR: f64 = 48_000.0;
const BEAT: f64 = PPQ as f64;
/// Four beats at 120 BPM, the native length of every test sample.
const FRAMES: usize = 96_000;

fn beats_mode(beats_loop: BeatsLoop, transient_envelope: f32) -> WarpMode {
    WarpMode::Beats {
        preserve: BeatsPreserve::Transients,
        beats_loop,
        transient_envelope,
    }
}

fn straight_map(frames: usize, ticks: f64) -> Option<WarpMap> {
    WarpMap::new([
        WarpMarker::new(0.0, 0.0),
        WarpMarker::new(frames as f64, ticks),
    ])
}

fn tone(hz: f64) -> Arc<SampleBuffer> {
    let left: Vec<f32> = (0..FRAMES)
        .map(|i| (i as f64 / SR * hz * std::f64::consts::TAU).sin() as f32 * 0.5)
        .collect();
    Arc::new(SampleBuffer {
        name: "tone".into(),
        right: left.clone(),
        left,
        sample_rate: SR,
    })
}

fn hits(positions: &[usize]) -> Arc<SampleBuffer> {
    let mut left = vec![0.0f32; FRAMES];
    for &at in positions {
        for (i, s) in left[at..].iter_mut().take(4000).enumerate() {
            let env = (-(i as f32) / 800.0).exp();
            *s += env * if i % 2 == 0 { 0.8 } else { -0.8 };
        }
    }
    Arc::new(SampleBuffer {
        name: "hits".into(),
        right: left.clone(),
        left,
        sample_rate: SR,
    })
}

fn clip(sample: Arc<SampleBuffer>, mode: WarpMode, slices: &[usize]) -> WarpedClip {
    WarpedClip {
        audio: AudioClip {
            scale: None,
            sample,
            length: 4 * PPQ,
            looped: false,
            gain: 1.0,
        },
        map: straight_map(FRAMES, 4.0 * BEAT),
        mode,
        transpose: 0.0,
        slices: Arc::from(
            slices
                .iter()
                .map(|f| *f as f64)
                .collect::<Vec<_>>()
                .as_slice(),
        ),
    }
}

fn block(t0: f64, frames: usize, tempo: f64) -> BlockTime {
    let tps = ticks_per_sample(tempo, SR);
    BlockTime {
        t0,
        ticks_per_sample: tps,
        ramp: 0.0,
        end: t0 + frames as f64 * tps,
        frames,
        sample_rate: SR,
        tempo,
    }
}

struct Player {
    bank: StretchBank,
    cursor: AudioCursor,
    tick: f64,
}

impl Player {
    fn new() -> Self {
        let mut bank = bank(4096);
        bank.prepare(SR);
        let cursor = AudioCursor::new(0);
        Self {
            bank,
            cursor,
            tick: 0.0,
        }
    }

    fn block(&mut self, c: &WarpedClip, frames: usize, tempo: f64) -> (Vec<f32>, Vec<f32>) {
        let (mut l, mut r) = (vec![0.0; frames], vec![0.0; frames]);
        self.render_into(c, frames, tempo, &mut l, &mut r);
        (l, r)
    }

    fn render_into(
        &mut self,
        c: &WarpedClip,
        frames: usize,
        tempo: f64,
        l: &mut [f32],
        r: &mut [f32],
    ) {
        let time = block(self.tick, frames, tempo);
        let seg_end = if c.audio.looped {
            time.end
        } else {
            time.end.min(c.audio.length as f64)
        };
        render_audio(
            &c.audio,
            Some(c),
            0.0,
            self.tick,
            seg_end,
            time,
            &mut self.cursor,
            l,
            r,
            &mut self.bank,
        );
        self.tick = time.end;
    }

    /// Plays `frames` in blocks of `blocks` frames at `tempo`.
    fn play(&mut self, c: &WarpedClip, frames: usize, tempo: f64, blocks: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(frames);
        while out.len() < frames {
            let n = blocks.min(frames - out.len());
            out.extend_from_slice(&self.block(c, n, tempo).0);
        }
        out
    }
}

fn frames_for(ticks: f64, tempo: f64) -> usize {
    (ticks / ticks_per_sample(tempo, SR)).round() as usize
}

/// Frames where a burst starts: a level at least `ratio` of the peak after a quiet run.
fn onsets(signal: &[f32], ratio: f32, quiet: usize) -> Vec<usize> {
    let top = peak(signal);
    let mut found = Vec::new();
    let mut silent_run = quiet;
    for (i, s) in signal.iter().enumerate() {
        if s.abs() >= top * ratio {
            if silent_run >= quiet {
                found.push(i);
            }
            silent_run = 0;
        } else {
            silent_run += 1;
        }
    }
    found
}

/// The average frequency of a signal, from its upward zero crossings.
fn measured_hz(signal: &[f32]) -> f64 {
    let mut crossings = Vec::new();
    for (i, w) in signal.windows(2).enumerate() {
        if w[0] <= 0.0 && w[1] > 0.0 {
            crossings.push(i as f64 + (-w[0] / (w[1] - w[0])) as f64);
        }
    }
    let (first, last) = (crossings[0], *crossings.last().unwrap());
    (crossings.len() - 1) as f64 / (last - first) * SR
}

#[test]
fn re_pitch_follows_the_tempo_exactly_at_several_tempos() {
    let c = clip(hits(&[90_000]), WarpMode::RePitch, &[]);
    for tempo in [60.0, 90.0, 120.0, 137.5, 174.0, 240.0] {
        let total = frames_for(4.0 * BEAT, tempo);
        let mut p = Player::new();
        let out = p.play(&c, total + 2000, tempo, 512);
        assert!((p.cursor.src - 96_000.0).abs() < 2.0, "at {tempo}");
        let at = onsets(&out, 0.25, 500);
        assert_eq!(at.len(), 1, "at {tempo}: {at:?}");
        let want = 90_000.0 * 120.0 / tempo;
        assert!(
            (at[0] as f64 - want).abs() <= 3.0,
            "at {tempo}: heard {} wanted {want}",
            at[0]
        );
    }
}

#[test]
fn beats_follows_the_tempo_exactly_at_several_tempos() {
    let positions = [12_000, 36_000, 60_000, 84_000];
    let c = clip(
        hits(&positions),
        beats_mode(BeatsLoop::Off, 100.0),
        &positions,
    );
    for tempo in [60.0, 90.0, 120.0, 174.0, 240.0] {
        let total = frames_for(4.0 * BEAT, tempo);
        let mut p = Player::new();
        let out = p.play(&c, total + 2000, tempo, 512);
        assert!((p.cursor.src - 96_000.0).abs() < 2.0, "at {tempo}");
        let at = onsets(&out, 0.25, 400);
        assert_eq!(at.len(), positions.len(), "at {tempo}: {at:?}");
        for (got, want) in at.iter().zip(positions) {
            let want = want as f64 * 120.0 / tempo;
            assert!(
                (*got as f64 - want).abs() <= 60.0,
                "at {tempo}: heard {got} wanted {want}"
            );
        }
    }
}

#[test]
fn a_tempo_change_mid_clip_stays_locked_to_the_timeline() {
    let positions = [12_000, 36_000, 60_000, 84_000];
    for mode in [WarpMode::RePitch, beats_mode(BeatsLoop::Off, 100.0)] {
        let slices: &[usize] = if mode == WarpMode::RePitch {
            &[]
        } else {
            &positions
        };
        let c = clip(hits(&positions), mode, slices);
        let mut p = Player::new();
        // Two beats at 120, then the rest at 87.
        let first = frames_for(2.0 * BEAT, 120.0);
        let second = frames_for(2.0 * BEAT, 87.0);
        let mut out = p.play(&c, first, 120.0, 300);
        out.extend(p.play(&c, second + 500, 87.0, 300));
        assert!((p.cursor.src - 96_000.0).abs() < 2.0);
        let at = onsets(&out, 0.25, 400);
        assert_eq!(at.len(), positions.len(), "{mode:?}: {at:?}");
        for (got, src) in at.iter().zip(positions) {
            let tick = src as f64 / 96_000.0 * 4.0 * BEAT;
            let want = if tick <= 2.0 * BEAT {
                tick / ticks_per_sample(120.0, SR)
            } else {
                first as f64 + (tick - 2.0 * BEAT) / ticks_per_sample(87.0, SR)
            };
            assert!(
                (*got as f64 - want).abs() <= 60.0,
                "{mode:?}: heard {got} wanted {want}"
            );
        }
    }
}

#[test]
fn the_tempo_can_ramp_without_losing_the_timeline() {
    let c = clip(tone(440.0), WarpMode::RePitch, &[]);
    let mut p = Player::new();
    // A block whose tempo grows: 120 BPM rising through the block.
    let frames = 4096;
    let time = {
        let mut t = block(p.tick, frames, 120.0);
        t.ramp = 4e-6;
        t.end = t0_plus(&t, frames);
        t
    };
    let (mut l, mut r) = (vec![0.0; frames], vec![0.0; frames]);
    render_audio(
        &c.audio,
        Some(&c),
        0.0,
        0.0,
        time.end,
        time,
        &mut p.cursor,
        &mut l,
        &mut r,
        &mut p.bank,
    );
    let want = c.map.as_ref().unwrap().tick_to_src(time.end);
    assert!((p.cursor.src - want).abs() < 1e-6);
}

fn t0_plus(t: &BlockTime, frames: usize) -> f64 {
    t.tick_at(frames)
}

#[test]
fn re_pitch_changes_pitch_with_speed_and_ignores_transpose() {
    let hz = 440.0;
    for (tempo, ratio) in [(120.0, 1.0), (240.0, 2.0), (60.0, 0.5), (90.0, 0.75)] {
        let mut c = clip(tone(hz), WarpMode::RePitch, &[]);
        c.transpose = 7.0;
        let mut p = Player::new();
        let total = frames_for(4.0 * BEAT, tempo).min(40_000);
        let out = p.play(&c, total, tempo, 480);
        let got = measured_hz(&out[2000..]);
        let want = hz * ratio;
        assert!(
            (got / want - 1.0).abs() < 0.0006,
            "tempo {tempo}: {got} Hz, wanted {want}"
        );
    }
}

#[test]
fn beats_keeps_pitch_when_the_tempo_changes_and_transposes_on_request() {
    let hz = 440.0;
    let slices: Vec<usize> = (0..16).map(|i| i * 6000).collect();
    for transpose in [0.0, 12.0, 7.0] {
        for tempo in [100.0, 150.0] {
            let mut c = clip(tone(hz), beats_mode(BeatsLoop::Forward, 100.0), &slices);
            c.transpose = transpose;
            let mut p = Player::new();
            let out = p.play(&c, 30_000, tempo, 512);
            // Inside one slice, which plays at its own speed however fast the timeline goes.
            let got = measured_hz(&out[200..4000]);
            let want = hz * 2f64.powf(transpose as f64 / 12.0);
            assert!(
                (got / want - 1.0).abs() < 0.01,
                "tempo {tempo} transpose {transpose}: {got} Hz, wanted {want}"
            );
        }
    }
}

fn gap_rms(out: &[f32], from: usize, to: usize) -> f32 {
    rms(&out[from..to])
}

#[test]
fn a_source_at_another_sample_rate_keeps_its_pitch_and_length() {
    let rate = 44_100.0;
    let frames = 88_200;
    let left: Vec<f32> = (0..frames)
        .map(|i| (i as f64 / rate * 440.0 * std::f64::consts::TAU).sin() as f32 * 0.5)
        .collect();
    let sample = Arc::new(SampleBuffer {
        name: "tone".into(),
        right: left.clone(),
        left,
        sample_rate: rate,
    });
    let slices: Vec<usize> = (0..16).map(|i| i * 5_512).collect();
    for mode in [WarpMode::RePitch, beats_mode(BeatsLoop::Forward, 100.0)] {
        let used: &[usize] = if mode == WarpMode::RePitch {
            &[]
        } else {
            &slices
        };
        let mut c = clip(sample.clone(), mode, used);
        c.map = straight_map(frames, 4.0 * BEAT);
        let mut p = Player::new();
        let out = p.play(&c, 96_000, 120.0, 512);
        assert!((p.cursor.src - frames as f64).abs() < 2.0, "{mode:?}");
        let got = measured_hz(&out[200..4000]);
        assert!((got / 440.0 - 1.0).abs() < 0.0015, "{mode:?}: {got} Hz");
    }
}

#[test]
fn slice_loop_modes_fill_the_gap_the_tempo_opens() {
    let slice = 12_000usize;
    let slices: Vec<usize> = (0..8).map(|i| i * slice).collect();
    let source = tone(330.0);
    // Half speed: each slice runs out halfway to the next one.
    let tempo = 60.0;
    let gap_of = |mode: BeatsLoop| {
        let c = clip(source.clone(), beats_mode(mode, 100.0), &slices);
        let mut p = Player::new();
        let out = p.play(&c, 96_000, tempo, 512);
        // Slice 1 sits at 24 000..48 000 of the output and sounds for its first 12 000 frames.
        (
            gap_rms(&out, 37_000, 47_000),
            gap_rms(&out, 61_000, 71_000),
            out,
        )
    };
    let (off_a, _, _) = gap_of(BeatsLoop::Off);
    let (forward_a, forward_b, _) = gap_of(BeatsLoop::Forward);
    let (bounce_a, _, bounce) = gap_of(BeatsLoop::BackAndForth);
    assert!(off_a < 1e-4, "off leaves silence: {off_a}");
    assert!(forward_a > 0.2 && forward_b > 0.2, "forward: {forward_a}");
    assert!(bounce_a > 0.15, "back and forth: {bounce_a}");
    assert!(
        max_step(&bounce) < 0.25,
        "turning round has no step: {}",
        max_step(&bounce)
    );
}

#[test]
fn the_loop_is_where_the_timeline_says_every_time_round() {
    let positions = [12_000, 48_000];
    let mut c = clip(
        hits(&positions),
        beats_mode(BeatsLoop::Off, 100.0),
        &positions,
    );
    c.audio.looped = true;
    let mut p = Player::new();
    let out = p.play(&c, 96_000 * 3, 120.0, 777);
    let at = onsets(&out, 0.25, 800);
    let want: Vec<usize> = (0..3)
        .flat_map(|cycle| positions.iter().map(move |f| f + cycle * 96_000))
        .collect();
    assert_eq!(at.len(), want.len(), "{at:?}");
    for (got, want) in at.iter().zip(want) {
        assert!(got.abs_diff(want) <= 40, "{got} {want}");
    }
}

#[test]
fn transient_envelope_fades_each_slice() {
    let slice = 12_000usize;
    let slices: Vec<usize> = (0..8).map(|i| i * slice).collect();
    let source = tone(330.0);
    let level_late = |envelope: f32| {
        let c = clip(
            source.clone(),
            beats_mode(BeatsLoop::Off, envelope),
            &slices,
        );
        let out = Player::new().play(&c, 48_000, 120.0, 512);
        rms(&out[slice + 3000..slice + 5500])
    };
    let full = level_late(100.0);
    let half = level_late(50.0);
    let none = level_late(0.0);
    assert!(full > 0.3, "no fade at 100: {full}");
    assert!(none < 1e-4, "gated at 0: {none}");
    assert!(none < half && half < full, "{none} {half} {full}");
}

#[test]
fn the_block_size_does_not_change_what_a_warped_clip_plays() {
    let positions = [12_000, 36_000, 60_000, 84_000];
    for mode in [WarpMode::RePitch, beats_mode(BeatsLoop::Forward, 90.0)] {
        let used: &[usize] = if mode == WarpMode::RePitch {
            &[]
        } else {
            &positions
        };
        let c = clip(hits(&positions), mode, used);
        let render = |blocks: usize| Player::new().play(&c, 80_000, 133.0, blocks);
        let (a, b) = (render(512), render(97));
        for (i, (x, y)) in a.iter().zip(&b).enumerate() {
            assert!((x - y).abs() < 1e-2, "{mode:?} at {i}: {x} {y}");
        }
    }
}

#[test]
fn seeking_makes_no_click() {
    let slices: Vec<usize> = (0..16).map(|i| i * 6000).collect();
    for mode in [WarpMode::RePitch, beats_mode(BeatsLoop::Forward, 100.0)] {
        let used: &[usize] = if mode == WarpMode::RePitch {
            &[]
        } else {
            &slices
        };
        let c = clip(tone(200.0), mode, used);
        let mut p = Player::new();
        let mut out = p.play(&c, 8_000, 120.0, 512);
        // A jump in the song position that lands far from a zero-crossing.
        p.tick = 1.7 * BEAT;
        out.extend(p.play(&c, 8_000, 120.0, 512));
        let nominal = 0.5 * (200.0 / SR as f32) * std::f32::consts::TAU;
        let jump = max_step(&out);
        assert!(
            jump < nominal * 4.0,
            "{mode:?}: step of {jump} against nominal {nominal}"
        );
    }
}

#[test]
fn nothing_allocates_while_playing_seeking_and_looping() {
    let positions = [12_000, 36_000, 60_000, 84_000];
    for mode in [
        WarpMode::RePitch,
        beats_mode(BeatsLoop::Forward, 80.0),
        beats_mode(BeatsLoop::BackAndForth, 100.0),
        beats_mode(BeatsLoop::Off, 100.0),
    ] {
        let mut c = clip(hits(&positions), mode, &positions);
        c.audio.looped = true;
        let mut p = Player::new();
        let (mut l, mut r) = (vec![0.0; 777], vec![0.0; 777]);
        assert_no_alloc(|| {
            for block in 0..400 {
                let tempo = if block % 3 == 0 { 174.0 } else { 88.0 };
                p.render_into(&c, 777, tempo, &mut l, &mut r);
                if block % 41 == 40 {
                    p.tick += 1234.5;
                }
            }
        });
    }
}

#[test]
fn a_warped_clip_with_no_map_plays_like_warp_off() {
    let source = tone(440.0);
    let audio = AudioClip {
        scale: None,
        sample: source,
        length: 4 * PPQ,
        looped: false,
        gain: 0.5,
    };
    let warped = WarpedClip {
        audio: audio.clone(),
        map: None,
        mode: beats_mode(BeatsLoop::Off, 100.0),
        transpose: 3.0,
        slices: Arc::from(Vec::new()),
    };
    let render = |clip: &AudioClip, warp: Option<&WarpedClip>| {
        let mut p = Player::new();
        let time = block(0.0, 2048, 90.0);
        let (mut l, mut r) = (vec![0.0; 2048], vec![0.0; 2048]);
        render_audio(
            clip,
            warp,
            0.0,
            0.0,
            time.end,
            time,
            &mut p.cursor,
            &mut l,
            &mut r,
            &mut p.bank,
        );
        l
    };
    assert_eq!(render(&audio, None), render(&warped.audio, Some(&warped)));
}

#[test]
fn a_passthrough_mode_still_reads_at_native_speed() {
    let c = clip(tone(440.0), WarpMode::Complex, &[]);
    let mut p = Player::new();
    let out = p.play(&c, 8192, 60.0, 512);
    assert_eq!(p.cursor.src, 8192.0);
    assert!((measured_hz(&out[2000..]) / 440.0 - 1.0).abs() < 0.001);
}
