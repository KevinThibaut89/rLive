//! Every block run under the allocation guard, with its parameters being thrown around.

use super::SR;
use crate::dsp::convolution::*;
use crate::dsp::delay::*;
use crate::dsp::envelope::*;
use crate::dsp::fft::{Complex32, Fft};
use crate::dsp::filter::*;
use crate::dsp::follower::*;
use crate::dsp::granular::*;
use crate::dsp::lfo::*;
use crate::dsp::lookahead::*;
use crate::dsp::modmatrix::*;
use crate::dsp::oversample::*;
use crate::dsp::pitch::*;
use crate::dsp::sidechain::*;
use crate::dsp::spectrum::*;
use crate::dsp::stft::*;
use crate::dsp::tempo::BeatClock;
use crate::dsp::util::Rng;
use crate::dsp::voice::*;
use crate::dsp::wavetable::*;
use rlive_core::{AudioBuffer, AudioInput, MidiMessage, ViewChannel, ViewId, ViewWriter};
use rlive_testkit::assert_no_alloc;
use std::sync::Arc;

const BLOCK: usize = 256;

fn noise(n: usize, seed: u32) -> Vec<f32> {
    let mut rng = Rng::new(seed);
    (0..n).map(|_| rng.bipolar()).collect()
}

#[test]
fn voice_allocation_does_not_allocate() {
    let mut a = VoiceAllocator::new(16);
    a.prepare(SR);
    a.set(&VoiceSettings {
        unison: 2,
        glide: GlideMode::Always,
        glide_time: 0.01,
        ..Default::default()
    });
    assert_no_alloc(|| {
        for i in 0..4_000u32 {
            let key = (i % 88) as u8 + 12;
            a.handle(
                MidiMessage::NoteOn {
                    channel: 0,
                    key,
                    velocity: 0.7,
                    note_id: i + 1,
                    tuning: 0.0,
                },
                |_| {},
            );
            a.advance(16);
            if i % 3 == 0 {
                a.handle(
                    MidiMessage::NoteOff {
                        channel: 0,
                        key,
                        velocity: 0.0,
                        note_id: i + 1,
                    },
                    |_| {},
                );
            }
            std::hint::black_box(a.pitch(0));
        }
        a.set(&VoiceSettings {
            mode: VoiceMode::Legato,
            ..Default::default()
        });
        for i in 0..64u32 {
            a.handle(
                MidiMessage::NoteOn {
                    channel: 0,
                    key: i as u8,
                    velocity: 1.0,
                    note_id: 0,
                    tuning: 0.0,
                },
                |_| {},
            );
        }
        a.handle(MidiMessage::AllNotesOff, |_| {});
    });
}

#[test]
fn modulation_does_not_allocate() {
    let mut m = ModMatrix::new(8, 8, 32);
    let mut bus = m.bus();
    for slot in 0..32 {
        m.set_route(
            slot,
            Some(Route {
                source: slot % 8,
                dest: (slot * 3) % 8,
                amount: slot as f32 * 0.1,
                kind: if slot % 2 == 0 {
                    RouteKind::Add
                } else {
                    RouteKind::Mul
                },
                via: None,
            }),
        );
    }
    let sources = noise(8, 1);
    assert_no_alloc(|| {
        for _ in 0..10_000 {
            m.evaluate(&sources, &mut bus);
            std::hint::black_box(bus.apply(3, 1.0));
        }
    });
}

#[test]
fn envelopes_lfos_and_followers_do_not_allocate() {
    let mut env = Envelope::new();
    env.prepare(SR);
    env.set(&EnvelopeSettings {
        loop_mode: LoopMode::Sync,
        repeat_beats: 0.25,
        ..Default::default()
    });
    let mut lfo = Lfo::new(1);
    lfo.prepare(SR);
    lfo.set(&LfoSettings {
        shape: LfoShape::SmoothRandom,
        rate: LfoRate::Beats(0.5),
        phase: 0.25,
        spin: 0.1,
        ..Default::default()
    });
    let mut follower = EnvelopeFollower::new();
    follower.prepare(SR);
    let mut clock = BeatClock::new(128.0, SR);
    clock.playing = true;
    let input = noise(BLOCK, 3);
    assert_no_alloc(|| {
        for _ in 0..4_096 {
            env.set_clock(&clock);
            lfo.set_clock(&clock);
            for &x in &input {
                std::hint::black_box(env.next());
                std::hint::black_box(lfo.next());
                std::hint::black_box(follower.process(x));
            }
            env.gate_on();
            clock.advance(BLOCK);
        }
    });
}

