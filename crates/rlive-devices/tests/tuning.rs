//! Tuning systems reach every instrument through the note-on's tuning: a tuned note must sound at
//! the tuned pitch, and with no tuning nothing changes.

use rlive_core::{MidiEvent, MidiMessage, Processor, SampleBuffer, TuningTable, TUNING_SLOT};
use rlive_devices::{find, DeviceInit};
use rlive_testkit::{assert_no_alloc, Harness};
use std::sync::Arc;

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;

fn sine() -> Arc<SampleBuffer> {
    // A second of A3 at the rate the samplers will read it, so key 69 plays it at unity.
    let left: Vec<f32> = (0..48_000)
        .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin())
        .collect();
    Arc::new(SampleBuffer {
        name: "sine".into(),
        right: left.clone(),
        left,
        sample_rate: 48_000.0,
    })
}

fn create(id: &str, samples: Vec<Option<Arc<SampleBuffer>>>) -> Box<dyn Processor> {
    let info = find(id).unwrap_or_else(|| panic!("no device {id}"));
    let desc = (info.desc)();
    let mut p = (info.create)(&DeviceInit {
        samples,
        state: None,
    });
    p.activate(SR, BLOCK);
    for param in &desc.params {
        p.set_param(param.id, param.default);
    }
    p
}

/// Plays one held note and returns the sound.
fn render(p: &mut dyn Processor, key: u8, tuning: f32, frames: usize) -> Vec<f32> {
    let mut h = Harness::new(SR);
    let on = [MidiEvent::new(
        0,
        MidiMessage::NoteOn {
            channel: 0,
            key,
            velocity: 1.0,
            note_id: 1,
            tuning,
        },
    )];
    let mut out = Vec::with_capacity(frames);
    for b in 0..frames.div_ceil(BLOCK) {
        let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
        let ev: &[MidiEvent] = if b == 0 { &on } else { &[] };
        h.process(p, &mut l, &mut r, ev, &[]);
        out.extend_from_slice(&l);
    }
    out
}

/// The period of `x`, from the strongest autocorrelation peak in a musical range.
fn detected_hz(x: &[f32]) -> f64 {
    let x = &x[x.len() / 3..];
    let energy: f32 = x.iter().map(|s| s * s).sum();
    assert!(energy > 1e-6, "silence");
    let (low, high) = ((SR / 2_000.0) as usize, (SR / 40.0) as usize);
    let correlation = |lag: usize| -> f64 {
        x[..x.len() - lag]
            .iter()
            .zip(&x[lag..])
            .map(|(a, b)| f64::from(a * b))
            .sum()
    };
    let (mut best, mut best_at) = (f64::MIN, low);
    for lag in low..high {
        let c = correlation(lag);
        if c > best {
            (best, best_at) = (c, lag);
        }
    }
    // Refine with a parabola through the peak, so that high notes aren't stuck on whole frames.
    let (a, b, c) = (
        correlation(best_at - 1),
        correlation(best_at),
        correlation(best_at + 1),
    );
    let shift = 0.5 * (a - c) / (a - 2.0 * b + c);
    SR / (best_at as f64 + shift)
}

fn cents(hz: f64, expected: f64) -> f64 {
    1200.0 * (hz / expected).log2()
}

