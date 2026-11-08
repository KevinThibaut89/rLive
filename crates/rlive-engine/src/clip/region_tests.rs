//! Clip markers in playback: start, end and loop start, for MIDI and audio, launched and on the
//! timeline, including running in to a loop.

use super::audio::{AudioCursor, LANE_VOICE, SESSION_VOICE};
use super::data::{AudioClip, ClipData, MidiClip, Note, WarpedClip};
use super::envelopes::{ClipCurve, ClipEnvelopes, EnvCurve, EnvTiming, EnvelopedClip};
use super::midi::Voices;
use super::{chase, render_range, BlockTime, ClipPlayer, LaunchEnv, Pending, SlotLaunch};
use crate::lane::{ArrangementLane, LaneClip, LaneFades, LanePlayer};
use rlive_core::fade::EdgeFades;
use rlive_core::time::{ticks_per_sample, BarMap};
use rlive_core::warp::{WarpMap, WarpMarker, WarpMode};
use rlive_core::{
    ClipRegion, FollowActions, MidiEvent, MidiMessage, NoteSpace, ParamRange, Quantize,
    SampleBuffer, TimeSignature, PPQ,
};
use std::sync::Arc;

const SR: f64 = 48_000.0;
const BEAT_FRAMES: usize = 24_000;
const BEAT: u64 = PPQ;

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

fn note(start: u64, key: u8) -> Note {
    Note {
        start,
        length: BEAT / 2,
        key,
        velocity: 1.0,
        ..Note::default()
    }
}

fn with_region(clip: ClipData, region: ClipRegion) -> Arc<ClipData> {
    Arc::new(ClipData::Enveloped(Box::new(EnvelopedClip {
        clip,
        envelopes: ClipEnvelopes::default(),
        region: Some(region),
        edge_fade: false,
        hi_q: false,
    })))
}

fn midi(notes: Vec<Note>, length: u64, looped: bool) -> ClipData {
    ClipData::Midi(MidiClip::new(length, notes, looped))
}

/// Every note-on `render` makes over `frames`, as (tick, key), in blocks of 256.
fn note_ons(
    frames: usize,
    mut render: impl FnMut(BlockTime, &mut Vec<MidiEvent>),
) -> Vec<(f64, u8)> {
    let mut out = Vec::new();
    let mut t = 0.0;
    let mut done = 0;
    while done < frames {
        let n = 256.min(frames - done);
        let block = time(t, n);
        let mut ev = Vec::with_capacity(512);
        render(block, &mut ev);
        for e in &ev {
            if let MidiMessage::NoteOn { key, .. } = e.message {
                out.push((t + e.offset as f64 * block.ticks_per_sample, key));
            }
        }
        t = block.end;
        done += n;
    }
    out
}

fn range_ons(clip: &ClipData, region: ClipRegion, frames: usize) -> Vec<(f64, u8)> {
    let mut voices = Voices::new(NoteSpace::Session);
    let mut cursor = AudioCursor::new(0);
    let mut stretch = rlive_stretch::bank(512);
    stretch.prepare(SR);
    note_ons(frames, |block, ev| {
        let (mut l, mut r) = (vec![0.0f32; block.frames], vec![0.0f32; block.frames]);
        render_range(
            clip,
            region,
            0.0,
            block.t0,
            block.end,
            block,
            &mut voices,
            &mut cursor,
            ev,
            &mut l,
            &mut r,
            &mut stretch,
        );
    })
}

/// One block of a player with nothing but default launch settings.
fn render_player(
    player: &mut ClipPlayer,
    block: BlockTime,
    ev: &mut Vec<MidiEvent>,
    l: &mut [f32],
    r: &mut [f32],
    slots: &[Option<Arc<ClipData>>],
    stretch: &mut rlive_core::warp::StretchBank,
) {
    render_with(player, block, ev, l, r, slots, &[], stretch);
}

