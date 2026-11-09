//! Batch B2: the modulators, measured through their mapping outputs.

use rlive_core::{
    AudioBuffer, AudioInput, EventQueue, MidiEvent, MidiMessage, ParamEvent, ParamId,
    ParamOutQueue, ProcessContext, Processor, TransportInfo, ViewWriter, PPQ,
};
use rlive_devices::devices::b2::breakpoints::{Point, Shape};
use rlive_devices::devices::b2::common::{map_param, MAP_AMOUNT, MAP_MAX, MAP_MIN, MAP_MODE};
use rlive_devices::devices::b2::{
    envelope_follower as ef, envelope_midi as em, lfo, shaper, shaper_midi as sm,
};
use rlive_devices::{find, DeviceInit};
use rlive_testkit::assert_no_alloc;

const SR: f64 = 48_000.0;
const BLOCK: usize = 480;

fn device(id: &str, state: Option<&str>) -> Box<dyn Processor> {
    let info = find(id).unwrap();
    let mut p = (info.create)(&DeviceInit {
        samples: Vec::new(),
        state: state.map(String::from),
    });
    p.activate(SR, BLOCK);
    p
}

struct Bed {
    transport: TransportInfo,
    out: EventQueue,
    param: ParamOutQueue,
    mods: [f32; 16],
    /// Stereo input for the next block, and sidechain.
    input: f32,
    side: f32,
}

impl Bed {
    fn new() -> Self {
        Self {
            transport: TransportInfo {
                sample_rate: SR,
                ..TransportInfo::default()
            },
            out: EventQueue::with_capacity(32),
            param: ParamOutQueue::with_capacity(4),
            mods: [0.0; 16],
            input: 0.0,
            side: f32::NAN,
        }
    }

    fn block(&mut self, p: &mut dyn Processor, events: &[MidiEvent], params: &[ParamEvent]) {
        let (mut l, mut r) = ([self.input; BLOCK], [self.input; BLOCK]);
        let view = ViewWriter::none();
        self.out.clear();
        let side = [self.side; BLOCK];
        let sidechain = (!self.side.is_nan()).then_some(AudioInput {
            left: &side,
            right: &side,
        });
        let (transport, out, param, mods) = (
            &self.transport,
            &mut self.out,
            &mut self.param,
            &mut self.mods,
        );
        assert_no_alloc(|| {
            p.process(&mut ProcessContext {
                audio: AudioBuffer {
                    left: &mut l,
                    right: &mut r,
                },
                sidechain,
                aux_out: &mut [],
                events,
                events_out: out,
                params,
                transport,
                param_out: param,
                mod_out: mods,
                view: &view,
            })
        });
        if self.transport.playing {
            self.transport.tick += BLOCK as f64 * self.transport.ticks_per_sample();
        }
    }

    /// Run `frames` frames and return the mapping output at the end.
    fn run(&mut self, p: &mut dyn Processor, frames: usize) -> f32 {
        for _ in 0..frames.div_ceil(BLOCK) {
            self.block(p, &[], &[]);
        }
        self.mods[0]
    }

    /// `Modulation` output 0, turned back into the modulator's position.
    fn position(&self) -> f32 {
        (self.mods[0] + 1.0) * 0.5
    }
}

fn set(p: &mut dyn Processor, id: u32, value: f64) {
    p.set_param(id, value);
}

/// `id` set `value` at the start of the next block.
fn event(id: u32, value: f64) -> [ParamEvent; 1] {
    [ParamEvent {
        offset: 0,
        id: ParamId(id),
        value,
    }]
}

fn note_on(key: u8, velocity: f32) -> MidiEvent {
    MidiEvent::new(
        0,
        MidiMessage::NoteOn {
            channel: 0,
            key,
            velocity,
            note_id: 0,
            tuning: 0.0,
        },
    )
}

fn note_off(key: u8) -> MidiEvent {
    MidiEvent::new(
        0,
        MidiMessage::NoteOff {
            channel: 0,
            key,
            velocity: 0.0,
            note_id: 0,
        },
    )
}

// ---- LFO ----

/// A 1 Hz LFO of a waveform, and the position it reaches `frames` in.
fn lfo_at(wave: usize, frames: usize) -> f32 {
    let mut p = device("rlive.lfo", None);
    set(&mut *p, lfo::WAVEFORM, wave as f64);
    set(&mut *p, lfo::RATE, 1.0);
    let mut bed = Bed::new();
    bed.run(&mut *p, frames);
    bed.position()
}

