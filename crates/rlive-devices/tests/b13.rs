//! Batch B13: the Drum Synths.

use rlive_core::{MidiEvent, MidiMessage, ParamEvent, ParamId, Processor, Tail, TransportInfo};
use rlive_devices::devices::b13::{ds_clang, ds_clap, ds_cymbal, ds_fm, ds_hh, ds_kick, ds_snare};
use rlive_devices::{find, DeviceInit};
use rlive_testkit::{assert_no_alloc, peak, rms, Harness};

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;
const ALL: [&str; 8] = [
    "rlive.ds_kick",
    "rlive.ds_snare",
    "rlive.ds_clap",
    "rlive.ds_tom",
    "rlive.ds_hh",
    "rlive.ds_cymbal",
    "rlive.ds_fm",
    "rlive.ds_clang",
];

fn device(id: &str) -> Box<dyn Processor> {
    let info = find(id).unwrap();
    let mut p = (info.create)(&DeviceInit::default());
    p.activate(SR, BLOCK);
    p
}

fn note_on(velocity: f32) -> MidiEvent {
    MidiEvent::new(
        0,
        MidiMessage::NoteOn {
            channel: 0,
            key: 60,
            velocity,
            note_id: 0,
            tuning: 0.0,
        },
    )
}

fn render(p: &mut dyn Processor, events: &[MidiEvent], seconds: f64) -> (Vec<f32>, Vec<f32>) {
    let mut h = Harness::new(SR);
    h.transport = TransportInfo {
        sample_rate: SR,
        ..h.transport
    };
    let frames = (SR * seconds) as usize;
    let (mut left, mut right) = (Vec::new(), Vec::new());
    for b in 0..frames / BLOCK {
        let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
        let e: &[MidiEvent] = if b == 0 { events } else { &[] };
        assert_no_alloc(|| h.process(p, &mut l, &mut r, e, &[]));
        left.extend_from_slice(&l);
        right.extend_from_slice(&r);
    }
    (left, right)
}

/// Sets a parameter by name.
fn set(p: &mut dyn Processor, info_id: &str, name: &str, value: f64) {
    let desc = (find(info_id).unwrap().desc)();
    let param = desc.params.iter().find(|p| p.name == name).unwrap();
    p.set_param(param.id, value);
}

#[test]
fn every_drum_makes_a_stable_sound_that_starts_loud_and_ends() {
    for id in ALL {
        let mut p = device(id);
        let (l, r) = render(&mut *p, &[note_on(1.0)], 4.0);
        assert!(
            l.iter().chain(&r).all(|s| s.is_finite()),
            "{id} went non-finite"
        );
        let early = rms(&l[..2400]).max(rms(&r[..2400]));
        assert!(early > 0.01, "{id} is audible: {early}");
        assert!(peak(&l) < 1.01, "{id} stays within range");
        let tail = rms(&l[l.len() - 4800..]);
        assert!(tail < early * 0.01, "{id} decays: {tail} from {early}");
    }
}

#[test]
fn every_drum_is_silent_until_it_is_struck_and_has_no_hung_note() {
    for id in ALL {
        let mut p = device(id);
        let (l, _) = render(&mut *p, &[], 0.2);
        assert!(l.iter().all(|s| *s == 0.0), "{id}");
        // A note-off changes nothing, and the voice stops by itself anyway.
        let off = [MidiEvent::new(
            0,
            MidiMessage::NoteOff {
                channel: 0,
                key: 60,
                velocity: 0.0,
                note_id: 0,
            },
        )];
        let (l, _) = render(&mut *p, &off, 0.1);
        assert!(l.iter().all(|s| s.abs() < 1e-9), "{id}");
    }
}

#[test]
fn velocity_sets_how_hard_it_hits() {
    for id in ALL {
        let loud = {
            let mut p = device(id);
            peak(&render(&mut *p, &[note_on(1.0)], 0.3).0)
        };
        let soft = {
            let mut p = device(id);
            peak(&render(&mut *p, &[note_on(0.2)], 0.3).0)
        };
        assert!(soft < loud * 0.6, "{id}: {soft} vs {loud}");
    }
}

