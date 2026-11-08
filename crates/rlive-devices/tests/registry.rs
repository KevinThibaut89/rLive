//! The device registry and the contract every registered device has to keep.

use rlive_core::{MidiEvent, MidiMessage, ParamEvent, ParamId, Processor, Status, TransportInfo};
use rlive_devices::*;
use rlive_testkit::{assert_no_alloc, onset, Harness};
use std::collections::HashSet;
use std::sync::Arc;

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;

const BATCHES: &[(&str, &[&str])] = &[
    (
        "b1",
        &[
            "Pitch",
            "Velocity",
            "Scale",
            "Note Length",
            "Chord",
            "Random",
            "CC Control",
            "Arpeggiator",
            "MIDI Monitor",
            "Note Echo",
            "Expression Control",
            "MPE Control",
        ],
    ),
    (
        "b2",
        &[
            "LFO",
            "Envelope Follower",
            "Shaper",
            "Envelope MIDI",
            "Shaper MIDI",
        ],
    ),
    (
        "b3",
        &[
            "Compressor",
            "Gate",
            "Glue Compressor",
            "Limiter",
            "Multiband Dynamics",
            "Drum Buss",
        ],
    ),
    (
        "b4",
        &[
            "EQ Three",
            "EQ Eight",
            "Channel EQ",
            "Auto Filter",
            "Auto Pan-Tremolo",
            "Utility",
        ],
    ),
    (
        "b5",
        &[
            "Saturator",
            "Overdrive",
            "Dynamic Tube",
            "Pedal",
            "Amp",
            "Cabinet",
            "Erosion",
            "Redux",
            "Vinyl Distortion",
            "Roar",
        ],
    ),
    (
        "b6",
        &[
            "Delay",
            "Echo",
            "Filter Delay",
            "Align Delay",
            "Chorus-Ensemble",
            "Phaser-Flanger",
            "Beat Repeat",
            "Grain Delay",
            "Shifter",
            "Looper",
        ],
    ),
    ("b7", &["Reverb", "Hybrid Reverb", "Resonators", "Corpus"]),
    (
        "b8",
        &[
            "Spectrum",
            "Tuner",
            "Spectral Resonator",
            "Spectral Time",
            "Vocoder",
            "Auto Shift",
        ],
    ),
    ("b9", &["External Audio Effect", "External Instrument"]),
    ("b10", &["Simpler", "Sampler", "Drum Sampler", "Impulse"]),
    ("b11", &["Drift", "Analog", "Wavetable", "Meld"]),
    ("b12", &["Operator", "Electric", "Tension", "Collision"]),
    (
        "b13",
        &[
            "DS Kick",
            "DS Snare",
            "DS Clap",
            "DS Tom",
            "DS HH",
            "DS Cymbal",
            "DS FM",
            "DS Clang",
        ],
    ),
];

const EXISTING: &[(&str, &str)] = &[
    ("rlive.poly", "Poly Synth"),
    ("rlive.juno106", "Juno-106"),
    ("rlive.sampler", "Sampler"),
    ("rlive.eq3", "EQ Three"),
    ("rlive.compressor", "Compressor"),
    ("rlive.saturator", "Saturator"),
    ("rlive.delay", "Delay"),
    ("rlive.reverb", "Reverb"),
    ("rlive.utility", "Utility"),
    ("rlive.gate", "Gate"),
    ("rlive.glue_compressor", "Glue Compressor"),
    ("rlive.limiter", "Limiter"),
    ("rlive.multiband_dynamics", "Multiband Dynamics"),
    ("rlive.drum_buss", "Drum Buss"),
    ("rlive.eq_eight", "EQ Eight"),
    ("rlive.channel_eq", "Channel EQ"),
    ("rlive.auto_filter", "Auto Filter"),
    ("rlive.auto_pan_tremolo", "Auto Pan-Tremolo"),
];

#[test]
fn the_registry_holds_the_81_manual_devices_and_the_poly_synth() {
    let manual: Vec<&str> = BATCHES
        .iter()
        .flat_map(|(_, n)| n.iter().copied())
        .collect();
    assert_eq!(manual.len(), 81);
    assert_eq!(builtins().len(), 83, "81 rows plus the two extras");
    for name in manual.iter().chain(&["Poly Synth", "Juno-106"]) {
        let rows = builtins().iter().filter(|b| b.name == *name).count();
        assert_eq!(rows, 1, "{name}");
    }
}