#[test]
fn every_shape_of_the_lfo_is_where_it_should_be_a_quarter_way_through() {
    const QUARTER: usize = 12_000;
    assert!((lfo_at(0, QUARTER) - 1.0).abs() < 0.02, "sine peaks");
    assert!((lfo_at(1, QUARTER) - 0.25).abs() < 0.02, "up");
    assert!((lfo_at(2, QUARTER) - 0.75).abs() < 0.02, "down");
    assert!((lfo_at(3, QUARTER) - 1.0).abs() < 0.02, "triangle peaks");
    assert_eq!(lfo_at(4, QUARTER), 1.0, "square");
}

#[test]
fn the_random_waves_stay_put_within_a_cycle_and_change_between() {
    for wave in [5, 6] {
        let mut p = device("rlive.lfo", None);
        set(&mut *p, lfo::WAVEFORM, wave as f64);
        set(&mut *p, lfo::RATE, 10.0);
        let mut bed = Bed::new();
        let mut held = Vec::new();
        for _ in 0..20 {
            bed.run(&mut *p, 4_800);
            held.push(bed.position());
        }
        // The output flickers at the cycle edge only: most steps repeat.
        let repeats = held.windows(2).filter(|w| w[0] == w[1]).count();
        assert!(repeats < held.len(), "wave {wave} changes between cycles");
    }
}

#[test]
fn bin_only_ever_visits_its_two_levels() {
    let mut p = device("rlive.lfo", None);
    set(&mut *p, lfo::WAVEFORM, 6.0);
    set(&mut *p, lfo::RATE, 10.0);
    let mut bed = Bed::new();
    for _ in 0..50 {
        bed.run(&mut *p, 2_400);
        let v = bed.position();
        assert!(v == 0.0 || v == 1.0, "{v}");
    }
}

#[test]
fn stray_and_glider_move_between_their_points() {
    for wave in [7, 8] {
        let mut p = device("rlive.lfo", None);
        set(&mut *p, lfo::WAVEFORM, wave as f64);
        set(&mut *p, lfo::RATE, 2.0);
        let mut bed = Bed::new();
        let mut last = bed.run(&mut *p, 240);
        let mut biggest = 0.0f32;
        for _ in 0..200 {
            let v = bed.run(&mut *p, 240);
            biggest = biggest.max((v - last).abs());
            last = v;
        }
        assert!(biggest < 0.2, "wave {wave} glides: {biggest}");
    }
}

#[test]
fn shape_bends_the_wave_and_steps_quantize_it() {
    let warped = {
        let mut p = device("rlive.lfo", None);
        set(&mut *p, lfo::WAVEFORM, 1.0);
        set(&mut *p, lfo::SHAPE, 80.0);
        set(&mut *p, lfo::RATE, 1.0);
        let mut bed = Bed::new();
        bed.run(&mut *p, 12_000);
        bed.position()
    };
    assert!(warped < 0.2, "bent early: {warped}");

    let mut p = device("rlive.lfo", None);
    set(&mut *p, lfo::WAVEFORM, 1.0);
    set(&mut *p, lfo::STEPS, 4.0);
    set(&mut *p, lfo::RATE, 1.0);
    let mut bed = Bed::new();
    let mut levels: Vec<f32> = Vec::new();
    for _ in 0..200 {
        bed.run(&mut *p, 240);
        let v = bed.position();
        if !levels.iter().any(|l| (l - v).abs() < 1e-3) {
            levels.push(v);
        }
    }
    assert!(levels.len() <= 4, "{levels:?}");
}

#[test]
fn phase_moves_the_start_of_the_cycle() {
    let mut p = device("rlive.lfo", None);
    set(&mut *p, lfo::WAVEFORM, 0.0);
    set(&mut *p, lfo::RATE, 1.0);
    set(&mut *p, lfo::PHASE, 25.0);
    let mut bed = Bed::new();
    bed.run(&mut *p, 32);
    assert!(
        bed.position() > 0.98,
        "starts at the peak: {}",
        bed.position()
    );
}

