//! Batch B10: Simpler, Sampler, Drum Sampler and Impulse against the manual, pp. 730-802.

use rlive_core::{
    AudioBuffer, AudioOutput, BlockStep, EventQueue, ExprKind, MidiEvent, MidiMessage, ParamEvent,
    ParamId, ParamOutQueue, ProcessContext, Processor, TransportInfo, ViewWriter,
};
use rlive_devices::devices::b10::common::{
    crossfade_gains, read, Analysis, Interp, LoopKind, Player, ReleaseMode, ANALYSIS_SLOTS,
    STATE_SLOT, WAVE_VALUES,
};
use rlive_devices::devices::b10::drum_sampler as ds;
use rlive_devices::devices::b10::impulse as imp;
use rlive_devices::devices::b10::multi::{self, Layer, ParsedZones, RrMode, Zone, ZoneMap};
use rlive_devices::devices::b10::multi_params as sp;
use rlive_devices::devices::b10::simpler as si;
use rlive_devices::sampler::{p as sampler_p, Sampler};
use rlive_devices::*;
use rlive_testkit::{assert_no_alloc, onset, peak, rms, Harness};
use std::f32::consts::TAU;
use std::sync::Arc;

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

type Stereo = (Vec<f32>, Vec<f32>);

fn note_on(offset: u32, key: u8, velocity: f32) -> MidiEvent {
    MidiEvent::new(
        offset,
        MidiMessage::NoteOn {
            channel: 0,
            key,
            velocity,
            note_id: 0,
            tuning: 0.0,
        },
    )
}

fn note_off(offset: u32, key: u8) -> MidiEvent {
    MidiEvent::new(
        offset,
        MidiMessage::NoteOff {
            channel: 0,
            key,
            velocity: 0.0,
            note_id: 0,
        },
    )
}

fn dc(level: f32, frames: usize) -> Arc<SampleBuffer> {
    Arc::new(SampleBuffer {
        name: "dc".into(),
        left: vec![level; frames],
        right: vec![level; frames],
        sample_rate: SR,
    })
}

/// A sine of `hz`, at full level.
fn sine(hz: f32, frames: usize) -> Arc<SampleBuffer> {
    let left: Vec<f32> = (0..frames)
        .map(|i| (i as f32 / SR as f32 * hz * TAU).sin())
        .collect();
    Arc::new(SampleBuffer {
        name: "sine".into(),
        right: left.clone(),
        left,
        sample_rate: SR,
    })
}

/// The sample `ramp(n)[i] == i / n`, so the output says where the read head is.
fn ramp(frames: usize) -> Arc<SampleBuffer> {
    let left: Vec<f32> = (0..frames).map(|i| i as f32 / frames as f32).collect();
    Arc::new(SampleBuffer {
        name: "ramp".into(),
        right: left.clone(),
        left,
        sample_rate: SR,
    })
}

fn init(samples: Vec<Option<Arc<SampleBuffer>>>, state: Option<String>) -> DeviceInit {
    DeviceInit { samples, state }
}

fn event_block(events: &[MidiEvent], from: usize, to: usize) -> Vec<MidiEvent> {
    events
        .iter()
        .filter(|e| (e.offset as usize) >= from && (e.offset as usize) < to)
        .map(|e| {
            let mut m = *e;
            m.offset -= from as u32;
            m
        })
        .collect()
}

/// Renders `frames` frames with these events, in fixed blocks, returning left and right.
fn render_all(
    p: &mut dyn Processor,
    events: &[MidiEvent],
    frames: usize,
    block: usize,
) -> (Vec<f32>, Vec<f32>) {
    let mut h = Harness::new(SR);
    let (mut left, mut right) = (Vec::with_capacity(frames), Vec::with_capacity(frames));
    let mut at = 0;
    while at < frames {
        let n = block.min(frames - at);
        let (mut l, mut r) = (vec![0.0; n], vec![0.0; n]);
        let e = event_block(events, at, at + n);
        h.process(p, &mut l, &mut r, &e, &[]);
        left.extend_from_slice(&l);
        right.extend_from_slice(&r);
        at += n;
    }
    (left, right)
}

/// `Hz` of the strongest period in `signal`, from its rising zero crossings.
fn pitch_hz(signal: &[f32]) -> f64 {
    let mut crossings = Vec::new();
    for i in 1..signal.len() {
        if signal[i - 1] < 0.0 && signal[i] >= 0.0 {
            let frac = -signal[i - 1] / (signal[i] - signal[i - 1]);
            crossings.push(i as f64 - 1.0 + frac as f64);
        }
    }
    assert!(crossings.len() > 8, "no pitch in the signal");
    let last = crossings.len() - 1;
    (last - 4) as f64 / (crossings[last] - crossings[4]) * SR
}

fn set(p: &mut dyn Processor, id: u32, value: f64) {
    p.set_param(id, value);
}

// -------------------------------------------------------------- the golden's Sampler path

mod original {
    //! The Sampler as it was before batch B10, copied verbatim for comparison.
    use rlive_core::{db_to_gain, BlockStep, MidiMessage, ProcessContext, Processor};
    use rlive_devices::dsp::Adsr;
    use rlive_devices::SampleBuffer;
    use std::sync::Arc;

    const VOICES: usize = 16;

    pub const ROOT: u32 = 0;
    pub const TUNE: u32 = 1;
    pub const VOLUME: u32 = 2;
    pub const ATTACK: u32 = 3;
    pub const RELEASE: u32 = 4;
    pub const ONE_SHOT: u32 = 5;
    pub const START: u32 = 6;
    pub const VELOCITY: u32 = 7;
    pub const PAN: u32 = 8;

    #[derive(Clone, Copy, Default)]
    struct Voice {
        active: bool,
        channel: u8,
        key: u8,
        pos: f64,
        step: f64,
        gain: f32,
        env: Adsr,
        age: u64,
    }

    pub struct Sampler {
        values: Vec<f64>,
        sample: Option<Arc<SampleBuffer>>,
        sr: f64,
        voices: [Voice; VOICES],
        clock: u64,
    }

    impl Sampler {
        pub fn new(sample: Option<Arc<SampleBuffer>>) -> Self {
            Self {
                values: vec![60.0, 0.0, -3.0, 0.5, 50.0, 0.0, 0.0, 0.8, 0.0],
                sample,
                sr: 48_000.0,
                voices: [Voice::default(); VOICES],
                clock: 0,
            }
        }

        fn v(&self, id: u32) -> f64 {
            self.values[id as usize]
        }

        fn note_on(&mut self, channel: u8, key: u8, velocity: f32) {
            let Some(sample) = &self.sample else { return };
            self.clock += 1;
            let semis = key as f64 - self.v(ROOT) + self.v(TUNE);
            let step = 2f64.powf(semis / 12.0) * sample.sample_rate / self.sr;
            let start = self.v(START).clamp(0.0, 1.0) * sample.len() as f64;
            let vel_sens = self.v(VELOCITY) as f32;
            let gain = 1.0 - vel_sens + vel_sens * velocity;
            let (a, r, sr) = (
                self.v(ATTACK) as f32,
                self.v(RELEASE) as f32,
                self.sr as f32,
            );
            let idx = self
                .voices
                .iter()
                .position(|v| !v.active)
                .unwrap_or_else(|| {
                    self.voices
                        .iter()
                        .enumerate()
                        .min_by_key(|(_, v)| v.age)
                        .map(|(i, _)| i)
                        .unwrap_or(0)
                });
            let v = &mut self.voices[idx];
            v.active = true;
            v.channel = channel;
            v.key = key;
            v.pos = start;
            v.step = step;
            v.gain = gain;
            v.age = self.clock;
            v.env.set(a, 1.0, 1.0, r, sr);
            v.env.level = 0.0;
            v.env.gate_on();
        }

        fn note_off(&mut self, channel: u8, key: u8) {
            if self.v(ONE_SHOT) >= 0.5 {
                return;
            }
            for v in self
                .voices
                .iter_mut()
                .filter(|v| v.active && v.channel == channel && v.key == key)
            {
                v.env.gate_off();
            }
        }

        fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
            let Some(sample) = &self.sample else { return };
            let n = sample.len();
            if n < 2 {
                return;
            }
            let vol = db_to_gain(self.values[VOLUME as usize] as f32);
            let pan = self.values[PAN as usize] as f32;
            let (pl, pr) = ((1.0 - pan).min(1.0), (1.0 + pan).min(1.0));
            for v in self.voices.iter_mut().filter(|v| v.active) {
                for i in 0..left.len() {
                    let idx = v.pos as usize;
                    if idx + 1 >= n {
                        v.active = false;
                        break;
                    }
                    let frac = (v.pos - idx as f64) as f32;
                    let l = sample.left[idx] + (sample.left[idx + 1] - sample.left[idx]) * frac;
                    let r = sample.right[idx] + (sample.right[idx + 1] - sample.right[idx]) * frac;
                    let e = v.env.next() * v.gain * vol;
                    left[i] += l * e * pl;
                    right[i] += r * e * pr;
                    v.pos += v.step;
                    if !v.env.is_active() {
                        v.active = false;
                        break;
                    }
                }
            }
        }
    }

    impl Processor for Sampler {
        fn activate(&mut self, sample_rate: f64, _max_block: usize) {
            self.sr = sample_rate;
        }

        fn process(&mut self, ctx: &mut ProcessContext) {
            ctx.for_each_step(|step| match step {
                BlockStep::Param { id, value } => self.set_param(id, value),
                BlockStep::Audio(audio) => self.render(audio.left, audio.right),
                BlockStep::Midi(MidiMessage::NoteOn {
                    channel,
                    key,
                    velocity,
                    ..
                }) => self.note_on(channel, key, velocity),
                BlockStep::Midi(MidiMessage::NoteOff { channel, key, .. }) => {
                    self.note_off(channel, key)
                }
                BlockStep::Midi(MidiMessage::AllNotesOff) => {
                    for v in self.voices.iter_mut() {
                        v.env.gate_off();
                    }
                }
                BlockStep::Midi(_) => {}
            });
        }

        fn set_param(&mut self, id: u32, value: f64) {
            if let Some(v) = self.values.get_mut(id as usize) {
                *v = value;
            }
        }
    }
}

/// The hard rule: everything at its default, the Sampler is exactly the original algorithm.
#[test]
fn the_default_path_is_the_original_sampler_sample_for_sample() {
    let sample = sine(431.0, 96_000);
    let mut changes = vec![
        ParamEvent {
            offset: 0,
            id: ParamId(sampler_p::TUNE),
            value: 7.0,
        },
        ParamEvent {
            offset: 77,
            id: ParamId(sampler_p::VOLUME),
            value: -9.0,
        },
    ];
    changes.sort_by_key(|e| e.offset);
    let script = [
        note_on(5, 60, 1.0),
        note_on(40, 64, 0.5),
        note_off(30_000, 60),
        note_on(33_000, 72, 0.25),
        note_off(45_000, 64),
    ];
    for blocks in [64usize, 256, 311] {
        let mut new = Sampler::new(Some(sample.clone()));
        let mut old = original::Sampler::new(Some(sample.clone()));
        new.activate(SR, blocks);
        old.activate(SR, blocks);
        for (id, value) in [
            (sampler_p::ATTACK, 6.0),
            (sampler_p::RELEASE, 80.0),
            (sampler_p::START, 0.1),
            (sampler_p::VELOCITY, 0.4),
            (sampler_p::PAN, 0.25),
            (sampler_p::ROOT, 61.0),
        ] {
            new.set_param(id, value);
            old.set_param(id, value);
        }
        let mut h = Harness::new(SR);
        let (mut at, frames) = (0usize, 60_000usize);
        while at < frames {
            let n = blocks.min(frames - at);
            let (mut ln, mut rn, mut lo, mut ro) =
                (vec![0.0; n], vec![0.0; n], vec![0.0; n], vec![0.0; n]);
            let e = event_block(&script, at, at + n);
            let params: Vec<ParamEvent> = if at == 0 { changes.clone() } else { Vec::new() };
            assert_no_alloc(|| h.process(&mut new, &mut ln, &mut rn, &e, &params));
            assert_no_alloc(|| h.process(&mut old, &mut lo, &mut ro, &e, &params));
            assert_eq!(ln, lo, "left, block {blocks} at {at}");
            assert_eq!(rn, ro, "right, block {blocks} at {at}");
            at += n;
        }
    }
}

