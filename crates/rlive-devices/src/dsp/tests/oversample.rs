use super::SR;
use crate::dsp::fft::{Complex32, Fft};
use crate::dsp::oversample::*;
use rlive_testkit::sine;

const FACTORS: [OversampleFactor; 3] = [
    OversampleFactor::X2,
    OversampleFactor::X4,
    OversampleFactor::X8,
];

#[test]
fn off_is_a_passthrough() {
    let mut os = Oversampler::new(OversampleFactor::X1, 128);
    let input = sine(440.0, SR, 128);
    let mut output = vec![0.0; 128];
    os.process(&input, &mut output, |_| {});
    assert_eq!(output, input);
    assert_eq!(os.latency(), 0);
}

#[test]
fn a_path_through_has_exactly_the_reported_latency() {
    for factor in FACTORS {
        let mut os = Oversampler::new(factor, 64);
        let input = sine(1_000.0, SR, 9_600);
        let mut output = vec![0.0; 9_600];
        for (i, o) in input.chunks(64).zip(output.chunks_mut(64)) {
            os.process(i, o, |_| {});
        }
        let latency = os.latency() as usize;
        for t in 2_000..9_000 {
            assert!(
                (output[t + latency] - input[t]).abs() < 1e-3,
                "{factor:?} at {t}: {} vs {}",
                output[t + latency],
                input[t]
            );
        }
    }
}

#[test]
fn the_oversampled_block_is_the_right_length() {
    for factor in FACTORS {
        let mut os = Oversampler::new(factor, 64);
        let mut seen = 0;
        let mut output = vec![0.0; 64];
        os.process(&[0.0; 64], &mut output, |hi| seen = hi.len());
        assert_eq!(seen, 64 * factor.multiplier());
    }
}

#[test]
fn passband_is_flat_and_stopband_is_rejected() {
    for factor in FACTORS {
        for hz in [100.0, 1_000.0, 10_000.0, 18_000.0] {
            let mut os = Oversampler::new(factor, 512);
            let input = sine(hz, SR, 48_000);
            let mut output = vec![0.0; 48_000];
            for (i, o) in input.chunks(512).zip(output.chunks_mut(512)) {
                os.process(i, o, |_| {});
            }
            let gain = rlive_testkit::tone_amplitude(&output[4_096..], hz as f64, SR);
            assert!((gain - 1.0).abs() < 0.02, "{factor:?} at {hz}: {gain}");
        }
    }
}

const NFFT: usize = 1 << 15;
// A bin that isn't a factor of the transform size, so aliases don't land on harmonics.
const TONE_BIN: usize = 2_731;

/// Power of a hard-clipped tone that landed anywhere other than its harmonics.
fn alias_power(mut block: impl FnMut(&[f32], &mut [f32]), hz: f32) -> f64 {
    let input = sine(hz, SR, NFFT * 2);
    let mut output = vec![0.0; NFFT * 2];
    for (i, o) in input.chunks(256).zip(output.chunks_mut(256)) {
        block(i, o);
    }
    let mut fft = Fft::new(NFFT);
    let mut frame = output[NFFT..].to_vec();
    let mut spectrum = vec![Complex32::new(0.0, 0.0); fft.bins()];
    fft.forward(&mut frame, &mut spectrum);
    spectrum
        .iter()
        .enumerate()
        .filter(|(i, _)| i % TONE_BIN != 0)
        .map(|(_, c)| c.norm_sqr() as f64)
        .sum()
}

fn clip_block(x: &mut [f32]) {
    for s in x {
        *s = s.clamp(-0.7, 0.7);
    }
}

#[test]
fn oversampling_rejects_aliases_against_the_direct_path() {
    let hz = TONE_BIN as f32 * SR as f32 / NFFT as f32;
    let reference = alias_power(
        |i, o| {
            o.copy_from_slice(i);
            clip_block(o);
        },
        hz,
    );
    let mut last = reference;
    let mut gains = Vec::new();
    for factor in FACTORS {
        let mut os = Oversampler::new(factor, 256);
        let power = alias_power(|i, o| os.process(i, o, clip_block), hz);
        let db = 10.0 * (reference / power).log10();
        gains.push((factor, db));
        assert!(power < last, "{factor:?} improves on the one before it");
        last = power;
    }
    // Hard clipping has harmonics that decay slowly, so these are a floor on the benefit.
    assert!(gains[0].1 > 12.0, "{gains:?}");
    assert!(gains[1].1 > 20.0, "{gains:?}");
    assert!(gains[2].1 > 22.0, "{gains:?}");
}

#[test]
fn a_soft_saturator_is_much_cleaner_at_4x() {
    let hz = TONE_BIN as f32 * SR as f32 / NFFT as f32;
    let shape = |x: &mut [f32]| {
        for s in x {
            *s = (*s * 3.0).tanh();
        }
    };
    let reference = alias_power(
        |i, o| {
            o.copy_from_slice(i);
            shape(o);
        },
        hz,
    );
    let mut os = Oversampler::new(OversampleFactor::X4, 256);
    let power = alias_power(|i, o| os.process(i, o, shape), hz);
    let db = 10.0 * (reference / power).log10();
    assert!(db > 25.0, "{db} dB");
}

#[test]
fn in_place_matches_separate_buffers() {
    let mut a = Oversampler::new(OversampleFactor::X4, 64);
    let mut b = Oversampler::new(OversampleFactor::X4, 64);
    let input = sine(3_000.0, SR, 512);
    let mut in_place = input.clone();
    let mut output = vec![0.0; 512];
    for chunk in in_place.chunks_mut(64) {
        a.process_in_place(chunk, clip_block);
    }
    for (i, o) in input.chunks(64).zip(output.chunks_mut(64)) {
        b.process(i, o, clip_block);
    }
    assert_eq!(in_place, output);
}

#[test]
fn reset_clears_the_filters() {
    let mut os = Oversampler::new(OversampleFactor::X8, 64);
    let mut out = vec![0.0; 64];
    os.process(&[1.0; 64], &mut out, |_| {});
    os.reset();
    let mut out = vec![1.0; 64];
    os.process(&[0.0; 64], &mut out, |_| {});
    assert!(out.iter().all(|x| x.abs() < 1e-6));
}