#[test]
fn depth_and_offset_scale_and_centre_the_output() {
    let mut p = device("rlive.lfo", None);
    set(&mut *p, lfo::WAVEFORM, 0.0);
    set(&mut *p, lfo::RATE, 1.0);
    set(&mut *p, lfo::DEPTH, 50.0);
    let mut bed = Bed::new();
    bed.run(&mut *p, 12_000);
    assert!((bed.position() - 0.75).abs() < 0.02, "{}", bed.position());

    let mut p = device("rlive.lfo", None);
    set(&mut *p, lfo::WAVEFORM, 0.0);
    set(&mut *p, lfo::DEPTH, 0.0);
    set(&mut *p, lfo::OFFSET, 50.0);
    let mut bed = Bed::new();
    bed.run(&mut *p, 960);
    assert!((bed.position() - 0.75).abs() < 1e-3, "{}", bed.position());
}

#[test]
fn a_synced_lfo_is_locked_to_the_song() {
    let mut p = device("rlive.lfo", None);
    set(&mut *p, lfo::WAVEFORM, 1.0); // Up
    set(&mut *p, lfo::TIME_MODE, 1.0);
    set(&mut *p, lfo::SYNC, 16.0); // 1 bar
    let mut bed = Bed::new();
    bed.transport.playing = true;
    bed.transport.tick = 2.0 * PPQ as f64; // Halfway through the bar.
    bed.block(&mut *p, &[], &[]);
    // One cycle per four beats, starting from half way: position 0.5 and a small step on.
    assert!((bed.position() - 0.5).abs() < 0.02, "{}", bed.position());
}

#[test]
fn jitter_adds_randomness_and_smooth_takes_the_edge_off() {
    let run = |jitter: f64, smooth: f64| {
        let mut p = device("rlive.lfo", None);
        set(&mut *p, lfo::WAVEFORM, 0.0);
        set(&mut *p, lfo::RATE, 0.1);
        set(&mut *p, lfo::JITTER, jitter);
        set(&mut *p, lfo::SMOOTH, smooth);
        let mut bed = Bed::new();
        let mut last = bed.run(&mut *p, 480);
        let mut biggest = 0.0f32;
        for _ in 0..200 {
            let v = bed.run(&mut *p, 480);
            assert!((0.0..=1.0).contains(&bed.position()));
            biggest = biggest.max((v - last).abs());
            last = v;
        }
        biggest
    };
    let clean = run(0.0, 0.0);
    let noisy = run(100.0, 0.0);
    let calm = run(100.0, 100.0);
    assert!(noisy > clean * 10.0, "{noisy} vs {clean}");
    assert!(calm < noisy, "{calm} vs {noisy}");
}

#[test]
fn hold_freezes_and_releasing_resumes_from_the_held_point() {
    let mut p = device("rlive.lfo", None);
    set(&mut *p, lfo::WAVEFORM, 1.0);
    set(&mut *p, lfo::RATE, 1.0);
    let mut bed = Bed::new();
    bed.run(&mut *p, 12_000);
    let held = bed.position();
    bed.block(&mut *p, &[], &event(lfo::HOLD, 1.0));
    let frozen = bed.position();
    bed.run(&mut *p, 24_000);
    assert_eq!(bed.position(), frozen, "frozen");
    bed.block(&mut *p, &[], &event(lfo::HOLD, 0.0));
    bed.run(&mut *p, 480);
    assert!(
        bed.position() - held < 0.08,
        "carries on from it: {} from {held}",
        bed.position()
    );
}

#[test]
fn retrigger_goes_back_to_the_phase() {
    let mut p = device("rlive.lfo", None);
    set(&mut *p, lfo::WAVEFORM, 1.0);
    set(&mut *p, lfo::RATE, 1.0);
    let mut bed = Bed::new();
    bed.run(&mut *p, 24_000);
    assert!(bed.position() > 0.4);
    bed.block(&mut *p, &[], &event(lfo::RETRIGGER, 1.0));
    assert!(bed.position() < 0.05, "{}", bed.position());
}

// ---- The mapping controls every modulator has ----

