use crate::dsp::lookahead::*;
use crate::dsp::util::Rng;

#[test]
fn the_delay_is_sample_exact() {
    for len in [0usize, 1, 7, 480] {
        let mut l = LookaheadBuffer::<1>::new(1_024);
        l.set_len(len);
        let mut rng = Rng::new(1);
        let input: Vec<f32> = (0..4_096).map(|_| rng.bipolar()).collect();
        let output: Vec<f32> = input.iter().map(|x| l.process([*x])[0]).collect();
        assert_eq!(l.latency(), len as u32);
        for t in 0..input.len() - len {
            assert_eq!(output[t + len], input[t]);
        }
        assert!(output[..len].iter().all(|x| *x == 0.0));
    }
}

#[test]
fn channels_stay_together() {
    let mut l = LookaheadBuffer::<2>::new(16);
    l.set_len(3);
    l.process([1.0, 2.0]);
    l.process([3.0, 4.0]);
    l.process([5.0, 6.0]);
    assert_eq!(l.process([7.0, 8.0]), [1.0, 2.0]);
}

#[test]
fn the_window_peak_sees_every_sample_about_to_leave() {
    let len = 37;
    let mut l = LookaheadBuffer::<2>::new(128);
    l.set_len(len);
    let mut rng = Rng::new(9);
    let mut history: Vec<[f32; 2]> = Vec::new();
    for _ in 0..20_000 {
        let frame = [rng.bipolar(), rng.bipolar() * 0.5];
        history.push(frame);
        l.process(frame);
        let from = history.len().saturating_sub(len + 1);
        let want = history[from..]
            .iter()
            .flatten()
            .fold(0.0f32, |m, s| m.max(s.abs()));
        assert_eq!(l.window_peak(), want);
    }
}

#[test]
fn a_peak_is_seen_exactly_len_frames_before_it_leaves() {
    let len = 10;
    let mut l = LookaheadBuffer::<1>::new(32);
    l.set_len(len);
    for _ in 0..50 {
        l.process([0.0]);
    }
    let out = l.process([1.0]);
    assert_eq!(out, [0.0]);
    assert_eq!(l.window_peak(), 1.0, "seen as soon as it goes in");
    for _ in 0..len - 1 {
        assert_eq!(l.process([0.0]), [0.0]);
        assert_eq!(l.window_peak(), 1.0);
    }
    assert_eq!(l.process([0.0]), [1.0]);
    assert_eq!(l.window_peak(), 1.0, "still counted as it leaves");
    l.process([0.0]);
    assert_eq!(l.window_peak(), 0.0, "and forgotten after");
}

#[test]
fn changing_the_length_keeps_the_window_right() {
    let mut l = LookaheadBuffer::<1>::new(64);
    l.set_len(8);
    for i in 0..30 {
        l.process([i as f32 + 1.0]);
    }
    l.set_len(3);
    assert_eq!(l.window_peak(), 30.0);
    l.process([0.0]);
    assert_eq!(l.window_peak(), 30.0);
    // Shrink the window past the loud part.
    l.set_len(0);
    assert_eq!(l.window_peak(), 0.0);
}

#[test]
fn reset_forgets_everything() {
    let mut l = LookaheadBuffer::<1>::new(8);
    l.set_len(4);
    l.process([1.0]);
    l.reset();
    assert_eq!(l.window_peak(), 0.0);
    for _ in 0..4 {
        assert_eq!(l.process([0.0]), [0.0]);
    }
}