#[allow(clippy::too_many_arguments)]
fn render_with(
    player: &mut ClipPlayer,
    block: BlockTime,
    ev: &mut Vec<MidiEvent>,
    l: &mut [f32],
    r: &mut [f32],
    slots: &[Option<Arc<ClipData>>],
    launch: &[SlotLaunch],
    stretch: &mut rlive_core::warp::StretchBank,
) {
    let bars = BarMap::new(TimeSignature::default(), []);
    let env = LaunchEnv {
        bars: &bars,
        global: Quantize::None,
        chase: false,
    };
    player.render(block, ev, l, r, &mut |_| {}, slots, launch, env, stretch);
}

fn beats(ons: &[(f64, u8)]) -> Vec<(u64, u8)> {
    ons.iter()
        .map(|(t, k)| ((t / BEAT as f64).round() as u64, *k))
        .collect()
}

fn four_notes() -> Vec<Note> {
    vec![
        note(0, 60),
        note(BEAT, 61),
        note(2 * BEAT, 62),
        note(3 * BEAT, 63),
    ]
}

#[test]
fn a_session_midi_clip_starts_at_its_start_marker() {
    let region = ClipRegion::new(BEAT as f64, None, 0.0, 4.0 * BEAT as f64, true);
    let ons = range_ons(&midi(four_notes(), 4 * BEAT, true), region, 6 * BEAT_FRAMES);
    // Plays 1, 2, 3 from the start marker, then the whole loop from its own start.
    assert_eq!(
        beats(&ons),
        vec![(0, 61), (1, 62), (2, 63), (3, 60), (4, 61), (5, 62)]
    );
}

#[test]
fn a_midi_clip_runs_in_before_its_loop() {
    // Loop of beats 2-4, played from the top.
    let region = ClipRegion::new(0.0, None, 2.0 * BEAT as f64, 2.0 * BEAT as f64, true);
    let ons = range_ons(&midi(four_notes(), 2 * BEAT, true), region, 8 * BEAT_FRAMES);
    let got: Vec<u8> = ons.iter().map(|o| o.1).collect();
    assert_eq!(got, vec![60, 61, 62, 63, 62, 63, 62, 63]);
    assert_eq!(
        beats(&ons)[4],
        (4, 62),
        "the loop comes round when the run-in has played through"
    );
}

#[test]
fn a_loop_with_notes_before_it_never_plays_them_once_inside() {
    let region = ClipRegion::new(
        2.0 * BEAT as f64,
        None,
        2.0 * BEAT as f64,
        BEAT as f64,
        true,
    );
    let ons = range_ons(&midi(four_notes(), BEAT, true), region, 3 * BEAT_FRAMES);
    assert_eq!(beats(&ons), vec![(0, 62), (1, 62), (2, 62)]);
}

#[test]
fn an_unlooped_midi_clip_stops_at_its_end_marker() {
    let region = ClipRegion::new(0.0, Some(2.0 * BEAT as f64), 0.0, 4.0 * BEAT as f64, false);
    let ons = range_ons(
        &midi(four_notes(), 4 * BEAT, false),
        region,
        6 * BEAT_FRAMES,
    );
    assert_eq!(beats(&ons), vec![(0, 60), (1, 61)]);
}

#[test]
fn the_launcher_plays_the_region_and_reports_its_position() {
    let clip = with_region(
        midi(four_notes(), 2 * BEAT, true),
        ClipRegion::new(0.0, None, 2.0 * BEAT as f64, 2.0 * BEAT as f64, true),
    );
    let slots = vec![Some(clip)];
    let mut player = ClipPlayer::new();
    player.pending = Some(Pending::Launch {
        slot: 0,
        at: 0.0,
        velocity: 1.0,
    });
    let mut stretch = rlive_stretch::bank(512);
    stretch.prepare(SR);
    let ons = note_ons(5 * BEAT_FRAMES, |block, ev| {
        let (mut l, mut r) = (vec![0.0f32; block.frames], vec![0.0f32; block.frames]);
        render_player(&mut player, block, ev, &mut l, &mut r, &slots, &mut stretch);
    });
    assert_eq!(
        ons.iter().map(|o| o.1).collect::<Vec<_>>(),
        [60, 61, 62, 63, 62]
    );
    let at = player.position(5.0 * BEAT as f64 + 10.0);
    assert!((at - (3.0 * BEAT as f64 + 10.0)).abs() < 1e-6, "{at}");
}