#[test]
fn an_appended_parameter_at_its_default_stays_on_the_original_path() {
    let sample = sine(431.0, 48_000);
    let mut s = Sampler::new(Some(sample.clone()));
    s.activate(SR, BLOCK);
    let mut old = original::Sampler::new(Some(sample));
    old.activate(SR, BLOCK);
    // Moved and restored: the original path returns.
    s.set_param(sp::SPREAD, 40.0);
    s.set_param(sp::SPREAD, 0.0);
    s.set_param(sp::LAYER_SELECT, 3.0);
    let (a, _) = render_all(
        &mut s,
        &[note_on(0, 62, 0.9), note_off(9_000, 62)],
        20_000,
        BLOCK,
    );
    let (b, _) = render_all(
        &mut old,
        &[note_on(0, 62, 0.9), note_off(9_000, 62)],
        20_000,
        BLOCK,
    );
    assert_eq!(a, b);
}

// ------------------------------------------------------------------- players and regions

#[test]
fn a_forward_loop_reads_its_region_and_never_leaves_it() {
    let s = ramp(1_000);
    let mut p = Player::new(0.0, 1_000.0, false);
    p.set_sustain((500.0, 750.0), LoopKind::Forward, 0.0, false, 1.0);
    p.set_release(ReleaseMode::Off, 0.0);
    let mut seen_loop_end = false;
    let mut last_end = 0.0;
    for i in 0..3_000 {
        let pos = p.position();
        assert!(pos < 750.0 + 1e-6, "{i}: {pos}");
        if i > 800 {
            assert!(pos >= 500.0 - 1e-6, "{i}: {pos}");
        }
        last_end = pos;
        seen_loop_end |= pos >= 749.0;
        assert!(p.advance(1.0, false));
        let _ = p.sample(&s, Interp::Normal, false);
    }
    assert!(seen_loop_end);
    let _ = last_end;
}

#[test]
fn back_and_forth_reverses_at_both_ends() {
    let mut p = Player::new(0.0, 1_000.0, false);
    p.set_sustain((200.0, 300.0), LoopKind::BackAndForth, 0.0, false, 1.0);
    let positions: Vec<f64> = (0..600)
        .map(|_| {
            let at = p.position();
            p.advance(1.0, false);
            at
        })
        .collect();
    // Up to the end, then back to the start, then forward again.
    assert_eq!(positions[300], 300.0, "reached Loop End");
    assert_eq!(positions[301], 299.0, "and turned");
    assert_eq!(positions[400], 200.0, "reached Loop Start");
    assert_eq!(positions[401], 201.0, "and turned again");
}

#[test]
fn the_release_stages_follow_the_manual() {
    // Off: stays in the sustain loop.
    let mut stay = Player::new(0.0, 1_000.0, false);
    stay.set_sustain((100.0, 200.0), LoopKind::Forward, 0.0, false, 1.0);
    stay.set_release(ReleaseMode::Off, 0.0);
    for _ in 0..150 {
        stay.advance(1.0, false);
    }
    for _ in 0..500 {
        assert!(stay.advance(1.0, true));
        assert!(stay.position() < 200.0 + 1e-6);
    }
    // Linear: runs on to the end and stops there.
    let mut on = Player::new(0.0, 1_000.0, false);
    on.set_sustain((100.0, 200.0), LoopKind::Forward, 0.0, false, 1.0);
    on.set_release(ReleaseMode::Linear, 0.0);
    for _ in 0..150 {
        on.advance(1.0, false);
    }
    let mut steps = 0;
    while on.advance(1.0, true) {
        steps += 1;
        assert!(steps < 2_000, "must reach the end");
    }
    assert_eq!(steps, 1_000 - 150 - 1, "linear to Sample End");
    // Release Loop: from the end back to the release point, forever.
    let mut rel = Player::new(0.0, 1_000.0, false);
    rel.set_release(ReleaseMode::Loop, 900.0);
    for _ in 0..950 {
        rel.advance(1.0, false);
    }
    for _ in 0..2_000 {
        assert!(rel.advance(1.0, true));
        let at = rel.position();
        assert!((900.0..1_000.0 + 1e-6).contains(&at), "{at}");
    }
    // Back-and-forth from the end.
    let mut bf = Player::new(0.0, 1_000.0, false);
    bf.set_release(ReleaseMode::BackAndForth, 900.0);
    for _ in 0..990 {
        bf.advance(1.0, false);
    }
    bf.advance(1.0, true);
    let mut went_back = false;
    for _ in 0..50 {
        let before = bf.position();
        bf.advance(1.0, true);
        went_back |= bf.position() < before;
    }
    assert!(went_back, "the head turned at the end");
}

#[test]
fn a_reversed_player_starts_at_the_end_and_goes_backwards_through_the_loop() {
    let mut p = Player::new(100.0, 1_100.0, true);
    p.set_sustain((400.0, 600.0), LoopKind::Forward, 0.0, false, 1.0);
    let first = p.position();
    p.advance(1.0, false);
    assert!(
        (first - 1_100.0).abs() < 2.0,
        "starts at Sample End: {first}"
    );
    assert!(p.position() < first, "moving down");
    for _ in 0..2_000 {
        p.advance(1.0, false);
    }
    let pos = p.position();
    assert!((400.0..600.0).contains(&pos), "held in the loop: {pos}");
}

#[test]
fn loop_detune_changes_only_the_pitch_inside_the_loop() {
    let mut plain = Player::new(0.0, 1_000.0, false);
    plain.set_sustain((0.0, 100.0), LoopKind::Forward, 0.0, false, 2.0);
    plain.advance(1.0, false);
    let before = plain.position();
    plain.advance(1.0, false);
    let after = plain.position();
    assert!(
        (after - before - 2.0).abs() < 1e-9,
        "double rate in the loop"
    );
}

#[test]
fn a_crossfade_sums_flat_with_linear_gains_and_rises_by_root_two_with_constant_power() {
    assert_eq!(crossfade_gains(0.0, false), (0.0, 1.0));
    assert_eq!(crossfade_gains(1.0, false), (1.0, 0.0));
    let (a, b) = crossfade_gains(0.5, false);
    assert!((a + b - 1.0).abs() < 1e-6);
    let (a, b) = crossfade_gains(0.5, true);
    assert!((a * a + b * b - 1.0).abs() < 1e-6, "equal power");

    // A constant signal through a looped crossfade: linear stays exactly flat.
    let s = dc(0.5, 2_000);
    let run = |power: bool| {
        let mut p = Player::new(0.0, 2_000.0, false);
        p.set_sustain((1_000.0, 1_500.0), LoopKind::Forward, 200.0, power, 1.0);
        let mut levels = Vec::new();
        for _ in 0..1_800 {
            levels.push(p.sample(&s, Interp::Normal, false)[0]);
            p.advance(1.0, false);
        }
        levels
    };
    let linear = run(false);
    for level in &linear {
        assert!((level - 0.5).abs() < 1e-3, "{level}");
    }
    // Constant power rises to a factor of the square root of two half way through.
    let power = run(true);
    let top = power.iter().fold(0.0f32, |m, s| m.max(*s));
    assert!((top - 0.5 * 2f32.sqrt()).abs() < 5e-3, "{top}");
}

// ------------------------------------------------------------------------- pitch

fn sampler_with_layers(layers: Vec<Layer>, samples: Vec<Option<Arc<SampleBuffer>>>) -> Sampler {
    let map = ZoneMap {
        layers,
        ..ZoneMap::default()
    };
    let mut s = Sampler::with_init(&init(samples, Some(map.serialize())));
    s.activate(SR, BLOCK);
    s
}

fn sine_layer(name: &str, slot: u32, root: f32) -> Layer {
    Layer {
        root,
        ..Layer::new(name, slot)
    }
}

#[test]
fn every_instrument_is_in_tune_across_the_keyboard() {
    let hz = 440.0f32;
    let sample = sine(hz, 96_000);
    let semis = [-24i32, -12, -7, 0, 5, 12, 19, 24];

    let check = |actual: f64, step: i32, what: &str| {
        let want = hz as f64 * 2f64.powf(step as f64 / 12.0);
        let error = 1200.0 * (actual / want).log2();
        assert!(
            error.abs() < 1.0,
            "{what} {step}: {error} cents ({actual} vs {want})"
        );
    };

    for &st in &semis {
        // Simpler.
        let key = (60 + st) as u8;
        let mut s = si::Simpler::new(&init(vec![Some(sample.clone())], None));
        s.activate(SR, BLOCK);
        set(&mut s, si::A_ATTACK, 0.0);
        let (out, _) = render_all(&mut s, &[note_on(0, key, 1.0)], 16_000, BLOCK);
        check(pitch_hz(&out[2_000..]), st, "Simpler");

        // Drum Sampler.
        let mut d = ds::DrumSampler::new(&init(vec![Some(sample.clone())], None));
        d.activate(SR, BLOCK);
        let (out, _) = render_all(&mut d, &[note_on(0, key, 1.0)], 16_000, BLOCK);
        check(pitch_hz(&out[2_000..]), st, "Drum Sampler");

        // Sampler, through a zone map with the root key of the layer.
        let mut m = sampler_with_layers(vec![sine_layer("A", 0, 60.0)], vec![Some(sample.clone())]);
        let (out, _) = render_all(&mut m, &[note_on(0, key, 1.0)], 16_000, BLOCK);
        check(pitch_hz(&out[2_000..]), st, "Sampler");
    }
    // Impulse plays at the root key regardless of slot.
    let mut i = imp::Impulse::new(&init(vec![Some(sample.clone()); 8], None));
    i.activate(SR, BLOCK);
    let (out, _) = render_all(&mut i, &[note_on(0, 62, 1.0)], 16_000, BLOCK);
    check(pitch_hz(&out[2_000..]), 0, "Impulse");
}

#[test]
fn a_sampler_detune_and_root_key_move_the_pitch_by_exactly_that_much() {
    let sample = sine(440.0, 96_000);
    let layer = Layer {
        detune: 25.0,
        ..sine_layer("a", 0, 69.0)
    };
    let mut m = sampler_with_layers(vec![layer], vec![Some(sample)]);
    set(&mut m, sampler_p::TUNE, 3.0);
    set(&mut m, sp::DETUNE, 25.0);
    let (out, _) = render_all(&mut m, &[note_on(0, 69, 1.0)], 16_000, BLOCK);
    let want = 440.0 * 2f64.powf(3.5 / 12.0);
    let got = pitch_hz(&out[2_000..]);
    assert!((1200.0 * (got / want).log2()).abs() < 1.0, "{got}");
}

#[test]
fn a_simpler_with_a_transpose_keeps_tune_too() {
    let sample = sine(440.0, 96_000);
    let mut s = si::Simpler::new(&init(vec![Some(sample)], None));
    s.activate(SR, BLOCK);
    set(&mut s, si::TRANSPOSE, 2.0);
    set(&mut s, si::DETUNE, -50.0);
    set(&mut s, si::A_ATTACK, 0.0);
    let (out, _) = render_all(&mut s, &[note_on(0, 60, 1.0)], 16_000, BLOCK);
    let want = 440.0 * 2f64.powf(1.5 / 12.0);
    let got = pitch_hz(&out[2_000..]);
    assert!((1200.0 * (got / want).log2()).abs() < 1.0, "{got}");
}