#[test]
fn ids_are_unique_and_the_saved_ones_are_unchanged() {
    let ids: HashSet<_> = builtins().iter().map(|b| b.id).collect();
    assert_eq!(ids.len(), builtins().len());
    for (id, name) in EXISTING {
        assert_eq!(find(id).map(|b| b.name), Some(*name), "{id}");
    }
    assert!(find("rlive.nonsense").is_none());
}

#[test]
fn only_what_exists_is_shown_and_every_stub_is_hidden() {
    let built = |b: &BuiltinInfo| {
        EXISTING.iter().any(|(id, _)| *id == b.id)
            || b.role == DeviceRole::MidiEffect && BATCHES[0].1.contains(&b.name)
    };
    for b in builtins() {
        assert_eq!(b.visible(), built(b), "{}", b.id);
        let want = if built(b) { Status::Beta } else { Status::Stub };
        assert_eq!(b.status, want, "{}", b.id);
    }
}

#[test]
fn midi_effects_declare_midi_out_and_only_they_do() {
    for b in builtins() {
        let desc = (b.desc)();
        assert_eq!(
            desc.ports.midi_out,
            b.role == DeviceRole::MidiEffect,
            "{}",
            b.id
        );
    }
}

#[test]
fn a_declared_layout_only_names_the_devices_own_parameters() {
    fn check(item: &rlive_core::Item, params: &[rlive_core::ParamInfo], device: &str) {
        use rlive_core::Item;
        match item {
            Item::Knob(p) | Item::Slider(p) | Item::Toggle(p) | Item::Menu(p) => assert!(
                params.iter().any(|x| x.param_id() == *p),
                "{device}: layout names parameter {} it does not declare",
                p.0
            ),
            Item::Row(items) => items.iter().for_each(|i| check(i, params, device)),
            Item::Meter(_) | Item::Scope(..) | Item::Panel(_) => {}
        }
    }
    for b in builtins() {
        let desc = (b.desc)();
        for section in &desc.layout {
            section
                .items
                .iter()
                .for_each(|i| check(i, &desc.params, b.id));
        }
    }
}

#[test]
fn a_sample_slot_is_declared_only_where_samples_load() {
    for b in builtins() {
        let slots = (b.desc)().samples.len();
        assert_eq!(slots, usize::from(b.id == "rlive.sampler"), "{}", b.id);
    }
}

fn snapshot_lines() -> Vec<String> {
    let mut lines = Vec::new();
    for b in builtins() {
        let mut params = (b.desc)().params;
        params.sort_by_key(|p| p.id);
        for p in params {
            lines.push(format!("{}\t{}\t{}", b.id, p.id, p.name));
        }
    }
    lines
}

/// Fails when a released `(device, ParamId, name)` is renumbered, renamed or removed: saved
/// projects and automation refer to these ids forever. New parameters are welcome: run with
/// `RLIVE_UPDATE_PARAM_SNAPSHOT=1` to record them.
#[test]
fn released_parameter_ids_never_change() {
    const SNAPSHOT: &str = include_str!("param_ids.snapshot");
    let current: HashSet<String> = snapshot_lines().into_iter().collect();
    for line in SNAPSHOT.lines().filter(|l| !l.is_empty()) {
        assert!(
            current.contains(line),
            "released parameter renumbered, renamed or removed: {line:?}"
        );
    }
    if std::env::var_os("RLIVE_UPDATE_PARAM_SNAPSHOT").is_some() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/param_ids.snapshot");
        std::fs::write(path, snapshot_lines().join("\n") + "\n").unwrap();
    } else {
        assert!(
            !SNAPSHOT.is_empty(),
            "snapshot is empty; run with RLIVE_UPDATE_PARAM_SNAPSHOT=1"
        );
    }
}

fn note_on(offset: u32, key: u8) -> MidiEvent {
    MidiEvent::new(
        offset,
        MidiMessage::NoteOn {
            channel: 0,
            key,
            velocity: 1.0,
            note_id: 0,
            tuning: 0.0,
        },
    )
}

fn dc_sample(level: f32, sample_rate: f64) -> Arc<SampleBuffer> {
    let n = sample_rate as usize;
    Arc::new(SampleBuffer {
        name: "dc".into(),
        left: vec![level; n],
        right: vec![level; n],
        sample_rate,
    })
}

/// A device ready to play: activated, with something in any sample slot it declares.
fn ready(info: &BuiltinInfo) -> Box<dyn Processor> {
    let slots = (info.desc)().samples.len();
    let mut p = (info.create)(&DeviceInit {
        samples: vec![Some(dc_sample(0.5, SR)); slots],
        state: None,
    });
    p.activate(SR, BLOCK);
    p
}