#[test]
fn a_timeline_clip_runs_from_its_markers_and_chases() {
    let region = ClipRegion::new(0.0, None, 2.0 * BEAT as f64, 2.0 * BEAT as f64, true);
    let mut data = four_notes();
    data[1].length = 3 * BEAT;
    let clip = with_region(midi(data, 2 * BEAT, true), region);
    let lane = Arc::new(ArrangementLane {
        clips: vec![LaneClip {
            start: BEAT,
            end: 7 * BEAT,
            offset: 0,
            data: clip.clone(),
            fades: Default::default(),
        }],
    });
    let mut player = LanePlayer::new(lane.clone());
    let mut stretch = rlive_stretch::bank(512);
    stretch.prepare(SR);
    let ons = note_ons(8 * BEAT_FRAMES, |block, ev| {
        let (mut l, mut r) = (vec![0.0f32; block.frames], vec![0.0f32; block.frames]);
        player.render(block, block.t0, block.end, ev, &mut l, &mut r, &mut stretch);
    });
    assert_eq!(
        beats(&ons),
        vec![(1, 60), (2, 61), (3, 62), (4, 63), (5, 62), (6, 63)]
    );

    // Entering mid-way chases the note already sounding in its pass.
    let mut voices = Voices::new(NoteSpace::Arrangement);
    let mut ev = Vec::with_capacity(16);
    chase(
        &clip,
        region,
        0.0,
        BEAT as f64 + 100.0,
        0,
        &mut voices,
        &mut ev,
    );
    assert!(
        matches!(ev[0].message, MidiMessage::NoteOn { key: 61, .. }),
        "{ev:?}"
    );
}

/// A file whose every beat is a constant level: 0.1 for the first, 0.2 for the second and so on.
fn stepped(beats: usize) -> Arc<SampleBuffer> {
    let left: Vec<f32> = (0..beats * BEAT_FRAMES)
        .map(|i| (i / BEAT_FRAMES + 1) as f32 * 0.1)
        .collect();
    Arc::new(SampleBuffer {
        name: "steps".into(),
        right: left.clone(),
        left,
        sample_rate: SR,
    })
}

fn audio(length: u64, looped: bool) -> AudioClip {
    AudioClip {
        sample: stepped(4),
        length,
        looped,
        gain: 1.0,
        scale: None,
    }
}

/// The level of beat `n` of what `clip` plays, by looking halfway through it.
fn levels(clip: &ClipData, region: ClipRegion, count: usize) -> Vec<f32> {
    let mut voices = Voices::new(NoteSpace::Session);
    let mut cursor = AudioCursor::new(SESSION_VOICE);
    let mut stretch = rlive_stretch::bank(512);
    stretch.prepare(SR);
    let mut left = vec![0.0f32; count * BEAT_FRAMES];
    let mut t = 0.0;
    let mut done = 0;
    while done < left.len() {
        let n = 256.min(left.len() - done);
        let block = time(t, n);
        let (mut l, mut r) = (vec![0.0f32; n], vec![0.0f32; n]);
        let mut ev = Vec::with_capacity(16);
        render_range(
            clip,
            region,
            0.0,
            block.t0,
            block.end,
            block,
            &mut voices,
            &mut cursor,
            &mut ev,
            &mut l,
            &mut r,
            &mut stretch,
        );
        left[done..done + n].copy_from_slice(&l);
        t = block.end;
        done += n;
    }
    (0..count)
        .map(|b| left[b * BEAT_FRAMES + BEAT_FRAMES / 2])
        .collect()
}