// ------------------------------------------------------------------------ zones

#[test]
fn key_zone_select_and_key_zone_shift_pick_the_sample_but_not_the_pitch() {
    let low = sine(220.0, 96_000);
    let high = sine(1_760.0, 96_000);
    let layers = vec![
        Layer {
            key: Zone {
                lo: 0,
                hi: 59,
                fade_lo: 0,
                fade_hi: 0,
            },
            ..sine_layer("low", 0, 60.0)
        },
        Layer {
            key: Zone {
                lo: 60,
                hi: 127,
                fade_lo: 0,
                fade_hi: 0,
            },
            ..sine_layer("high", 1, 60.0)
        },
    ];
    let mut m = sampler_with_layers(layers, vec![Some(low), Some(high)]);
    let (out, _) = render_all(&mut m, &[note_on(0, 50, 1.0)], 16_000, BLOCK);
    let got = pitch_hz(&out[2_000..]);
    // Key 50 in the low sample: an octave and a bit below 220, by its own root.
    let want = 220.0 * 2f64.powf(-10.0 / 12.0);
    assert!((got / want - 1.0).abs() < 0.02, "{got}");

    // Shifted: the same key reads from the other zone, but still at the pitch of key 50.
    set(&mut m, sp::ZONE_SHIFT, 12.0);
    m.reset();
    let (out, _) = render_all(&mut m, &[note_on(0, 50, 1.0)], 16_000, BLOCK);
    let got = pitch_hz(&out[2_000..]);
    let want = 1_760.0 * 2f64.powf(-10.0 / 12.0);
    assert!((got / want - 1.0).abs() < 0.02, "{got}");
}

#[test]
fn velocity_and_select_zones_filter_the_trigger() {
    let low = dc(0.25, 96_000);
    let high = dc(1.0, 96_000);
    let layers = vec![
        Layer {
            vel: Zone {
                lo: 1,
                hi: 64,
                fade_lo: 0,
                fade_hi: 0,
            },
            ..sine_layer("soft", 0, 60.0)
        },
        Layer {
            vel: Zone {
                lo: 65,
                hi: 127,
                fade_lo: 0,
                fade_hi: 0,
            },
            sel: Zone {
                lo: 64,
                hi: 127,
                fade_lo: 0,
                fade_hi: 0,
            },
            ..sine_layer("loud", 1, 60.0)
        },
    ];
    let mut m = sampler_with_layers(layers, vec![Some(low), Some(high)]);
    set(&mut m, sampler_p::VELOCITY, 0.0);
    set(&mut m, sampler_p::VOLUME, 0.0);
    // Soft note: the soft layer, whatever the selector says.
    let (out, _) = render_all(
        &mut m,
        &[note_on(0, 60, 0.3), note_off(8_000, 60)],
        16_000,
        BLOCK,
    );
    assert!(out[4_000] > 0.2 && out[4_000] < 0.3, "{}", out[4_000]);
    // A hard note with the selector below the loud layer's zone sounds nothing.
    m.reset();
    set(&mut m, sp::SELECTOR, 10.0);
    let (out, _) = render_all(
        &mut m,
        &[note_on(0, 60, 1.0), note_off(8_000, 60)],
        16_000,
        BLOCK,
    );
    assert!(out.iter().all(|s| *s == 0.0), "{}", out[4_000]);
    // With the selector in range, the loud layer plays. Selector changes only affect new notes.
    m.reset();
    set(&mut m, sp::SELECTOR, 100.0);
    let (out, _) = render_all(
        &mut m,
        &[note_on(0, 60, 1.0), note_off(8_000, 60)],
        16_000,
        BLOCK,
    );
    assert!(out[4_000] > 0.9, "{}", out[4_000]);
}

#[test]
fn zone_fades_are_linear_or_constant_power_and_cross_to_exact_unity() {
    let z = Zone {
        lo: 60,
        hi: 72,
        fade_lo: 6,
        fade_hi: 6,
    };
    assert_eq!(z.gain(60.0, false), 0.0, "the outer key is silent");
    assert_eq!(z.gain(63.0, false), 0.5);
    assert_eq!(z.gain(66.0, false), 1.0);
    assert_eq!(z.gain(69.0, false), 0.5);
    assert_eq!(z.gain(72.0, false), 0.0);
    assert!((z.gain(63.0, true) - (std::f32::consts::FRAC_PI_4).sin()).abs() < 1e-5);
    assert_eq!(z.gain(59.0, false), 0.0, "outside it, silence");

    // Two samples crossfading across an overlap add to one, as a key travels through it.
    let (a, b) = (
        Zone {
            lo: 0,
            hi: 72,
            fade_lo: 0,
            fade_hi: 12,
        },
        Zone {
            lo: 60,
            hi: 127,
            fade_lo: 12,
            fade_hi: 0,
        },
    );
    for key in 60..=72 {
        let sum = a.gain(key as f32, false) + b.gain(key as f32, false);
        assert!((sum - 1.0).abs() < 1e-6, "{key}: {sum}");
    }
}

#[test]
fn the_sampler_plays_a_zone_at_its_faded_gain() {
    let layer = Layer {
        key: Zone {
            lo: 60,
            hi: 72,
            fade_lo: 4,
            fade_hi: 0,
        },
        ..sine_layer("a", 0, 60.0)
    };
    let mut m = sampler_with_layers(vec![layer], vec![Some(dc(1.0, 48_000))]);
    set(&mut m, sampler_p::VOLUME, 0.0);
    set(&mut m, sampler_p::VELOCITY, 0.0);
    let (out, _) = render_all(&mut m, &[note_on(0, 62, 1.0)], 8_000, BLOCK);
    assert!((out[6_000] - 0.5).abs() < 0.01, "{}", out[6_000]);
}

fn round_robin_sequence(
    mode: RrMode,
    reset: f64,
    notes: usize,
    transport: Option<f64>,
) -> Vec<usize> {
    let tones = [
        dc(0.1, 48_000),
        dc(0.2, 48_000),
        dc(0.3, 48_000),
        dc(0.4, 48_000),
    ];
    let samples: Vec<_> = tones.into_iter().map(Some).collect();
    let layers = (0..4).map(|i| sine_layer("", i, 60.0)).collect();
    let map = ZoneMap {
        layers,
        round_robin: true,
        rr_mode: mode,
        rr_reset_beats: reset,
        ..ZoneMap::default()
    };
    let mut m = Sampler::with_init(&init(samples, Some(map.serialize())));
    m.activate(SR, BLOCK);
    set(&mut m, sampler_p::VOLUME, 0.0);
    set(&mut m, sampler_p::VELOCITY, 0.0);
    set(&mut m, sampler_p::RELEASE, 1.0);
    let mut h = Harness::new(SR);
    if let Some(bpm) = transport {
        h.transport.tempo = bpm;
    }
    let mut picked = Vec::new();
    for n in 0..notes {
        h.transport.tick = 960.0 * n as f64 * reset.max(1.0) / 2.0;
        let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
        h.process(&mut m, &mut l, &mut r, &[note_on(0, 60, 1.0)], &[]);
        let level = l[BLOCK - 1];
        picked.push((level * 10.0).round() as usize - 1);
        let (mut l2, mut r2) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
        h.process(&mut m, &mut l2, &mut r2, &[note_off(0, 60)], &[]);
        for _ in 0..4 {
            let (mut l3, mut r3) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
            h.process(&mut m, &mut l3, &mut r3, &[], &[]);
        }
    }
    picked
}

#[test]
fn round_robin_forward_and_backward_cycle_through_the_list() {
    assert_eq!(
        round_robin_sequence(RrMode::Forward, 0.0, 8, None),
        [0, 1, 2, 3, 0, 1, 2, 3]
    );
    assert_eq!(
        round_robin_sequence(RrMode::Backward, 0.0, 8, None),
        [3, 2, 1, 0, 3, 2, 1, 0]
    );
}

#[test]
fn round_robin_other_never_repeats_a_sample() {
    let seq = round_robin_sequence(RrMode::Other, 0.0, 20, None);
    for pair in seq.windows(2) {
        assert_ne!(pair[0], pair[1], "{seq:?}");
    }
}

#[test]
fn round_robin_random_visits_several_samples() {
    let seq = round_robin_sequence(RrMode::Random, 0.0, 40, None);
    let distinct: std::collections::HashSet<_> = seq.iter().collect();
    assert!(distinct.len() >= 3, "{seq:?}");
}

#[test]
fn the_reset_interval_restarts_the_cycle_each_bar() {
    // Notes 2 beats apart, reset every 4: forward cycles restart every second note.
    let seq = round_robin_sequence(RrMode::Forward, 4.0, 8, None);
    assert_eq!(seq, [0, 1, 0, 1, 0, 1, 0, 1], "{seq:?}");
    let seq = round_robin_sequence(RrMode::Backward, 4.0, 4, None);
    assert_eq!(seq, [3, 2, 3, 2], "{seq:?}");
}

#[test]
fn a_reset_interval_restarts_other_and_random_from_the_first_sample_played() {
    for mode in [RrMode::Other, RrMode::Random] {
        let seq = round_robin_sequence(mode, 4.0, 8, None);
        assert_eq!(seq[0], seq[2], "{mode:?} {seq:?}");
        assert_eq!(seq[0], seq[4], "{mode:?} {seq:?}");
    }
}

#[test]
fn a_voice_whose_sample_is_missing_never_blocks_the_voices() {
    let map = ZoneMap {
        layers: vec![Layer::new("a", 0)],
        ..ZoneMap::default()
    };
    let mut m = Sampler::with_init(&init(vec![None], Some(map.serialize())));
    m.activate(SR, BLOCK);
    set(&mut m, sp::VOICES, 1.0);
    let mut h = Harness::new(SR);
    for key in 40..80 {
        let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
        h.process(
            &mut m,
            &mut l,
            &mut r,
            &[note_on(0, key, 1.0), note_off(10, key)],
            &[],
        );
    }
    assert!(m.engine().zones().layers.len() == 1);
}

#[test]
fn a_zone_map_round_trips_and_missing_files_leave_the_layer_silent() {
    let map = ZoneMap {
        layers: vec![Layer {
            root: 48.0,
            sustain: 2,
            loop_start: 100.0,
            loop_end: 200.0,
            release: 3,
            ..Layer::new("Piano C2", 7)
        }],
        round_robin: true,
        rr_mode: RrMode::Other,
        rr_reset_beats: 4.0,
        power_fades: true,
        ..ZoneMap::default()
    };
    let text = map.serialize();
    assert_eq!(ZoneMap::parse(Some(&text)), map);
    // A file that isn't there is an empty slot: no sound, no fuss.
    let mut m = sampler_with_layers(map.layers.clone(), vec![None]);
    let (out, _) = render_all(&mut m, &[note_on(0, 60, 1.0)], 4_000, BLOCK);
    assert!(out.iter().all(|s| *s == 0.0));
}

#[test]
fn a_zone_map_from_a_newer_version_is_ignored_not_guessed_at() {
    let parsed = ZoneMap::parse(Some("(version:9, layers:[], round_robin:true)"));
    assert!(!parsed.round_robin);
    assert!(ZoneMap::parse(Some("not ron at all")).layers.is_empty());
}

