use rlive_core::{
    AudioBuffer, MidiEvent, MidiMessage, ParamOutQueue, ProcessContext, Processor, TransportInfo,
};
use rlive_devices::*;
use std::sync::Arc;

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;

fn render(p: &mut dyn Processor, input: f32, events: &[MidiEvent], blocks: usize) -> Vec<f32> {
    let transport = TransportInfo {
        playing: true,
        sample_rate: SR,
        ..Default::default()
    };
    let mut out = Vec::new();
    let mut q = ParamOutQueue::with_capacity(16);
    for b in 0..blocks {
        let mut l = vec![input; BLOCK];
        let mut r = vec![input; BLOCK];
        let ev: &[MidiEvent] = if b == 0 { events } else { &[] };
        p.process(&mut ProcessContext {
            audio: AudioBuffer {
                left: &mut l,
                right: &mut r,
            },
            events: ev,
            transport: &transport,
            param_out: &mut q,
        });
        out.extend_from_slice(&l);
    }
    out
}

fn note_on(key: u8) -> MidiEvent {
    MidiEvent::new(
        0,
        MidiMessage::NoteOn {
            channel: 0,
            key,
            velocity: 1.0,
        },
    )
}

/// Estimate the fundamental from rising zero crossings.
fn estimate_hz(buf: &[f32]) -> f64 {
    let crossings = buf.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
    crossings as f64 * SR / buf.len() as f64
}

#[test]
fn every_builtin_processes_finite_audio() {
    for info in builtins() {
        let mut p = (info.create)(&DeviceInit::default());
        p.activate(SR, BLOCK);
        // Defaults must be valid param values.
        for param in (info.params)() {
            assert!(
                param.default >= param.min && param.default <= param.max,
                "{} {}",
                info.id,
                param.name
            );
            p.set_param(param.id, param.default);
        }
        let out = render(&mut *p, 0.1, &[note_on(60)], 20);
        assert!(
            out.iter().all(|s| s.is_finite()),
            "{} produced NaN",
            info.id
        );
    }
}

#[test]
fn poly_synth_plays_correct_pitch() {
    let mut synth = synth::PolySynth::new();
    synth.activate(SR, BLOCK);
    // Pure sine, filter wide open, no noise.
    synth.set_param(synth::p::OSC1_WAVE, 3.0);
    synth.set_param(synth::p::OSC_MIX, 0.0);
    synth.set_param(synth::p::CUTOFF, 20_000.0);
    synth.set_param(synth::p::FILTER_ENV, 0.0);
    synth.set_param(synth::p::KEY_TRACK, 0.0);
    let out = render(&mut synth, 0.0, &[note_on(69)], 94); // ~1 s
    let hz = estimate_hz(&out[4800..]);
    assert!((hz - 440.0).abs() < 3.0, "{hz}");
    assert!(out.iter().any(|s| s.abs() > 0.05));
}

#[test]
fn poly_synth_releases() {
    let mut synth = synth::PolySynth::new();
    synth.activate(SR, BLOCK);
    synth.set_param(synth::p::AMP_R, 10.0);
    render(&mut synth, 0.0, &[note_on(60)], 10);
    let off = MidiEvent::new(
        0,
        MidiMessage::NoteOff {
            channel: 0,
            key: 60,
            velocity: 0.0,
        },
    );
    let out = render(&mut synth, 0.0, &[off], 20);
    assert!(out[out.len() - 100..].iter().all(|s| s.abs() < 1e-4));
}

#[test]
fn sampler_plays_sample_at_pitch() {
    // 1 s of 100 Hz sine.
    let n = SR as usize;
    let sine: Vec<f32> = (0..n)
        .map(|i| (i as f32 / SR as f32 * 100.0 * std::f32::consts::TAU).sin())
        .collect();
    let sample = Arc::new(SampleBuffer {
        name: "sine".into(),
        left: sine.clone(),
        right: sine,
        sample_rate: SR,
    });
    let mut s = sampler::Sampler::new(Some(sample));
    s.activate(SR, BLOCK);
    // One octave up from root 60.
    let out = render(&mut s, 0.0, &[note_on(72)], 40);
    let hz = estimate_hz(&out[1000..20_000]);
    assert!((hz - 200.0).abs() < 3.0, "{hz}");
}

#[test]
fn delay_echoes() {
    let mut d = effects::Delay::default();
    d.activate(SR, BLOCK);
    d.set_param(effects::delay::SYNC, 0.0);
    d.set_param(effects::delay::TIME, 100.0);
    d.set_param(effects::delay::MIX, 1.0);
    d.set_param(effects::delay::PING_PONG, 0.0);
    d.set_param(effects::delay::FEEDBACK, 0.0);
    // Impulse then silence.
    let transport = TransportInfo::default();
    let mut q = ParamOutQueue::with_capacity(4);
    let mut l = vec![0.0; 9600];
    let mut r = vec![0.0; 9600];
    l[0] = 1.0;
    r[0] = 1.0;
    d.process(&mut ProcessContext {
        audio: AudioBuffer {
            left: &mut l,
            right: &mut r,
        },
        events: &[],
        transport: &transport,
        param_out: &mut q,
    });
    let peak = l
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
        .unwrap()
        .0;
    assert!((peak as i64 - 4800).abs() < 5, "{peak}");
}

#[test]
fn compressor_reduces_loud_signal() {
    let mut c = effects::Compressor::default();
    c.activate(SR, BLOCK);
    c.set_param(effects::comp::THRESHOLD, -20.0);
    c.set_param(effects::comp::RATIO, 10.0);
    c.set_param(effects::comp::KNEE, 0.0);
    let out = render(&mut c, 0.5, &[], 40);
    // 0.5 ≈ -6 dB, 14 dB over → ~12.6 dB reduction.
    let last = *out.last().unwrap();
    assert!(last < 0.15 && last > 0.08, "{last}");
}

#[test]
fn wav_roundtrip() {
    let dir = std::env::temp_dir().join(format!("rlive-wav-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("t.wav");
    let l: Vec<f32> = (0..100).map(|i| i as f32 / 100.0).collect();
    let r: Vec<f32> = l.iter().map(|x| -x).collect();
    write_wav(&path, &l, &r, 44_100).unwrap();
    let s = load_wav(&path).unwrap();
    assert_eq!(s.left, l);
    assert_eq!(s.right, r);
    assert_eq!(s.sample_rate, 44_100.0);
    std::fs::remove_dir_all(&dir).ok();
}
