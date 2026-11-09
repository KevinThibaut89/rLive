//! Launch modes, Legato, clip quantization, Follow Actions and scene timing, against the engine.

use rlive_core::time::{BarMap, TempoMap, TimeMaps};
use rlive_core::{
    DeviceId, FollowAction, FollowActions, LaunchMode, MidiMessage, ProcessContext, Processor,
    Quantize, SampleBuffer, TimeSignature, TrackId,
};
use rlive_engine::*;
use rlive_testkit::assert_no_alloc;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;
/// At 120 BPM.
const BEAT: usize = 24_000;
const BAR: u64 = 96_000;

#[derive(Default)]
struct Timeline {
    notes: Vec<(u64, MidiMessage)>,
}

struct FrameRecorder {
    frames: u64,
    log: Arc<Mutex<Timeline>>,
}

impl Processor for FrameRecorder {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        let mut log = self.log.lock().unwrap();
        for e in ctx.events {
            log.notes.push((self.frames + e.offset as u64, e.message));
        }
        self.frames += ctx.audio.left.len() as u64;
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

/// A clip of `beats` beats playing key `key` on every beat, a sixteenth long.
fn beats_clip(beats: u64, key: u8, looped: bool) -> Arc<ClipData> {
    let notes = (0..beats)
        .map(|b| Note {
            start: b * 960,
            length: 240,
            key,
            velocity: 0.8,
            ..Note::default()
        })
        .collect();
    Arc::new(ClipData::Midi(MidiClip::new(beats * 960, notes, looped)))
}

/// A clip that holds one note of key `key` for its whole length.
fn held_clip(beats: u64, key: u8) -> Arc<ClipData> {
    let note = Note {
        start: 0,
        length: beats * 960,
        key,
        velocity: 0.8,
        ..Note::default()
    };
    Arc::new(ClipData::Midi(MidiClip::new(beats * 960, vec![note], true)))
}

struct Rig {
    engine: Engine,
    h: EngineHandle,
    track: TrackId,
    log: Arc<Mutex<Timeline>>,
    shared: Arc<rlive_engine::shared::TrackShared>,
}

/// A track with `clips` in their slots, and `launch` settings for some of them.
fn rig(clips: Vec<(usize, Arc<ClipData>)>, launch: Vec<(usize, SlotLaunch)>) -> Rig {
    let (engine, mut h) = create_engine(SR, BLOCK);
    let log = Arc::new(Mutex::new(Timeline::default()));
    let id = TrackId::new();
    let shared = Arc::new(rlive_engine::shared::TrackShared::default());
    let mut t = TrackNode::new(id, BLOCK).with_shared(shared.clone());
    t.devices.push(Box::new(DeviceSlot::new(
        DeviceId::new(),
        Box::new(FrameRecorder {
            frames: 0,
            log: log.clone(),
        }),
    )));
    let mut t = Box::new(t);
    for (slot, clip) in clips {
        t.set_slot(slot, Some(clip));
    }
    for (slot, l) in launch {
        t.set_launch(slot, l);
    }
    let _ = h.send(EngineCommand::graph(GraphOp::AddTrack {
        index: 0,
        track: t,
    }));
    let _ = h.send(EngineCommand::Launch(LaunchCmd::SeedFollow(7)));
    Rig {
        engine,
        h,
        track: id,
        log,
        shared,
    }
}

impl Rig {
    fn cmd(&mut self, c: EngineCommand) {
        assert!(self.h.send(c).is_ok(), "command queue full");
    }

    fn launch(&mut self, c: LaunchCmd) {
        self.cmd(EngineCommand::Launch(c));
    }

    fn quantize(&mut self, q: Quantize) {
        self.cmd(EngineCommand::Transport(TransportCmd::SetQuantize(q)));
    }

    fn down(&mut self, slot: usize) {
        let track = self.track;
        self.launch(LaunchCmd::ClipDown {
            track,
            slot,
            velocity: 1.0,
        });
    }

    fn up(&mut self, slot: usize) {
        let track = self.track;
        self.launch(LaunchCmd::ClipUp { track, slot });
    }

    fn play(&mut self, slot: usize) {
        let track = self.track;
        self.launch(LaunchCmd::LaunchClip { track, slot });
    }

    /// Render `frames`, in a host buffer size that isn't a multiple of anything.
    fn run(&mut self, frames: usize) {
        let engine = &mut self.engine;
        rlive_testkit::render_chunks(frames, 300, |l, r| engine.process_stereo(l, r));
    }

    fn ons(&self, key: u8) -> Vec<u64> {
        self.log
            .lock()
            .unwrap()
            .notes
            .iter()
            .filter(|(_, m)| matches!(m, MidiMessage::NoteOn { key: k, .. } if *k == key))
            .map(|(f, _)| *f)
            .collect()
    }

    fn offs(&self, key: u8) -> Vec<u64> {
        self.log
            .lock()
            .unwrap()
            .notes
            .iter()
            .filter(|(_, m)| matches!(m, MidiMessage::NoteOff { key: k, .. } if *k == key))
            .map(|(f, _)| *f)
            .collect()
    }

    /// Every clip started, in order, as (frame, slot), where slot `n` plays key `60 + n`.
    fn order(&self) -> Vec<(u64, u8)> {
        self.log
            .lock()
            .unwrap()
            .notes
            .iter()
            .filter_map(|(f, m)| match m {
                MidiMessage::NoteOn { key, .. } => Some((*f, *key - 60)),
                _ => None,
            })
            .collect()
    }

    fn playing(&self) -> i32 {
        self.shared.playing_slot.load(Ordering::Relaxed)
    }