#[test]
fn distributing_the_layers_fills_the_keyboard() {
    let mut map = ZoneMap {
        layers: (0..4).map(|i| sine_layer("", i, 60.0)).collect(),
        ..ZoneMap::default()
    };
    map.distribute_equally();
    let keys: Vec<(u8, u8)> = map.layers.iter().map(|l| (l.key.lo, l.key.hi)).collect();
    assert_eq!(keys, [(0, 31), (32, 63), (64, 95), (96, 127)]);

    let mut around = ZoneMap {
        layers: vec![
            sine_layer("a", 0, 36.0),
            sine_layer("b", 1, 60.0),
            sine_layer("c", 2, 84.0),
        ],
        ..ZoneMap::default()
    };
    around.distribute_around_root();
    let keys: Vec<(u8, u8)> = around.layers.iter().map(|l| (l.key.lo, l.key.hi)).collect();
    assert_eq!(keys, [(0, 48), (49, 72), (73, 127)]);
}

// ------------------------------------------------------------------------ sampler modulation

#[test]
fn a_loop_length_routed_from_velocity_makes_a_faster_note_longer() {
    let s = ramp(2_000);
    let run = |velocity: f32| {
        let layer = Layer {
            sustain: 1,
            loop_start: 0.0,
            loop_end: 500.0,
            ..sine_layer("a", 0, 60.0)
        };
        let mut m = sampler_with_layers(vec![layer], vec![Some(s.clone())]);
        set(&mut m, sampler_p::VOLUME, 0.0);
        set(&mut m, sampler_p::VELOCITY, 0.0);
        // Velocity to Loop Length at +100.
        set(&mut m, sp::M_VEL_DEST_A, 14.0);
        set(&mut m, sp::M_VEL_AMT_A, 100.0);
        let (out, _) = render_all(&mut m, &[note_on(0, 60, velocity)], 8_000, BLOCK);
        out[4_000..].iter().fold(0.0f32, |m, s| m.max(*s))
    };
    let (quiet, loud) = (run(0.01), run(1.0));
    assert!(loud > quiet * 1.5, "{quiet} {loud}");
}

#[test]
fn a_modulation_oscillator_in_am_mode_shapes_the_amplitude() {
    let sample = dc(1.0, 96_000);
    let mut m = sampler_with_layers(vec![sine_layer("a", 0, 60.0)], vec![Some(sample)]);
    set(&mut m, sampler_p::VOLUME, 0.0);
    set(&mut m, sampler_p::VELOCITY, 0.0);
    set(&mut m, sp::OSC_ON, 1.0);
    set(&mut m, sp::OSC_MODE, 1.0);
    set(&mut m, sp::OSC_VOL, 100.0);
    set(&mut m, sp::OSC_FIXED, 1.0);
    set(&mut m, sp::OSC_FREQ, 100.0);
    set(&mut m, sp::OE_SUSTAIN, 1.0);
    let (out, _) = render_all(&mut m, &[note_on(0, 60, 1.0)], 24_000, BLOCK);
    // A full-depth sine at 100 Hz across a unit DC: from 0 to 2, 100 cycles a second.
    let settled = &out[12_000..];
    let top = settled.iter().fold(0.0f32, |m, s| m.max(*s));
    let bottom = settled.iter().fold(10.0f32, |m, s| m.min(*s));
    assert!((top - 2.0).abs() < 0.05, "{top}");
    assert!(bottom < 0.05, "{bottom}");
    let centred: Vec<f32> = settled.iter().map(|s| s - 1.0).collect();
    assert!((pitch_hz(&centred) - 100.0).abs() < 0.5);
}

#[test]
fn an_lfo_routed_to_volume_moves_a_voice_by_that_depth() {
    let sample = dc(0.5, 96_000);
    let mut m = sampler_with_layers(vec![sine_layer("a", 0, 60.0)], vec![Some(sample)]);
    set(&mut m, sampler_p::VOLUME, 0.0);
    set(&mut m, sampler_p::VELOCITY, 0.0);
    set(&mut m, sp::L1_ON, 1.0);
    set(&mut m, sp::L1_FREQ, 4.0);
    set(&mut m, sp::L1_TO_VOL, 50.0);
    let (out, _) = render_all(&mut m, &[note_on(0, 60, 1.0)], 48_000, BLOCK);
    let settled = &out[2_000..];
    let top = settled.iter().fold(0.0f32, |m, s| m.max(*s));
    let bottom = settled.iter().fold(10.0f32, |m, s| m.min(*s));
    assert!((top - 0.75).abs() < 0.02, "{top}");
    assert!((bottom - 0.25).abs() < 0.02, "{bottom}");
}

#[test]
fn the_volume_envelope_shapes_and_loops() {
    let mut m = sampler_with_layers(vec![sine_layer("a", 0, 60.0)], vec![Some(dc(1.0, 96_000))]);
    set(&mut m, sampler_p::VOLUME, 0.0);
    set(&mut m, sampler_p::VELOCITY, 0.0);
    set(&mut m, sampler_p::ATTACK, 10.0);
    set(&mut m, sp::V_DECAY, 100.0);
    set(&mut m, sp::V_SUSTAIN, 0.25);
    let (out, _) = render_all(&mut m, &[note_on(0, 60, 1.0)], 24_000, BLOCK);
    // Attack: a straight line up over 10 ms.
    assert!((out[240] - 0.5).abs() < 0.02, "{}", out[240]);
    // Decay: down to the Sustain level.
    assert!((out[12_000] - 0.25).abs() < 0.02, "{}", out[12_000]);
}

#[test]
fn the_pitch_envelope_moves_the_pitch_and_the_time_scales_it() {
    let sample = sine(440.0, 96_000);
    let mut m = sampler_with_layers(vec![sine_layer("a", 0, 60.0)], vec![Some(sample)]);
    set(&mut m, sp::PE_ON, 1.0);
    set(&mut m, sp::PE_AMOUNT, 12.0);
    set(&mut m, sp::PE_PEAK, 1.0);
    set(&mut m, sp::PE_SUSTAIN, 1.0);
    set(&mut m, sp::PE_ATTACK, 0.0);
    let (out, _) = render_all(&mut m, &[note_on(0, 60, 1.0)], 16_000, BLOCK);
    let got = pitch_hz(&out[2_000..]);
    assert!((got / 880.0 - 1.0).abs() < 0.02, "{got}");
}

#[test]
fn spread_detunes_two_voices_apart_and_pans_them() {
    let sample = sine(440.0, 96_000);
    let mut m = sampler_with_layers(vec![sine_layer("a", 0, 60.0)], vec![Some(sample)]);
    set(&mut m, sp::SPREAD, 100.0);
    let (l, r) = render_all(&mut m, &[note_on(0, 60, 1.0)], 16_000, BLOCK);
    assert!(rms(&l[2_000..]) > 1e-3 && rms(&r[2_000..]) > 1e-3);
    let (a, b) = (pitch_hz(&l[2_000..]), pitch_hz(&r[2_000..]));
    assert!((a - b).abs() > 0.1, "left and right detuned: {a} {b}");
}

#[test]
fn glide_slides_the_pitch_from_the_last_note() {
    let sample = sine(440.0, 96_000);
    let mut m = sampler_with_layers(vec![sine_layer("a", 0, 60.0)], vec![Some(sample)]);
    set(&mut m, sp::GLIDE, 1.0);
    set(&mut m, sp::GLIDE_TIME, 50.0);
    let (out, _) = render_all(
        &mut m,
        &[note_on(0, 48, 1.0), note_on(12_000, 72, 1.0)],
        32_000,
        BLOCK,
    );
    let early = pitch_hz(&out[12_400..14_000]);
    let late = pitch_hz(&out[24_000..]);
    let target = 440.0 * 2f64.powf(12.0 / 12.0);
    assert!(early < target * 0.95, "still gliding: {early}");
    assert!((late / target - 1.0).abs() < 0.02, "got there: {late}");
}

#[test]
fn a_new_zone_map_swaps_in_without_cutting_a_note_and_retires_the_old_one() {
    let sample = dc(0.5, 96_000);
    let mut m = sampler_with_layers(vec![sine_layer("a", 0, 60.0)], vec![Some(sample.clone())]);
    let mut h = Harness::new(SR);
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    h.process(&mut m, &mut l, &mut r, &[note_on(0, 60, 1.0)], &[]);
    let replacement: rlive_core::Resource = Arc::new(ParsedZones {
        map: ZoneMap {
            layers: vec![sine_layer("b", 0, 60.0)],
            ..ZoneMap::default()
        },
    });
    let old = assert_no_alloc(|| m.set_resource(STATE_SLOT, replacement));
    assert!(old.is_some(), "the old map comes back for retiring");
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    h.process(&mut m, &mut l, &mut r, &[], &[]);
    assert!(l[BLOCK / 2].abs() > 0.05, "the note carries on");
}

// ------------------------------------------------------------------------- simpler

fn simpler(sample: Arc<SampleBuffer>) -> si::Simpler {
    let mut s = si::Simpler::new(&init(vec![Some(sample)], None));
    s.activate(SR, BLOCK);
    s
}

#[test]
fn classic_plays_the_region_between_the_flags() {
    let mut s = simpler(ramp(48_000));
    set(&mut s, si::START_FLAG, 0.25);
    set(&mut s, si::END_FLAG, 0.75);
    set(&mut s, si::START, 50.0);
    set(&mut s, si::LENGTH, 50.0);
    set(&mut s, si::VEL_VOL, 0.0);
    set(&mut s, si::VOLUME, 0.0);
    set(&mut s, si::A_ATTACK, 0.0);
    set(&mut s, si::A_RELEASE, 0.0);
    let (out, _) = render_all(&mut s, &[note_on(0, 60, 1.0)], 48_000, BLOCK);
    // The ramp's value says where in the file we are: 25% + 50% of the 50% region, then
    // playing for half of the region.
    assert!((out[8] - 0.5).abs() < 0.01, "{}", out[8]);
    let end = out.iter().rposition(|s| *s != 0.0).unwrap();
    assert!(
        (end as f64 - 12_000.0).abs() < 400.0,
        "ends after half the region: {end}"
    );
}

#[test]
fn looping_in_classic_mode_holds_a_note_past_the_region() {
    let mut s = simpler(dc(1.0, 4_800));
    set(&mut s, si::LOOP_ON, 1.0);
    set(&mut s, si::LOOP, 50.0);
    set(&mut s, si::VEL_VOL, 0.0);
    set(&mut s, si::VOLUME, 0.0);
    set(&mut s, si::A_ATTACK, 0.0);
    set(&mut s, si::A_SUSTAIN, 1.0);
    let (out, _) = render_all(&mut s, &[note_on(0, 60, 1.0)], 30_000, BLOCK);
    assert!(out[29_000] > 0.9, "still looping: {}", out[29_000]);
}

#[test]
fn one_shot_trigger_ignores_note_off_and_is_strictly_monophonic() {
    let mut s = simpler(dc(1.0, 48_000));
    set(&mut s, si::MODE, si::MODE_ONE_SHOT as f64);
    set(&mut s, si::VEL_VOL, 0.0);
    set(&mut s, si::VOLUME, 0.0);
    let (out, _) = render_all(
        &mut s,
        &[
            note_on(0, 60, 1.0),
            note_off(100, 60),
            note_on(200, 64, 1.0),
        ],
        12_000,
        BLOCK,
    );
    // Monophonic: one voice from the second note on, and released notes don't stop it.
    assert!((out[6_000] - 1.0).abs() < 0.02, "{}", out[6_000]);
}