fn near(got: Vec<f32>, want: &[f32]) {
    assert_eq!(got.len(), want.len());
    for (g, w) in got.iter().zip(want) {
        assert!((g - w).abs() < 1e-3, "{got:?} vs {want:?}");
    }
}

#[test]
fn unwarped_audio_starts_at_its_start_marker_and_loops_on_its_own_loop() {
    let clip = ClipData::Audio(audio(2 * BEAT, true));
    let region = ClipRegion::new(
        BEAT as f64,
        None,
        2.0 * BEAT as f64,
        2.0 * BEAT as f64,
        true,
    );
    near(levels(&clip, region, 6), &[0.2, 0.3, 0.4, 0.3, 0.4, 0.3]);
}

#[test]
fn an_unlooped_audio_clip_ends_at_its_end_marker() {
    let clip = ClipData::Audio(audio(4 * BEAT, false));
    let region = ClipRegion::new(
        BEAT as f64,
        Some(3.0 * BEAT as f64),
        0.0,
        4.0 * BEAT as f64,
        false,
    );
    near(levels(&clip, region, 3), &[0.2, 0.3, 0.0]);
}

fn warped(mode: WarpMode) -> ClipData {
    let frames = 4.0 * BEAT_FRAMES as f64;
    ClipData::Warped(WarpedClip {
        audio: audio(2 * BEAT, true),
        map: WarpMap::new([
            WarpMarker::new(0.0, 0.0),
            WarpMarker::new(frames, 4.0 * BEAT as f64),
        ]),
        mode,
        transpose: 0.0,
        slices: Arc::from(Vec::new()),
    })
}

#[test]
fn a_following_warped_clip_honours_its_markers() {
    let region = ClipRegion::new(
        BEAT as f64,
        None,
        2.0 * BEAT as f64,
        2.0 * BEAT as f64,
        true,
    );
    near(
        levels(&warped(WarpMode::RePitch), region, 6),
        &[0.2, 0.3, 0.4, 0.3, 0.4, 0.3],
    );
}

#[test]
fn a_passthrough_warped_clip_honours_its_markers() {
    let region = ClipRegion::new(
        BEAT as f64,
        None,
        2.0 * BEAT as f64,
        2.0 * BEAT as f64,
        true,
    );
    near(
        levels(&warped(WarpMode::Complex), region, 6),
        &[0.2, 0.3, 0.4, 0.3, 0.4, 0.3],
    );
}

#[test]
fn a_timeline_audio_clip_plays_from_its_offset_in_the_run() {
    let clip = with_region(
        ClipData::Audio(audio(2 * BEAT, true)),
        ClipRegion::new(0.0, None, 2.0 * BEAT as f64, 2.0 * BEAT as f64, true),
    );
    let lane = Arc::new(ArrangementLane {
        clips: vec![LaneClip {
            start: 0,
            end: 6 * BEAT,
            // Trimmed one beat into its run.
            offset: BEAT,
            data: clip,
            fades: Default::default(),
        }],
    });
    let mut player = LanePlayer::new(lane);
    let mut stretch = rlive_stretch::bank(512);
    stretch.prepare(SR);
    let mut left = vec![0.0f32; 6 * BEAT_FRAMES];
    let mut t = 0.0;
    let mut done = 0;
    while done < left.len() {
        let n = 256.min(left.len() - done);
        let block = time(t, n);
        let (mut l, mut r) = (vec![0.0f32; n], vec![0.0f32; n]);
        let mut ev = Vec::with_capacity(16);
        player.render(
            block,
            block.t0,
            block.end,
            &mut ev,
            &mut l,
            &mut r,
            &mut stretch,
        );
        left[done..done + n].copy_from_slice(&l);
        t = block.end;
        done += n;
    }
    let at: Vec<f32> = (0..6)
        .map(|b| left[b * BEAT_FRAMES + BEAT_FRAMES / 2])
        .collect();
    near(at, &[0.2, 0.3, 0.4, 0.3, 0.4, 0.3]);
}