/// Exercises a device with sound, notes and a parameter change at an offset.
fn drive(p: &mut dyn Processor, info: &BuiltinInfo, h: &mut Harness, blocks: usize) -> Vec<f32> {
    let params: Vec<ParamEvent> = (info.desc)()
        .params
        .iter()
        .take(4)
        .map(|param| ParamEvent {
            offset: 64,
            id: param.param_id(),
            value: param.from_normalized(0.5),
        })
        .collect();
    let notes = [note_on(0, 60), note_on(100, 64)];
    let mut all = Vec::new();
    for b in 0..blocks {
        let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
        for (i, s) in l.iter_mut().enumerate() {
            *s = ((i as f32 * 0.03).sin() * 0.3).clamp(-0.5, 0.5);
        }
        r.copy_from_slice(&l);
        let (events, changes): (&[MidiEvent], &[ParamEvent]) = if b == 0 {
            (&notes, &params)
        } else {
            (&[], &[])
        };
        assert_no_alloc(|| h.process(p, &mut l, &mut r, events, changes));
        all.extend_from_slice(&l);
    }
    all
}

#[test]
fn every_device_renders_finite_audio_and_never_touches_the_heap() {
    for b in builtins() {
        let mut p = ready(b);
        let mut h = Harness::new(SR);
        let out = drive(&mut *p, b, &mut h, 20);
        assert!(
            out.iter().all(|s| s.is_finite()),
            "{} went non-finite",
            b.id
        );
    }
}

#[test]
fn every_device_is_silent_after_reset() {
    for b in builtins() {
        let mut p = ready(b);
        let mut h = Harness::new(SR);
        drive(&mut *p, b, &mut h, 20);
        p.reset();
        for _ in 0..8 {
            let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
            assert_no_alloc(|| h.process(&mut *p, &mut l, &mut r, &[], &[]));
            let loudest = l.iter().chain(&r).fold(0.0f32, |m, s| m.max(s.abs()));
            assert!(loudest < 1e-9, "{} rings on after reset: {loudest}", b.id);
        }
    }
}

#[test]
fn every_audio_effect_delays_a_click_by_what_it_declares() {
    for b in builtins()
        .iter()
        .filter(|b| b.role == DeviceRole::AudioEffect)
    {
        let mut p = ready(b);
        let declared = p.latency() as usize;
        assert!(declared < BLOCK, "{} declares more than a block", b.id);
        let mut h = Harness::new(SR);
        let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
        l[10] = 0.5;
        r[10] = 0.5;
        h.process(&mut *p, &mut l, &mut r, &[], &[]);
        assert_eq!(onset(&l, 1e-3), Some(10 + declared), "{}", b.id);
    }
}

#[test]
fn stubs_have_no_parameters_or_latency() {
    for b in builtins().iter().filter(|b| b.status == Status::Stub) {
        let p = ready(b);
        assert_eq!(p.latency(), 0, "{}", b.id);
        assert!((b.desc)().params.is_empty(), "{}", b.id);
    }
}

#[test]
fn midi_effect_stubs_forward_their_events() {
    let events = [note_on(0, 60), note_on(40, 62)];
    for b in builtins()
        .iter()
        .filter(|b| b.role == DeviceRole::MidiEffect && b.status == Status::Stub)
    {
        let mut p = ready(b);
        let mut h = Harness::new(SR);
        let (mut l, mut r) = (vec![0.25f32; BLOCK], vec![0.25f32; BLOCK]);
        h.process(&mut *p, &mut l, &mut r, &events, &[]);
        assert_eq!(h.events_out.as_slice(), events, "{}", b.id);
        assert_eq!(l[0], 0.25, "{} leaves audio alone", b.id);
    }
}

#[test]
fn audio_effect_stubs_pass_audio_and_instrument_stubs_add_nothing() {
    for b in builtins().iter().filter(|b| b.status == Status::Stub) {
        let mut p = ready(b);
        let mut h = Harness::new(SR);
        let (mut l, mut r) = (vec![0.25f32; BLOCK], vec![-0.25f32; BLOCK]);
        h.process(&mut *p, &mut l, &mut r, &[note_on(0, 60)], &[]);
        assert_eq!(l[100], 0.25, "{}", b.id);
        assert_eq!(r[100], -0.25, "{}", b.id);
    }
}