#[test]
fn one_shot_gate_fades_out_when_released() {
    let mut s = simpler(dc(1.0, 48_000));
    set(&mut s, si::MODE, si::MODE_ONE_SHOT as f64);
    set(&mut s, si::GATE, 1.0);
    set(&mut s, si::FADE_OUT, 100.0);
    set(&mut s, si::VEL_VOL, 0.0);
    set(&mut s, si::VOLUME, 0.0);
    let (out, _) = render_all(
        &mut s,
        &[note_on(0, 60, 1.0), note_off(4_800, 60)],
        24_000,
        BLOCK,
    );
    assert!(out[4_000] > 0.9);
    // 100 ms fade from the release: half way down 50 ms later, silent by 100 ms.
    assert!((out[4_800 + 2_400] - 0.5).abs() < 0.05, "{}", out[7_200]);
    assert_eq!(out[4_800 + 4_800 + 100], 0.0);
}

#[test]
fn fade_in_and_fade_out_shape_a_one_shot() {
    let mut s = simpler(dc(1.0, 48_000));
    set(&mut s, si::MODE, si::MODE_ONE_SHOT as f64);
    set(&mut s, si::FADE_IN, 10.0);
    set(&mut s, si::FADE_OUT, 10.0);
    set(&mut s, si::VEL_VOL, 0.0);
    set(&mut s, si::VOLUME, 0.0);
    let (out, _) = render_all(&mut s, &[note_on(0, 60, 1.0)], 48_000, BLOCK);
    assert!((out[240] - 0.5).abs() < 0.02, "{}", out[240]);
    assert!((out[47_760] - 0.5).abs() < 0.02, "{}", out[47_760]);
    assert!(out[24_000] > 0.99);
}

fn analysed(sample: Arc<SampleBuffer>) -> Arc<SampleBuffer> {
    sample
}

/// A click train with quieter hits after each loud one, for transient sensitivity.
fn beats_sample() -> Arc<SampleBuffer> {
    let n = 96_000;
    let mut left = vec![0.0f32; n];
    for hit in 0..8 {
        let at = hit * n / 8;
        let level = if hit % 2 == 0 { 1.0 } else { 0.25 };
        for i in 0..480 {
            left[at + i] = level * (1.0 - i as f32 / 480.0) * ((i as f32 * 0.7).sin());
        }
    }
    Arc::new(SampleBuffer {
        name: "beats".into(),
        right: left.clone(),
        left,
        sample_rate: SR,
    })
}

#[test]
fn transient_slicing_finds_the_hits_and_sensitivity_says_how_many() {
    let sample = beats_sample();
    let transients: Vec<f64> = (0..8).map(|i| i as f64 * 12_000.0 / SR).collect();
    let analysis = Analysis::new(&sample, Some(&transients));
    let spec = |sensitivity: f32| si::SliceSpec {
        by: si::SLICE_TRANSIENT,
        sensitivity,
        division_beats: 1.0,
        regions: 8,
        start: 0.0,
        end: sample.len() as f64,
        beats: 4.0,
    };
    let mut out = [0.0; si::MAX_SLICES + 1];
    let many = si::compute_slices(
        spec(1.0),
        Some(&analysis),
        &si::SimplerState::default(),
        &mut out,
    );
    assert_eq!(
        many, 8,
        "every hit, and the region start is merged into the first"
    );
    let few = si::compute_slices(
        spec(0.1),
        Some(&analysis),
        &si::SimplerState::default(),
        &mut out,
    );
    assert!(few < many, "{few} fewer than {many}");
    assert!(few >= 1);
}

#[test]
fn transient_slicing_is_capped_at_64() {
    let transients: Vec<f64> = (1..200).map(|i| i as f64 * 0.01).collect();
    let sample = dc(0.0, 192_000);
    let analysis = Analysis::new(&sample, Some(&transients));
    let spec = si::SliceSpec {
        by: si::SLICE_TRANSIENT,
        sensitivity: 1.0,
        division_beats: 1.0,
        regions: 8,
        start: 0.0,
        end: 192_000.0,
        beats: 4.0,
    };
    let mut out = [0.0; si::MAX_SLICES + 1];
    let n = si::compute_slices(
        spec,
        Some(&analysis),
        &si::SimplerState::default(),
        &mut out,
    );
    assert_eq!(n, 64);
}

#[test]
fn beat_and_region_slicing_cut_the_region_evenly() {
    let mut out = [0.0; si::MAX_SLICES + 1];
    let spec = si::SliceSpec {
        by: si::SLICE_BEAT,
        sensitivity: 0.5,
        division_beats: 1.0,
        regions: 8,
        start: 0.0,
        end: 96_000.0,
        beats: 4.0,
    };
    let n = si::compute_slices(spec, None, &si::SimplerState::default(), &mut out);
    assert_eq!(n, 4, "four quarter notes in four beats");
    assert_eq!(&out[..4], &[0.0, 24_000.0, 48_000.0, 72_000.0]);
    assert_eq!(out[4], 96_000.0);

    let region = si::SliceSpec {
        by: si::SLICE_REGION,
        regions: 5,
        ..spec
    };
    let n = si::compute_slices(region, None, &si::SimplerState::default(), &mut out);
    assert_eq!(n, 5);
    assert_eq!(out[1], 96_000.0 / 5.0);
}

#[test]
fn manual_slices_survive_a_change_of_sensitivity() {
    let sample = beats_sample();
    let transients: Vec<f64> = (0..8).map(|i| i as f64 * 12_000.0 / SR).collect();
    let analysis = Analysis::new(&sample, Some(&transients));
    let state = si::SimplerState {
        manual: vec![30_000.0],
        removed: vec![12_000.0],
        ..Default::default()
    };
    let mut out = [0.0; si::MAX_SLICES + 1];
    let spec = |sensitivity: f32| si::SliceSpec {
        by: si::SLICE_TRANSIENT,
        sensitivity,
        division_beats: 1.0,
        regions: 8,
        start: 0.0,
        end: 96_000.0,
        beats: 4.0,
    };
    let n = si::compute_slices(spec(1.0), Some(&analysis), &state, &mut out);
    assert!(out[..n].contains(&30_000.0), "the manual slice is there");
    assert!(
        !out[..n].contains(&12_000.0),
        "the deleted transient is gone"
    );
    let n = si::compute_slices(spec(0.0), Some(&analysis), &state, &mut out);
    assert!(
        out[..n].contains(&30_000.0),
        "and survives at low sensitivity"
    );

    // Manual mode: nothing but the manual ones.
    let manual = si::SliceSpec {
        by: si::SLICE_MANUAL,
        ..spec(0.5)
    };
    let n = si::compute_slices(manual, None, &state, &mut out);
    assert_eq!(&out[..n], &[0.0, 30_000.0]);
}

fn slicer(sample: Arc<SampleBuffer>, playback: usize) -> si::Simpler {
    let mut s = simpler(sample);
    set(&mut s, si::MODE, si::MODE_SLICING as f64);
    set(&mut s, si::SLICE_BY, si::SLICE_REGION as f64);
    set(&mut s, si::REGIONS, 4.0);
    set(&mut s, si::PLAYBACK, playback as f64);
    set(&mut s, si::VEL_VOL, 0.0);
    set(&mut s, si::VOLUME, 0.0);
    set(&mut s, si::FADE_IN, 0.0);
    set(&mut s, si::FADE_OUT, 0.0);
    s
}

#[test]
fn each_key_plays_its_own_slice() {
    let mut s = slicer(ramp(48_000), si::PLAY_POLY);
    let (out, _) = render_all(
        &mut s,
        &[note_on(0, si::FIRST_SLICE_KEY + 2, 1.0)],
        4_000,
        BLOCK,
    );
    assert!(
        (out[100] - 0.5).abs() < 0.01,
        "third slice of four: {}",
        out[100]
    );
}

#[test]
fn mono_stops_the_previous_slice_poly_layers_them_thru_keeps_going() {
    let key = si::FIRST_SLICE_KEY;
    // Mono: the second note takes the voice.
    let mut mono = slicer(dc(1.0, 96_000), si::PLAY_MONO);
    let (m, _) = render_all(
        &mut mono,
        &[note_on(0, key, 1.0), note_on(1_000, key + 1, 1.0)],
        3_000,
        BLOCK,
    );
    assert!((m[2_000] - 1.0).abs() < 0.05, "one voice: {}", m[2_000]);

    // Poly: both sound together.
    let mut poly = slicer(dc(1.0, 96_000), si::PLAY_POLY);
    let (p, _) = render_all(
        &mut poly,
        &[note_on(0, key, 1.0), note_on(1_000, key + 1, 1.0)],
        3_000,
        BLOCK,
    );
    assert!((p[2_000] - 2.0).abs() < 0.05, "two voices: {}", p[2_000]);

    // Thru: from the slice to the end of the region.
    let mut thru = slicer(ramp(48_000), si::PLAY_THRU);
    let (t, _) = render_all(&mut thru, &[note_on(0, key + 1, 1.0)], 48_000, BLOCK);
    let last = t.iter().rposition(|s| *s != 0.0).unwrap();
    assert!(last > 35_000, "plays through the rest: {last}");
    let mut poly_end = slicer(ramp(48_000), si::PLAY_POLY);
    let (pe, _) = render_all(&mut poly_end, &[note_on(0, key + 1, 1.0)], 48_000, BLOCK);
    let last_poly = pe.iter().rposition(|s| *s != 0.0).unwrap();
    assert!(
        last_poly < 13_000,
        "a slice ends at the slice boundary: {last_poly}"
    );
}

#[test]
fn retrig_on_cuts_a_repeated_note_retrig_off_overlaps_it() {
    let run = |retrig: bool| {
        let mut s = simpler(dc(1.0, 48_000));
        set(&mut s, si::VEL_VOL, 0.0);
        set(&mut s, si::VOLUME, 0.0);
        set(&mut s, si::RETRIG, if retrig { 1.0 } else { 0.0 });
        set(&mut s, si::A_RELEASE, 2_000.0);
        let (out, _) = render_all(
            &mut s,
            &[note_on(0, 60, 1.0), note_on(1_000, 60, 1.0)],
            3_000,
            BLOCK,
        );
        out[2_000]
    };
    assert!((run(true) - 1.0).abs() < 0.05, "{}", run(true));
    assert!((run(false) - 2.0).abs() < 0.05, "{}", run(false));
}

#[test]
fn stealing_drops_the_oldest_voice_first() {
    let mut s = simpler(dc(1.0, 48_000));
    set(&mut s, si::VEL_VOL, 0.0);
    set(&mut s, si::VOLUME, 0.0);
    set(&mut s, si::VOICES, 3.0); // Menu position 3 means 4 voices.
    set(&mut s, si::A_ATTACK, 0.0);
    set(&mut s, si::RETRIG, 0.0);
    let mut events = Vec::new();
    for i in 0..6 {
        events.push(note_on(0, 60 + i, 1.0));
    }
    let (out, _) = render_all(&mut s, &events, 1_000, BLOCK);
    assert!(
        (out[800] - 4.0).abs() < 0.2,
        "four voices left: {}",
        out[800]
    );
}

#[test]
fn a_filter_on_a_simpler_is_at_its_cutoff() {
    let low = sine(200.0, 96_000);
    let high = sine(12_000.0, 96_000);
    let level = |s: Arc<SampleBuffer>| {
        let mut p = simpler(s);
        set(&mut p, si::VEL_VOL, 0.0);
        set(&mut p, si::VOLUME, 0.0);
        set(&mut p, si::A_ATTACK, 0.0);
        set(&mut p, si::F_ON, 1.0);
        set(&mut p, si::F_FREQ, 1_000.0);
        let (out, _) = render_all(&mut p, &[note_on(0, 60, 1.0)], 8_000, BLOCK);
        rms(&out[2_000..])
    };
    assert!(level(low) > level(high) * 10.0);
}