#[test]
fn a_default_region_renders_the_same_bits_with_and_without_the_wrapper() {
    let plain = ClipData::Audio(audio(BEAT, true));
    let region = ClipRegion::whole(BEAT as f64, true);
    let render = |clip: &ClipData| {
        let mut voices = Voices::new(NoteSpace::Session);
        let mut cursor = AudioCursor::new(LANE_VOICE);
        let mut stretch = rlive_stretch::bank(1024);
        stretch.prepare(SR);
        let mut bits = Vec::new();
        let mut t = 0.0;
        for _ in 0..40 {
            let block = time(t, 777);
            let (mut l, mut r) = (vec![0.0f32; 777], vec![0.0f32; 777]);
            let mut ev = Vec::with_capacity(16);
            render_range(
                clip,
                region,
                0.0,
                block.t0,
                block.end,
                block,
                &mut voices,
                &mut cursor,
                &mut ev,
                &mut l,
                &mut r,
                &mut stretch,
            );
            bits.extend(l.iter().map(|s| s.to_bits()));
            t = block.end;
        }
        bits
    };
    let wrapped = with_region(plain.clone(), region);
    assert_eq!(render(&plain), render(&wrapped));
}

#[test]
fn a_scrub_jumps_and_a_held_one_repeats() {
    let slots = vec![Some(Arc::new(midi(four_notes(), 4 * BEAT, true)))];
    let mut player = ClipPlayer::new();
    player.pending = Some(Pending::Scrub {
        slot: 0,
        at: 0.0,
        position: 2.0 * BEAT as f64,
        repeat: Some(BEAT as f64),
    });
    let mut stretch = rlive_stretch::bank(512);
    stretch.prepare(SR);
    let ons = note_ons(3 * BEAT_FRAMES, |block, ev| {
        let (mut l, mut r) = (vec![0.0f32; block.frames], vec![0.0f32; block.frames]);
        render_player(&mut player, block, ev, &mut l, &mut r, &slots, &mut stretch);
    });
    assert_eq!(beats(&ons), vec![(0, 62), (1, 62), (2, 62)]);
}

#[test]
fn starting_a_clip_sends_its_bank_and_program() {
    use super::data::ProgramChange;
    use super::envelopes::enter_clip;
    let clip = ClipData::Midi(MidiClip::new(BEAT, vec![], true).with_program(Some(
        ProgramChange {
            bank: Some(3),
            sub_bank: None,
            program: Some(7),
        },
    )));
    let mut ev = Vec::with_capacity(8);
    enter_clip(&clip, 0.0, 12, false, &mut ev);
    assert_eq!(ev.len(), 2, "no sub-bank asked for");
    assert!(matches!(
        ev[0].message,
        MidiMessage::ControlChange {
            cc: 0,
            value: 3,
            ..
        }
    ));
    assert!(matches!(
        ev[1].message,
        MidiMessage::ProgramChange { program: 7, .. }
    ));
    assert_eq!(ev[0].offset, 12);
}