#[test]
fn a_mapping_is_bipolar_unipolar_or_a_remote_control() {
    let mut p = device("rlive.lfo", None);
    set(&mut *p, lfo::WAVEFORM, 0.0);
    set(&mut *p, lfo::DEPTH, 0.0);
    set(&mut *p, lfo::OFFSET, 50.0); // Stuck at 0.75.
    let mut bed = Bed::new();
    bed.run(&mut *p, 960);
    assert!((bed.mods[0] - 0.5).abs() < 1e-3, "bipolar: {}", bed.mods[0]);

    set(&mut *p, map_param(0, 1), 1.0);
    bed.run(&mut *p, 480);
    assert!(
        (bed.mods[0] - 0.75).abs() < 1e-3,
        "unipolar: {}",
        bed.mods[0]
    );

    set(&mut *p, map_param(0, MAP_AMOUNT), 50.0);
    bed.run(&mut *p, 480);
    assert!((bed.mods[0] - 0.375).abs() < 1e-3, "{}", bed.mods[0]);

    assert_eq!(bed.mods[8], 0.0, "not a Remote Control");
    set(&mut *p, map_param(0, MAP_MODE), 1.0);
    set(&mut *p, map_param(0, MAP_MIN), 20.0);
    set(&mut *p, map_param(0, MAP_MAX), 40.0);
    bed.run(&mut *p, 480);
    assert_eq!(bed.mods[8], 1.0, "a Remote Control");
    assert!((bed.mods[0] - 0.35).abs() < 1e-3, "{}", bed.mods[0]);
}

// ---- Envelope Follower ----

#[test]
fn the_follower_rises_and_falls_at_its_settings() {
    let mut p = device("rlive.envelope_follower", None);
    set(&mut *p, ef::RISE, 10.0);
    set(&mut *p, ef::FALL, 100.0);
    set(&mut *p, map_param(0, MAP_MODE), 1.0);
    let mut bed = Bed::new();
    bed.input = 1.0;
    bed.run(&mut *p, 480);
    let rise = bed.mods[0];
    assert!((rise - 0.632).abs() < 0.08, "63% after one rise: {rise}");
    bed.run(&mut *p, 48_000);
    assert!(bed.mods[0] > 0.99, "settles at the level");
    bed.input = 0.0;
    bed.run(&mut *p, 4_800);
    let fall = bed.mods[0];
    assert!((fall - 0.368).abs() < 0.08, "37% after one fall: {fall}");
}

#[test]
fn the_follower_delays_and_scales() {
    let mut p = device("rlive.envelope_follower", None);
    set(&mut *p, ef::RISE, 0.0);
    set(&mut *p, ef::FALL, 0.0);
    set(&mut *p, ef::DELAY, 50.0);
    set(&mut *p, ef::GAIN, -6.0);
    set(&mut *p, map_param(0, MAP_MODE), 1.0);
    let mut bed = Bed::new();
    bed.input = 1.0;
    bed.run(&mut *p, 1_920); // 40 ms: not yet.
    assert!(bed.mods[0].abs() < 1e-3, "{}", bed.mods[0]);
    bed.run(&mut *p, 1_440); // 70 ms.
    assert!((bed.mods[0] - 0.5).abs() < 0.02, "{}", bed.mods[0]);
}

#[test]
fn the_sidechain_mix_blends_in_the_key() {
    let mut p = device("rlive.envelope_follower", None);
    set(&mut *p, ef::RISE, 0.0);
    set(&mut *p, ef::FALL, 0.0);
    set(&mut *p, map_param(0, MAP_MODE), 1.0);
    let mut bed = Bed::new();
    bed.input = 0.2;
    bed.side = 0.8;
    bed.run(&mut *p, 1_920);
    assert!(
        (bed.mods[0] - 0.2).abs() < 1e-2,
        "track input: {}",
        bed.mods[0]
    );
    set(&mut *p, ef::SIDECHAIN, 1.0);
    bed.run(&mut *p, 1_920);
    assert!(
        (bed.mods[0] - 0.8).abs() < 1e-2,
        "sidechain: {}",
        bed.mods[0]
    );
    set(&mut *p, ef::SIDECHAIN_MIX, 50.0);
    bed.run(&mut *p, 1_920);
    assert!((bed.mods[0] - 0.5).abs() < 1e-2, "half: {}", bed.mods[0]);
}

// ---- Shaper ----

fn ramp() -> String {
    Shape::preset(1).to_text() // -1 to 1 over the cycle.
}

