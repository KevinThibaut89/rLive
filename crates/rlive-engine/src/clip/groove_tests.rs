//! Clips playing through grooves: where notes land, how hard they play, and what is left alone.

use super::audio::{AudioCursor, SESSION_VOICE};
use super::data::{ClipData, MidiClip, Note};
use super::midi::Voices;
use super::{chase, render_range, BlockTime};
use rlive_core::time::ticks_per_sample;
use rlive_core::warp::StretchBank;
use rlive_core::{
    ClipRegion, GrooveAmounts, GrooveBank, GrooveHit, GrooveMap, MidiEvent, MidiMessage, NoteSpace,
    PPQ,
};
use rlive_testkit::assert_no_alloc;
use std::sync::Arc;

const SR: f64 = 48_000.0;
const SIXTEENTH: u64 = PPQ / 4;

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

fn note(start: u64) -> Note {
    Note {
        start,
        length: SIXTEENTH / 2,
        key: 60,
        velocity: 0.5,
        ..Note::default()
    }
}

fn amounts(quantize: f32, timing: f32, random: f32, velocity: f32) -> GrooveAmounts {
    GrooveAmounts {
        quantize,
        timing,
        random,
        velocity,
    }
}

/// A one-bar groove delaying every other sixteenth by `delay` and accenting the first of each pair.
fn swing(delay: f64, a: GrooveAmounts) -> Arc<GrooveBank> {
    let hits: Vec<GrooveHit> = (0..16u64)
        .map(|i| GrooveHit {
            tick: (i * SIXTEENTH) as f64 + if i % 2 == 1 { delay } else { 0.0 },
            velocity: if i % 2 == 0 { 1.0 } else { 0.5 },
        })
        .collect();
    Arc::new(GrooveBank {
        maps: vec![GrooveMap::new(&hits, (PPQ * 4) as f64, SIXTEENTH as f64, a)],
        amount: 1.0,
    })
}

fn grooved(notes: Vec<Note>, looped: bool) -> ClipData {
    ClipData::Midi(MidiClip::new(PPQ * 4, notes, looped).with_groove(Some(0)))
}

struct Heard {
    ons: Vec<(f64, f32)>,
    offs: Vec<f64>,
}

/// What `clip` plays over `frames`, as (tick, velocity) for each note-on and the tick of each
/// note-off, rendered in small blocks. Ticks are quantised by the block's frame grid.
fn play(clip: &ClipData, bank: &Arc<GrooveBank>, seed: u64, frames: usize) -> Heard {
    let mut voices = Voices::new(NoteSpace::Session);
    voices.adopt_grooves(&Some(bank.clone()));
    voices.seed(seed);
    let mut audio = AudioCursor::new(SESSION_VOICE);
    let mut stretch = StretchBank::new(512);
    let region = ClipRegion::whole(clip.length() as f64, clip.looped());
    let (mut l, mut r) = (vec![0.0f32; 64], vec![0.0f32; 64]);
    let mut heard = Heard {
        ons: Vec::new(),
        offs: Vec::new(),
    };
    let mut t = 0.0;
    let mut done = 0;
    while done < frames {
        let n = 64.min(frames - done);
        let block = time(t, n);
        let mut ev = Vec::with_capacity(64);
        render_range(
            clip,
            region,
            0.0,
            t,
            block.end,
            block,
            &mut voices,
            &mut audio,
            &mut ev,
            &mut l[..n],
            &mut r[..n],
            &mut stretch,
        );
        for e in &ev {
            let tick = block.tick_at(e.offset as usize);
            match e.message {
                MidiMessage::NoteOn { velocity, .. } => heard.ons.push((tick, velocity)),
                MidiMessage::NoteOff { .. } => heard.offs.push(tick),
                _ => {}
            }
        }
        t = block.end;
        done += n;
    }
    heard
}

/// Rendering lands an event on a frame, so ticks are only as fine as the frame grid.
fn near(a: f64, b: f64) {
    let frame = ticks_per_sample(120.0, SR);
    assert!((a - b).abs() <= frame, "{a} is not {b}");
}

const BAR_FRAMES: usize = 96_000;