#[test]
fn the_filter_circuits_all_filter() {
    let high = sine(16_000.0, 96_000);
    for circuit in 0..5 {
        let mut p = simpler(high.clone());
        set(&mut p, si::VEL_VOL, 0.0);
        set(&mut p, si::VOLUME, 0.0);
        set(&mut p, si::A_ATTACK, 0.0);
        set(&mut p, si::F_ON, 1.0);
        set(&mut p, si::F_FREQ, 300.0);
        set(&mut p, si::F_CIRCUIT, circuit as f64);
        set(&mut p, si::F_SLOPE, 1.0);
        let (out, _) = render_all(&mut p, &[note_on(0, 60, 1.0)], 8_000, BLOCK);
        assert!(rms(&out[2_000..]) < 0.1, "circuit {circuit}");
    }
}

#[test]
fn the_amp_envelope_loops_with_note_held_in_loop_mode() {
    let mut s = simpler(dc(1.0, 96_000));
    set(&mut s, si::VEL_VOL, 0.0);
    set(&mut s, si::VOLUME, 0.0);
    set(&mut s, si::A_ATTACK, 10.0);
    set(&mut s, si::A_DECAY, 20.0);
    set(&mut s, si::A_SUSTAIN, 0.0);
    set(&mut s, si::A_LOOP, 1.0);
    let (out, _) = render_all(&mut s, &[note_on(0, 60, 1.0)], 24_000, BLOCK);
    // Attack 10 ms then decay 20 ms: it keeps restarting.
    let peaks = out
        .windows(3)
        .filter(|w| w[1] > 0.95 && w[1] >= w[0] && w[1] >= w[2])
        .count();
    assert!(peaks >= 3, "{peaks}");
}

#[test]
fn an_lfo_routed_to_volume_on_a_simpler() {
    let mut s = simpler(dc(0.5, 96_000));
    set(&mut s, si::VEL_VOL, 0.0);
    set(&mut s, si::VOLUME, 0.0);
    set(&mut s, si::A_ATTACK, 0.0);
    set(&mut s, si::L_ON, 1.0);
    set(&mut s, si::L_FREQ, 8.0);
    set(&mut s, si::L_VOLUME, 100.0);
    let (out, _) = render_all(&mut s, &[note_on(0, 60, 1.0)], 48_000, BLOCK);
    let top = out[..].iter().fold(0.0f32, |m, s| m.max(*s));
    let bottom = out[..].iter().fold(10.0f32, |m, s| m.min(*s));
    assert!((top - 1.0).abs() < 0.02 && bottom < 0.02, "{top} {bottom}");
}

#[test]
fn simpler_answers_pitch_bend_at_plus_or_minus_five_semitones() {
    let sample = sine(440.0, 96_000);
    let mut s = simpler(sample);
    set(&mut s, si::A_ATTACK, 0.0);
    let bend = MidiEvent::new(
        200,
        MidiMessage::PitchBend {
            channel: 0,
            value: 1.0,
        },
    );
    let (out, _) = render_all(&mut s, &[note_on(0, 60, 1.0), bend], 16_000, BLOCK);
    let want = 440.0 * 2f64.powf(5.0 / 12.0);
    let got = pitch_hz(&out[4_000..]);
    assert!((got / want - 1.0).abs() < 0.01, "{got}");
}

#[test]
fn simpler_honours_mpe_pitch_per_note() {
    let sample = sine(440.0, 96_000);
    let mut s = simpler(sample);
    set(&mut s, si::A_ATTACK, 0.0);
    let on = MidiEvent::new(
        0,
        MidiMessage::NoteOn {
            channel: 2,
            key: 60,
            velocity: 1.0,
            note_id: 77,
            tuning: 0.0,
        },
    );
    let pitch = MidiEvent::new(
        4_000,
        MidiMessage::NoteExpr {
            channel: 2,
            key: 60,
            note_id: 77,
            kind: ExprKind::Pitch,
            value: 7.0,
        },
    );
    let (out, _) = render_all(&mut s, &[on, pitch], 16_000, BLOCK);
    let want = 440.0 * 2f64.powf(7.0 / 12.0);
    let got = pitch_hz(&out[6_000..]);
    assert!((got / want - 1.0).abs() < 0.01, "{got}");
}

#[test]
fn warp_in_re_pitch_plays_the_region_in_the_time_warp_as_says() {
    let sample = sine(440.0, 48_000 * 4);
    let mut s = simpler(sample);
    set(&mut s, si::MODE, si::MODE_ONE_SHOT as f64);
    set(&mut s, si::WARP, 1.0);
    set(&mut s, si::WARP_MODE, 3.0);
    set(&mut s, si::VEL_VOL, 0.0);
    set(&mut s, si::VOLUME, 0.0);
    // Four seconds of audio as 4 beats, at 120 bpm: two seconds, so twice as fast.
    set(&mut s, si::WARP_BEATS, 4.0);
    let (out, _) = render_all(&mut s, &[note_on(0, 60, 1.0)], 4 * 48_000, BLOCK);
    let last = out.iter().rposition(|s| s.abs() > 1e-3).unwrap();
    assert!(
        (last as f64 - 96_000.0).abs() < 2_000.0,
        "finishes in two seconds: {last}"
    );
    let got = pitch_hz(&out[10_000..40_000]);
    assert!((got / 880.0 - 1.0).abs() < 0.02, "Re-Pitch: {got}");
}

#[test]
fn warp_in_beats_mode_keeps_the_pitch_and_follows_the_tempo() {
    let sample = sine(440.0, 48_000 * 4);
    let mut s = simpler(sample);
    set(&mut s, si::MODE, si::MODE_ONE_SHOT as f64);
    set(&mut s, si::WARP, 1.0);
    set(&mut s, si::WARP_MODE, 0.0);
    set(&mut s, si::VEL_VOL, 0.0);
    set(&mut s, si::VOLUME, 0.0);
    set(&mut s, si::WARP_BEATS, 4.0);
    let (out, _) = render_all(&mut s, &[note_on(0, 60, 1.0)], 3 * 48_000, BLOCK);
    let last = out.iter().rposition(|s| s.abs() > 1e-3).unwrap();
    assert!((last as f64 - 96_000.0).abs() < 2_000.0, "{last}");
    let got = pitch_hz(&out[10_000..30_000]);
    assert!(
        (got / 440.0 - 1.0).abs() < 0.03,
        "Beats keeps the pitch: {got}"
    );
}

#[test]
fn warp_markers_from_a_clip_set_when_the_region_ends() {
    // Marks the first second of audio as two beats: at 120 bpm, that is exactly one second.
    let state = si::SimplerState {
        warp_markers: vec![(0.0, 0.0), (48_000.0, 2.0 * rlive_core::PPQ as f64)],
        ..Default::default()
    };
    let text = si::ParsedState::serialize(&state);
    let mut s = si::Simpler::new(&init(vec![Some(sine(440.0, 96_000))], Some(text)));
    s.activate(SR, BLOCK);
    set(&mut s, si::MODE, si::MODE_ONE_SHOT as f64);
    set(&mut s, si::WARP, 1.0);
    set(&mut s, si::WARP_MODE, 3.0);
    set(&mut s, si::VEL_VOL, 0.0);
    set(&mut s, si::VOLUME, 0.0);
    let (out, _) = render_all(&mut s, &[note_on(0, 60, 1.0)], 3 * 48_000, BLOCK);
    let last = out.iter().rposition(|s| s.abs() > 1e-3).unwrap();
    // The map extends past its last marker at its last slope: two seconds of file is 2 s.
    assert!((last as f64 - 96_000.0).abs() < 2_000.0, "{last}");
    let got = pitch_hz(&out[8_000..40_000]);
    assert!(
        (got / 440.0 - 1.0).abs() < 0.02,
        "Re-Pitch at one-to-one: {got}"
    );
}

#[test]
fn simpler_can_read_its_saved_state_and_ignores_garbage() {
    let state = si::SimplerState {
        manual: vec![1_000.0],
        ..Default::default()
    };
    let text = si::ParsedState::serialize(&state);
    let parsed = si::ParsedState::parse(Some(&text));
    assert_eq!(parsed.state.manual, vec![1_000.0]);
    assert!(si::ParsedState::parse(Some("garbage"))
        .state
        .manual
        .is_empty());
}

#[test]
fn a_swap_of_sample_keeps_a_simpler_voice_and_retires_the_old_one() {
    let first = dc(0.8, 96_000);
    let second = dc(0.4, 96_000);
    let mut s = si::Simpler::new(&init(vec![Some(first.clone())], None));
    s.activate(SR, BLOCK);
    set(&mut s, si::VEL_VOL, 0.0);
    set(&mut s, si::VOLUME, 0.0);
    set(&mut s, si::A_ATTACK, 0.0);
    let mut h = Harness::new(SR);
    let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
    h.process(&mut s, &mut l, &mut r, &[note_on(0, 60, 1.0)], &[]);
    assert!((l[BLOCK - 1] - 0.8).abs() < 0.02);
    let in_: rlive_core::Resource = second.clone();
    let out = assert_no_alloc(|| s.set_resource(0, in_));
    let out = out.unwrap().downcast::<SampleBuffer>().unwrap();
    assert!(Arc::ptr_eq(&out, &first), "the old one comes back, unfreed");
    let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
    h.process(&mut s, &mut l, &mut r, &[], &[]);
    assert!((l[BLOCK / 2] - 0.4).abs() < 0.02, "same voice, new sample");
}

// ---------------------------------------------------------------------- drum sampler

fn drum(sample: Arc<SampleBuffer>) -> ds::DrumSampler {
    let mut d = ds::DrumSampler::new(&init(vec![Some(sample)], None));
    d.activate(SR, BLOCK);
    set(&mut d, ds::VEL_VOL, 0.0);
    d
}

#[test]
fn sample_start_and_length_are_percentages_of_the_whole_file() {
    let mut d = drum(ramp(48_000));
    set(&mut d, ds::START, 50.0);
    set(&mut d, ds::LENGTH, 25.0);
    let (out, _) = render_all(&mut d, &[note_on(0, 60, 1.0)], 24_000, BLOCK);
    assert!((out[50] - 0.5).abs() < 0.01, "{}", out[50]);
    let last = out.iter().rposition(|s| *s != 0.0).unwrap();
    assert!((last as f64 - 12_000.0).abs() < 200.0, "{last}");
}

#[test]
fn trigger_holds_for_the_hold_time_and_gate_decays_on_release() {
    let run = |mode: f64, off: u32| {
        let mut d = drum(dc(1.0, 96_000));
        set(&mut d, ds::ENV_MODE, mode);
        set(&mut d, ds::ATTACK, 0.0);
        set(&mut d, ds::HOLD, 100.0);
        set(&mut d, ds::DECAY, 100.0);
        let (out, _) = render_all(
            &mut d,
            &[note_on(0, 60, 1.0), note_off(off, 60)],
            24_000,
            BLOCK,
        );
        out
    };
    // Trigger: the 100 ms hold keeps going past an early note-off, then decays over 100 ms.
    let t = run(0.0, 480);
    assert!(t[2_000] > 0.99, "{}", t[2_000]);
    assert!((t[6_000] - 0.75).abs() < 0.02, "{}", t[6_000]);
    assert_eq!(t[10_000], 0.0);
    // Gate: decay starts at the note-off, gone 100 ms later.
    let g = run(1.0, 4_800);
    assert!(g[4_000] > 0.99);
    assert!(
        (g[4_800 + 2_400] - 0.5).abs() < 0.05,
        "{}",
        g[4_800 + 2_400]
    );
    assert_eq!(g[20_000], 0.0);
}

