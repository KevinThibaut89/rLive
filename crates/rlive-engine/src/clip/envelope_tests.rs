//! Clip envelopes for a clip's own content, through the real clip renderer and stretchers.

use super::audio::AudioCursor;
use super::data::{AudioClip, ClipData, WarpedClip};
use super::envelopes::{enter_clip, ClipCurve, ClipEnvelopes, EnvCurve, EnvTiming, EnvelopedClip};
use super::midi::Voices;
use super::{render_range, BlockTime, MidiClip};
use rlive_core::time::ticks_per_sample;
use rlive_core::warp::{BeatsLoop, BeatsPreserve, StretchBank, WarpMap, WarpMarker, WarpMode};
use rlive_core::{MidiEvent, MidiMessage, NoteSpace, ParamRange, SampleBuffer, PPQ};
use rlive_stretch::bank;
use rlive_testkit::assert_no_alloc;
use std::sync::Arc;

const SR: f64 = 48_000.0;
const BEAT: f64 = PPQ as f64;
const FRAMES: usize = 96_000;
const BLOCK: usize = 256;

fn range(min: f64, max: f64) -> ParamRange {
    ParamRange {
        min,
        max,
        skew: 1.0,
        quantized: false,
    }
}

fn beats() -> WarpMode {
    WarpMode::Beats {
        preserve: BeatsPreserve::Transients,
        beats_loop: BeatsLoop::Off,
        transient_envelope: 100.0,
    }
}

fn tone(hz: f64) -> Arc<SampleBuffer> {
    let left: Vec<f32> = (0..FRAMES)
        .map(|i| (i as f64 / SR * hz * std::f64::consts::TAU).sin() as f32 * 0.5)
        .collect();
    Arc::new(SampleBuffer {
        name: "tone".into(),
        right: left.clone(),
        left,
        sample_rate: SR,
    })
}

fn audio(sample: Arc<SampleBuffer>, looped: bool) -> AudioClip {
    AudioClip {
        scale: None,
        sample,
        length: 4 * PPQ,
        looped,
        gain: 1.0,
    }
}

fn warped(sample: Arc<SampleBuffer>, mode: WarpMode, slices: &[f64]) -> ClipData {
    ClipData::Warped(WarpedClip {
        audio: audio(sample, true),
        map: WarpMap::new([
            WarpMarker::new(0.0, 0.0),
            WarpMarker::new(FRAMES as f64, 4.0 * BEAT),
        ]),
        mode,
        transpose: 0.0,
        slices: Arc::from(slices.to_vec()),
    })
}

fn linked_curve(range: ParamRange, points: &[(f64, f64)]) -> ClipCurve {
    ClipCurve::new(
        EnvCurve::new(range, points.iter().map(|&(t, v)| (t, v, 0.0))).unwrap(),
        EnvTiming::Linked {
            length: 4.0 * BEAT,
            looped: true,
        },
    )
}

fn with(clip: ClipData, envelopes: ClipEnvelopes) -> ClipData {
    ClipData::Enveloped(Box::new(EnvelopedClip {
        clip,
        envelopes,
        region: None,
        edge_fade: false,
        hi_q: false,
    }))
}

fn time(t0: f64, frames: usize) -> BlockTime {
    let tps = ticks_per_sample(120.0, SR);
    BlockTime {
        t0,
        ticks_per_sample: tps,
        ramp: 0.0,
        end: t0 + frames as f64 * tps,
        frames,
        sample_rate: SR,
        tempo: 120.0,
    }
}

struct Rendered {
    left: Vec<f32>,
    cursor: AudioCursor,
    events: Vec<MidiEvent>,
}