#[test]
fn filters_do_not_allocate_and_survive_wild_automation() {
    let mut rng = Rng::new(11);
    for circuit in [
        Circuit::Svf,
        Circuit::Ladder,
        Circuit::Ms2,
        Circuit::Dfm,
        Circuit::Prd,
    ] {
        let mut f = Filter::new();
        f.prepare(SR);
        assert_no_alloc(|| {
            for _ in 0..500 {
                let s = FilterSettings {
                    circuit,
                    kind: match rng.next_u32() % 5 {
                        0 => FilterKind::LowPass,
                        1 => FilterKind::BandPass,
                        2 => FilterKind::HighPass,
                        3 => FilterKind::Notch,
                        _ => FilterKind::Morph,
                    },
                    slope: if rng.next_u32().is_multiple_of(2) {
                        FilterSlope::Db12
                    } else {
                        FilterSlope::Db24
                    },
                    cutoff: 20.0 * 1_200f32.powf(rng.unipolar()),
                    resonance: rng.unipolar(),
                    drive: rng.unipolar() * 36.0,
                    morph: rng.unipolar(),
                };
                f.set(&s);
                for _ in 0..64 {
                    let y = f.process(rng.bipolar());
                    assert!(y.is_finite() && y.abs() <= 32.0, "{circuit:?} {y}");
                }
            }
        });
    }
}

#[test]
fn the_oversampling_path_does_not_allocate() {
    for factor in [
        OversampleFactor::X1,
        OversampleFactor::X2,
        OversampleFactor::X4,
        OversampleFactor::X8,
    ] {
        let mut os = Oversampler::new(factor, BLOCK);
        let input = noise(BLOCK, 4);
        let mut output = vec![0.0; BLOCK];
        assert_no_alloc(|| {
            for _ in 0..1_000 {
                os.process(&input, &mut output, |hi| {
                    for s in hi {
                        *s = s.tanh();
                    }
                });
            }
        });
    }
}

#[test]
fn the_sidechain_and_lookahead_do_not_allocate() {
    let mut d = SidechainDetector::new();
    d.prepare(SR, BLOCK);
    d.set(&SidechainSettings {
        eq: SidechainEq::HighPass,
        listen: true,
        ..Default::default()
    });
    let main = noise(BLOCK, 5);
    let ext = noise(BLOCK, 6);
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    let mut look = LookaheadBuffer::<2>::new(1_024);
    look.set_len(256);
    assert_no_alloc(|| {
        for i in 0..1_000 {
            d.process(
                &main,
                &main,
                Some(AudioInput {
                    left: &ext,
                    right: &ext,
                }),
            );
            d.listen(&mut AudioBuffer {
                left: &mut l,
                right: &mut r,
            });
            for x in &main {
                std::hint::black_box(look.process([*x, -*x]));
                std::hint::black_box(look.window_peak());
            }
            if i % 100 == 0 {
                look.set_len(100 + i);
            }
        }
    });
}

#[test]
fn spectral_blocks_do_not_allocate() {
    let mut stft = Stft::new(1_024, 256);
    let mut pitch = PitchDetector::new(SR, 40.0, 5_000.0, 256);
    let channel = Arc::new(ViewChannel::new(128));
    let _reader = channel.attach();
    let view = ViewWriter::new(&channel);
    let mut spectrum = SpectrumFeed::new(SR, 2_048, 512);
    let mut fft = Fft::new(512);
    let mut time = vec![0.0f32; 512];
    let mut bins = vec![Complex32::new(0.0, 0.0); fft.bins()];
    let input = noise(BLOCK, 7);
    let mut output = vec![0.0; BLOCK];
    assert_no_alloc(|| {
        for _ in 0..4_000 {
            stft.process(&input, &mut output, |b| b[0] = Complex32::new(0.0, 0.0));
            std::hint::black_box(pitch.process(&input));
            spectrum.process(&input, &view, ViewId(0));
            fft.forward(&mut time, &mut bins);
            fft.inverse(&mut bins, &mut time);
        }
    });
}

#[test]
fn convolution_does_not_allocate() {
    let ir = Arc::new(ConvolverIr::new(&noise(9_600, 8), 256));
    let other = Arc::new(ConvolverIr::new(&noise(1_024, 9), 256));
    let mut c = Convolver::with_ir(ir);
    let input = noise(BLOCK, 10);
    let mut output = vec![0.0; BLOCK];
    let old = assert_no_alloc(|| {
        for _ in 0..1_000 {
            c.process(&input, &mut output);
        }
        c.set_ir(other)
    });
    drop(old);
}

#[test]
fn delays_grains_and_wavetables_do_not_allocate() {
    let mut delay = InterpDelay::new(4_800);
    let mut smear = SmearDelay::new(4_800, 512);
    smear.set_smear(256.0);
    let mut ring = GrainRing::new(48_000);
    let mut grains = GranularEngine::new(64, 1);
    grains.prepare(SR);
    grains.set(&GrainSettings {
        rate: 200.0,
        spray: 0.02,
        pan_spread: 0.5,
        ..Default::default()
    });
    let table = generate_wavetable(TableKind::Fm, 8, 1);
    let mut osc = WavetableOsc::default();
    let mut rng = Rng::new(12);
    assert_no_alloc(|| {
        for _ in 0..200_000 {
            let x = rng.bipolar();
            delay.write(x);
            smear.write(x);
            ring.write(x);
            std::hint::black_box(delay.read(100.3, Interpolation::Cubic));
            std::hint::black_box(smear.read(300.0));
            std::hint::black_box(grains.next(&ring));
            std::hint::black_box(osc.next(&table, 0.3, 0.01));
        }
    });
}