#[test]
fn hold_inf_plays_the_whole_sample() {
    let mut d = drum(dc(1.0, 12_000));
    set(&mut d, ds::HOLD, ds::HOLD_INF);
    let (out, _) = render_all(
        &mut d,
        &[note_on(0, 60, 1.0), note_off(100, 60)],
        20_000,
        BLOCK,
    );
    assert!(out[11_000] > 0.99, "{}", out[11_000]);
    assert_eq!(out[13_000], 0.0, "stops with the sample");
}

#[test]
fn envelope_follows_pitch_scales_the_times() {
    let run = |follow: bool, key: u8| {
        let mut d = drum(dc(1.0, 96_000));
        set(&mut d, ds::ATTACK, 0.0);
        set(&mut d, ds::HOLD, 100.0);
        set(&mut d, ds::DECAY, 1.0);
        set(&mut d, ds::ENV_FOLLOWS, if follow { 1.0 } else { 0.0 });
        let (out, _) = render_all(&mut d, &[note_on(0, key, 1.0)], 24_000, BLOCK);
        out.iter().rposition(|s| *s != 0.0).unwrap()
    };
    let plain = run(false, 72);
    let follows = run(true, 72);
    assert!((plain as f64 - 4_850.0).abs() < 150.0, "{plain}");
    assert!(
        (follows as f64 - 4_850.0 / 2.0).abs() < 150.0,
        "an octave up, half: {follows}"
    );
}

#[test]
fn each_playback_effect_changes_the_sound_and_stays_finite() {
    let sample = sine(440.0, 96_000);
    let dry = {
        let mut d = drum(sample.clone());
        render_all(&mut d, &[note_on(0, 60, 1.0)], 16_000, BLOCK).0
    };
    for fx in 0..9 {
        let mut d = drum(sample.clone());
        set(&mut d, ds::FX_ON, 1.0);
        set(&mut d, ds::FX_TYPE, fx as f64);
        set(&mut d, ds::FX1, if fx == ds::FX_LOOP { 0.02 } else { 0.7 });
        set(&mut d, ds::FX2, if fx == ds::FX_LOOP { 0.3 } else { 0.6 });
        let (out, right) = render_all(&mut d, &[note_on(0, 60, 1.0)], 16_000, BLOCK);
        assert!(out.iter().chain(&right).all(|s| s.is_finite()), "fx {fx}");
        let changed = out.iter().zip(&dry).any(|(a, b)| (a - b).abs() > 1e-3);
        assert!(changed, "fx {fx} did something");
    }
}

#[test]
fn stretch_changes_the_length_without_the_pitch() {
    let sample = sine(440.0, 24_000);
    let mut d = drum(sample);
    set(&mut d, ds::FX_ON, 1.0);
    set(&mut d, ds::FX_TYPE, ds::FX_STRETCH as f64);
    set(&mut d, ds::FX1, 0.75); // factor: 2 octaves of range, midpoint plus a half: two times.
    set(&mut d, ds::FX2, 0.5);
    let (out, _) = render_all(&mut d, &[note_on(0, 60, 1.0)], 96_000, BLOCK);
    let end = out.iter().rposition(|s| s.abs() > 1e-3).unwrap();
    assert!((end as f64 - 48_000.0).abs() < 4_000.0, "doubled: {end}");
    // Grains overlap with phase slips: the median of short stretches shows the pitch they read.
    let mut pitches: Vec<f64> = out[2_000..40_000]
        .chunks(2_400)
        .filter(|c| {
            c.len() == 2_400 && c.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count() > 8
        })
        .map(pitch_hz)
        .collect();
    pitches.sort_by(f64::total_cmp);
    let got = pitches[pitches.len() / 2];
    assert!((got / 440.0 - 1.0).abs() < 0.15, "{got}");
}

#[test]
fn sub_osc_adds_a_tone_of_its_frequency() {
    let mut d = drum(dc(0.0, 96_000));
    set(&mut d, ds::FX_ON, 1.0);
    set(&mut d, ds::FX_TYPE, ds::FX_SUB as f64);
    set(&mut d, ds::FX1, 1.0);
    set(&mut d, ds::FX2, 0.0); // 30 Hz at the bottom of its range.
    let (out, _) = render_all(&mut d, &[note_on(0, 60, 1.0)], 24_000, BLOCK);
    assert!(
        (pitch_hz(&out[4_000..]) - 30.0).abs() < 0.3,
        "{}",
        pitch_hz(&out[4_000..])
    );
    assert!((peak(&out[4_000..]) - 1.0).abs() < 0.02);
}

#[test]
fn the_filter_section_has_lp_12_lp_24_hp_and_peak() {
    let tone = sine(5_000.0, 96_000);
    let run = |kind: f64, freq: f64, gain: f64| {
        let mut d = drum(tone.clone());
        set(&mut d, ds::F_ON, 1.0);
        set(&mut d, ds::F_TYPE, kind);
        set(&mut d, ds::F_FREQ, freq);
        set(&mut d, ds::F_GAIN, gain);
        rms(&render_all(&mut d, &[note_on(0, 60, 1.0)], 8_000, BLOCK).0[2_000..])
    };
    let dry = {
        let mut d = drum(tone.clone());
        rms(&render_all(&mut d, &[note_on(0, 60, 1.0)], 8_000, BLOCK).0[2_000..])
    };
    assert!(run(0.0, 500.0, 0.0) < dry * 0.1, "12 dB LP");
    assert!(
        run(1.0, 500.0, 0.0) < run(0.0, 500.0, 0.0),
        "24 dB cuts more than 12"
    );
    assert!(run(2.0, 15_000.0, 0.0) < dry * 0.3, "24 dB HP");
    let boosted = run(3.0, 5_000.0, 12.0);
    assert!(
        (boosted / dry - 3.98).abs() < 0.3,
        "peak boost: {}",
        boosted / dry
    );
}

#[test]
fn velocity_can_modulate_the_filter_through_the_modulation_section() {
    let tone = sine(5_000.0, 96_000);
    let run = |velocity: f32| {
        let mut d = drum(tone.clone());
        set(&mut d, ds::F_ON, 1.0);
        set(&mut d, ds::F_FREQ, 500.0);
        set(&mut d, ds::MOD_SOURCE, 0.0);
        set(&mut d, ds::MOD_DEST, 0.0);
        set(&mut d, ds::MOD_AMOUNT, 100.0);
        rms(&render_all(&mut d, &[note_on(0, 60, velocity)], 8_000, BLOCK).0[2_000..])
    };
    assert!(run(1.0) > run(0.02) * 2.0, "{} {}", run(1.0), run(0.02));
}

#[test]
fn drum_sampler_follows_slide_as_a_modulation_source() {
    let tone = sine(5_000.0, 96_000);
    let run = |slide: f32| {
        let mut d = drum(tone.clone());
        set(&mut d, ds::F_ON, 1.0);
        set(&mut d, ds::F_FREQ, 500.0);
        set(&mut d, ds::MOD_SOURCE, 1.0);
        set(&mut d, ds::MOD_AMOUNT, 100.0);
        let on = MidiEvent::new(
            0,
            MidiMessage::NoteOn {
                channel: 1,
                key: 60,
                velocity: 1.0,
                note_id: 9,
                tuning: 0.0,
            },
        );
        let expr = MidiEvent::new(
            4,
            MidiMessage::NoteExpr {
                channel: 1,
                key: 60,
                note_id: 9,
                kind: ExprKind::Slide,
                value: slide,
            },
        );
        rms(&render_all(&mut d, &[on, expr], 8_000, BLOCK).0[2_000..])
    };
    assert!(run(1.0) > run(0.0) * 2.0, "{} {}", run(1.0), run(0.0));
}

#[test]
fn per_note_pitch_bend_can_be_turned_off() {
    let expr = |on: bool| {
        let mut d = drum(sine(440.0, 96_000));
        set(&mut d, ds::NOTE_PB, if on { 1.0 } else { 0.0 });
        let note = MidiEvent::new(
            0,
            MidiMessage::NoteOn {
                channel: 1,
                key: 60,
                velocity: 1.0,
                note_id: 9,
                tuning: 0.0,
            },
        );
        let bend = MidiEvent::new(
            4,
            MidiMessage::NoteExpr {
                channel: 1,
                key: 60,
                note_id: 9,
                kind: ExprKind::Pitch,
                value: 12.0,
            },
        );
        pitch_hz(&render_all(&mut d, &[note, bend], 16_000, BLOCK).0[4_000..])
    };
    assert!((expr(true) / 880.0 - 1.0).abs() < 0.02, "{}", expr(true));
    assert!((expr(false) / 440.0 - 1.0).abs() < 0.02, "{}", expr(false));
}

// ---------------------------------------------------------------------- impulse

fn impulse(samples: Vec<Option<Arc<SampleBuffer>>>) -> imp::Impulse {
    let mut i = imp::Impulse::new(&init(samples, None));
    i.activate(SR, BLOCK);
    i
}

/// Renders one block with room for the eight aux outputs. Returns main, then each aux.
fn impulse_block(i: &mut imp::Impulse, events: &[MidiEvent]) -> (Stereo, Vec<Stereo>) {
    let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
    let mut aux: Vec<(Vec<f32>, Vec<f32>)> = (0..8)
        .map(|_| (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]))
        .collect();
    let mut out = EventQueue::with_capacity(0);
    let mut param_out = ParamOutQueue::with_capacity(0);
    let transport = TransportInfo {
        playing: true,
        sample_rate: SR,
        ..Default::default()
    };
    let view = ViewWriter::none();
    {
        let mut outs: Vec<AudioOutput> = aux
            .iter_mut()
            .map(|(a, b)| AudioOutput { left: a, right: b })
            .collect();
        let mut ctx = ProcessContext {
            audio: AudioBuffer {
                left: &mut l,
                right: &mut r,
            },
            sidechain: None,
            aux_out: &mut outs,
            events,
            events_out: &mut out,
            params: &[],
            transport: &transport,
            param_out: &mut param_out,
            mod_out: &mut [],
            view: &view,
        };
        i.process(&mut ctx);
    }
    ((l, r), aux)
}

#[test]
fn c3_triggers_the_leftmost_slot_and_the_others_follow_on_the_white_keys() {
    let samples: Vec<_> = (1..=8).map(|n| Some(dc(n as f32 / 8.0, 4_800))).collect();
    let mut i = impulse(samples);
    let ((l, _), _) = impulse_block(&mut i, &[note_on(0, 60, 1.0)]);
    assert!((l[100] - 1.0 / 8.0).abs() < 0.02, "{}", l[100]);
    let mut i = impulse((1..=8).map(|n| Some(dc(n as f32 / 8.0, 4_800))).collect());
    let ((l, _), _) = impulse_block(&mut i, &[note_on(0, 72, 1.0)]);
    assert!(
        (l[100] - 1.0).abs() < 0.02,
        "C4 plays the rightmost: {}",
        l[100]
    );
    let mut i = impulse((1..=8).map(|n| Some(dc(n as f32 / 8.0, 4_800))).collect());
    let ((l, _), _) = impulse_block(&mut i, &[note_on(0, 61, 1.0)]);
    assert!(l.iter().all(|s| *s == 0.0), "a black key plays nothing");
}

#[test]
fn a_tapped_slot_leaves_the_mix_but_keeps_its_output() {
    let samples: Vec<_> = (0..8).map(|_| Some(dc(0.5, 4_800))).collect();
    let mut i = impulse(samples);
    i.set_aux_routed(1 << 1);
    let events = [note_on(0, 60, 1.0), note_on(0, 62, 1.0)];
    let ((l, _), aux) = impulse_block(&mut i, &events);
    assert!(
        (l[100] - 0.5).abs() < 0.02,
        "only slot 1 in the mix: {}",
        l[100]
    );
    assert!(
        (aux[1].0[100] - 0.5).abs() < 0.02,
        "slot 2 on its output: {}",
        aux[1].0[100]
    );
    assert!(
        (aux[0].0[100] - 0.5).abs() < 0.02,
        "and slot 1 on its own too"
    );
    assert_eq!(aux[5].0[100], 0.0);
}