fn render(clip: &ClipData, frames: usize) -> Rendered {
    let mut stretch: StretchBank = bank(BLOCK);
    stretch.prepare(SR);
    let mut voices = Voices::new(NoteSpace::Session);
    let mut cursor = AudioCursor::new(0);
    let mut left = vec![0.0f32; frames];
    let mut right = vec![0.0f32; frames];
    let mut events = Vec::with_capacity(4096);
    let mut done = 0;
    let mut t = 0.0;
    while done < frames {
        let n = BLOCK.min(frames - done);
        let block = time(t, n);
        let (mut bl, mut br) = (vec![0.0f32; n], vec![0.0f32; n]);
        let mut chunk = Vec::with_capacity(4096);
        assert_no_alloc(|| {
            render_range(
                clip,
                clip.region(),
                0.0,
                t,
                block.end,
                block,
                &mut voices,
                &mut cursor,
                &mut chunk,
                &mut bl,
                &mut br,
                &mut stretch,
            );
        });
        for mut e in chunk {
            e.offset += done as u32;
            events.push(e);
        }
        left[done..done + n].copy_from_slice(&bl);
        right[done..done + n].copy_from_slice(&br);
        done += n;
        t = block.end;
    }
    Rendered {
        left,
        cursor,
        events,
    }
}

fn rms(s: &[f32]) -> f64 {
    (s.iter().map(|x| (x * x) as f64).sum::<f64>() / s.len() as f64).sqrt()
}

/// Frequency from positive-going zero crossings.
fn pitch_hz(s: &[f32]) -> f64 {
    let crossings: Vec<usize> = (1..s.len())
        .filter(|&i| s[i - 1] <= 0.0 && s[i] > 0.0)
        .collect();
    let (first, last) = (crossings[0], *crossings.last().unwrap());
    (crossings.len() - 1) as f64 / ((last - first) as f64 / SR)
}

#[test]
fn a_gain_envelope_is_a_share_of_the_clip_gain() {
    let gain = linked_curve(range(0.0, 1.0), &[(0.0, 0.5), (4.0 * BEAT, 0.5)]);
    let clip = with(
        ClipData::Audio(audio(tone(440.0), true)),
        ClipEnvelopes {
            gain: Some(gain),
            ..ClipEnvelopes::default()
        },
    );
    let out = render(&clip, 48_000).left;
    let plain = render(&ClipData::Audio(audio(tone(440.0), true)), 48_000).left;
    let ratio = rms(&out[4_000..]) / rms(&plain[4_000..]);
    assert!((ratio - 0.5).abs() < 0.01, "{ratio}");
}

#[test]
fn a_stepped_gain_envelope_can_silence_a_part_of_the_clip() {
    let gain = linked_curve(
        range(0.0, 1.0),
        &[
            (0.0, 1.0),
            (BEAT, 1.0),
            (BEAT, 0.0),
            (2.0 * BEAT, 0.0),
            (2.0 * BEAT, 1.0),
        ],
    );
    let clip = with(
        warped(tone(440.0), WarpMode::RePitch, &[]),
        ClipEnvelopes {
            gain: Some(gain),
            ..ClipEnvelopes::default()
        },
    );
    let out = render(&clip, FRAMES / 2).left;
    let beat = (SR / 2.0) as usize;
    assert!(rms(&out[2_000..beat - 1_000]) > 0.2, "sounding");
    assert!(rms(&out[beat + 2_000..2 * beat - 1_000]) < 1e-3, "silenced");
    // The step is smoothed.
    assert!((out[beat - 1] as f64 - out[beat] as f64).abs() < 0.2);
}

