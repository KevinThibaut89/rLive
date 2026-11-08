use super::SR;
use crate::dsp::spectrum::*;
use rlive_core::{ViewChannel, ViewId, ViewWriter, SCOPE_VALUES};
use rlive_testkit::sine;
use std::sync::Arc;

fn band_of(hz: f32) -> usize {
    let n = SR as f32 * 0.5;
    ((hz / 20.0).ln() / (n / 20.0).ln() * SCOPE_VALUES as f32).floor() as usize
}

#[test]
fn a_sine_lights_its_band_at_full_scale() {
    let channel = Arc::new(ViewChannel::new(SCOPE_VALUES));
    let reader = channel.attach();
    let view = ViewWriter::new(&channel);
    let mut feed = SpectrumFeed::new(SR, 4_096, 1_024);
    let tone = sine(1_000.0, SR, 48_000);
    feed.process(&tone, &view, ViewId(0));

    let mut out = [0.0f32; SCOPE_VALUES];
    assert!(reader.read(ViewId(0), &mut out));
    let band = band_of(1_000.0);
    let (max_at, max) =
        out.iter().enumerate().fold(
            (0, 0.0f32),
            |acc, (i, v)| {
                if *v > acc.1 {
                    (i, *v)
                } else {
                    acc
                }
            },
        );
    assert!(
        (max_at as i32 - band as i32).abs() <= 1,
        "{max_at} vs {band}"
    );
    assert!((max - 1.0).abs() < 0.06, "{max}");
    assert!(out[band_of(100.0)] < 0.5, "far bands stay low");
}

#[test]
fn six_db_down_reads_six_db_down() {
    let channel = Arc::new(ViewChannel::new(SCOPE_VALUES));
    let reader = channel.attach();
    let view = ViewWriter::new(&channel);
    let mut feed = SpectrumFeed::new(SR, 4_096, 1_024);
    let tone: Vec<f32> = sine(2_000.0, SR, 48_000).iter().map(|x| x * 0.5).collect();
    feed.process(&tone, &view, ViewId(0));
    let mut out = [0.0f32; SCOPE_VALUES];
    reader.read(ViewId(0), &mut out);
    let got = out[band_of(2_000.0)];
    let want = (96.0 - 6.02) / 96.0;
    assert!((got - want).abs() < 0.03, "{got} vs {want}");
}

#[test]
fn nothing_is_measured_with_no_reader() {
    let channel = ViewChannel::new(SCOPE_VALUES);
    let view = ViewWriter::new(&channel);
    let mut feed = SpectrumFeed::new(SR, 1_024, 256);
    feed.process(&sine(1_000.0, SR, 4_800), &view, ViewId(0));
    assert_eq!(feed.bands().iter().copied().fold(0.0, f32::max), 0.0);
    feed.process(&[0.0; 16], &ViewWriter::none(), ViewId(0));
}

#[test]
fn stereo_is_summed_to_mono() {
    let channel = Arc::new(ViewChannel::new(SCOPE_VALUES));
    let _reader = channel.attach();
    let view = ViewWriter::new(&channel);
    let mut feed = SpectrumFeed::new(SR, 1_024, 256);
    let tone = sine(1_000.0, SR, 4_800);
    let inverted: Vec<f32> = tone.iter().map(|x| -x).collect();
    feed.process_stereo(&tone, &inverted, &view, ViewId(0));
    assert_eq!(feed.bands().iter().copied().fold(0.0, f32::max), 0.0);
}