#[test]
fn a_looping_shaper_runs_at_its_rate() {
    let mut p = device("rlive.shaper", Some(&ramp()));
    set(&mut *p, shaper::TIME_MODE, 0.0);
    set(&mut *p, shaper::RATE, 1.0);
    let mut bed = Bed::new();
    bed.run(&mut *p, 24_000);
    assert!((bed.position() - 0.5).abs() < 0.02, "{}", bed.position());
    bed.run(&mut *p, 24_000);
    assert!(bed.position() < 0.05, "looped: {}", bed.position());
}

#[test]
fn a_synced_shaper_follows_the_song() {
    let mut p = device("rlive.shaper", Some(&ramp()));
    set(&mut *p, shaper::SYNC, 16.0); // 1 bar.
    let mut bed = Bed::new();
    bed.transport.playing = true;
    bed.transport.tick = 3.0 * PPQ as f64;
    bed.block(&mut *p, &[], &[]);
    assert!((bed.position() - 0.75).abs() < 0.02, "{}", bed.position());
}

#[test]
fn one_shot_runs_once_and_stays_at_the_end() {
    let mut p = device("rlive.shaper", Some(&ramp()));
    set(&mut *p, shaper::MODE, 1.0);
    set(&mut *p, shaper::TIME_MODE, 0.0);
    set(&mut *p, shaper::RATE, 2.0);
    let mut bed = Bed::new();
    bed.run(&mut *p, 480);
    bed.block(&mut *p, &[], &event(shaper::TRIGGER, 1.0));
    bed.run(&mut *p, 12_000);
    assert!((bed.position() - 0.5).abs() < 0.05, "{}", bed.position());
    bed.run(&mut *p, 24_000);
    assert!(bed.position() > 0.99, "{}", bed.position());
}

#[test]
fn manual_scrubs_the_envelope() {
    let mut p = device("rlive.shaper", Some(&ramp()));
    set(&mut *p, shaper::MODE, 2.0);
    set(&mut *p, shaper::MANUAL, 25.0);
    let mut bed = Bed::new();
    bed.run(&mut *p, 480);
    assert!((bed.position() - 0.25).abs() < 0.02, "{}", bed.position());
    set(&mut *p, shaper::MANUAL, 75.0);
    bed.run(&mut *p, 480);
    assert!((bed.position() - 0.75).abs() < 0.02, "{}", bed.position());
}

#[test]
fn a_new_shape_swaps_in_without_stopping() {
    let mut p = device("rlive.shaper", Some(&ramp()));
    let mut bed = Bed::new();
    bed.run(&mut *p, 480);
    let parsed = Shape::preset(2).to_text();
    let resource: rlive_core::Resource = std::sync::Arc::new(
        rlive_devices::devices::b2::breakpoints::ParsedShape::parse(Some(&parsed)),
    );
    let old = assert_no_alloc(|| {
        p.set_resource(rlive_devices::devices::b10::common::STATE_SLOT, resource)
    });
    assert!(old.is_some(), "the old shape comes back to be retired");
}

// ---- Shaper MIDI ----

fn hump() -> String {
    Shape {
        points: vec![
            Point {
                time: 0.0,
                value: 0.0,
                curve: 0.0,
            },
            Point {
                time: 0.25,
                value: 1.0,
                curve: 0.0,
            },
            Point {
                time: 0.5,
                value: 0.5,
                curve: 0.0,
            },
            Point {
                time: 1.0,
                value: 0.0,
                curve: 0.0,
            },
        ],
        sustain: None,
    }
    .to_text()
}

fn midi_device(state: &str) -> Box<dyn Processor> {
    let mut p = device("rlive.shaper_midi", Some(state));
    set(&mut *p, sm::RATE, 2.0);
    p
}

#[test]
fn a_note_fires_the_breakpoint_envelope() {
    let mut p = midi_device(&hump());
    let mut bed = Bed::new();
    bed.run(&mut *p, 480);
    assert!((bed.position() - 0.5).abs() < 1e-3, "at rest");
    bed.block(&mut *p, &[note_on(60, 1.0)], &[]);
    assert_eq!(bed.out.as_slice().len(), 1, "the note carries on");
    bed.run(&mut *p, 5_520); // 125 ms: a quarter of a 500 ms envelope.
    assert!(bed.position() > 0.9, "{}", bed.position());
    bed.run(&mut *p, 24_000);
    assert!(
        (bed.position() - 0.5).abs() < 1e-3,
        "over: {}",
        bed.position()
    );
}