    fn position(&self) -> f64 {
        self.shared.clip_position.load()
    }
}

fn mode(mode: LaunchMode, quantize: Option<Quantize>) -> SlotLaunch {
    SlotLaunch {
        mode,
        quantize,
        ..SlotLaunch::default()
    }
}

// ---------------------------------------------------------------- Launch Modes

#[test]
fn trigger_starts_on_down_and_ignores_up() {
    let mut r = rig(vec![(0, beats_clip(4, 60, true))], vec![]);
    r.quantize(Quantize::Beat);
    r.down(0);
    r.up(0);
    r.run(BEAT * 5);
    assert_eq!(r.playing(), 0, "up is ignored");
    assert_eq!(r.ons(60)[0], 0);
}

#[test]
fn gate_stops_on_the_up_at_the_quantize_rate() {
    let mut r = rig(
        vec![(0, beats_clip(4, 60, true))],
        vec![(0, mode(LaunchMode::Gate, Some(Quantize::Beat)))],
    );
    r.down(0);
    r.run(BEAT * 2 + 100);
    assert_eq!(r.playing(), 0);
    r.up(0);
    r.run(BEAT * 2);
    assert_eq!(r.playing(), rlive_engine::shared::NO_SLOT, "stopped");
    let ons = r.ons(60);
    assert_eq!(
        ons.iter().filter(|f| **f < 3 * BEAT as u64).count(),
        3,
        "beats 0, 1 and 2: the beat 3 note never comes, as the stop is on it"
    );
    assert_eq!(ons.len(), 3);
}

#[test]
fn gate_up_before_the_launch_point_plays_one_quantum() {
    let mut r = rig(
        vec![(0, beats_clip(4, 60, true))],
        vec![(0, mode(LaunchMode::Gate, Some(Quantize::Bar)))],
    );
    r.down(0);
    r.run(100);
    let at_launch_stopped = r.playing();
    r.up(0);
    r.run(BAR as usize * 3);
    assert_eq!(at_launch_stopped, 0, "the first bar is at tick 0");
    assert_eq!(r.playing(), rlive_engine::shared::NO_SLOT);
    // Started at 0 and released inside its first bar: stops at the end of that bar.
    assert_eq!(r.ons(60).len(), 4);
}

#[test]
fn gate_released_while_still_waiting_starts_then_stops() {
    let mut r = rig(
        vec![(0, beats_clip(1, 60, true)), (1, beats_clip(1, 61, true))],
        vec![(1, mode(LaunchMode::Gate, Some(Quantize::Bar)))],
    );
    r.quantize(Quantize::Bar);
    r.play(0);
    r.run(1000);
    r.down(1);
    r.run(1000);
    r.up(1);
    r.run(BAR as usize * 3);
    assert_eq!(r.ons(61).first().copied(), Some(BAR), "launches at the bar");
    assert_eq!(r.ons(61).len(), 4, "and plays exactly one bar");
}

#[test]
fn toggle_stops_on_the_next_down() {
    let mut r = rig(
        vec![(0, beats_clip(4, 60, true))],
        vec![(0, mode(LaunchMode::Toggle, Some(Quantize::Beat)))],
    );
    r.down(0);
    r.up(0);
    r.run(BEAT * 2 + 100);
    assert_eq!(r.playing(), 0, "up is ignored");
    r.down(0);
    r.up(0);
    r.run(BEAT * 2);
    assert_eq!(r.playing(), rlive_engine::shared::NO_SLOT);
    r.down(0);
    r.run(BEAT);
    assert_eq!(r.playing(), 0, "and starts again on the one after");
}

#[test]
fn repeat_retriggers_at_the_quantize_rate_until_released() {
    // A two-beat clip with a note on each beat: only retriggering puts key 60 on every beat
    // and key 61 on none but the first pass.
    let notes = vec![
        Note {
            start: 0,
            length: 100,
            key: 60,
            velocity: 0.8,
            ..Note::default()
        },
        Note {
            start: 960,
            length: 100,
            key: 61,
            velocity: 0.8,
            ..Note::default()
        },
    ];
    let clip = Arc::new(ClipData::Midi(MidiClip::new(1920, notes, true)));
    let mut r = rig(
        vec![(0, clip)],
        vec![(0, mode(LaunchMode::Repeat, Some(Quantize::Beat)))],
    );
    r.down(0);
    r.run(BEAT * 3 + 100);
    assert!(
        r.ons(61).is_empty(),
        "retriggered before reaching the second note"
    );
    assert_eq!(
        r.ons(60),
        vec![0, BEAT as u64, 2 * BEAT as u64, 3 * BEAT as u64]
    );
    r.up(0);
    r.run(BEAT * 2);
    assert_eq!(
        r.playing(),
        rlive_engine::shared::NO_SLOT,
        "released: it stops"
    );
    assert_eq!(r.ons(60).len(), 4, "with no further triggers");
}

#[test]
fn repeat_with_no_quantization_just_plays_like_gate() {
    let mut r = rig(
        vec![(0, beats_clip(4, 60, true))],
        vec![(0, mode(LaunchMode::Repeat, Some(Quantize::None)))],
    );
    r.down(0);
    r.run(BEAT * 2 + 100);
    assert_eq!(r.ons(60).len(), 3);
    r.up(0);
    r.run(100);
    assert_eq!(r.playing(), rlive_engine::shared::NO_SLOT);
}

// ---------------------------------------------------------------- Legato

#[test]
fn legato_takes_over_the_play_position() {
    let mut r = rig(
        vec![(0, beats_clip(4, 60, true)), (1, beats_clip(4, 61, true))],
        vec![(
            1,
            SlotLaunch {
                legato: true,
                quantize: Some(Quantize::None),
                ..SlotLaunch::default()
            },
        )],
    );
    r.quantize(Quantize::None);
    r.play(0);
    r.run(BEAT + 300);
    r.play(1);
    r.run(BEAT * 3);
    // Clip 0 had got past beat 1, so clip 1 carries on at the same point: its beat 0 and 1
    // notes are skipped and the first it plays is beat 2's.
    let ons = r.ons(61);
    assert_eq!(
        ons[0],
        (BEAT + 300) as u64,
        "beat 1's note is still sounding: chased"
    );
    assert_eq!(ons[1], 2 * BEAT as u64, "{ons:?}");
}

#[test]
fn legato_works_unquantized_and_wraps_to_the_new_length() {
    let mut r = rig(
        vec![(0, beats_clip(8, 60, true)), (1, beats_clip(2, 61, true))],
        vec![(
            1,
            SlotLaunch {
                legato: true,
                quantize: Some(Quantize::None),
                ..SlotLaunch::default()
            },
        )],
    );
    r.quantize(Quantize::None);
    r.play(0);
    r.run(BEAT * 5 + 100);
    r.play(1);
    r.run(BEAT + 100);
    // Position 5 beats in, in a two-beat clip, is one beat in. Beat 6 lands on its start.
    assert_eq!(r.ons(61), vec![(BEAT * 5 + 100) as u64, 6 * BEAT as u64]);
}

#[test]
fn legato_chases_a_held_note() {
    let mut r = rig(
        vec![(0, held_clip(4, 60)), (1, held_clip(4, 61))],
        vec![(
            1,
            SlotLaunch {
                legato: true,
                quantize: Some(Quantize::None),
                ..SlotLaunch::default()
            },
        )],
    );
    r.quantize(Quantize::None);
    r.play(0);
    r.run(BEAT);
    r.play(1);
    r.run(1000);
    assert_eq!(r.offs(60).len(), 1, "the old note ends");
    assert_eq!(r.ons(61).len(), 1, "and the new clip's held note is chased");
}

#[test]
fn a_clip_without_legato_starts_from_the_top() {
    let mut r = rig(
        vec![(0, beats_clip(4, 60, true)), (1, beats_clip(4, 61, true))],
        vec![],
    );
    r.quantize(Quantize::None);
    r.play(0);
    r.run(BEAT + 300);
    r.play(1);
    r.run(100);
    assert!(r.ons(61)[0] >= (BEAT + 300) as u64);
    assert!(r.ons(61)[0] <= (BEAT + 600) as u64);
}

// ---------------------------------------------------------------- Clip Quantization

#[test]
fn none_launches_at_once_and_global_follows_the_transport_setting() {
    let mut r = rig(
        vec![(0, beats_clip(1, 60, true)), (1, beats_clip(1, 61, true))],
        vec![(1, mode(LaunchMode::Trigger, Some(Quantize::None)))],
    );
    r.quantize(Quantize::Bar);
    r.play(0);
    r.run(BEAT + 300);
    assert_eq!(r.playing(), 0, "slot 0 is Global, and bar 1 was at tick 0");
    r.play(1);
    r.run(600);
    let on = r.ons(61)[0];
    assert!(
        on >= (BEAT + 300) as u64 && on < (BEAT + 900) as u64,
        "{on}"
    );
}

#[test]
fn a_clip_quantization_overrides_the_global_setting() {
    let mut r = rig(
        vec![(0, beats_clip(1, 60, true)), (1, beats_clip(1, 61, true))],
        vec![(1, mode(LaunchMode::Trigger, Some(Quantize::Beat)))],
    );
    r.quantize(Quantize::FourBars);
    r.play(0);
    r.run(BEAT + 300);
    r.play(1);
    r.run(BEAT);
    assert_eq!(r.ons(61)[0], 2 * BEAT as u64);
}

#[test]
fn clip_quantization_keeps_to_the_bars_of_the_signature_in_force() {
    let mut r = rig(
        vec![(0, beats_clip(1, 60, true)), (1, beats_clip(1, 61, true))],
        vec![(1, mode(LaunchMode::Trigger, Some(Quantize::Bar)))],
    );
    let three_four = TimeSignature {
        numerator: 3,
        denominator: 4,
    };
    // 4/4 for one bar, then 3/4 from there.
    let maps = TimeMaps {
        tempo: TempoMap::constant(120.0),
        bars: BarMap::new(TimeSignature::default(), [(3840u64, three_four)]),
    };
    r.cmd(EngineCommand::Transport(TransportCmd::SetTimeMaps(
        Arc::new(maps),
    )));
    r.quantize(Quantize::None);
    r.play(0);
    r.run(BAR as usize + 100);
    r.play(1);
    r.run(BAR as usize);
    assert_eq!(
        r.ons(61)[0],
        BAR + 3 * BEAT as u64,
        "one 3/4 bar after the 4/4 one"
    );
}

// ---------------------------------------------------------------- Velocity

/// An audio clip of DC.
fn dc_clip(level: f32, seconds: usize) -> Arc<ClipData> {
    let sample = Arc::new(SampleBuffer {
        name: "dc".into(),
        left: vec![level; seconds * SR as usize],
        right: vec![level; seconds * SR as usize],
        sample_rate: SR,
    });
    Arc::new(ClipData::Audio(AudioClip {
        sample,
        length: 960 * 2 * seconds as u64,
        looped: true,
        gain: 1.0,
        scale: None,
    }))
}

#[test]
fn velocity_amount_scales_notes_and_audio() {
    assert_eq!(velocity_gain(0.0, 0.1), 1.0, "no influence at zero");
    assert_eq!(
        velocity_gain(1.0, 0.0),
        0.0,
        "the softest is silent at 100%"
    );
    assert_eq!(velocity_gain(1.0, 1.0), 1.0);

    let settings = SlotLaunch {
        velocity_amount: 1.0,
        quantize: Some(Quantize::None),
        ..SlotLaunch::default()
    };
    let mut r = rig(vec![(0, beats_clip(4, 60, true))], vec![(0, settings)]);
    let track = r.track;
    r.launch(LaunchCmd::ClipDown {
        track,
        slot: 0,
        velocity: 0.5,
    });
    r.run(BEAT / 2);
    let notes = r.log.lock().unwrap();
    let MidiMessage::NoteOn { velocity, .. } = notes.notes[0].1 else {
        panic!("{:?}", notes.notes[0]);
    };
    assert!((velocity - 0.4).abs() < 1e-5, "{velocity}");
    drop(notes);

    // Audio: launched at half velocity with the amount all the way up, half level.
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let id = TrackId::new();
    let mut t = Box::new(TrackNode::new(id, BLOCK));
    t.set_slot(0, Some(dc_clip(0.8, 4)));
    t.set_launch(0, settings);
    let _ = h.send(EngineCommand::graph(GraphOp::AddTrack {
        index: 0,
        track: t,
    }));
    let _ = h.send(EngineCommand::Launch(LaunchCmd::ClipDown {
        track: id,
        slot: 0,
        velocity: 0.5,
    }));
    let (l, _) = rlive_testkit::render_chunks(2000, 300, |l, r| engine.process_stereo(l, r));
    assert!((l[1500] - 0.4).abs() < 0.01, "{}", l[1500]);
}

#[test]
fn full_velocity_leaves_the_clip_untouched_at_any_amount() {
    let settings = SlotLaunch {
        velocity_amount: 1.0,
        quantize: Some(Quantize::None),
        ..SlotLaunch::default()
    };
    let mut r = rig(vec![(0, beats_clip(4, 60, true))], vec![(0, settings)]);
    r.down(0);
    r.run(BEAT / 2);
    let notes = r.log.lock().unwrap();
    let MidiMessage::NoteOn { velocity, .. } = notes.notes[0].1 else {
        panic!();
    };
    assert_eq!(velocity, 0.8);
}

// ---------------------------------------------------------------- Nudge and offsets

#[test]
fn nudge_jumps_by_the_global_quantization() {
    let mut r = rig(vec![(0, beats_clip(8, 60, true))], vec![]);
    r.quantize(Quantize::Beat);
    r.play(0);
    r.run(BEAT + 100);
    let before = r.position();
    let track = r.track;
    r.launch(LaunchCmd::Nudge {
        track,
        forward: true,
    });
    r.run(1);
    assert!(
        (r.position() - before - 960.0).abs() < 2.0,
        "{}",
        r.position()
    );
    r.launch(LaunchCmd::Nudge {
        track,
        forward: false,
    });
    r.launch(LaunchCmd::Nudge {
        track,
        forward: false,
    });
    r.run(1);
    assert!(
        (r.position() - (before - 960.0)).abs() < 2.0,
        "{}",
        r.position()
    );
}

#[test]
fn nudge_does_nothing_with_quantization_off() {
    let mut r = rig(vec![(0, beats_clip(8, 60, true))], vec![]);
    r.quantize(Quantize::None);
    r.play(0);
    r.run(BEAT);
    let before = r.position();
    let track = r.track;
    r.launch(LaunchCmd::Nudge {
        track,
        forward: true,
    });
    r.run(1);
    assert!((r.position() - before).abs() < 1.0);
}

#[test]
fn an_offset_from_the_master_clock_is_retained() {
    let mut r = rig(
        vec![(0, beats_clip(4, 60, true))],
        vec![(0, mode(LaunchMode::Trigger, Some(Quantize::Beat)))],
    );
    r.cmd(EngineCommand::Transport(TransportCmd::Play));
    r.run(BEAT + 100);
    r.play(0);
    r.run(BEAT * 9);
    // Launched on beat 2: every pass is still a beat off the bar.
    for on in r.ons(60) {
        assert_eq!(on % BEAT as u64, 0);
    }
    assert_eq!(r.ons(60)[0], 2 * BEAT as u64);
    assert_eq!(r.ons(60)[4], 6 * BEAT as u64, "a loop later, still offset");
}

// ---------------------------------------------------------------- Follow Actions

fn follow(a: FollowAction) -> SlotLaunch {
    SlotLaunch {
        follow: FollowActions {
            enabled: true,
            action_a: a,
            chance_a: 1.0,
            chance_b: 0.0,
            ..FollowActions::default()
        },
        quantize: Some(Quantize::None),
        ..SlotLaunch::default()
    }
}

fn group(n: usize, first: usize, a: FollowAction) -> Rig {
    let clips = (0..n)
        .map(|i| (first + i, beats_clip(1, 60 + (first + i) as u8, true)))
        .collect();
    let launch = (0..n).map(|i| (first + i, follow(a))).collect();
    let mut r = rig(clips, launch);
    r.quantize(Quantize::None);
    r
}

/// The slots played on the first beats, one a beat.
fn firsts(r: &Rig, beats: usize) -> Vec<u8> {
    r.order().into_iter().take(beats).map(|(_, s)| s).collect()
}

#[test]
fn follow_actions_are_off_by_default() {
    let mut r = rig(
        vec![(0, beats_clip(1, 60, true)), (1, beats_clip(1, 61, true))],
        vec![(
            0,
            SlotLaunch {
                follow: FollowActions {
                    action_a: FollowAction::Next,
                    ..FollowActions::default()
                },
                ..SlotLaunch::default()
            },
        )],
    );
    r.quantize(Quantize::None);
    r.play(0);
    r.run(BEAT * 4);
    assert_eq!(r.playing(), 0);
    assert!(r.ons(61).is_empty());
}

#[test]
fn stop_stops_after_the_follow_time_overriding_the_loop() {
    let mut r = group(1, 0, FollowAction::Stop);
    r.play(0);
    r.run(BEAT * 3);
    assert_eq!(r.playing(), rlive_engine::shared::NO_SLOT);
    assert_eq!(r.ons(60), vec![0]);
    assert_eq!(
        r.offs(60),
        vec![(BEAT / 4) as u64],
        "the note had ended by itself"
    );
}

#[test]
fn play_again_restarts_the_clip() {
    let notes = vec![Note {
        start: 480,
        length: 100,
        key: 60,
        velocity: 0.8,
        ..Note::default()
    }];
    let clip = Arc::new(ClipData::Midi(MidiClip::new(1920, notes, true)));
    let mut settings = follow(FollowAction::PlayAgain);
    settings.follow.linked = false;
    settings.follow.time = 960;
    let mut r = rig(vec![(0, clip)], vec![(0, settings)]);
    r.quantize(Quantize::None);
    r.play(0);
    r.run(BEAT * 2 + 100);
    let (half, one_and_half) = (BEAT as u64 / 2, BEAT as u64 + BEAT as u64 / 2);
    assert_eq!(r.ons(60), vec![half, one_and_half]);
}

#[test]
fn next_moves_down_the_group_and_wraps() {
    let mut r = group(3, 0, FollowAction::Next);
    r.play(0);
    r.run(BEAT * 4 + 100);
    assert_eq!(firsts(&r, 5), vec![0, 1, 2, 0, 1]);
    let frames: Vec<u64> = r.order().iter().map(|o| o.0).take(4).collect();
    assert_eq!(
        frames,
        vec![0, BEAT as u64, 2 * BEAT as u64, 3 * BEAT as u64],
        "exactly on the end of each clip"
    );
}

#[test]
fn previous_moves_up_and_stops_at_the_top_of_the_group() {
    let mut r = group(3, 0, FollowAction::Previous);
    r.play(2);
    r.run(BEAT * 4 + 100);
    let order = firsts(&r, 3);
    assert_eq!(order, vec![2, 1, 0]);
    assert_eq!(r.playing(), 0, "nothing above the top clip: it carries on");
}

#[test]
fn first_and_last_go_to_the_ends_of_the_group() {
    let mut r = group(4, 2, FollowAction::First);
    r.play(4);
    r.run(BEAT + 100);
    assert_eq!(firsts(&r, 2), vec![4, 2]);

    let mut r = group(4, 2, FollowAction::Last);
    r.play(3);
    r.run(BEAT + 100);
    assert_eq!(firsts(&r, 2), vec![3, 5]);
}

#[test]
fn any_can_replay_the_same_clip_and_covers_the_group() {
    let mut r = group(3, 0, FollowAction::Any);
    r.play(0);
    r.run(BEAT * 200);
    let order = firsts(&r, 200);
    let repeats = order.windows(2).filter(|w| w[0] == w[1]).count();
    assert!(repeats > 0, "Any may pick the clip that's playing");
    for slot in 0..3 {
        assert!(order.contains(&slot), "{slot}");
    }
}

#[test]
fn other_never_plays_the_same_clip_twice_in_a_row() {
    let mut r = group(3, 0, FollowAction::Other);
    r.play(0);
    r.run(BEAT * 200);
    let order = firsts(&r, 200);
    assert!(order.windows(2).all(|w| w[0] != w[1]), "{order:?}");
    for slot in 0..3 {
        assert!(order.contains(&slot), "{slot}");
    }
}

#[test]
fn other_replays_a_clip_alone_in_its_group() {
    let mut r = group(1, 3, FollowAction::Other);
    r.play(3);
    r.run(BEAT * 2 + 100);
    assert_eq!(firsts(&r, 3), vec![3, 3, 3]);
}

#[test]
fn no_action_ends_the_following_for_good() {
    let mut r = group(2, 0, FollowAction::None);
    r.play(0);
    r.run(BEAT * 3);
    assert_eq!(firsts(&r, 3), vec![0, 0, 0], "it just keeps looping");
    assert_eq!(r.playing(), 0);
}

#[test]
fn jump_goes_to_its_target_slot_even_across_a_gap() {
    let clips = vec![
        (0, beats_clip(1, 60, true)),
        (1, beats_clip(1, 61, true)),
        (5, beats_clip(1, 65, true)),
    ];
    let mut r = rig(clips, vec![(0, follow(FollowAction::Jump(5)))]);
    r.quantize(Quantize::None);
    r.play(0);
    r.run(BEAT + 100);
    assert_eq!(firsts(&r, 2), vec![0, 5]);
}

#[test]
fn jump_to_an_empty_slot_does_nothing() {
    let mut r = rig(
        vec![(0, beats_clip(1, 60, true))],
        vec![(0, follow(FollowAction::Jump(9)))],
    );
    r.quantize(Quantize::None);
    r.play(0);
    r.run(BEAT * 2 + 100);
    assert_eq!(firsts(&r, 3), vec![0, 0, 0]);
    assert_eq!(r.playing(), 0);
}

#[test]
fn groups_are_broken_by_empty_slots() {
    // Slots 0-1 and 3-4 are separate groups.
    let clips = vec![
        (0, beats_clip(1, 60, true)),
        (1, beats_clip(1, 61, true)),
        (3, beats_clip(1, 63, true)),
        (4, beats_clip(1, 64, true)),
    ];
    let launch = [0, 1, 3, 4]
        .into_iter()
        .map(|s| (s, follow(FollowAction::Next)))
        .collect();
    let mut r = rig(clips, launch);
    r.quantize(Quantize::None);
    r.play(0);
    r.run(BEAT * 5 + 100);
    assert_eq!(
        firsts(&r, 6),
        vec![0, 1, 0, 1, 0, 1],
        "never reaches slot 3"
    );
}

#[test]
fn a_follow_action_never_lands_on_a_deactivated_clip() {
    let mut off = follow(FollowAction::Next);
    off.active = false;
    let clips = vec![
        (0, beats_clip(1, 60, true)),
        (1, beats_clip(1, 61, true)),
        (2, beats_clip(1, 62, true)),
    ];
    let mut r = rig(
        clips,
        vec![
            (0, follow(FollowAction::Next)),
            (1, off),
            (2, follow(FollowAction::Next)),
        ],
    );
    r.quantize(Quantize::None);
    r.play(0);
    r.run(BEAT * 3 + 100);
    assert_eq!(firsts(&r, 4), vec![0, 2, 0, 2]);
}

#[test]
fn chance_picks_each_action_in_proportion_to_its_seed() {
    let mut rule = follow(FollowAction::Next);
    rule.follow.action_b = FollowAction::PlayAgain;
    rule.follow.chance_a = 0.1;
    rule.follow.chance_b = 0.9;
    let run = |seed: u64| {
        let clips = vec![(0, beats_clip(1, 60, true)), (1, beats_clip(1, 61, true))];
        let settings = rule;
        let mut r = rig(clips, vec![(0, settings), (1, settings)]);
        r.quantize(Quantize::None);
        r.launch(LaunchCmd::SeedFollow(seed));
        r.play(0);
        r.run(BEAT * 2000);
        r.order()
    };
    let order = run(0xdead_beef);
    let moves = order.windows(2).filter(|w| w[0].1 != w[1].1).count();
    let share = moves as f64 / (order.len() - 1) as f64;
    assert!((share - 0.1).abs() < 0.03, "{share}");
    assert_eq!(order, run(0xdead_beef), "a seed always gives the same run");
    assert_ne!(order, run(0xfeed_face));
}

#[test]
fn chance_a_at_full_always_picks_a() {
    let mut rule = follow(FollowAction::Next);
    rule.follow.action_b = FollowAction::Stop;
    let mut r = rig(
        vec![(0, beats_clip(1, 60, true)), (1, beats_clip(1, 61, true))],
        vec![(0, rule), (1, rule)],
    );
    r.quantize(Quantize::None);
    r.play(0);
    r.run(BEAT * 30);
    assert!(r.playing() >= 0, "never stopped");
}

#[test]
fn linked_fires_at_the_end_of_the_clip_times_the_multiplier() {
    let mut a = follow(FollowAction::Next);
    a.follow.multiplier = 3;
    let mut r = rig(
        vec![(0, beats_clip(1, 60, true)), (1, beats_clip(1, 61, true))],
        vec![(0, a)],
    );
    r.quantize(Quantize::None);
    r.play(0);
    r.run(BEAT * 4 + 100);
    assert_eq!(r.ons(61)[0], 3 * BEAT as u64);
}

#[test]
fn unlinked_fires_after_the_follow_time_whatever_the_clip_length() {
    let mut a = follow(FollowAction::Next);
    a.follow.linked = false;
    a.follow.time = 480;
    let mut r = rig(
        vec![(0, beats_clip(4, 60, true)), (1, beats_clip(1, 61, true))],
        vec![(0, a)],
    );
    r.quantize(Quantize::None);
    r.play(0);
    r.run(BEAT);
    assert_eq!(r.ons(61)[0], BEAT as u64 / 2);
}

#[test]
fn a_oneshot_reaching_its_end_still_follows() {
    let mut r = rig(
        vec![(0, beats_clip(2, 60, false)), (1, beats_clip(1, 61, true))],
        vec![(0, follow(FollowAction::Next))],
    );
    r.quantize(Quantize::None);
    r.play(0);
    r.run(BEAT * 2 + 100);
    assert_eq!(r.ons(61)[0], 2 * BEAT as u64);
}

#[test]
fn a_stopped_clip_cancels_its_follow_action() {
    let mut r = group(2, 0, FollowAction::Next);
    r.play(0);
    r.run(100);
    let track = r.track;
    r.launch(LaunchCmd::StopTrack(track));
    r.run(BEAT * 2);
    assert!(r.ons(61).is_empty());
}

#[test]
fn follow_timing_bypasses_global_quantization() {
    let mut a = follow(FollowAction::Next);
    a.quantize = None; // Global
    let mut r = rig(
        vec![(0, beats_clip(1, 60, true)), (1, beats_clip(1, 61, true))],
        vec![(0, a), (1, a)],
    );
    r.quantize(Quantize::FourBars);
    r.play(0);
    r.run(BEAT + 100);
    assert_eq!(r.ons(61)[0], BEAT as u64, "not held back to the four bars");
}

#[test]
fn follow_timing_honours_a_specific_clip_quantization() {
    let target = SlotLaunch {
        quantize: Some(Quantize::Bar),
        ..follow(FollowAction::Next)
    };
    let mut r = rig(
        vec![(0, beats_clip(1, 60, true)), (1, beats_clip(1, 61, true))],
        vec![(0, follow(FollowAction::Next)), (1, target)],
    );
    r.quantize(Quantize::None);
    r.play(0);
    r.run(BAR as usize + 100);
    assert_eq!(
        r.ons(61)[0],
        BAR,
        "fired on the beat, launched on the next bar"
    );
}

#[test]
fn the_global_button_disables_every_follow_action() {
    let mut r = group(2, 0, FollowAction::Next);
    r.launch(LaunchCmd::SetFollowEnabled(false));
    r.play(0);
    r.run(BEAT * 3);
    assert_eq!(r.playing(), 0);
    assert!(r.ons(61).is_empty());
}

#[test]
fn a_loop_wrap_moves_a_waiting_follow_action_with_the_clip() {
    let mut r = group(2, 0, FollowAction::Next);
    r.cmd(EngineCommand::Transport(TransportCmd::SetLoop(Some((
        0.0, 480.0,
    )))));
    r.play(0);
    r.run(BEAT * 2);
    // The Arrangement loops every half beat; a beat-long clip still fires a beat after launch.
    assert_eq!(r.ons(61).first().copied(), Some(BEAT as u64));
}

// ---------------------------------------------------------------- Scenes

type TrackLogs = Vec<(TrackId, Arc<Mutex<Timeline>>)>;

fn scene_rig(tracks: usize) -> (Engine, EngineHandle, TrackLogs) {
    let (engine, mut h) = create_engine(SR, BLOCK);
    let mut out = Vec::new();
    for index in 0..tracks {
        let id = TrackId::new();
        let log = Arc::new(Mutex::new(Timeline::default()));
        let mut t = TrackNode::new(id, BLOCK);
        t.devices.push(Box::new(DeviceSlot::new(
            DeviceId::new(),
            Box::new(FrameRecorder {
                frames: 0,
                log: log.clone(),
            }),
        )));
        let mut t = Box::new(t);
        for slot in 0..4 {
            t.set_slot(slot, Some(beats_clip(1, 60 + slot as u8, true)));
        }
        let _ = h.send(EngineCommand::graph(GraphOp::AddTrack { index, track: t }));
        out.push((id, log));
    }
    let _ = h.send(EngineCommand::Launch(LaunchCmd::SetSceneCount(4)));
    let _ = h.send(EngineCommand::Launch(LaunchCmd::SeedFollow(3)));
    (engine, h, out)
}

fn scene_follow(a: FollowAction, time: u64) -> FollowActions {
    FollowActions {
        enabled: true,
        action_a: a,
        chance_a: 1.0,
        chance_b: 0.0,
        linked: false,
        time,
        ..FollowActions::default()
    }
}

fn keys(log: &Mutex<Timeline>) -> Vec<(u64, u8)> {
    log.lock()
        .unwrap()
        .notes
        .iter()
        .filter_map(|(f, m)| match m {
            MidiMessage::NoteOn { key, .. } => Some((*f, *key)),
            _ => None,
        })
        .collect()
}

fn render(engine: &mut Engine, frames: usize) {
    rlive_testkit::render_chunks(frames, 300, |l, r| engine.process_stereo(l, r));
}

#[test]
fn a_scene_sets_its_tempo_exactly_at_the_launch_boundary() {
    let (mut engine, mut h, _) = scene_rig(1);
    let _ = h.send(EngineCommand::Transport(TransportCmd::SetQuantize(
        Quantize::Bar,
    )));
    let _ = h.send(EngineCommand::Launch(LaunchCmd::SetScene {
        index: 1,
        info: SceneInfo {
            tempo: Some(60.0),
            ..SceneInfo::default()
        },
    }));
    let _ = h.send(EngineCommand::Transport(TransportCmd::Play));
    render(&mut engine, 1000);
    let _ = h.send(EngineCommand::Launch(LaunchCmd::LaunchScene(1)));
    render(&mut engine, 96_000 - 1000 - 300);
    assert_eq!(h.shared.tempo.load(), 120.0, "not yet");
    render(&mut engine, 600);
    assert_eq!(h.shared.tempo.load(), 60.0, "and now it has changed");
    let event = std::iter::from_fn(|| h.poll_event())
        .find(|e| matches!(e, EngineEvent::SceneTiming { .. }))
        .expect("the UI hears about it");
    let EngineEvent::SceneTiming {
        scene, tick, tempo, ..
    } = event
    else {
        unreachable!()
    };
    assert_eq!((scene, tick, tempo), (1, 3840.0, Some(60.0)));
    // A bar at 60 BPM from there is twice as long.
    render(&mut engine, 96_000);
    assert!((h.shared.tick.load() - (3840.0 + 3840.0 / 2.0)).abs() < 40.0);
}

#[test]
fn a_scene_signature_restarts_the_bar_grid_at_the_launch_point() {
    let (mut engine, mut h, tracks) = scene_rig(1);
    let _ = h.send(EngineCommand::Transport(TransportCmd::SetQuantize(
        Quantize::Bar,
    )));
    let _ = h.send(EngineCommand::Launch(LaunchCmd::SetScene {
        index: 1,
        info: SceneInfo {
            signature: Some(TimeSignature {
                numerator: 3,
                denominator: 4,
            }),
            ..SceneInfo::default()
        },
    }));
    let _ = h.send(EngineCommand::Launch(LaunchCmd::LaunchScene(0)));
    render(&mut engine, 100);
    let _ = h.send(EngineCommand::Launch(LaunchCmd::LaunchScene(1)));
    render(&mut engine, BAR as usize + 100);
    let _ = h.send(EngineCommand::Launch(LaunchCmd::LaunchScene(2)));
    render(&mut engine, BAR as usize);
    // Scene 1 landed on bar 2; the next bar line is a 3/4 bar on from there.
    let on = keys(&tracks[0].1)
        .into_iter()
        .find(|(_, k)| *k == 62)
        .unwrap();
    assert_eq!(on.0, BAR + 3 * BEAT as u64);
}

#[test]
fn scene_follow_actions_launch_scenes_on_their_time() {
    let (mut engine, mut h, tracks) = scene_rig(2);
    let _ = h.send(EngineCommand::Transport(TransportCmd::SetQuantize(
        Quantize::None,
    )));
    for index in 0..4 {
        let _ = h.send(EngineCommand::Launch(LaunchCmd::SetScene {
            index,
            info: SceneInfo {
                follow: scene_follow(FollowAction::Next, 960),
                ..SceneInfo::default()
            },
        }));
    }
    let _ = h.send(EngineCommand::Launch(LaunchCmd::LaunchScene(0)));
    render(&mut engine, BEAT * 5 + 100);
    for (_, log) in &tracks {
        let k: Vec<u8> = keys(log).iter().take(5).map(|o| o.1).collect();
        assert_eq!(k, vec![60, 61, 62, 63, 60], "wraps at the last scene");
    }
}

#[test]
fn scene_stop_stops_the_scene_and_scene_jump_goes_there() {
    let (mut engine, mut h, tracks) = scene_rig(1);
    let _ = h.send(EngineCommand::Transport(TransportCmd::SetQuantize(
        Quantize::None,
    )));
    let _ = h.send(EngineCommand::Launch(LaunchCmd::SetScene {
        index: 0,
        info: SceneInfo {
            follow: scene_follow(FollowAction::Jump(2), 960),
            ..SceneInfo::default()
        },
    }));
    let _ = h.send(EngineCommand::Launch(LaunchCmd::SetScene {
        index: 2,
        info: SceneInfo {
            follow: scene_follow(FollowAction::Stop, 960),
            ..SceneInfo::default()
        },
    }));
    let _ = h.send(EngineCommand::Launch(LaunchCmd::LaunchScene(0)));
    render(&mut engine, BEAT * 3 + 100);
    let k: Vec<(u64, u8)> = keys(&tracks[0].1);
    assert_eq!(
        k.iter().map(|o| o.1).collect::<Vec<_>>(),
        vec![60, 62],
        "stops once it has played a beat of scene 3"
    );
}

#[test]
fn a_scene_follow_action_takes_precedence_once_fired() {
    let (mut engine, mut h, tracks) = scene_rig(1);
    let id = tracks[0].0;
    let _ = h.send(EngineCommand::Transport(TransportCmd::SetQuantize(
        Quantize::None,
    )));
    // The clip would go to slot 3 at beat 1, and the scene to scene 2 on the same tick.
    let clip = SlotLaunch {
        follow: scene_follow(FollowAction::Jump(3), 960),
        ..SlotLaunch::default()
    };
    let _ = h.send(EngineCommand::Clip(ClipCmd::SetLaunch {
        track: id,
        slot: 0,
        launch: clip,
    }));
    let _ = h.send(EngineCommand::Launch(LaunchCmd::SetScene {
        index: 0,
        info: SceneInfo {
            follow: scene_follow(FollowAction::Jump(2), 960),
            ..SceneInfo::default()
        },
    }));
    let _ = h.send(EngineCommand::Launch(LaunchCmd::LaunchScene(0)));
    render(&mut engine, BEAT + 100);
    assert_eq!(
        keys(&tracks[0].1).iter().map(|o| o.1).collect::<Vec<_>>(),
        vec![60, 62],
        "the scene's action wins"
    );
}

#[test]
fn clip_follow_actions_run_until_the_scene_action_fires() {
    let (mut engine, mut h, tracks) = scene_rig(1);
    let id = tracks[0].0;
    let _ = h.send(EngineCommand::Transport(TransportCmd::SetQuantize(
        Quantize::None,
    )));
    let clip = SlotLaunch {
        follow: scene_follow(FollowAction::Jump(3), 480),
        ..SlotLaunch::default()
    };
    let _ = h.send(EngineCommand::Clip(ClipCmd::SetLaunch {
        track: id,
        slot: 0,
        launch: clip,
    }));
    let _ = h.send(EngineCommand::Launch(LaunchCmd::SetScene {
        index: 0,
        info: SceneInfo {
            follow: scene_follow(FollowAction::Jump(2), 960),
            ..SceneInfo::default()
        },
    }));
    let _ = h.send(EngineCommand::Launch(LaunchCmd::LaunchScene(0)));
    render(&mut engine, BEAT * 2);
    let k = keys(&tracks[0].1);
    assert_eq!(
        k.iter().take(3).copied().collect::<Vec<_>>(),
        vec![(0, 60), (BEAT as u64 / 2, 63), (BEAT as u64, 62)],
        "the clip moved first, then the scene took over"
    );
}

#[test]
fn a_pending_scene_launch_can_be_cancelled() {
    let (mut engine, mut h, tracks) = scene_rig(1);
    let _ = h.send(EngineCommand::Launch(LaunchCmd::SetScene {
        index: 1,
        info: SceneInfo {
            tempo: Some(70.0),
            ..SceneInfo::default()
        },
    }));
    let _ = h.send(EngineCommand::Launch(LaunchCmd::LaunchScene(0)));
    render(&mut engine, 1000);
    let _ = h.send(EngineCommand::Launch(LaunchCmd::LaunchScene(1)));
    render(&mut engine, 1000);
    let _ = h.send(EngineCommand::Launch(LaunchCmd::CancelSceneLaunch));
    render(&mut engine, BAR as usize);
    assert!(keys(&tracks[0].1).iter().all(|o| o.1 == 60));
    assert_eq!(h.shared.tempo.load(), 120.0);
}

#[test]
fn without_its_stop_button_an_empty_slot_leaves_the_track_alone() {
    let (mut engine, mut h, tracks) = scene_rig(1);
    let id = tracks[0].0;
    let _ = h.send(EngineCommand::Clip(ClipCmd::SetClip {
        track: id,
        slot: 1,
        clip: None,
    }));
    let _ = h.send(EngineCommand::Clip(ClipCmd::SetLaunch {
        track: id,
        slot: 1,
        launch: SlotLaunch {
            stop_button: false,
            ..SlotLaunch::default()
        },
    }));
    let _ = h.send(EngineCommand::Transport(TransportCmd::SetQuantize(
        Quantize::None,
    )));
    let _ = h.send(EngineCommand::Launch(LaunchCmd::LaunchScene(0)));
    render(&mut engine, 1000);
    let _ = h.send(EngineCommand::Launch(LaunchCmd::LaunchScene(1)));
    render(&mut engine, BEAT);
    assert_eq!(
        keys(&tracks[0].1).len(),
        2,
        "beat one comes round: still playing"
    );
}

#[test]
fn stop_all_clips_stops_everything_and_the_scene_following_too() {
    let (mut engine, mut h, tracks) = scene_rig(2);
    let _ = h.send(EngineCommand::Transport(TransportCmd::SetQuantize(
        Quantize::None,
    )));
    let _ = h.send(EngineCommand::Launch(LaunchCmd::SetScene {
        index: 0,
        info: SceneInfo {
            follow: scene_follow(FollowAction::Next, 960),
            ..SceneInfo::default()
        },
    }));
    let _ = h.send(EngineCommand::Launch(LaunchCmd::LaunchScene(0)));
    render(&mut engine, 1000);
    let _ = h.send(EngineCommand::Launch(LaunchCmd::StopAllClips));
    render(&mut engine, BEAT * 2);
    for (_, log) in &tracks {
        assert_eq!(keys(log).len(), 1, "one note, then nothing");
    }
}

// ---------------------------------------------------------------- Capture and Insert Scene

/// A clip of audio that can't sound the same at any two offsets.
fn ramp_clip() -> Arc<ClipData> {
    let n = 2 * SR as usize;
    let left: Vec<f32> = (0..n).map(|i| i as f32 / n as f32).collect();
    let sample = Arc::new(SampleBuffer {
        name: "ramp".into(),
        right: left.clone(),
        left,
        sample_rate: SR,
    });
    Arc::new(ClipData::Audio(AudioClip {
        sample,
        length: 3840,
        looped: true,
        gain: 1.0,
        scale: None,
    }))
}

fn capture_engine() -> (Engine, EngineHandle, TrackId) {
    let (engine, mut h) = create_engine(SR, BLOCK);
    let id = TrackId::new();
    let mut t = Box::new(TrackNode::new(id, BLOCK));
    t.set_slot(0, Some(ramp_clip()));
    let _ = h.send(EngineCommand::graph(GraphOp::AddTrack {
        index: 0,
        track: t,
    }));
    let _ = h.send(EngineCommand::Transport(TransportCmd::SetQuantize(
        Quantize::None,
    )));
    let _ = h.send(EngineCommand::Launch(LaunchCmd::LaunchClip {
        track: id,
        slot: 0,
    }));
    (engine, h, id)
}

#[test]
fn capture_and_insert_scene_continues_sample_accurately() {
    let (mut uninterrupted, _h1, _) = capture_engine();
    let (mut captured, mut h, id) = capture_engine();
    let (a, _) =
        rlive_testkit::render_chunks(50_000, 300, |l, r| uninterrupted.process_stereo(l, r));
    let (mut b, _) =
        rlive_testkit::render_chunks(20_000, 300, |l, r| captured.process_stereo(l, r));
    // The running clip moves down a row, a copy goes into the new scene, and it carries on.
    let _ = h.send(EngineCommand::Clip(ClipCmd::RemapSlots(Arc::new(vec![
        NO_OLD_SLOT,
        0,
    ]))));
    let _ = h.send(EngineCommand::Clip(ClipCmd::SetClip {
        track: id,
        slot: 0,
        clip: Some(ramp_clip()),
    }));
    let _ = h.send(EngineCommand::Launch(LaunchCmd::Adopt {
        track: id,
        slot: 0,
    }));
    let (rest, _) = rlive_testkit::render_chunks(30_000, 300, |l, r| captured.process_stereo(l, r));
    b.extend_from_slice(&rest);
    assert_eq!(a, b, "bit for bit");
}

#[test]
fn rearranging_slots_keeps_the_running_clip_on_its_new_row() {
    let mut r = rig(
        vec![(0, beats_clip(4, 60, true)), (1, beats_clip(4, 61, true))],
        vec![],
    );
    r.quantize(Quantize::None);
    r.play(0);
    r.run(1000);
    r.cmd(EngineCommand::Clip(ClipCmd::RemapSlots(Arc::new(vec![
        1, 0,
    ]))));
    r.run(1000);
    assert_eq!(r.playing(), 1, "slot 0 moved to slot 1");
    assert!(r.position() > 0.0);
    assert!(r.offs(60).is_empty(), "with no interruption");
}

#[test]
fn deleting_the_running_slot_ends_the_clip() {
    let mut r = rig(
        vec![(0, held_clip(4, 60)), (1, beats_clip(4, 61, true))],
        vec![],
    );
    r.quantize(Quantize::None);
    r.play(0);
    r.run(1000);
    r.cmd(EngineCommand::Clip(ClipCmd::RemapSlots(Arc::new(vec![1]))));
    r.run(1000);
    assert_eq!(r.playing(), rlive_engine::shared::NO_SLOT);
    assert_eq!(r.offs(60).len(), 1);
}

// ---------------------------------------------------------------- Logging and allocation

#[test]
fn only_a_recording_logs_what_is_played() {
    let (mut engine, mut h, tracks) = scene_rig(1);
    let _ = h.send(EngineCommand::Transport(TransportCmd::SetQuantize(
        Quantize::None,
    )));
    let _ = h.send(EngineCommand::Launch(LaunchCmd::LaunchScene(0)));
    render(&mut engine, 1000);
    assert!(std::iter::from_fn(|| h.poll_event()).next().is_none());

    let _ = h.send(EngineCommand::Record(RecordCmd::SetRecording(true)));
    render(&mut engine, 1000);
    let _ = h.send(EngineCommand::Launch(LaunchCmd::LaunchScene(1)));
    render(&mut engine, 1000);
    let id = tracks[0].0;
    let slots: Vec<_> = std::iter::from_fn(|| h.poll_event())
        .filter_map(|e| match e {
            EngineEvent::SessionPlayback { track, slot, .. } if track == id => Some(slot),
            _ => None,
        })
        .collect();
    assert_eq!(slots, vec![Some(0), Some(1)]);
}

#[test]
fn launching_and_following_never_allocate() {
    let (mut engine, mut h, tracks) = scene_rig(3);
    let mut warm = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    for (_, log) in &tracks {
        // The test's own recorder mustn't be what allocates.
        log.lock().unwrap().notes.reserve(1_000_000);
    }
    let _ = h.send(EngineCommand::Transport(TransportCmd::SetQuantize(
        Quantize::None,
    )));
    let rule = scene_follow(FollowAction::Next, 240);
    for (id, _) in &tracks {
        for slot in 0..4 {
            let _ = h.send(EngineCommand::Clip(ClipCmd::SetLaunch {
                track: *id,
                slot,
                launch: SlotLaunch {
                    follow: rule,
                    mode: LaunchMode::Repeat,
                    legato: true,
                    velocity_amount: 0.5,
                    ..SlotLaunch::default()
                },
            }));
        }
    }
    for index in 0..4 {
        let _ = h.send(EngineCommand::Launch(LaunchCmd::SetScene {
            index,
            info: SceneInfo {
                tempo: Some(130.0 + index as f64),
                signature: Some(TimeSignature {
                    numerator: 3 + index as u8,
                    denominator: 4,
                }),
                follow: scene_follow(FollowAction::Other, 960),
            },
        }));
    }
    let _ = h.send(EngineCommand::Launch(LaunchCmd::LaunchScene(0)));
    let _ = h.send(EngineCommand::Launch(LaunchCmd::ClipDown {
        track: tracks[0].0,
        slot: 1,
        velocity: 0.7,
    }));
    engine.process_stereo(&mut warm.0, &mut warm.1);
    for _ in 0..4000 {
        assert_no_alloc(|| engine.process_stereo(&mut warm.0, &mut warm.1));
        h.collect_garbage();
    }
}

#[test]
fn notes_land_where_the_tempo_map_says_under_a_ramp_and_a_curve() {
    for curve in [0.0f32, 0.8, -0.8] {
        let tempo = TempoMap::new([
            rlive_core::time::TempoPoint::curved(0.0, 80.0, curve),
            rlive_core::time::TempoPoint::new(7680.0, 170.0),
        ]);
        let mut r = rig(vec![(0, beats_clip(8, 60, false))], vec![]);
        r.cmd(EngineCommand::Transport(TransportCmd::SetTimeMaps(
            Arc::new(TimeMaps {
                tempo: tempo.clone(),
                bars: BarMap::default(),
            }),
        )));
        r.quantize(Quantize::None);
        r.play(0);
        r.cmd(EngineCommand::Transport(TransportCmd::Play));
        let last = tempo.tick_to_seconds(7000.0) * SR;
        r.run(last as usize + 4000);
        let got = r.ons(60);
        assert_eq!(got.len(), 8, "{got:?}");
        for (beat, frame) in got.iter().enumerate() {
            let due = tempo.tick_to_seconds(beat as f64 * 960.0) * SR;
            assert!(
                (*frame as f64 - due).abs() <= 1.5,
                "curve {curve} beat {beat}: at {frame}, due {due}"
            );
        }
    }
}

#[test]
fn audio_starts_where_the_tempo_map_says_under_a_ramp() {
    let tempo = TempoMap::new([
        rlive_core::time::TempoPoint::curved(0.0, 90.0, 0.5),
        rlive_core::time::TempoPoint::new(3840.0, 160.0),
    ]);
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let id = TrackId::new();
    let mut t = Box::new(TrackNode::new(id, BLOCK));
    t.set_slot(0, Some(dc_clip(0.5, 4)));
    let _ = h.send(EngineCommand::graph(GraphOp::AddTrack {
        index: 0,
        track: t,
    }));
    let _ = h.send(EngineCommand::Transport(TransportCmd::SetTimeMaps(
        Arc::new(TimeMaps {
            tempo: tempo.clone(),
            bars: BarMap::default(),
        }),
    )));
    let _ = h.send(EngineCommand::Transport(TransportCmd::SetQuantize(
        Quantize::Beat,
    )));
    let _ = h.send(EngineCommand::Transport(TransportCmd::Play));
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    engine.process_stereo(&mut l, &mut r);
    // Launched just after the start, so it waits for beat 2 and the ramp is what places it.
    let _ = h.send(EngineCommand::Launch(LaunchCmd::LaunchClip {
        track: id,
        slot: 0,
    }));
    let due = tempo.tick_to_seconds(960.0) * SR;
    let mut signal = Vec::new();
    let total = due as usize + 4000;
    let mut left = vec![0.0f32; total];
    let mut right = vec![0.0f32; total];
    for (cl, cr) in left.chunks_mut(BLOCK).zip(right.chunks_mut(BLOCK)) {
        engine.process_stereo(cl, cr);
    }
    signal.extend_from_slice(&left);
    let first = signal.iter().position(|s| s.abs() > 1e-6).unwrap() + BLOCK;
    assert!(
        (first as f64 - due).abs() <= 1.5,
        "starts at {first}, due {due}"
    );
}