#[test]
fn a_groove_moves_notes_where_its_pattern_says() {
    let bank = swing(40.0, amounts(0.0, 1.0, 0.0, 0.0));
    let clip = grooved((0..8).map(|i| note(i * SIXTEENTH)).collect(), false);
    let heard = play(&clip, &bank, 1, BAR_FRAMES);
    assert_eq!(heard.ons.len(), 8);
    for (i, (tick, _)) in heard.ons.iter().enumerate() {
        let want = (i as u64 * SIXTEENTH) as f64 + if i % 2 == 1 { 40.0 } else { 0.0 };
        near(*tick, want);
    }
}

#[test]
fn timing_scales_the_move() {
    let clip = grooved(vec![note(SIXTEENTH)], false);
    let at = |timing| {
        play(
            &clip,
            &swing(40.0, amounts(0.0, timing, 0.0, 0.0)),
            1,
            BAR_FRAMES,
        )
        .ons[0]
            .0
    };
    near(at(1.0), SIXTEENTH as f64 + 40.0);
    near(at(0.5), SIXTEENTH as f64 + 20.0);
    near(at(0.0), SIXTEENTH as f64);
}

#[test]
fn the_note_keeps_its_length_when_it_moves() {
    let bank = swing(40.0, amounts(0.0, 1.0, 0.0, 0.0));
    let clip = grooved(vec![note(SIXTEENTH)], false);
    let heard = play(&clip, &bank, 1, BAR_FRAMES);
    near(heard.offs[0] - heard.ons[0].0, (SIXTEENTH / 2) as f64);
}

#[test]
fn velocity_follows_the_pattern_and_reverses_below_zero() {
    let clip = grooved(vec![note(0), note(SIXTEENTH)], false);
    let up = play(
        &clip,
        &swing(0.0, amounts(0.0, 0.0, 0.0, 1.0)),
        1,
        BAR_FRAMES,
    );
    assert_eq!((up.ons[0].1, up.ons[1].1), (0.75, 0.25));
    let down = play(
        &clip,
        &swing(0.0, amounts(0.0, 0.0, 0.0, -1.0)),
        1,
        BAR_FRAMES,
    );
    assert_eq!((down.ons[0].1, down.ons[1].1), (0.25, 0.75));
}

#[test]
fn quantize_alone_is_a_live_quantize_that_ignores_the_pattern() {
    let bank = swing(40.0, amounts(1.0, 0.0, 0.0, 0.0));
    let clip = grooved(vec![note(SIXTEENTH + 70), note(PPQ + 10)], false);
    let heard = play(&clip, &bank, 1, BAR_FRAMES);
    near(heard.ons[0].0, SIXTEENTH as f64);
    near(heard.ons[1].0, PPQ as f64);
    assert_eq!(heard.ons[0].1, 0.5, "velocity is untouched");
}

#[test]
fn base_decides_what_notes_are_measured_against() {
    let bank = Arc::new(GrooveBank {
        maps: vec![GrooveMap::new(
            &[GrooveHit {
                tick: 960.0 + 60.0,
                velocity: 0.5,
            }],
            (PPQ * 4) as f64,
            PPQ as f64,
            amounts(0.0, 1.0, 0.0, 0.0),
        )],
        amount: 1.0,
    });
    let clip = grooved(vec![note(PPQ), note(PPQ * 2)], false);
    let heard = play(&clip, &bank, 1, BAR_FRAMES);
    near(heard.ons[0].0, PPQ as f64 + 60.0);
    near(heard.ons[1].0, (PPQ * 2) as f64);
}

#[test]
fn random_repeats_with_its_seed_and_separates_notes_played_together() {
    let bank = swing(0.0, amounts(0.0, 0.0, 1.0, 0.0));
    let notes = (0..8).map(|_| note(PPQ)).collect();
    let clip = grooved(notes, false);
    let a = play(&clip, &bank, 7, BAR_FRAMES).ons;
    let b = play(&clip, &bank, 7, BAR_FRAMES).ons;
    assert_eq!(a, b, "the same seed plays the same");
    assert!(
        a.windows(2).any(|w| w[0].0 != w[1].0),
        "voices of a chord land apart"
    );
    let c = play(&clip, &bank, 8, BAR_FRAMES).ons;
    assert_ne!(a, c, "another run of playback differs");
}