#[test]
fn the_sustain_point_holds_while_a_note_does() {
    let mut s = Shape::parse(&hump()).unwrap();
    s.sustain = Some(2);
    let mut p = midi_device(&s.to_text());
    let mut bed = Bed::new();
    bed.block(&mut *p, &[note_on(60, 1.0)], &[]);
    bed.run(&mut *p, 24_000);
    assert!(
        (bed.position() - 0.75).abs() < 0.02,
        "held: {}",
        bed.position()
    );
    bed.run(&mut *p, 24_000);
    assert!(
        (bed.position() - 0.75).abs() < 0.02,
        "still: {}",
        bed.position()
    );
    bed.block(&mut *p, &[note_off(60)], &[]);
    bed.run(&mut *p, 24_000);
    assert!(
        (bed.position() - 0.5).abs() < 0.02,
        "released: {}",
        bed.position()
    );
}

#[test]
fn loop_repeats_while_a_note_is_held_and_passes_over_the_sustain() {
    let mut s = Shape::parse(&hump()).unwrap();
    s.sustain = Some(2);
    let mut p = midi_device(&s.to_text());
    set(&mut *p, sm::LOOP, 1.0);
    let mut bed = Bed::new();
    bed.block(&mut *p, &[note_on(60, 1.0)], &[]);
    let mut high = 0;
    for _ in 0..16 {
        bed.run(&mut *p, 3_000);
        if bed.position() > 0.9 {
            high += 1;
        }
    }
    assert!(high >= 2, "comes round again: {high}");
    bed.block(&mut *p, &[note_off(60)], &[]);
    bed.run(&mut *p, 48_000);
    assert!((bed.position() - 0.5).abs() < 0.02, "ends with the note");
}

#[test]
fn velocity_scales_the_swing_by_its_amount() {
    let peak = |amount: f64, velocity: f32| {
        let mut p = midi_device(&hump());
        set(&mut *p, sm::VELOCITY, amount);
        let mut bed = Bed::new();
        bed.block(&mut *p, &[note_on(60, velocity)], &[]);
        bed.run(&mut *p, 5_520);
        bed.position()
    };
    assert!(peak(0.0, 0.25) > 0.9, "no effect at 0%");
    assert!(peak(100.0, 0.25) < 0.7, "softer at 100%");
    assert!(peak(100.0, 1.0) > 0.9, "full velocity is full");
}

#[test]
fn echoes_repeat_the_envelope_quieter_each_time() {
    let mut p = midi_device(&hump());
    set(&mut *p, sm::RATE, 10.0);
    set(&mut *p, sm::ECHO, 50.0);
    set(&mut *p, sm::TIME, 200.0);
    let mut bed = Bed::new();
    bed.block(&mut *p, &[note_on(60, 1.0)], &[]);
    let mut peaks = Vec::new();
    for _ in 0..5 {
        let mut peak = 0.0f32;
        for _ in 0..20 {
            bed.run(&mut *p, 480);
            peak = peak.max(bed.position());
        }
        peaks.push(peak);
    }
    assert!(peaks[0] > peaks[1], "{peaks:?}");
    assert!(peaks[1] > peaks[2], "{peaks:?}");
    assert!(peaks[1] > 0.55, "it does repeat: {peaks:?}");
}

// ---- Envelope MIDI ----

fn adsr() -> Box<dyn Processor> {
    let mut p = device("rlive.envelope_midi", None);
    set(&mut *p, em::ATTACK, 10.0);
    set(&mut *p, em::DECAY, 100.0);
    set(&mut *p, em::SUSTAIN, 50.0);
    set(&mut *p, em::RELEASE, 100.0);
    set(&mut *p, map_param(0, MAP_MODE), 1.0);
    p
}

