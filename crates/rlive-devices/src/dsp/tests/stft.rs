use super::SR;
use crate::dsp::stft::*;
use crate::dsp::util::Rng;
use rlive_testkit::sine;

fn noise(n: usize) -> Vec<f32> {
    let mut rng = Rng::new(5);
    (0..n).map(|_| rng.bipolar() * 0.8).collect()
}

fn run(stft: &mut Stft, input: &[f32], chunk: usize) -> Vec<f32> {
    let mut output = vec![0.0; input.len()];
    for (i, o) in input.chunks(chunk).zip(output.chunks_mut(chunk)) {
        stft.process(i, o, |_| {});
    }
    output
}

#[test]
fn the_identity_process_reconstructs_perfectly() {
    let n = 20_000;
    let input = noise(n);
    for (size, hop) in [(256usize, 128usize), (1024, 256), (2048, 512), (2048, 256)] {
        for chunk in [1, 37, 512] {
            let mut stft = Stft::new(size, hop);
            let output = run(&mut stft, &input, chunk);
            let latency = stft.latency() as usize;
            assert_eq!(latency, size);
            let mut worst = 0.0f32;
            for t in size..n - latency {
                worst = worst.max((output[t + latency] - input[t]).abs());
            }
            assert!(worst < 1e-4, "size {size} hop {hop} chunk {chunk}: {worst}");
        }
    }
}

#[test]
fn a_spectral_process_is_heard() {
    let mut stft = Stft::new(1024, 256);
    let input = sine(1_000.0, SR, 48_000);
    let mut output = vec![0.0; input.len()];
    stft.process(&input, &mut output, |bins| {
        for b in bins.iter_mut() {
            *b *= 0.5;
        }
    });
    let level = rlive_testkit::rms(&output[8_000..]);
    assert!(
        (level - 0.5 * std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-3,
        "{level}"
    );
}

#[test]
fn zeroing_a_band_removes_it() {
    let mut stft = Stft::new(2048, 512);
    let bin_hz = SR as f32 / 2048.0;
    let tone = sine(5_000.0, SR, 48_000);
    let mut output = vec![0.0; tone.len()];
    let bins = stft.bins();
    stft.process(&tone, &mut output, |spec| {
        let cut = (3_000.0 / bin_hz) as usize;
        spec[cut..bins].fill(Default::default());
    });
    assert!(rlive_testkit::rms(&output[8_000..]) < 1e-3);
}

#[test]
fn the_callback_fires_once_per_hop() {
    let mut stft = Stft::new(256, 64);
    let mut calls = 0;
    let mut out = vec![0.0; 6_400];
    stft.process(&[0.0; 6_400], &mut out, |_| calls += 1);
    assert_eq!(calls, 100);
}

#[test]
#[should_panic(expected = "hop")]
fn a_bad_hop_is_rejected() {
    let _ = Stft::new(256, 100);
}