#[test]
fn a_note_pushed_past_the_end_of_the_loop_still_plays() {
    let bank = swing(100.0, amounts(0.0, 1.0, 0.0, 0.0));
    let clip = grooved(vec![note(PPQ * 4 - SIXTEENTH)], true);
    let heard = play(&clip, &bank, 1, BAR_FRAMES * 2 + 8_000);
    assert_eq!(heard.ons.len(), 2, "{:?}", heard.ons);
    near(heard.ons[0].0, (PPQ * 4 - SIXTEENTH + 100) as f64);
    near(heard.ons[1].0, (PPQ * 8 - SIXTEENTH + 100) as f64);
}

#[test]
fn nothing_plays_before_its_pass_has_begun() {
    let bank = swing(0.0, amounts(1.0, 0.0, 0.0, 0.0));
    // A note just ahead of the first grid line of the clip snaps to it, and no earlier.
    let clip = grooved(vec![note(0)], false);
    assert_eq!(play(&clip, &bank, 1, BAR_FRAMES).ons.len(), 1);
}

#[test]
fn notes_in_the_clip_are_never_touched() {
    let bank = swing(40.0, amounts(1.0, 1.0, 1.0, 1.0));
    let clip = grooved(vec![note(SIXTEENTH)], false);
    let before = clip.clone();
    play(&clip, &bank, 1, BAR_FRAMES);
    assert_eq!(clip, before);
}

#[test]
fn a_clip_with_no_groove_plays_as_written() {
    let bank = swing(40.0, amounts(1.0, 1.0, 1.0, 1.0));
    let clip = ClipData::Midi(MidiClip::new(PPQ * 4, vec![note(SIXTEENTH + 7)], false));
    let heard = play(&clip, &bank, 1, BAR_FRAMES);
    near(heard.ons[0].0, (SIXTEENTH + 7) as f64);
    assert_eq!(heard.ons[0].1, 0.5);
}

#[test]
fn chase_starts_the_note_a_groove_has_moved_under_the_playhead() {
    let bank = swing(40.0, amounts(0.0, 1.0, 0.0, 0.0));
    let clip = grooved(vec![note(SIXTEENTH)], false);
    let region = ClipRegion::whole(clip.length() as f64, false);
    let mut voices = Voices::new(NoteSpace::Session);
    voices.adopt_grooves(&Some(bank));
    let mut ev: Vec<MidiEvent> = Vec::with_capacity(16);
    // Before the note as written has ended, and after where the groove begins it.
    chase(
        &clip,
        region,
        0.0,
        SIXTEENTH as f64 + 41.0,
        0,
        &mut voices,
        &mut ev,
    );
    assert!(matches!(ev[0].message, MidiMessage::NoteOn { key: 60, .. }));

    // Where the note is written, the groove hasn't begun it yet.
    let mut voices = Voices::new(NoteSpace::Session);
    let bank = swing(40.0, amounts(0.0, 1.0, 0.0, 0.0));
    voices.adopt_grooves(&Some(bank));
    let mut ev: Vec<MidiEvent> = Vec::with_capacity(16);
    chase(
        &clip,
        region,
        0.0,
        SIXTEENTH as f64 + 10.0,
        0,
        &mut voices,
        &mut ev,
    );
    assert!(ev.is_empty());
}

#[test]
fn grooved_playback_never_allocates() {
    let bank = swing(40.0, amounts(0.5, 1.0, 0.5, 0.5));
    let clip = grooved((0..16).map(|i| note(i * SIXTEENTH)).collect(), true);
    let mut voices = Voices::new(NoteSpace::Session);
    voices.adopt_grooves(&Some(bank));
    let mut audio = AudioCursor::new(SESSION_VOICE);
    let mut stretch = StretchBank::new(512);
    let region = ClipRegion::whole(clip.length() as f64, true);
    let mut ev: Vec<MidiEvent> = Vec::with_capacity(256);
    let (mut l, mut r) = (vec![0.0f32; 256], vec![0.0f32; 256]);
    assert_no_alloc(|| {
        let mut t = 0.0;
        for _ in 0..2000 {
            ev.clear();
            let block = time(t, 256);
            render_range(
                &clip,
                region,
                0.0,
                t,
                block.end,
                block,
                &mut voices,
                &mut audio,
                &mut ev,
                &mut l,
                &mut r,
                &mut stretch,
            );
            t = block.end;
        }
    });
}