#[test]
fn an_edge_fade_follows_how_loud_the_edge_is() {
    use super::audio::Quality;
    let mut loud = audio(2 * BEAT, true);
    let mut quiet = audio(2 * BEAT, true);
    // Everything but the start of the loud file's second beat is silence.
    let n = 4 * BEAT_FRAMES;
    let silence: Vec<f32> = vec![0.0; n];
    let mut hot = silence.clone();
    hot[BEAT_FRAMES..].fill(0.8);
    let make = |left: Vec<f32>| {
        Arc::new(SampleBuffer {
            name: "s".into(),
            right: left.clone(),
            left,
            sample_rate: SR,
        })
    };
    loud.sample = make(hot);
    quiet.sample = make(silence);
    let region = ClipRegion::new(BEAT as f64, None, 0.0, 2.0 * BEAT as f64, true);
    let quality = Quality {
        hi_q: false,
        edge_fade: true,
    };
    let first_block = |clip: &AudioClip| {
        let mut cursor = AudioCursor::new(SESSION_VOICE);
        let mut stretch = rlive_stretch::bank(512);
        stretch.prepare(SR);
        let block = time(0.0, 512);
        let (mut l, mut r) = (vec![0.0f32; 512], vec![0.0f32; 512]);
        super::audio::render_audio_env(
            clip,
            region,
            quality,
            None,
            None,
            0.0,
            block.t0,
            block.end,
            block,
            &mut cursor,
            &mut l,
            &mut r,
            &mut stretch,
        );
        l
    };
    let l = first_block(&loud);
    assert!(l[0].abs() < 0.01, "starts from nothing: {}", l[0]);
    assert!(l[60] < 0.8 * 0.5, "still ramping: {}", l[60]);
    assert!((l[400] - 0.8).abs() < 1e-3, "and it reaches the signal");
    // 0.8 is 3.2 ms of the 4 ms the fade can take.
    assert!(l[100] > 0.1 && l[100] < 0.8);
    let silent = first_block(&quiet);
    assert!(silent.iter().all(|s| *s == 0.0));
}

fn fade_over_first_beat() -> EdgeFades {
    EdgeFades {
        in_from: 0.0,
        in_to: BEAT as f64,
        ..EdgeFades::default()
    }
}

/// Renders one clip for `beats` with the given fades and returns the left channel.
fn with_fades(clip: &ClipData, region: ClipRegion, fades: EdgeFades, beats: usize) -> Vec<f32> {
    let mut voices = Voices::new(NoteSpace::Arrangement);
    let mut cursor = AudioCursor::new(LANE_VOICE);
    cursor.fades = fades;
    let mut stretch = rlive_stretch::bank(512);
    stretch.prepare(SR);
    let mut left = vec![0.0f32; beats * BEAT_FRAMES];
    let mut t = 0.0;
    let mut done = 0;
    while done < left.len() {
        let n = 256.min(left.len() - done);
        let block = time(t, n);
        let (mut l, mut r) = (vec![0.0f32; n], vec![0.0f32; n]);
        let mut ev = Vec::with_capacity(16);
        render_range(
            clip,
            region,
            0.0,
            block.t0,
            block.end,
            block,
            &mut voices,
            &mut cursor,
            &mut ev,
            &mut l,
            &mut r,
            &mut stretch,
        );
        left[done..done + n].copy_from_slice(&l);
        t = block.end;
        done += n;
    }
    left
}

#[test]
fn hi_q_an_envelope_and_a_fade_all_apply_on_both_readers() {
    // A level that doesn't change, at a rate that has to be resampled.
    let level = vec![0.8f32; 96_000 * 8];
    let sample = Arc::new(SampleBuffer {
        name: "s".into(),
        right: level.clone(),
        left: level,
        sample_rate: 96_000.0,
    });
    let region = ClipRegion::new(BEAT as f64, None, 0.0, 4.0 * BEAT as f64, true);
    let audio_clip = AudioClip {
        sample,
        length: 4 * BEAT,
        looped: true,
        gain: 1.0,
        scale: None,
    };
    let frames = 8.0 * BEAT_FRAMES as f64 * 2.0;
    let readers = [
        ClipData::Audio(audio_clip.clone()),
        ClipData::Warped(WarpedClip {
            audio: audio_clip,
            map: WarpMap::new([
                WarpMarker::new(0.0, 0.0),
                WarpMarker::new(frames, 8.0 * BEAT as f64),
            ]),
            mode: WarpMode::RePitch,
            transpose: 0.0,
            slices: Arc::from(Vec::new()),
        }),
    ];
    let range = ParamRange {
        min: 0.0,
        max: 1.0,
        skew: 1.0,
        quantized: false,
    };
    for inner in readers {
        let clip = ClipData::Enveloped(Box::new(EnvelopedClip {
            clip: inner,
            envelopes: ClipEnvelopes {
                gain: Some(ClipCurve::new(
                    EnvCurve::new(range, [(0.0, 0.5, 0.0)]).unwrap(),
                    EnvTiming::Region(region),
                )),
                ..ClipEnvelopes::default()
            },
            region: Some(region),
            edge_fade: false,
            hi_q: true,
        }));
        let left = with_fades(&clip, region, fade_over_first_beat(), 2);
        // Half way through the fade: 0.8 from the sample, half from the envelope, half from it.
        let at = BEAT_FRAMES / 2;
        assert!((left[at] - 0.2).abs() < 0.01, "{}", left[at]);
        assert!((left[BEAT_FRAMES + 100] - 0.4).abs() < 0.01);
    }
}