#[test]
fn decay_sets_the_length_and_tail_reports_it() {
    for id in ALL {
        let short = {
            let mut p = device(id);
            set(&mut *p, id, "Decay", 50.0);
            let (l, _) = render(&mut *p, &[note_on(1.0)], 1.0);
            rms(&l[48_000 / 4..])
        };
        let long = {
            let mut p = device(id);
            set(&mut *p, id, "Decay", 800.0);
            let (l, _) = render(&mut *p, &[note_on(1.0)], 1.0);
            rms(&l[48_000 / 4..])
        };
        assert!(long > short * 2.0, "{id}: {long} vs {short}");
        let p = device(id);
        assert!(matches!(p.tail(), Tail::Frames(n) if n > 0), "{id}");
    }
}

#[test]
fn a_new_hit_cuts_the_last_one_without_a_click() {
    for id in ALL {
        let mut p = device(id);
        let mut h = Harness::new(SR);
        h.transport.sample_rate = SR;
        let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
        h.process(&mut *p, &mut l, &mut r, &[note_on(1.0)], &[]);
        let (mut l2, mut r2) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
        h.process(&mut *p, &mut l2, &mut r2, &[note_on(1.0)], &[]);
        assert!(l2.iter().all(|s| s.is_finite()), "{id}");
    }
}

#[test]
fn volume_and_pitch_do_what_they_say() {
    let mut p = device("rlive.ds_kick");
    let loud = peak(&render(&mut *p, &[note_on(1.0)], 0.2).0);
    let mut p = device("rlive.ds_kick");
    set(&mut *p, "rlive.ds_kick", "Volume", -24.0);
    let quiet = peak(&render(&mut *p, &[note_on(1.0)], 0.2).0);
    assert!(quiet < loud * 0.2);

    // One cycle of the settled body takes a period of the Pitch control.
    let mut p = device("rlive.ds_kick");
    set(&mut *p, "rlive.ds_kick", "Pitch", 100.0);
    set(&mut *p, "rlive.ds_kick", "Env", 0.0);
    set(&mut *p, "rlive.ds_kick", "Click", 0.0);
    let (l, _) = render(&mut *p, &[note_on(1.0)], 0.2);
    let start = 480 * 4;
    let crossings = l[start..start + 4800]
        .windows(2)
        .filter(|w| w[0] <= 0.0 && w[1] > 0.0)
        .count();
    assert_eq!(crossings, 10, "100 Hz over 100 ms");
}

#[test]
fn the_snare_answers_to_its_filter_and_the_hat_to_its_noise() {
    let snare_mode = |mode: f64| {
        let mut p = device("rlive.ds_snare");
        p.set_param(ds_snare::FILTER, mode);
        render(&mut *p, &[note_on(1.0)], 0.2).0
    };
    assert_ne!(snare_mode(0.0), snare_mode(2.0), "filter type changes it");
    let hat = |noise: f64| {
        let mut p = device("rlive.ds_hh");
        p.set_param(ds_hh::NOISE, noise);
        p.set_param(ds_hh::FILTER, 0.0);
        rms(&render(&mut *p, &[note_on(1.0)], 0.1).0)
    };
    assert_ne!(hat(0.0), hat(1.0), "pink isn't white");
}

#[test]
fn the_clave_repeats_and_the_cowbell_has_two_tones() {
    let hits = |repeat: f64| {
        let mut p = device("rlive.ds_clang");
        p.set_param(ds_clang::CLAVE, 1.0);
        p.set_param(ds_clang::REPEAT, repeat);
        let (l, _) = render(&mut *p, &[note_on(1.0)], 0.5);
        let env: Vec<f32> = l
            .chunks(240)
            .map(|c| c.iter().fold(0.0f32, |m, s| m.max(s.abs())))
            .collect();
        env.windows(2)
            .filter(|w| w[1] > w[0] * 1.5 && w[1] > 0.02)
            .count()
    };
    assert!(hits(4.0) > hits(0.0), "{} vs {}", hits(4.0), hits(0.0));

    let one = {
        let mut p = device("rlive.ds_clang");
        p.set_param(ds_clang::TONE_B, 0.0);
        render(&mut *p, &[note_on(1.0)], 0.1).0
    };
    let both = {
        let mut p = device("rlive.ds_clang");
        render(&mut *p, &[note_on(1.0)], 0.1).0
    };
    assert_ne!(one, both);
}