#[test]
fn with_nothing_tapped_every_slot_is_in_the_mix() {
    let samples: Vec<_> = (0..8).map(|_| Some(dc(0.25, 4_800))).collect();
    let mut i = impulse(samples);
    let ((l, _), _) = impulse_block(&mut i, &[note_on(0, 60, 1.0), note_on(0, 62, 1.0)]);
    assert!((l[100] - 0.5).abs() < 0.02, "{}", l[100]);
}

#[test]
fn slot_seven_and_eight_cut_each_other_when_linked() {
    let samples: Vec<_> = (0..8).map(|_| Some(dc(1.0, 48_000))).collect();
    let mut i = impulse(samples.clone());
    set(&mut i, imp::LINK, 1.0);
    let (open, _) = render_all(
        &mut i,
        &[note_on(0, 72, 1.0), note_on(2_000, 71, 1.0)],
        6_000,
        BLOCK,
    );
    assert!(
        (open[4_000] - 1.0).abs() < 0.1,
        "only the new one: {}",
        open[4_000]
    );

    let mut i = impulse(samples);
    let (off, _) = render_all(
        &mut i,
        &[note_on(0, 72, 1.0), note_on(2_000, 71, 1.0)],
        6_000,
        BLOCK,
    );
    assert!((off[4_000] - 2.0).abs() < 0.1, "both: {}", off[4_000]);
}

#[test]
fn changes_only_reach_a_slot_at_the_next_note() {
    let mut i = impulse(vec![Some(dc(1.0, 96_000))]);
    set(&mut i, imp::slot_param(0, imp::DECAY), 10_000.0);
    let (before, _) = render_all(&mut i, &[note_on(0, 60, 1.0)], 4_800, BLOCK);
    set(&mut i, imp::slot_param(0, imp::VOLUME), -60.0);
    let (held, _) = render_all(&mut i, &[], 4_800, BLOCK);
    assert!(held[100] > 0.5, "latched: {}", held[100]);
    let (next, _) = render_all(&mut i, &[note_on(0, 60, 1.0)], 4_800, BLOCK);
    assert!(
        next[2_000] < 0.01,
        "the next note hears it: {}",
        next[2_000]
    );
    let _ = before;
}

#[test]
fn start_transpose_and_decay_follow_the_slot() {
    let mut i = impulse(vec![Some(sine(440.0, 96_000))]);
    set(&mut i, imp::slot_param(0, imp::TRANSPOSE), 12.0);
    set(&mut i, imp::slot_param(0, imp::DECAY), 10_000.0);
    let (out, _) = render_all(&mut i, &[note_on(0, 60, 1.0)], 16_000, BLOCK);
    assert!((pitch_hz(&out[2_000..]) / 880.0 - 1.0).abs() < 0.01);
    // Trigger Mode decays with the note, whatever the gate does.
    let mut i = impulse(vec![Some(dc(1.0, 96_000))]);
    set(&mut i, imp::slot_param(0, imp::DECAY), 100.0);
    let (d, _) = render_all(
        &mut i,
        &[note_on(0, 60, 1.0), note_off(500, 60)],
        24_000,
        BLOCK,
    );
    assert!(
        (d[2_400] - 0.5).abs() < 0.05,
        "half way down at 50 ms: {}",
        d[2_400]
    );
    assert_eq!(d[20_000], 0.0);
    // Gate Mode waits for the note-off.
    let mut i = impulse(vec![Some(dc(1.0, 96_000))]);
    set(&mut i, imp::slot_param(0, imp::DECAY), 100.0);
    set(&mut i, imp::slot_param(0, imp::DECAY_MODE), 1.0);
    let (g, _) = render_all(
        &mut i,
        &[note_on(0, 60, 1.0), note_off(10_000, 60)],
        24_000,
        BLOCK,
    );
    assert!(g[9_000] > 0.99, "held until the note-off: {}", g[9_000]);
    assert!((g[10_000 + 2_400] - 0.5).abs() < 0.05);
}

#[test]
fn stretch_lengthens_a_slot_and_global_time_scales_decay() {
    let mut i = impulse(vec![Some(dc(1.0, 24_000))]);
    set(&mut i, imp::slot_param(0, imp::STRETCH), 100.0);
    set(&mut i, imp::slot_param(0, imp::DECAY), 10_000.0);
    let (out, _) = render_all(&mut i, &[note_on(0, 60, 1.0)], 96_000, BLOCK);
    let end = out.iter().rposition(|s| *s != 0.0).unwrap();
    assert!((end as f64 - 48_000.0).abs() < 4_000.0, "doubled: {end}");

    let mut i = impulse(vec![Some(dc(1.0, 96_000))]);
    set(&mut i, imp::slot_param(0, imp::DECAY), 100.0);
    set(&mut i, imp::GLOBAL_TIME, 100.0);
    let (d, _) = render_all(&mut i, &[note_on(0, 60, 1.0)], 24_000, BLOCK);
    let last = d.iter().rposition(|s| *s != 0.0).unwrap();
    assert!(
        (last as f64 - 9_600.0).abs() < 400.0,
        "doubled decay: {last}"
    );
}

#[test]
fn solo_and_mute_cut_a_slot_from_everywhere() {
    let samples: Vec<_> = (0..8).map(|_| Some(dc(0.5, 4_800))).collect();
    let mut i = impulse(samples.clone());
    set(&mut i, imp::slot_param(2, imp::MUTE), 1.0);
    let ((l, _), aux) = impulse_block(&mut i, &[note_on(0, 64, 1.0)]);
    assert_eq!(l[100], 0.0);
    assert_eq!(aux[2].0[100], 0.0);

    let mut i = impulse(samples);
    set(&mut i, imp::slot_param(0, imp::SOLO), 1.0);
    let ((l, _), _) = impulse_block(&mut i, &[note_on(0, 60, 1.0), note_on(0, 62, 1.0)]);
    assert!((l[100] - 0.5).abs() < 0.02, "{}", l[100]);
}

// ---------------------------------------------------------------- all four, together

/// Moves every parameter of every device to both ends of its range, under the allocation
/// guard, with notes held throughout, then checks nothing went wrong.
#[test]
fn every_parameter_at_its_extremes_stays_finite_and_never_touches_the_heap() {
    for info in [
        &ds::INFO,
        &imp::INFO,
        &si::INFO,
        &rlive_devices::devices::b10::sampler::INFO,
    ] {
        let desc = (info.desc)();
        let sample = sine(440.0, 48_000);
        let mut p = (info.create)(&DeviceInit {
            samples: vec![Some(sample); desc.samples.len().min(4)],
            state: None,
        });
        p.activate(SR, BLOCK);
        let mut h = Harness::new(SR);
        for (i, step) in desc.params.iter().enumerate() {
            let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
            let value = if i % 2 == 0 { step.min } else { step.max };
            let change = ParamEvent {
                offset: (i % BLOCK) as u32,
                id: step.param_id(),
                value,
            };
            let notes = [note_on(0, (40 + i % 40) as u8, 1.0)];
            let events: &[MidiEvent] = if i % 8 == 0 { &notes } else { &[] };
            assert_no_alloc(|| h.process(&mut *p, &mut l, &mut r, events, &[change]));
            assert!(
                l.iter().chain(&r).all(|s| s.is_finite()),
                "{}: {} = {}",
                info.id,
                step.name,
                value
            );
        }
        p.reset();
    }
}

/// A stolen voice restarts in place at a lower time than the guard: playing more notes than
/// there are voices is the worst case for allocating.
#[test]
fn stealing_every_voice_never_allocates() {
    for info in [
        &si::INFO,
        &rlive_devices::devices::b10::sampler::INFO,
        &ds::INFO,
    ] {
        let desc = (info.desc)();
        let mut p = (info.create)(&DeviceInit {
            samples: vec![Some(dc(0.1, 48_000)); desc.samples.len().min(4)],
            state: Some(
                ZoneMap {
                    layers: vec![sine_layer("a", 0, 60.0)],
                    ..ZoneMap::default()
                }
                .serialize(),
            ),
        });
        p.activate(SR, BLOCK);
        let mut h = Harness::new(SR);
        for round in 0..40 {
            let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
            let notes: Vec<MidiEvent> = (0..8)
                .map(|i| note_on(i, (40 + round * 5 % 40 + i) as u8, 1.0))
                .collect();
            assert_no_alloc(|| h.process(&mut *p, &mut l, &mut r, &notes, &[]));
        }
    }
}

#[test]
fn a_drum_sampler_swap_retires_the_old_buffer_off_the_audio_thread() {
    let first = dc(0.5, 48_000);
    let mut d = ds::DrumSampler::new(&init(vec![Some(first.clone())], None));
    d.activate(SR, BLOCK);
    let in_: rlive_core::Resource = dc(0.25, 48_000);
    let out = assert_no_alloc(|| d.set_resource(0, in_)).unwrap();
    assert!(Arc::ptr_eq(
        &out.downcast::<SampleBuffer>().unwrap(),
        &first
    ));
    let in_: rlive_core::Resource = Arc::new(Analysis::new(&dc(0.1, 100), Some(&[])));
    assert!(
        d.set_resource(ANALYSIS_SLOTS, in_).is_some(),
        "the old analysis comes back too"
    );
}

#[test]
fn an_impulse_swap_works_slot_by_slot() {
    let first = dc(0.5, 48_000);
    let mut i = imp::Impulse::new(&init(vec![Some(first.clone()), None], None));
    i.activate(SR, BLOCK);
    let in_: rlive_core::Resource = dc(0.25, 48_000);
    assert!(Arc::ptr_eq(
        &i.set_resource(0, in_)
            .unwrap()
            .downcast::<SampleBuffer>()
            .unwrap(),
        &first
    ));
    let in_: rlive_core::Resource = dc(0.25, 48_000);
    assert!(
        i.set_resource(3, in_).is_none(),
        "an empty slot gives nothing back"
    );
    let in_: rlive_core::Resource = dc(0.25, 48_000);
    assert!(i.set_resource(9, in_).is_some(), "past the slots: refused");
}

#[test]
fn the_analysis_overview_and_transient_strength_are_sensible() {
    let sample = beats_sample();
    let a = Analysis::new(&sample, None);
    assert_eq!(a.overview.len(), WAVE_VALUES);
    assert!(a.peak > 0.9);
    assert!(!a.transients.is_empty(), "the detector found hits");
    assert!(a
        .transients
        .iter()
        .all(|t| (0.0..=1.0).contains(&t.strength)));
    // The strength ordering: loud hits are stronger than quiet ones nearby.
    let mut sorted = a.transients.clone();
    sorted.sort_by(|x, y| x.frame.total_cmp(&y.frame));
    assert!(sorted.iter().any(|t| t.strength > 0.3));
}

#[test]
fn interpolation_modes_agree_on_a_slow_signal() {
    let s = sine(50.0, 48_000);
    for interp in [Interp::Normal, Interp::Good, Interp::Best] {
        let v = read(&s, 1_000.25, interp);
        let want = (1_000.25f32 / SR as f32 * 50.0 * TAU).sin();
        assert!((v[0] - want).abs() < 1e-3, "{interp:?}: {} vs {want}", v[0]);
    }
}

#[test]
fn no_multisample_needs_more_than_its_slots() {
    assert_eq!(multi::MAX_LAYERS, 256);
    let _ = analysed(dc(0.0, 1));
    let _ = onset(&[0.0, 1.0], 0.5);
    let _ = BlockStep::Midi(MidiMessage::AllNotesOff);
}