#[test]
fn a_crossfades_lead_reads_the_region_before_the_left_edge_on_the_second_voice() {
    let sample = stepped(4);
    let plain = |looped| AudioClip {
        sample: sample.clone(),
        length: 4 * BEAT,
        looped,
        gain: 1.0,
        scale: None,
    };
    let region = ClipRegion::new(BEAT as f64, None, 0.0, 4.0 * BEAT as f64, false);
    let incoming = with_region(ClipData::Audio(plain(false)), region);
    let lane = Arc::new(ArrangementLane {
        clips: vec![
            LaneClip {
                start: 0,
                end: 2 * BEAT,
                offset: 0,
                data: Arc::new(ClipData::Audio(AudioClip {
                    gain: 0.0,
                    ..plain(true)
                })),
                fades: LaneFades {
                    tail: 0,
                    ..LaneFades::default()
                },
            },
            LaneClip {
                start: 2 * BEAT,
                end: 4 * BEAT,
                // A beat into its run, so its left edge is the third beat of the file.
                offset: BEAT,
                data: incoming,
                fades: LaneFades {
                    lead: BEAT / 2,
                    voice: 1,
                    ..LaneFades::default()
                },
            },
        ],
    });
    let mut player = LanePlayer::new(lane);
    let mut stretch = rlive_stretch::bank(512);
    stretch.prepare(SR);
    let mut left = vec![0.0f32; 3 * BEAT_FRAMES];
    let mut t = 0.0;
    let mut done = 0;
    while done < left.len() {
        let n = 256.min(left.len() - done);
        let block = time(t, n);
        let (mut l, mut r) = (vec![0.0f32; n], vec![0.0f32; n]);
        let mut ev = Vec::with_capacity(16);
        player.render(
            block,
            block.t0,
            block.end,
            &mut ev,
            &mut l,
            &mut r,
            &mut stretch,
        );
        left[done..done + n].copy_from_slice(&l);
        t = block.end;
        done += n;
    }
    // Half way through the lead: the second beat of the file, half faded in.
    let at = 2 * BEAT_FRAMES - BEAT_FRAMES / 4;
    assert!((left[at] - 0.1).abs() < 0.005, "{}", left[at]);
    // At the left edge and on, the third beat at full level.
    assert!((left[2 * BEAT_FRAMES + 100] - 0.3).abs() < 1e-3);
}