#[test]
fn a_transposition_envelope_adds_to_the_clips_and_is_clamped() {
    let transpose = linked_curve(range(-48.0, 48.0), &[(0.0, 12.0)]);
    let mut clip = warped(tone(220.0), beats(), &[0.0]);
    if let ClipData::Warped(w) = &mut clip {
        w.transpose = 12.0;
    }
    let up = with(
        clip.clone(),
        ClipEnvelopes {
            transpose: Some(transpose),
            ..ClipEnvelopes::default()
        },
    );
    // 12 from the clip and 12 from the envelope.
    let out = render(&up, 40_000).left;
    let hz = pitch_hz(&out[8_000..]);
    assert!((hz - 880.0).abs() < 20.0, "{hz}");

    // A clip at 40 semitones with an envelope of 24 stops at 48.
    let top = linked_curve(range(-48.0, 48.0), &[(0.0, 24.0)]);
    if let ClipData::Warped(w) = &mut clip {
        w.transpose = 40.0;
    }
    let clamped = with(
        clip,
        ClipEnvelopes {
            transpose: Some(top),
            ..ClipEnvelopes::default()
        },
    );
    let out = render(&clamped, 20_000).left;
    let hz = pitch_hz(&out[4_000..]);
    let at_48 = 220.0 * 2f64.powf(48.0 / 12.0);
    assert!((hz - at_48).abs() / at_48 < 0.05, "{hz} vs {at_48}");
}

#[test]
fn sample_offset_moves_the_read_only_in_beats_mode() {
    let two_sixteenths = 2.0 * PPQ as f64 / 4.0;
    let offset = linked_curve(range(-8.0, 8.0), &[(0.0, 2.0)]);
    let beats_clip = with(
        warped(tone(440.0), beats(), &[0.0]),
        ClipEnvelopes {
            sample_offset: Some(offset.clone()),
            ..ClipEnvelopes::default()
        },
    );
    let frames = 256;
    let moved = render(&beats_clip, frames).cursor.src;
    let straight = render(&warped(tone(440.0), beats(), &[0.0]), frames)
        .cursor
        .src;
    // 960 ticks is 24 000 frames on this straight map.
    let per_tick = FRAMES as f64 / (4.0 * BEAT);
    assert!(
        ((moved - straight) - two_sixteenths * per_tick).abs() < 2.0,
        "{moved} {straight}"
    );

    let repitch = with(
        warped(tone(440.0), WarpMode::RePitch, &[]),
        ClipEnvelopes {
            sample_offset: Some(offset),
            ..ClipEnvelopes::default()
        },
    );
    let not_moved = render(&repitch, frames).cursor.src;
    assert!((not_moved - straight).abs() < 2.0, "{not_moved} {straight}");
}

#[test]
fn sample_offset_past_the_end_of_the_loop_comes_round() {
    let offset = linked_curve(range(-8.0, 8.0), &[(0.0, 8.0)]);
    let clip = with(
        warped(tone(440.0), beats(), &[0.0]),
        ClipEnvelopes {
            sample_offset: Some(offset),
            ..ClipEnvelopes::default()
        },
    );
    // After 3.5 beats, 8 sixteenths on is 1.5 beats past the end of the four-beat loop.
    let out = render(&clip, (3.5 * FRAMES as f64 / 4.0) as usize);
    let at = 1.5 * FRAMES as f64 / 4.0;
    assert!(
        (out.cursor.src - at).abs() < 64.0,
        "{} vs {at}",
        out.cursor.src
    );
}

fn cc_events(events: &[MidiEvent], cc: u8) -> Vec<(u32, u8)> {
    events
        .iter()
        .filter_map(|e| match e.message {
            MidiMessage::ControlChange { cc: c, value, .. } if c == cc => Some((e.offset, value)),
            _ => None,
        })
        .collect()
}

#[test]
fn midi_controller_changes_arrive_at_their_offsets() {
    // A step from 10 to 100 on beat 1, and a ramp up to 127 on the next.
    let curve = linked_curve(
        range(0.0, 127.0),
        &[
            (0.0, 10.0),
            (BEAT, 10.0),
            (BEAT, 100.0),
            (BEAT * 2.0, 127.0),
        ],
    );
    let clip = with(
        ClipData::Midi(MidiClip::new(4 * PPQ, Vec::new(), true)),
        ClipEnvelopes {
            midi: vec![(1, curve)].into_boxed_slice(),
            ..ClipEnvelopes::default()
        },
    );
    let out = render(&clip, 48_000);
    let events = cc_events(&out.events, 1);
    let tps = ticks_per_sample(120.0, SR);
    let step_frame = (BEAT / tps).round() as u32;
    let step = events
        .iter()
        .find(|(_, v)| *v == 100)
        .expect("the step arrives");
    assert!(
        step.0.abs_diff(step_frame) <= 1,
        "the step goes out at beat 1: {step:?} vs {step_frame}"
    );
    assert!(events.iter().any(|(_, v)| *v > 100), "the ramp follows it");
    let ramp: Vec<u8> = events.iter().map(|(_, v)| *v).collect();
    assert!(ramp.windows(2).all(|w| w[0] <= w[1]), "{ramp:?}");
}