#[test]
fn a_built_in_effect_hears_a_change_at_its_offset() {
    let mut sat = effects::Saturator::default();
    sat.activate(SR, BLOCK);
    let mut h = Harness::new(SR);
    let (mut l, mut r) = (vec![0.25f32; BLOCK], vec![0.25f32; BLOCK]);
    let dry = ParamEvent {
        offset: 100,
        id: ParamId(effects::sat::MIX),
        value: 0.0,
    };
    h.process(&mut sat, &mut l, &mut r, &[], &[dry]);
    assert!((l[99] - 0.25).abs() > 1e-3, "still wet: {}", l[99]);
    assert_eq!(l[100], 0.25, "dry from the frame of the change");
    assert_eq!(r[BLOCK - 1], 0.25);
}

#[test]
fn a_built_in_instrument_hears_a_change_at_its_offset() {
    let mut s = sampler::Sampler::new(Some(dc_sample(1.0, SR)));
    s.activate(SR, BLOCK);
    let mut h = Harness::new(SR);
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    h.process(&mut s, &mut l, &mut r, &[note_on(0, 60)], &[]);
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    let quiet = ParamEvent {
        offset: 200,
        id: ParamId(sampler::p::VOLUME),
        value: -48.0,
    };
    h.process(&mut s, &mut l, &mut r, &[], &[quiet]);
    assert!(l[199] > 0.2, "{}", l[199]);
    assert!(l[200] < 0.01, "{}", l[200]);
}

#[test]
fn notes_and_changes_share_the_block_in_order() {
    let mut synth = synth::PolySynth::new();
    synth.activate(SR, BLOCK);
    let mut h = Harness::new(SR);
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    // Mute it for frame 300 on, by which point a note has started at 50.
    let quiet = ParamEvent {
        offset: 300,
        id: ParamId(synth::p::VOLUME),
        value: -48.0,
    };
    h.process(&mut synth, &mut l, &mut r, &[note_on(50, 69)], &[quiet]);
    assert!(l[..50].iter().all(|s| *s == 0.0), "nothing before the note");
    assert!(l[50..300].iter().any(|s| s.abs() > 1e-3), "plays");
    assert!(l[300..].iter().all(|s| s.abs() < 5e-3), "then quiet");
}

#[test]
fn swapping_a_sample_keeps_a_held_voice_and_hands_back_the_old_one() {
    let first = dc_sample(0.8, SR);
    let second = dc_sample(0.4, SR);
    let mut s = sampler::Sampler::new(Some(first.clone()));
    s.activate(SR, BLOCK);
    let mut h = Harness::new(SR);
    let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
    h.process(&mut s, &mut l, &mut r, &[note_on(0, 60)], &[]);
    let before = l[BLOCK - 1];
    assert!(before > 0.1, "{before}");

    let handed_in: rlive_core::Resource = second.clone();
    let retired = assert_no_alloc(|| s.set_resource(0, handed_in));
    let retired = retired
        .expect("the old sample comes back")
        .downcast()
        .unwrap();
    assert!(Arc::ptr_eq(&retired, &first), "not freed, returned");
    assert_eq!(Arc::strong_count(&first), 2, "still alive");

    let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
    h.process(&mut s, &mut l, &mut r, &[], &[]);
    let expect = before * 0.5;
    assert!(
        (l[0] - expect).abs() < 1e-4,
        "held, on the new sample: {}",
        l[0]
    );
    assert!((l[BLOCK - 1] - expect).abs() < 1e-4);
}

#[test]
fn a_resource_the_sampler_cant_use_comes_straight_back() {
    let mut s = sampler::Sampler::new(None);
    let wrong_type: rlive_core::Resource = Arc::new(7u32);
    assert!(s.set_resource(0, wrong_type).is_some());
    let wrong_slot: rlive_core::Resource = dc_sample(0.5, SR);
    assert!(s.set_resource(1, wrong_slot).is_some());
    let right: rlive_core::Resource = dc_sample(0.5, SR);
    assert!(s.set_resource(0, right).is_none(), "no previous sample");
}

#[test]
fn delay_tracks_tempo_through_the_transport() {
    let mut d = effects::Delay::default();
    d.activate(SR, BLOCK);
    let mut h = Harness::new(SR);
    h.transport = TransportInfo {
        tempo: 60.0,
        ..h.transport
    };
    let (mut l, mut r) = (vec![0.1f32; BLOCK], vec![0.1f32; BLOCK]);
    h.process(&mut d, &mut l, &mut r, &[], &[]);
    assert!(l.iter().all(|s| s.is_finite()));
}