#[test]
fn every_voiced_instrument_sounds_at_its_tuned_pitch() {
    let cases: Vec<(&str, Vec<Option<Arc<SampleBuffer>>>)> = vec![
        ("rlive.poly", vec![]),
        ("rlive.juno106", vec![]),
        ("rlive.drift", vec![]),
        ("rlive.analog", vec![]),
        ("rlive.meld", vec![]),
        ("rlive.wavetable", vec![]),
        ("rlive.simpler", vec![Some(sine())]),
        ("rlive.sampler", vec![Some(sine())]),
    ];
    for (id, samples) in cases {
        let base = detected_hz(&render(&mut *create(id, samples.clone()), 60, 0.0, 36_000));
        let tuned = detected_hz(&render(&mut *create(id, samples.clone()), 60, 5.0, 36_000));
        let want = base * 2f64.powf(5.0 / 12.0);
        let off = cents(tuned, want);
        assert!(off.abs() < 12.0, "{id}: {off} cents off a fourth up");
        // Below a semitone a fraction has to come through too, or scales would collapse.
        let quarter = detected_hz(&render(&mut *create(id, samples.clone()), 60, 0.5, 36_000));
        let off = cents(quarter, base * 2f64.powf(0.5 / 12.0));
        assert!(off.abs() < 12.0, "{id}: {off} cents off a quarter tone");
    }
}

#[test]
fn a_tuned_note_stays_allocation_free() {
    let mut p = create("rlive.drift", vec![]);
    let mut h = Harness::new(SR);
    let on = [MidiEvent::new(
        0,
        MidiMessage::NoteOn {
            channel: 0,
            key: 60,
            velocity: 1.0,
            note_id: 1,
            tuning: 0.37,
        },
    )];
    let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
    assert_no_alloc(|| h.process(&mut *p, &mut l, &mut r, &on, &[]));
}

fn peak_near(signal: &[f32], hz: f64) -> f64 {
    let mut best = (0.0f64, hz);
    let mut c = -700.0;
    while c <= 700.0 {
        let f = hz * 2f64.powf(c / 1200.0);
        let a = rlive_testkit::tone_amplitude(signal, f, SR);
        if a > best.0 {
            best = (a, f);
        }
        c += 2.0;
    }
    best.1
}

#[test]
fn a_tuning_system_can_be_handed_to_a_device_that_reads_it() {
    let set_voice_one = |p: &mut dyn Processor| {
        use rlive_devices::devices::b7::resonators as res;
        for (id, v) in [
            (res::ON2, 0.0),
            (res::ON3, 0.0),
            (res::ON4, 0.0),
            (res::ON5, 0.0),
            (res::NOTE, 60.0),
            (res::COLOR, 100.0),
            (res::DECAY, 20_000.0),
            (res::CONST, 1.0),
        ] {
            p.set_param(id, v);
        }
    };
    let ring = |p: &mut dyn Processor| {
        let mut h = Harness::new(SR);
        let mut out = Vec::new();
        for b in 0..80 {
            let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
            if b == 0 {
                l[0] = 1.0;
                r[0] = 1.0;
            }
            h.process(p, &mut l, &mut r, &[], &[]);
            out.extend_from_slice(&l);
        }
        out[2_400..].to_vec()
    };
    let midi_hz = |n: f64| 440.0 * 2f64.powf((n - 69.0) / 12.0);

    let mut tuned = create("rlive.resonators", vec![]);
    set_voice_one(&mut *tuned);
    let table: Arc<TuningTable> = Arc::new(TuningTable::from_hz(|k| midi_hz(f64::from(k) + 7.0)));
    assert!(tuned.set_resource(TUNING_SLOT, table).is_none());
    let found = peak_near(&ring(&mut *tuned), midi_hz(67.0));
    assert!(cents(found, midi_hz(67.0)).abs() < 5.0, "{found} Hz");

    let mut plain = create("rlive.resonators", vec![]);
    set_voice_one(&mut *plain);
    let found = peak_near(&ring(&mut *plain), midi_hz(60.0));
    assert!(cents(found, midi_hz(60.0)).abs() < 5.0, "{found} Hz");
}

#[test]
fn every_device_refuses_a_tuning_it_cannot_use() {
    // The registry's stubs and instruments that don't read it hand it straight back.
    for id in ["rlive.drift", "rlive.poly", "rlive.juno106"] {
        let mut p = create(id, vec![]);
        let handed: Arc<TuningTable> = Arc::new(TuningTable::default());
        assert!(p.set_resource(TUNING_SLOT, handed).is_some(), "{id}");
    }
}