#[test]
fn spread_widens_the_clap_and_sloppy_moves_the_channels_apart() {
    let width = |spread: f64, sloppy: f64| {
        let mut p = device("rlive.ds_clap");
        p.set_param(ds_clap::SPREAD, spread);
        p.set_param(ds_clap::SLOPPY, sloppy);
        let (l, r) = render(&mut *p, &[note_on(1.0)], 0.2);
        let diff: Vec<f32> = l.iter().zip(&r).map(|(a, b)| a - b).collect();
        rms(&diff)
    };
    assert_eq!(width(0.0, 100.0), 0.0, "no width: a mono clap");
    assert!(width(100.0, 100.0) > 0.0, "widened");
    let mut p = device("rlive.ds_clap");
    p.set_param(ds_clap::SPREAD, 100.0);
    p.set_param(ds_clap::SLOPPY, 100.0);
    let (l, r) = render(&mut *p, &[note_on(1.0)], 0.1);
    let lead = |b: &[f32]| b.iter().position(|s| s.abs() > 0.01).unwrap();
    assert!(lead(&r) > lead(&l), "the right channel is sloppier");
}

#[test]
fn fm_and_cymbal_respond_to_their_controls() {
    let mut fm_a = device("rlive.ds_fm");
    fm_a.set_param(ds_fm::AMOUNT, 0.0);
    let plain = render(&mut *fm_a, &[note_on(1.0)], 0.1).0;
    let mut fm_b = device("rlive.ds_fm");
    fm_b.set_param(ds_fm::AMOUNT, 100.0);
    let bright = render(&mut *fm_b, &[note_on(1.0)], 0.1).0;
    assert_ne!(plain, bright);

    let hp = |tone: f64| {
        let mut p = device("rlive.ds_cymbal");
        p.set_param(ds_cymbal::TONE, tone);
        render(&mut *p, &[note_on(1.0)], 0.1).0
    };
    assert_ne!(hp(0.0), hp(100.0));
}

#[test]
fn the_kick_drive_control_adds_harmonics() {
    let measure = |drive: f64| {
        let mut p = device("rlive.ds_kick");
        p.set_param(ds_kick::DRIVE, drive);
        p.set_param(ds_kick::CLICK, 0.0);
        let (l, _) = render(&mut *p, &[note_on(1.0)], 0.2);
        peak(&l)
    };
    assert!(measure(100.0) > measure(0.0) * 0.8);
}

#[test]
fn reset_silences_a_ringing_drum() {
    for id in ALL {
        let mut p = device(id);
        render(&mut *p, &[note_on(1.0)], 0.05);
        p.reset();
        let (l, _) = render(&mut *p, &[], 0.05);
        assert!(l.iter().all(|s| s.abs() < 1e-9), "{id}");
    }
}

#[test]
fn a_parameter_change_mid_block_lands_without_a_glitch() {
    for id in ALL {
        let mut p = device(id);
        let desc = (find(id).unwrap().desc)();
        let change: Vec<ParamEvent> = desc
            .params
            .iter()
            .take(3)
            .map(|param| ParamEvent {
                offset: 64,
                id: ParamId(param.id),
                value: param.from_normalized(0.8),
            })
            .collect();
        let mut h = Harness::new(SR);
        h.transport.sample_rate = SR;
        let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
        assert_no_alloc(|| h.process(&mut *p, &mut l, &mut r, &[note_on(1.0)], &change));
        assert!(l.iter().all(|s| s.is_finite()), "{id}");
    }
}
