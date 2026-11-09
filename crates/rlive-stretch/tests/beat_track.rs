//! Tempo and beat tracking on generated music.

use rlive_stretch::{track_beats, BeatGrid};
use std::f64::consts::TAU;

const SR: f64 = 44_100.0;

fn noise(seed: &mut u32) -> f64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 17;
    *seed ^= *seed << 5;
    f64::from(*seed) / f64::from(u32::MAX) * 2.0 - 1.0
}

fn kick(out: &mut [f32], at: usize, gain: f64) {
    for i in 0..(0.18 * SR) as usize {
        let Some(s) = out.get_mut(at + i) else { break };
        let t = i as f64 / SR;
        let pitch = 60.0 + 120.0 * (-t * 40.0).exp();
        *s += ((t * pitch * TAU).sin() * (-t * 18.0).exp() * gain) as f32;
    }
}

fn hit(out: &mut [f32], at: usize, gain: f64, decay: f64, seed: &mut u32) {
    for i in 0..(0.12 * SR) as usize {
        let Some(s) = out.get_mut(at + i) else { break };
        let t = i as f64 / SR;
        *s += (noise(seed) * (-t * decay).exp() * gain) as f32;
    }
}

/// Kick on 1 and 3, snare on 2 and 4, hats on eighths, a held note underneath.
fn four_on_the_floor(bpm: f64, seconds: f64) -> Vec<f32> {
    let frames = (seconds * SR) as usize;
    let mut out = vec![0.0f32; frames];
    let beat = 60.0 / bpm * SR;
    let mut seed = 0x2468_ACE1u32;
    let mut step = 0usize;
    loop {
        let at = (step as f64 * beat * 0.5) as usize;
        if at >= frames {
            break;
        }
        match step % 8 {
            0 => kick(&mut out, at, 0.8),
            4 => kick(&mut out, at, 0.55),
            2 | 6 => hit(&mut out, at, 0.5, 30.0, &mut seed),
            _ => {}
        }
        hit(&mut out, at, 0.12, 180.0, &mut seed);
        step += 1;
    }
    for (i, s) in out.iter_mut().enumerate() {
        *s += ((i as f64 / SR * 110.0 * TAU).sin() * 0.08) as f32;
    }
    out
}

fn tracked(samples: &[f32]) -> BeatGrid {
    track_beats(samples, None, SR, 4).expect("a steady beat is trackable")
}

#[test]
fn the_tempo_is_found_at_tempos_across_the_usual_range() {
    for bpm in [84.0, 92.0, 100.0, 110.0, 120.0, 128.0, 140.0, 150.0, 165.0] {
        let grid = tracked(&four_on_the_floor(bpm, 40.0));
        assert!(
            (grid.bpm / bpm - 1.0).abs() < 0.005,
            "{bpm} BPM came out as {}",
            grid.bpm
        );
        assert!(grid.confidence > 0.1, "{bpm}: {}", grid.confidence);
    }
}

#[test]
fn a_groove_outside_the_reported_octave_comes_back_at_its_octave() {
    // Nothing in the audio separates a groove from its half or double. Whichever is reported, it
    // is the same grid at twice or half the speed, which is what ×2 and ÷2 are for.
    for bpm in [75.0, 178.0] {
        let grid = tracked(&four_on_the_floor(bpm, 40.0));
        let multiples = [0.5, 1.0, 2.0];
        assert!(
            multiples
                .iter()
                .any(|m| (grid.bpm / (bpm * m) - 1.0).abs() < 0.01),
            "{bpm} came out as {}",
            grid.bpm
        );
    }
}

#[test]
fn no_octave_errors_on_plain_quarter_notes() {
    // With only quarter notes, the lower or doubled tempo is the mistake a tracker makes.
    for bpm in [90.0, 128.0, 165.0] {
        let frames = (40.0 * SR) as usize;
        let mut out = vec![0.0f32; frames];
        let beat = 60.0 / bpm * SR;
        let mut seed = 77u32;
        let mut beat_at = 0usize;
        while (beat_at as f64 * beat) < frames as f64 {
            hit(
                &mut out,
                (beat_at as f64 * beat) as usize,
                0.7,
                50.0,
                &mut seed,
            );
            beat_at += 1;
        }
        let grid = tracked(&out);
        assert!(
            (grid.bpm / bpm - 1.0).abs() < 0.01,
            "{bpm} BPM came out as {}",
            grid.bpm
        );
    }
}

#[test]
fn half_time_drums_read_as_the_tempo_or_its_half() {
    // Kick on 1, snare on 3, sixteenths running: 70 and 140 are both fair readings of this.
    let (bpm, frames) = (140.0, (40.0 * SR) as usize);
    let mut out = vec![0.0f32; frames];
    let beat = 60.0 / bpm * SR;
    let mut seed = 99u32;
    let mut step = 0usize;
    while (step as f64 * beat * 0.25) < frames as f64 {
        let at = (step as f64 * beat * 0.25) as usize;
        match step % 16 {
            0 => kick(&mut out, at, 0.8),
            8 => hit(&mut out, at, 0.6, 28.0, &mut seed),
            _ => {}
        }
        hit(&mut out, at, 0.1, 200.0, &mut seed);
        step += 1;
    }
    let grid = tracked(&out);
    assert!(
        [bpm, bpm * 0.5]
            .iter()
            .any(|want| (grid.bpm / want - 1.0).abs() < 0.01),
        "came out as {}",
        grid.bpm
    );
}

#[test]
fn the_beats_land_on_the_beats() {
    let bpm = 128.0;
    let grid = tracked(&four_on_the_floor(bpm, 40.0));
    let beat = 60.0 / bpm;
    for b in &grid.beats {
        let off = ((b / beat).round() * beat - b).abs();
        assert!(off < 0.03, "{b} is {off} from the grid");
    }
    // Nothing skipped: the grid is close to every beat in the file.
    let expected = (40.0 / beat) as usize;
    assert!(
        grid.beats.len() + 6 >= expected,
        "{} of {expected}",
        grid.beats.len()
    );
}

#[test]
fn the_downbeat_is_on_the_kick() {
    let bpm = 120.0;
    let grid = tracked(&four_on_the_floor(bpm, 40.0));
    let beat = 60.0 / bpm;
    let first_bar = grid.bars(4).next().unwrap();
    let beats_in = (first_bar / beat).round() as i64;
    assert_eq!(beats_in % 2, 0, "a bar starts on a snare at {first_bar}");
}

#[test]
fn silence_and_tiny_files_have_no_grid() {
    assert!(track_beats(&vec![0.0; 441_000], None, SR, 4).is_none());
    assert!(track_beats(&vec![0.3; 4_410], None, SR, 4).is_none());
}