const BAD: [f32; 5] = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1e30, -1e30];

#[test]
fn bad_input_cannot_get_into_a_feedback_or_transform_path() {
    let mut input = noise(4_096, 13);
    for (i, s) in input.iter_mut().enumerate().step_by(61) {
        *s = BAD[i % BAD.len()];
    }
    let finite = |v: &[f32], name: &str| {
        assert!(
            v.iter().all(|x| x.is_finite()),
            "{name} produced a bad sample"
        );
    };

    let mut out = vec![0.0; input.len()];
    let mut os = Oversampler::new(OversampleFactor::X4, 256);
    os.process(&input, &mut out, |_| {});
    finite(&out, "oversampler");

    let mut stft = Stft::new(512, 128);
    stft.process(&input, &mut out, |_| {});
    finite(&out, "stft");

    let mut stft = Stft::new(512, 128);
    stft.process(&input, &mut out, |bins| {
        bins[3] = Complex32::new(f32::NAN, 0.0)
    });
    finite(&out, "stft with a broken process");

    let mut conv = Convolver::with_ir(Arc::new(ConvolverIr::new(&noise(1_000, 1), 128)));
    conv.process(&input, &mut out);
    finite(&out, "convolver");

    let mut pitch = PitchDetector::new(SR, 40.0, 5_000.0, 256);
    if let Some(p) = pitch.process(&input) {
        assert!(p.hz.is_finite() && p.confidence.is_finite());
    }

    let channel = Arc::new(ViewChannel::new(128));
    let _reader = channel.attach();
    let view = ViewWriter::new(&channel);
    let mut spectrum = SpectrumFeed::new(SR, 1_024, 256);
    spectrum.process(&input, &view, ViewId(0));
    finite(spectrum.bands(), "spectrum");

    let mut delay = InterpDelay::new(512);
    let mut smear = SmearDelay::new(512, 128);
    smear.set_smear(64.0);
    let mut look = LookaheadBuffer::<1>::new(32);
    look.set_len(8);
    for x in &input {
        delay.write(*x);
        smear.write(*x);
        for v in [
            delay.read(13.7, Interpolation::Cubic),
            smear.read(100.0),
            look.process([*x])[0],
        ] {
            assert!(v.is_finite());
        }
    }

    let mut ring = GrainRing::new(4_096);
    let mut grains = GranularEngine::new(32, 1);
    grains.prepare(SR);
    for x in &input {
        ring.write(*x);
        let [l, r] = grains.next(&ring);
        assert!(l.is_finite() && r.is_finite());
    }
}

#[test]
fn absurd_settings_stay_finite() {
    let mut rng = Rng::new(21);
    let mut wild = move || -> f32 {
        match rng.next_u32() % 6 {
            0 => f32::NAN,
            1 => f32::INFINITY,
            2 => -1.0e6,
            3 => 0.0,
            4 => 1.0e6,
            _ => rng.unipolar(),
        }
    };

    let mut env = Envelope::new();
    env.prepare(SR);
    let mut lfo = Lfo::new(1);
    lfo.prepare(SR);
    let mut grains = GranularEngine::new(16, 2);
    grains.prepare(SR);
    let mut ring = GrainRing::new(8_192);
    let mut filter = Filter::new();
    filter.prepare(SR);

    for block in 0..2_000 {
        env.set(&EnvelopeSettings {
            initial: wild(),
            peak: wild(),
            sustain: wild(),
            end: wild(),
            attack: wild(),
            decay: wild(),
            release: wild(),
            attack_slope: wild(),
            decay_slope: wild(),
            release_slope: wild(),
            loop_mode: LoopMode::Loop,
            loop_time: wild(),
            ..Default::default()
        });
        lfo.set(&LfoSettings {
            shape: LfoShape::SmoothRandom,
            rate: LfoRate::Hz(wild()),
            offset: wild(),
            phase: wild(),
            spin: wild(),
        });
        grains.set(&GrainSettings {
            rate: wild(),
            size: wild(),
            pitch: wild(),
            position: wild(),
            spray: wild(),
            jitter: wild(),
            pan_spread: wild(),
            pitch_random: wild(),
        });
        filter.set(&FilterSettings {
            circuit: Circuit::Prd,
            cutoff: wild(),
            resonance: wild(),
            drive: wild(),
            morph: wild(),
            ..Default::default()
        });
        if block % 16 == 0 {
            env.gate_on();
        }
        for _ in 0..64 {
            ring.write(0.5);
            let e = env.next();
            assert!(e.is_finite(), "envelope");
            let [l, r] = lfo.next();
            assert!(l.is_finite() && r.is_finite(), "lfo");
            let [gl, gr] = grains.next(&ring);
            assert!(gl.is_finite() && gr.is_finite(), "grains");
            assert!(filter.process(0.1).is_finite(), "filter");
        }
    }
}