#[test]
fn legato_carries_the_content_position_into_a_clip_with_moved_markers() {
    use super::data::ProgramChange;
    let plain = Arc::new(midi(four_notes(), 4 * BEAT, true));
    let higher: Vec<Note> = four_notes()
        .into_iter()
        .map(|n| Note {
            key: n.key + 12,
            ..n
        })
        .collect();
    let region = ClipRegion::new(BEAT as f64, None, 0.0, 4.0 * BEAT as f64, true);
    let moved = with_region(
        ClipData::Midi(
            MidiClip::new(4 * BEAT, higher, true).with_program(Some(ProgramChange {
                bank: None,
                sub_bank: None,
                program: Some(9),
            })),
        ),
        region,
    );
    let slots = vec![Some(plain), Some(moved)];
    let launch = [
        SlotLaunch::default(),
        SlotLaunch {
            legato: true,
            ..SlotLaunch::default()
        },
    ];
    let mut player = ClipPlayer::new();
    player.pending = Some(Pending::Launch {
        slot: 0,
        at: 0.0,
        velocity: 1.0,
    });
    let mut stretch = rlive_stretch::bank(512);
    stretch.prepare(SR);
    let switch = 2.5 * BEAT as f64;
    let mut programs = Vec::new();
    let ons = note_ons(5 * BEAT_FRAMES, |block, ev| {
        if player.pending.is_none()
            && player.playing.as_ref().is_some_and(|p| p.slot == 0)
            && block.end > switch
        {
            player.pending = Some(Pending::Launch {
                slot: 1,
                at: switch,
                velocity: 1.0,
            });
        }
        let (mut l, mut r) = (vec![0.0f32; block.frames], vec![0.0f32; block.frames]);
        render_with(
            &mut player,
            block,
            ev,
            &mut l,
            &mut r,
            &slots,
            &launch,
            &mut stretch,
        );
        programs.extend(ev.iter().filter_map(|e| match e.message {
            MidiMessage::ProgramChange { program, .. } => Some(program),
            _ => None,
        }));
    });
    assert_eq!(programs, [9], "a legato launch sends its program too");
    // Same place in the content: the next note is the fourth, not the marker's.
    assert_eq!(
        beats(&ons),
        vec![(0, 60), (1, 61), (2, 62), (3, 75), (4, 72)]
    );
    let run = player.elapsed(5.0 * BEAT as f64);
    assert!((run - 4.0 * BEAT as f64).abs() < 1.0, "{run}");
}

#[test]
fn a_linked_follow_action_is_due_at_the_end_of_the_clips_first_pass() {
    // Its start marker is a beat in, so the first pass is a beat shorter.
    let region = ClipRegion::new(BEAT as f64, None, 0.0, 4.0 * BEAT as f64, true);
    let clip = with_region(midi(four_notes(), 4 * BEAT, true), region);
    let slots = vec![Some(clip)];
    let launch = [SlotLaunch {
        follow: FollowActions {
            enabled: true,
            linked: true,
            multiplier: 2,
            ..FollowActions::default()
        },
        ..SlotLaunch::default()
    }];
    let mut player = ClipPlayer::new();
    player.pending = Some(Pending::Launch {
        slot: 0,
        at: 0.0,
        velocity: 1.0,
    });
    let mut stretch = rlive_stretch::bank(512);
    stretch.prepare(SR);
    let block = time(0.0, 256);
    let (mut l, mut r) = (vec![0.0f32; 256], vec![0.0f32; 256]);
    let mut ev = Vec::with_capacity(32);
    render_with(
        &mut player,
        block,
        &mut ev,
        &mut l,
        &mut r,
        &slots,
        &launch,
        &mut stretch,
    );
    let due = player.follow.expect("armed").at;
    assert_eq!(due, 7.0 * BEAT as f64, "three beats, then round the loop");

    // A scrub doesn't touch it.
    player.pending = Some(Pending::Scrub {
        slot: 0,
        at: block.end,
        position: 2.0 * BEAT as f64,
        repeat: None,
    });
    let next = time(block.end, 256);
    render_with(
        &mut player,
        next,
        &mut ev,
        &mut l,
        &mut r,
        &slots,
        &launch,
        &mut stretch,
    );
    assert_eq!(player.follow.expect("still armed").at, due);
    assert_eq!(player.playing.as_ref().unwrap().gain, 1.0);
}