#[test]
fn the_envelope_goes_through_its_stages() {
    let mut p = adsr();
    let mut bed = Bed::new();
    bed.block(&mut *p, &[note_on(60, 1.0)], &[]);
    assert_eq!(bed.out.as_slice().len(), 1, "the note carries on");
    bed.run(&mut *p, 480); // 20 ms in: past the attack.
    assert!(bed.mods[0] > 0.8, "{}", bed.mods[0]);
    bed.run(&mut *p, 9_600);
    assert!((bed.mods[0] - 0.5).abs() < 0.03, "sustain: {}", bed.mods[0]);
    bed.block(&mut *p, &[note_off(60)], &[]);
    bed.run(&mut *p, 9_600);
    assert!(bed.mods[0] < 0.05, "released: {}", bed.mods[0]);
}

#[test]
fn velocity_scales_the_peak_when_it_is_on() {
    let peak = |on: bool| {
        let mut p = adsr();
        set(&mut *p, em::VELOCITY, on as u8 as f64);
        let mut bed = Bed::new();
        bed.block(&mut *p, &[note_on(60, 0.5)], &[]);
        bed.run(&mut *p, 480);
        bed.mods[0]
    };
    assert!(peak(true) < 0.6, "{}", peak(true));
    assert!(peak(false) > 0.9, "{}", peak(false));
}

#[test]
fn without_sustain_it_runs_through_even_with_a_note_held() {
    let mut p = adsr();
    set(&mut *p, em::SUSTAIN_ON, 0.0);
    let mut bed = Bed::new();
    bed.block(&mut *p, &[note_on(60, 1.0)], &[]);
    bed.run(&mut *p, 19_200);
    assert!(bed.mods[0] < 0.05, "{}", bed.mods[0]);
}

#[test]
fn global_time_scales_the_stages() {
    let mut p = adsr();
    set(&mut *p, em::GLOBAL_TIME, 5.0);
    let mut bed = Bed::new();
    bed.block(&mut *p, &[note_on(60, 1.0)], &[]);
    bed.run(&mut *p, 480);
    let halfway = bed.mods[0];
    assert!(halfway > 0.2 && halfway < 0.8, "still in attack: {halfway}");
}

#[test]
fn echo_mode_repeats_the_envelope() {
    let mut p = adsr();
    set(&mut *p, em::LOOP_MODE, 3.0);
    set(&mut *p, em::ECHO_TIME, 500.0);
    set(&mut *p, em::FEEDBACK, 50.0);
    let mut bed = Bed::new();
    bed.block(&mut *p, &[note_on(60, 1.0)], &[]);
    bed.block(&mut *p, &[note_off(60)], &[]);
    let mut peaks = Vec::new();
    for _ in 0..3 {
        let mut peak = 0.0f32;
        for _ in 0..50 {
            bed.run(&mut *p, 480);
            peak = peak.max(bed.mods[0]);
        }
        peaks.push(peak);
    }
    assert!(peaks[0] > 0.8, "{peaks:?}");
    assert!((peaks[1] / peaks[0] - 0.5).abs() < 0.15, "{peaks:?}");
}

#[test]
fn loop_mode_keeps_going_while_held() {
    let mut p = adsr();
    set(&mut *p, em::LOOP_MODE, 2.0);
    set(&mut *p, em::SUSTAIN, 0.0);
    let mut bed = Bed::new();
    bed.block(&mut *p, &[note_on(60, 1.0)], &[]);
    let mut low = 0;
    let mut high = 0;
    for _ in 0..80 {
        bed.run(&mut *p, 480);
        if bed.mods[0] < 0.2 {
            low += 1;
        }
        if bed.mods[0] > 0.7 {
            high += 1;
        }
    }
    assert!(low > 0 && high > 0, "{low} {high}");
}

#[test]
fn sync_retriggers_on_the_song_grid() {
    let mut p = adsr();
    set(&mut *p, em::LOOP_MODE, 1.0);
    set(&mut *p, em::SYNC, 9.0); // 1/8: 250 ms.
    set(&mut *p, em::SUSTAIN, 0.0);
    set(&mut *p, em::DECAY, 50.0);
    let mut bed = Bed::new();
    bed.transport.playing = true;
    bed.block(&mut *p, &[note_on(60, 1.0)], &[]);
    let mut rises = 0;
    let mut last = 1.0f32;
    for _ in 0..200 {
        bed.run(&mut *p, 96);
        if bed.mods[0] > last + 0.1 {
            rises += 1;
        }
        last = bed.mods[0];
    }
    // 200 * 96 frames = 400 ms: at least one 250 ms boundary.
    assert!(rises >= 1, "{rises}");
}