#[test]
fn a_clip_starts_with_its_controllers_where_the_envelope_has_them() {
    let curve = linked_curve(range(0.0, 127.0), &[(0.0, 42.0)]);
    let clip = with(
        ClipData::Midi(MidiClip::new(4 * PPQ, Vec::new(), true)),
        ClipEnvelopes {
            midi: vec![(74, curve)].into_boxed_slice(),
            ..ClipEnvelopes::default()
        },
    );
    let mut ev = Vec::with_capacity(16);
    enter_clip(&clip, 0.0, 3, false, &mut ev);
    assert_eq!(cc_events(&ev, 74), [(3, 42)]);
}

#[test]
fn auto_reset_sets_controllers_back_before_a_midi_clip() {
    let clip = ClipData::Midi(MidiClip::new(4 * PPQ, Vec::new(), true));
    let mut ev = Vec::with_capacity(16);
    enter_clip(&clip, 0.0, 0, true, &mut ev);
    assert_eq!(cc_events(&ev, 1), [(0, 0)]);
    assert!(ev
        .iter()
        .any(|e| matches!(e.message, MidiMessage::PitchBend { value: 0.0, .. })));

    let mut ev = Vec::with_capacity(16);
    enter_clip(&clip, 0.0, 0, false, &mut ev);
    assert!(ev.is_empty());
    let audio = ClipData::Audio(audio(tone(440.0), true));
    let mut ev = Vec::with_capacity(16);
    enter_clip(&audio, 0.0, 0, true, &mut ev);
    assert!(ev.is_empty(), "there's no MIDI in an audio clip");
}

#[test]
fn an_odd_unlinked_gain_loop_comes_round_on_its_own_time() {
    // A loop of 3.25 beats, muted over its second half, on a clip of four beats.
    let len = 3.25 * BEAT;
    let curve = ClipCurve::new(
        EnvCurve::new(
            range(0.0, 1.0),
            [
                (0.0, 1.0, 0.0),
                (len / 2.0, 1.0, 0.0),
                (len / 2.0, 0.0, 0.0),
                (len, 0.0, 0.0),
            ],
        )
        .unwrap(),
        EnvTiming::Unlinked {
            start: 0.0,
            end: len,
            marker: 0.0,
            looped: true,
        },
    );
    let clip = with(
        ClipData::Audio(audio(tone(440.0), true)),
        ClipEnvelopes {
            gain: Some(curve),
            ..ClipEnvelopes::default()
        },
    );
    let tps = ticks_per_sample(120.0, SR);
    let out = render(&clip, 96_000).left;
    let frame = |beats: f64| (beats * BEAT / tps) as usize;
    assert!(rms(&out[frame(0.25)..frame(1.0)]) > 0.2);
    assert!(rms(&out[frame(2.0)..frame(3.0)]) < 1e-3);
    // It comes round at 3.25 beats, half a beat before the clip's own loop point.
    assert!(rms(&out[frame(3.4)..frame(3.9)]) > 0.2);
}

#[test]
fn a_clip_without_envelopes_plays_as_it_always_did() {
    let plain = warped(tone(440.0), beats(), &[0.0]);
    let wrapped = with(plain.clone(), ClipEnvelopes::default());
    let a = render(&plain, 8_000).left;
    let b = render(&wrapped, 8_000).left;
    assert_eq!(a, b, "an empty envelope set changes nothing");
}
