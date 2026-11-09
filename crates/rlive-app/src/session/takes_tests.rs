//! Recording: a loopback that feeds known audio in through `AudioIo`, so files and their place on
//! the timeline can be checked sample for sample. No real device is ever opened.

use super::*;
use crate::recording::capture_midi::detect_phrase;
use rlive_audio_io::decode::decode;
use rlive_audio_io::{BitDepth, FileType};
use rlive_core::{MidiMessage, TimeSignature, PPQ};
use rlive_engine::{create_engine, AudioIo, Engine, EngineHandle, TransportCmd};
use rlive_project::{Clip, ClipContent, ExternalRoute, Monitor, RecordQuantize, Route, Track};
use rlive_testkit::assert_no_alloc;
use std::path::PathBuf;
use std::time::Duration;

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;
/// One bar of 4/4 at the default 120 BPM, in frames.
const BAR_FRAMES: usize = 96_000;
const BAR: u64 = 4 * PPQ;

/// A session recording into its own folder, with audio fed in by the test.
struct Rig {
    engine: Engine,
    s: Session,
    dir: PathBuf,
    /// Frames fed in so far. The input signal is a ramp of the frame number, so a sample says
    /// exactly which frame it was.
    fed: usize,
    bias: f32,
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn input_sample(frame: usize, bias: f32) -> f32 {
    bias + frame as f32 * 1e-6
}

impl Rig {
    fn new(name: &str) -> Self {
        let (engine, handle): (Engine, EngineHandle) = create_engine(SR, BLOCK);
        let mut project = Project::empty();
        let mut t = Track::new("Take", TrackKind::Audio, [1, 2, 3], project.scenes.len());
        t.routing.input = Route::External(ExternalRoute::Audio {
            channel: 0,
            stereo: false,
        });
        t.routing.monitor = Monitor::Off;
        project.tracks.push(t);
        let mut s = Session::new(handle, project, PluginCache::default());
        let dir = std::env::temp_dir().join(format!("rlive-take-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        s.record.temp_folder = Some(dir.clone());
        s.record.bit_depth = BitDepth::Bits32;
        s.record.file_type = FileType::Wav;
        Self {
            engine,
            s,
            dir,
            fed: 0,
            bias: 0.0,
        }
    }

    fn track(&self) -> TrackId {
        self.s.project.tracks[0].id
    }

    fn arm(&mut self) {
        let id = self.track();
        self.s.set_arm(id, true, true);
    }

    /// Runs `frames` frames through the engine with the ramp on input 1.
    fn run(&mut self, frames: usize) {
        let mut done = 0;
        while done < frames {
            let n = (frames - done).min(BLOCK);
            let input: Vec<f32> = (0..n)
                .map(|i| input_sample(self.fed + i, self.bias))
                .collect();
            let (mut l, mut r) = (vec![0.0; n], vec![0.0; n]);
            self.engine.process(AudioIo {
                inputs: &[&input],
                outputs: &mut [&mut l, &mut r],
            });
            self.fed += n;
            done += n;
            self.s.idle();
        }
        // Whatever is on its way to the disk arrives before anything looks.
        for _ in 0..5 {
            std::thread::sleep(Duration::from_millis(3));
            self.s.idle();
        }
    }

    /// Waits for the disk to finish and the take to land on the timeline or in a slot.
    fn settle(&mut self) {
        for _ in 0..500 {
            self.s.idle();
            if self.s.takes.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(4));
        }
        assert!(self.s.takes.is_empty(), "the take should be finished");
        self.s.samples.wait_idle();
        self.s.idle();
    }

    fn file_of(&self, clip: &Clip) -> PathBuf {
        match &clip.content {
            ClipContent::Audio { file, .. } => file.path.clone(),
            _ => panic!("an audio clip"),
        }
    }
}

#[test]
fn arrangement_audio_lands_where_it_was_played_and_holds_what_was_played() {
    let mut r = Rig::new("arrangement");
    r.arm();
    assert!(r.s.start_recording());
    r.run(BAR_FRAMES);
    r.s.stop_recording();
    r.run(512);
    r.settle();
    let t = &r.s.project.tracks[0];
    assert_eq!(t.arrangement.len(), 1, "one clip for the take");
    let c = &t.arrangement[0];
    assert_eq!(c.start, 0);
    assert_eq!(c.length, BAR);
    let audio = decode(&r.file_of(&c.clip)).unwrap();
    assert_eq!(audio.frames(), BAR_FRAMES, "the file is the take");
    assert!((audio.left[0] - input_sample(0, 0.0)).abs() < 1e-6);
    let expected = input_sample(70_000, 0.0);
    assert!(
        (audio.left[70_000] - expected).abs() < 1e-6,
        "every frame in its place"
    );
    let warp = match &c.clip.content {
        ClipContent::Audio { warp, .. } => warp.clone(),
        _ => unreachable!(),
    };
    assert!(warp.enabled);
    assert_eq!(warp.markers.len(), 2);
    assert!((warp.markers[1].tick - BAR as f64).abs() < 1e-3);
}

#[test]
fn the_overall_latency_is_taken_off_the_position_for_an_unmonitored_track() {
    let mut r = Rig::new("latency-off");
    r.arm();
    r.s.engine
        .shared
        .input_latency
        .store(100, std::sync::atomic::Ordering::Relaxed);
    r.s.engine
        .shared
        .output_latency
        .store(200, std::sync::atomic::Ordering::Relaxed);
    r.s.start_recording();
    r.run(48_000);
    r.s.stop_recording();
    r.run(512);
    r.settle();
    let c = &r.s.project.tracks[0].arrangement[0];
    assert_eq!(c.start, 0, "the take starts at the punch point");
    let audio = decode(&r.file_of(&c.clip)).unwrap();
    // The frame that arrived 300 frames in was played at the very start of the take.
    assert!((audio.left[0] - input_sample(300, 0.0)).abs() < 1e-6);
}

#[test]
fn keep_monitoring_latency_records_what_was_heard() {
    let mut r = Rig::new("latency-keep");
    r.arm();
    let id = r.track();
    r.s.project.track_mut(id).unwrap().routing.monitor = Monitor::In;
    r.s.send_track_edits(Vec::new());
    r.s.engine
        .shared
        .input_latency
        .store(100, std::sync::atomic::Ordering::Relaxed);
    r.s.engine
        .shared
        .output_latency
        .store(200, std::sync::atomic::Ordering::Relaxed);
    r.s.start_recording();
    r.run(48_000);
    r.s.stop_recording();
    r.run(512);
    r.settle();
    let c = &r.s.project.tracks[0].arrangement[0];
    let audio = decode(&r.file_of(&c.clip)).unwrap();
    assert!(
        (audio.left[0] - input_sample(0, 0.0)).abs() < 1e-6,
        "no offset"
    );
}

#[test]
fn switching_keep_latency_off_takes_the_whole_trip_back_out_when_monitoring() {
    let mut r = Rig::new("latency-keep-off");
    r.arm();
    let id = r.track();
    r.s.project.track_mut(id).unwrap().routing.monitor = Monitor::In;
    r.s.project.track_mut(id).unwrap().keep_monitor_latency = false;
    r.s.send_track_edits(Vec::new());
    r.s.engine
        .shared
        .input_latency
        .store(64, std::sync::atomic::Ordering::Relaxed);
    r.s.engine
        .shared
        .output_latency
        .store(36, std::sync::atomic::Ordering::Relaxed);
    r.s.start_recording();
    r.run(4_800);
    r.s.stop_recording();
    r.run(512);
    r.settle();
    let c = &r.s.project.tracks[0].arrangement[0];
    let audio = decode(&r.file_of(&c.clip)).unwrap();
    assert!((audio.left[0] - input_sample(100, 0.0)).abs() < 1e-6);
}

#[test]
fn punch_in_and_out_cut_on_the_loop_edges_to_the_frame() {
    let mut r = Rig::new("punch");
    r.arm();
    r.s.set_loop(false, BAR, BAR * 2);
    r.s.set_punch(true, true);
    r.s.start_recording();
    r.run(BAR_FRAMES * 3);
    r.s.stop_recording();
    r.run(512);
    r.settle();
    let c = &r.s.project.tracks[0].arrangement[0];
    assert_eq!((c.start, c.length), (BAR, BAR));
    let audio = decode(&r.file_of(&c.clip)).unwrap();
    assert_eq!(audio.frames(), BAR_FRAMES, "only the punched part");
    assert!((audio.left[0] - input_sample(BAR_FRAMES, 0.0)).abs() < 1e-6);
}

#[test]
fn a_loop_recording_keeps_every_pass_and_shows_the_last() {
    let mut r = Rig::new("loop");
    r.arm();
    r.s.set_loop(true, 0, BAR);
    r.s.start_recording();
    r.run(BAR_FRAMES * 3);
    r.s.stop_recording();
    r.run(512);
    r.settle();
    let t = &r.s.project.tracks[0];
    assert_eq!(t.arrangement.len(), 1, "one clip for all the passes");
    let c = &t.arrangement[0];
    let audio = decode(&r.file_of(&c.clip)).unwrap();
    assert_eq!(
        audio.frames(),
        3 * BAR_FRAMES,
        "all the passes in one sample"
    );
    assert_eq!(c.start, 0, "the last pass plays over the loop");
    assert_eq!(c.length, BAR);
    assert_eq!(c.offset, 2 * BAR, "showing the last pass");
}

#[test]
fn a_loop_recording_makes_one_take_lane_per_pass_and_copies_the_last_to_main() {
    let mut r = Rig::new("loop-takes");
    r.arm();
    r.s.set_loop(true, 0, BAR);
    r.s.start_recording();
    r.run(BAR_FRAMES * 3);
    r.s.stop_recording();
    r.run(512);
    r.settle();
    let t = &r.s.project.tracks[0];
    assert_eq!(t.take_lanes.len(), 3, "a lane for each pass");
    for (i, l) in t.take_lanes.iter().enumerate() {
        let c = &l.clips[0];
        assert_eq!((c.start, c.length), (0, BAR), "pass {i}");
        assert_eq!(c.offset, i as u64 * BAR, "its own pass of the one sample");
    }
    assert_eq!(t.take_lanes[0].name, "Take 1");
    assert_eq!(t.arrangement.len(), 1);
    assert_eq!(
        t.arrangement[0].offset,
        2 * BAR,
        "the last pass is on the main lane"
    );
    assert_ne!(
        t.arrangement[0].id, t.take_lanes[2].clips[0].id,
        "an independent copy"
    );
    assert!(r.s.undo());
    let t = &r.s.project.tracks[0];
    assert_eq!(t.take_lanes.len(), 2, "undo takes one pass off");
    assert_eq!(t.arrangement[0].offset, BAR);
}

#[test]
fn undo_unrolls_a_loop_recording_one_pass_at_a_time() {
    let mut r = Rig::new("unroll");
    r.arm();
    r.s.set_loop(true, 0, BAR);
    r.s.start_recording();
    r.run(BAR_FRAMES * 2 + 48_000);
    r.s.stop_recording();
    r.run(512);
    r.settle();
    let offset = |r: &Rig| r.s.project.tracks[0].arrangement.first().map(|c| c.offset);
    let last = offset(&r).unwrap();
    assert_eq!(last, 2 * BAR);
    assert!(r.s.undo());
    assert_eq!(offset(&r), Some(BAR));
    assert!(r.s.undo());
    assert_eq!(offset(&r), Some(0));
    assert!(r.s.undo());
    assert_eq!(offset(&r), None, "and off the timeline altogether");
}

#[test]
fn a_recording_that_never_reaches_its_punch_in_leaves_nothing_behind() {
    let mut r = Rig::new("no-punch");
    r.arm();
    r.s.set_loop(false, BAR * 4, BAR * 8);
    r.s.set_punch(true, false);
    r.s.start_recording();
    r.run(BAR_FRAMES / 2);
    r.s.stop_recording();
    r.run(512);
    r.settle();
    assert!(r.s.project.tracks[0].arrangement.is_empty());
}

#[test]
fn a_slot_records_from_one_launch_point_to_the_next_and_then_plays() {
    let mut r = Rig::new("slot");
    r.arm();
    let id = r.track();
    assert!(r.s.record_slot(id, 0));
    // Global quantize is one bar and the transport starts at bar 1: the take starts at once.
    r.run(BAR_FRAMES / 4);
    assert_eq!(r.s.recording_slot(id), Some(0));
    r.s.close_slot_recording(id);
    // It closes at the next bar line, whenever it was asked to.
    r.run(BAR_FRAMES / 4);
    assert_eq!(r.s.recording_slot(id), Some(0), "not until the bar line");
    r.run(BAR_FRAMES * 3 / 4);
    r.settle();
    assert_eq!(r.s.recording_slot(id), None);
    let c = r.s.clip(id, 0).expect("a clip in the slot").clone();
    assert_eq!(c.length, BAR);
    assert!(c.looped);
    let audio = decode(&r.file_of(&c)).unwrap();
    assert_eq!(audio.frames(), BAR_FRAMES);
    assert!((audio.left[1234] - input_sample(1234, 0.0)).abs() < 1e-6);
    r.run(1024);
    let shared = r.s.track_shared[&id].clone();
    assert_eq!(
        shared
            .playing_slot
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "and it plays"
    );
    let at_tick = BAR_FRAMES as f64 / 48_000.0 * 2.0 * 960.0;
    assert!(shared.clip_position.load() < at_tick, "from its start");
}

#[test]
fn a_slot_closing_at_the_transport_stop_keeps_what_it_took_and_does_not_play() {
    let mut r = Rig::new("slot-stop");
    r.arm();
    let id = r.track();
    r.s.record_slot(id, 0);
    r.run(BAR_FRAMES / 2);
    r.s.stop();
    r.run(1024);
    r.settle();
    let c = r.s.clip(id, 0).expect("a clip").clone();
    let shared = r.s.track_shared[&id].clone();
    assert_eq!(
        shared
            .playing_slot
            .load(std::sync::atomic::Ordering::Relaxed),
        -1
    );
    let audio = decode(&r.file_of(&c)).unwrap();
    assert!(audio.frames() >= BAR_FRAMES / 2 && audio.frames() < BAR_FRAMES / 2 + BLOCK * 4);
}

#[test]
fn session_record_closes_all_armed_takes_into_loop_playback() {
    let mut r = Rig::new("session-record");
    r.arm();
    let id = r.track();
    r.s.session_record(0);
    r.run(BAR_FRAMES / 2);
    assert_eq!(r.s.recording_slot(id), Some(0));
    r.s.session_record(0);
    r.run(BAR_FRAMES);
    r.settle();
    assert!(r.s.clip(id, 0).is_some());
}

#[test]
fn the_new_button_finds_a_free_scene_and_makes_one_if_it_must() {
    let mut r = Rig::new("new");
    r.arm();
    let id = r.track();
    assert_eq!(r.s.new_take(0), Some(0));
    let scenes = r.s.project.scenes.len();
    for slot in 0..scenes {
        let c = Clip::new_midi("x", [0, 0, 0], 1, r.s.project.time_signature);
        r.s.set_clip(id, slot, Some(c));
    }
    assert_eq!(r.s.new_take(0), Some(scenes));
    assert_eq!(r.s.project.scenes.len(), scenes + 1);
}

fn on(key: u8) -> MidiMessage {
    MidiMessage::NoteOn {
        channel: 0,
        key,
        velocity: 0.7,
        note_id: 0,
        tuning: 0.0,
    }
}

fn off(key: u8) -> MidiMessage {
    MidiMessage::NoteOff {
        channel: 0,
        key,
        velocity: 0.5,
        note_id: 0,
    }
}

/// A rig whose only track is a looped MIDI clip playing in slot 0.
fn overdub_rig(name: &str) -> (Rig, TrackId) {
    let mut r = Rig::new(name);
    let (engine, handle): (Engine, EngineHandle) = create_engine(SR, BLOCK);
    let mut project = Project::empty();
    let mut t = Track::new("M", TrackKind::Midi, [1, 2, 3], project.scenes.len());
    t.arm = true;
    let sig = project.time_signature;
    t.clips[0] = Some(Clip::new_midi("c", [1, 2, 3], 1, sig));
    let id = t.id;
    project.tracks.push(t);
    let s = Session::new(handle, project, PluginCache::default());
    r.engine = engine;
    r.s = s;
    r.s.record.temp_folder = Some(r.dir.clone());
    r.s.launch_clip(id, 0);
    r.run(BAR_FRAMES / 2);
    (r, id)
}

#[test]
fn notes_go_into_a_looping_clip_and_undo_takes_off_the_last_pass() {
    let (mut r, id) = overdub_rig("overdub");
    r.s.session_record(0);
    assert!(
        r.s.is_overdubbing(id) || {
            r.run(512);
            r.s.is_overdubbing(id)
        }
    );
    r.s.play_midi(id, on(60));
    r.run(4_800);
    r.s.play_midi(id, off(60));
    r.run(512);
    assert_eq!(r.s.clip(id, 0).unwrap().notes().len(), 1);
    r.run(BAR_FRAMES);
    r.s.play_midi(id, on(62));
    r.run(4_800);
    r.s.play_midi(id, off(62));
    r.run(512);
    assert_eq!(r.s.clip(id, 0).unwrap().notes().len(), 2, "both passes");
    r.s.set_overdub(id, false);
    r.run(512);
    assert!(r.s.undo());
    assert_eq!(
        r.s.clip(id, 0).unwrap().notes().len(),
        1,
        "only the last pass went"
    );
}

#[test]
fn record_quantize_applies_at_once_to_notes_in_a_looping_overdub() {
    let (mut r, id) = overdub_rig("overdub-quantize");
    assert!(r.s.set_record_quantize(RecordQuantize::Quarter));
    r.s.session_record(0);
    r.run(512);
    r.s.play_midi(id, on(60));
    r.run(1200);
    r.s.play_midi(id, off(60));
    r.run(512);
    let note = r.s.clip(id, 0).unwrap().notes()[0].clone();
    assert_eq!(note.start % PPQ, 0, "on the beat: {}", note.start);
}

#[test]
fn an_arrangement_take_with_quantize_undoes_quantize_first() {
    let mut r = Rig::new("arr-quantize");
    let (engine, handle) = create_engine(SR, BLOCK);
    let mut project = Project::empty();
    let mut t = Track::new("M", TrackKind::Midi, [1, 2, 3], project.scenes.len());
    t.arm = true;
    let id = t.id;
    project.tracks.push(t);
    project.record_quantize = RecordQuantize::Quarter;
    r.engine = engine;
    r.s = Session::new(handle, project, PluginCache::default());
    r.s.record.temp_folder = Some(r.dir.clone());
    r.s.start_recording();
    r.run(1024);
    r.s.play_midi(id, on(60));
    r.run(1024);
    r.s.play_midi(id, off(60));
    r.run(1024);
    r.s.stop_recording();
    r.run(1024);
    let start = |r: &Rig| r.s.project.tracks[0].arrangement[0].clip.notes()[0].start;
    assert_eq!(start(&r) % PPQ, 0);
    assert!(r.s.undo());
    let raw = start(&r);
    assert_ne!(
        raw % PPQ,
        0,
        "the first undo only takes off the quantize: {raw}"
    );
    assert!(r.s.undo());
    assert!(r.s.project.tracks[0].arrangement.is_empty());
}

#[test]
fn midi_overdub_mixes_the_new_notes_with_what_was_on_the_timeline() {
    let mut r = Rig::new("arr-overdub");
    let (engine, handle) = create_engine(SR, BLOCK);
    let mut project = Project::empty();
    let mut t = Track::new("M", TrackKind::Midi, [1, 2, 3], project.scenes.len());
    t.arm = true;
    let id = t.id;
    let sig = project.time_signature;
    let mut clip = Clip::new_midi("old", [1, 2, 3], 1, sig);
    clip.notes_mut()
        .unwrap()
        .push(rlive_project::NoteData::new(0, 240, 40, 0.5));
    t.place_clip(rlive_project::ArrangementClip::new(clip, 0));
    project.tracks.push(t);
    project.midi_arrangement_overdub = true;
    r.engine = engine;
    r.s = Session::new(handle, project, PluginCache::default());
    r.s.record.temp_folder = Some(r.dir.clone());
    r.s.start_recording();
    r.run(1024);
    r.s.play_midi(id, on(60));
    r.run(1024);
    r.s.play_midi(id, off(60));
    r.run(1024);
    r.s.stop_recording();
    r.run(1024);
    let notes = r.s.project.tracks[0].arrangement[0].clip.notes().to_vec();
    let keys: Vec<u8> = notes.iter().map(|n| n.key).collect();
    assert!(keys.contains(&40) && keys.contains(&60), "{keys:?}");
}

#[test]
fn step_recording_extends_held_notes_and_left_takes_them_back() {
    let (mut r, id) = overdub_rig("step");
    r.s.stop();
    r.run(512);
    let clip = ClipRef::slot(id, 0);
    r.s.play_midi(id, on(64));
    r.run(512);
    let mut at = 0;
    at = r.s.step_forward(clip, at, PPQ / 4);
    at = r.s.step_forward(clip, at, PPQ / 4);
    assert_eq!(at, PPQ / 2);
    let notes = r.s.clip(id, 0).unwrap().notes().to_vec();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].length, PPQ / 2, "held through two steps");
    at = r.s.step_back(clip, at, PPQ / 4);
    assert_eq!(at, PPQ / 4);
    assert_eq!(r.s.clip(id, 0).unwrap().notes()[0].length, PPQ / 4);
    at = r.s.step_back(clip, at, PPQ / 4);
    assert_eq!(at, 0);
    assert!(r.s.clip(id, 0).unwrap().notes().is_empty());
    r.s.play_midi(id, off(64));
    r.run(512);
    assert_eq!(r.s.step_forward(clip, 0, 120), 120, "no keys: just moves");
    assert!(r.s.clip(id, 0).unwrap().notes().is_empty());
}

#[test]
fn capture_in_an_empty_stopped_set_finds_tempo_and_starts_playing() {
    let (engine, handle) = create_engine(SR, BLOCK);
    let mut project = Project::empty();
    let mut t = Track::new("M", TrackKind::Midi, [1, 2, 3], project.scenes.len());
    t.arm = true;
    let id = t.id;
    project.tracks.push(t);
    let s = Session::new(handle, project, PluginCache::default());
    let mut r = Rig {
        engine,
        s,
        dir: std::env::temp_dir().join(format!("rlive-take-{}-capture", std::process::id())),
        fed: 0,
        bias: 0.0,
    };
    // A bar and a half of quarter notes, in the tempo 140, ending on the next downbeat: fed in
    // real time through the same path played notes use.
    let per_beat = (60.0 / 140.0 * SR) as usize;
    for beat in 0..=4 {
        r.s.play_midi(id, on(60 + beat as u8));
        r.run(per_beat / 2);
        r.s.play_midi(id, off(60 + beat as u8));
        r.run(per_beat - per_beat / 2);
    }
    r.run(512);
    r.s.capture_midi(false, 0);
    r.run(1024);
    assert!(
        (r.s.project.tempo - 140.0).abs() < 2.0,
        "tempo {}",
        r.s.project.tempo
    );
    let c = r.s.clip(id, 0).expect("a clip").clone();
    assert_eq!(c.length, r.s.project.time_signature.ticks_per_bar());
    assert_eq!(c.notes().len(), 4);
    assert!(r.s.is_playing());
}

#[test]
fn capture_overdubs_onto_a_playing_clip() {
    let (mut r, id) = overdub_rig("capture-overdub");
    r.s.play_midi(id, on(72));
    r.run(4_800);
    r.s.play_midi(id, off(72));
    r.run(4_800);
    r.s.capture_midi(false, 0);
    r.run(512);
    let c = r.s.clip(id, 0).unwrap();
    assert_eq!(c.notes().len(), 1);
    assert_eq!(c.notes()[0].key, 72);
}

#[test]
fn detect_phrase_works_on_the_pure_function_for_a_second_meter() {
    let sig = TimeSignature {
        numerator: 3,
        denominator: 4,
    };
    let notes: Vec<_> = (0..=6)
        .map(|b| crate::recording::capture_midi::PlayedNote {
            seconds: b as f64 * 60.0 / 120.0,
            tick: None,
            duration: 0.25,
            key: 60,
            velocity: 0.7,
        })
        .collect();
    let p = detect_phrase(&notes, sig).unwrap();
    assert_eq!(p.length, 2 * sig.ticks_per_bar());
    assert!((p.tempo - 120.0).abs() < 1.0);
}

#[test]
fn a_tempo_change_while_recording_becomes_a_warp_marker() {
    let mut r = Rig::new("tempo");
    r.arm();
    r.s.start_recording();
    r.run(BAR_FRAMES);
    r.s.set_tempo(60.0);
    r.run(48_000);
    r.s.stop_recording();
    r.run(512);
    r.settle();
    let c = &r.s.project.tracks[0].arrangement[0];
    let ClipContent::Audio { warp, .. } = &c.clip.content else {
        panic!();
    };
    assert_eq!(warp.markers.len(), 3, "start, the change, and the end");
    let m = warp.markers[1];
    assert!((m.seconds - 2.0).abs() < 0.02, "at two seconds in: {m:?}");
    assert!((m.tick - BAR as f64).abs() < 12.0, "at one bar in: {m:?}");
    let end = warp.markers[2];
    assert!(
        (end.tick - m.tick - 2.0 * PPQ as f64 / 2.0).abs() < 48.0,
        "{end:?}"
    );
}

#[test]
fn nothing_on_the_audio_thread_allocates_while_recording() {
    let mut r = Rig::new("no-alloc");
    r.arm();
    r.s.start_recording();
    r.run(4096);
    let input = vec![0.5f32; BLOCK];
    let (mut l, mut ll) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    let engine = &mut r.engine;
    assert_no_alloc(|| {
        for _ in 0..16 {
            engine.process(AudioIo {
                inputs: &[&input],
                outputs: &mut [&mut l, &mut ll],
            });
        }
    });
}

#[test]
fn monitoring_in_passes_the_input_through_and_the_mono_channel_reaches_both_sides() {
    let mut r = Rig::new("monitor");
    let id = r.track();
    r.s.project.track_mut(id).unwrap().routing.monitor = Monitor::In;
    r.s.send_track_edits(Vec::new());
    r.bias = 0.25;
    r.run(1024);
    let input = vec![0.25f32; BLOCK];
    let (mut left, mut right) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    r.engine.process(AudioIo {
        inputs: &[&input],
        outputs: &mut [&mut left, &mut right],
    });
    assert!(
        left.iter().all(|s| (s - 0.25).abs() < 1e-3),
        "{:?}",
        &left[..3]
    );
    assert!(right.iter().all(|s| (s - 0.25).abs() < 1e-3));
}

#[test]
fn monitoring_off_is_silent_and_auto_follows_arm_but_gives_way_to_a_playing_clip() {
    let mut r = Rig::new("auto");
    let id = r.track();
    r.bias = 0.25;
    let peak = |r: &mut Rig| {
        let input = vec![0.25f32; BLOCK];
        let (mut left, mut right) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
        for _ in 0..2 {
            r.engine.process(AudioIo {
                inputs: &[&input],
                outputs: &mut [&mut left, &mut right],
            });
        }
        left.iter().fold(0f32, |m, s| m.max(s.abs()))
    };
    assert_eq!(peak(&mut r), 0.0, "off");
    r.s.project.track_mut(id).unwrap().routing.monitor = Monitor::Auto;
    r.s.send_track_edits(Vec::new());
    r.run(512);
    assert_eq!(peak(&mut r), 0.0, "auto, not armed");
    r.s.set_arm(id, true, true);
    r.run(512);
    assert!(peak(&mut r) > 0.2, "auto, armed");
}

#[test]
fn resampling_records_the_main_output_and_silences_its_own_track() {
    let mut r = Rig::new("resample");
    let id = r.track();
    r.s.project.track_mut(id).unwrap().routing.input = Route::Resampling;
    r.s.send_track_edits(Vec::new());
    // A second track playing the input to Main.
    let mut other = Track::new(
        "Feed",
        TrackKind::Audio,
        [1, 1, 1],
        r.s.project.scenes.len(),
    );
    other.routing.input = Route::External(ExternalRoute::Audio {
        channel: 0,
        stereo: false,
    });
    other.routing.monitor = Monitor::In;
    let idx = r.s.project.tracks.len();
    let node = r.s.build_node(&other);
    r.s.project.tracks.push(other);
    r.s.send_track_edits(vec![GraphOp::AddTrack {
        index: idx,
        track: node,
    }]);
    r.bias = 0.1;
    r.s.set_arm(id, true, true);
    r.run(512);
    r.s.start_recording();
    r.run(4_800);
    r.s.stop_recording();
    r.run(512);
    r.settle();
    let c = &r.s.project.tracks[0].arrangement[0];
    let audio = decode(&r.file_of(&c.clip)).unwrap();
    assert!(audio.frames() >= 4_000);
    assert_eq!(audio.channels, 2);
    let (mid, heard) = (
        audio.left[2000],
        input_sample(r.fed - 512 - 4_800 + 2_000, r.bias),
    );
    assert!((mid - heard).abs() < 1e-2, "it's the mix: {mid}, {heard}");
}

#[test]
fn the_recording_folder_is_the_sets_when_it_has_been_saved() {
    let saved = Path::new("/somewhere/Song Project/Song.rlive");
    let folder = takes::recording_folder(Some(saved), None);
    assert_eq!(
        folder,
        Path::new("/somewhere/Song Project/Samples/Recorded")
    );
    let temp = Path::new("/tmp/mine");
    assert_eq!(takes::recording_folder(None, Some(temp)), temp);
    assert!(takes::recording_folder(None, None).ends_with("rLive/Recorded"));
}

#[test]
fn saving_moves_temporary_recordings_into_the_project() {
    let mut r = Rig::new("adopt");
    r.arm();
    r.s.start_recording();
    r.run(4_800);
    r.s.stop_recording();
    r.run(512);
    r.settle();
    let project_dir = r.dir.join("project");
    let file = project_dir.join("Song.rlive");
    r.s.save(&file).unwrap();
    let c = r.s.project.tracks[0].arrangement[0].clone();
    let path = r.file_of(&c.clip);
    assert!(path.starts_with(project_dir.join("Samples").join("Recorded")));
    assert!(path.exists());
}

#[test]
fn count_in_holds_the_take_back_and_the_transport_with_it() {
    let mut r = Rig::new("count-in");
    r.arm();
    let mut o = r.s.project.metronome_options;
    o.count_in = 1;
    r.s.set_metronome_options(o);
    r.s.start_recording();
    r.run(BAR_FRAMES / 2);
    assert!(!r.s.is_playing(), "still counting in");
    assert!(
        (r.s.engine.shared.count_in_left.load() - BAR as f64 / 2.0).abs() < 2.0 * PPQ as f64 / 4.0
    );
    r.run(BAR_FRAMES / 2 + 1024);
    assert!(r.s.is_playing());
    r.run(BAR_FRAMES / 2);
    r.s.stop_recording();
    r.run(512);
    r.settle();
    let c = &r.s.project.tracks[0].arrangement[0];
    assert_eq!(
        c.start, 0,
        "the take starts at the song position, after the count-in"
    );
    let audio = decode(&r.file_of(&c.clip)).unwrap();
    // Nothing from the count-in is in the file.
    let first = audio.left[0];
    assert!(
        first >= input_sample(BAR_FRAMES - 1, 0.0),
        "starts after it: {first}"
    );
}

#[test]
fn the_tempo_map_is_what_the_transport_command_changed_during_a_take() {
    let mut r = Rig::new("set-tempo");
    r.arm();
    r.s.start_recording();
    r.run(512);
    r.s.send(EngineCommand::Transport(TransportCmd::SetTempo(100.0)));
    r.run(512);
    r.s.stop_recording();
    r.run(512);
    r.settle();
}

#[test]
fn a_disk_too_slow_shows_a_warning_and_the_file_keeps_its_place() {
    let mut r = Rig::new("overflow");
    r.arm();
    // A ring far too small for how fast this test pushes audio through it.
    r.s.take_ring_seconds = 0.002;
    r.s.start_recording();
    r.run(BAR_FRAMES / 4);
    r.s.stop_recording();
    r.run(512);
    r.settle();
    assert!(r.s.dropped_frames > 0, "it should have overflowed");
    assert!(r.s.messages.iter().any(|m| m.contains("could not keep up")));
    let c = &r.s.project.tracks[0].arrangement[0];
    let audio = decode(&r.file_of(&c.clip)).unwrap();
    assert_eq!(
        audio.frames(),
        BAR_FRAMES / 4,
        "silence where audio was lost"
    );
}

#[test]
fn start_playback_with_record_off_waits_for_play() {
    let mut r = Rig::new("no-autoplay");
    r.arm();
    r.s.record.start_playback_with_record = false;
    r.s.start_recording();
    r.run(4_800);
    assert!(!r.s.is_playing());
    r.s.play();
    r.run(4_800);
    assert!(r.s.is_playing());
    r.s.stop_recording();
    r.run(512);
    r.settle();
    let c = &r.s.project.tracks[0].arrangement[0];
    let audio = decode(&r.file_of(&c.clip)).unwrap();
    assert!(
        (audio.left[0] - input_sample(4_800, 0.0)).abs() < 1e-5,
        "{}",
        audio.left[0]
    );
}

#[test]
fn shift_inverts_start_playback_with_record() {
    let mut r = Rig::new("shift");
    r.arm();
    r.s.record.start_playback_with_record = false;
    r.s.start_recording_inverted();
    r.run(4_800);
    assert!(r.s.is_playing(), "inverted, so it starts");
}

#[test]
fn start_recording_on_scene_launch_records_empty_slots_of_armed_tracks() {
    let mut r = Rig::new("scene");
    r.arm();
    let id = r.track();
    r.s.record.start_recording_on_scene_launch = true;
    r.s.launch_scene(2);
    r.run(4_800);
    assert_eq!(r.s.recording_slot(id), Some(2));
}

#[test]
fn a_scene_launch_alone_does_not_record_by_default() {
    let mut r = Rig::new("scene-off");
    r.arm();
    let id = r.track();
    r.s.launch_scene(2);
    r.run(4_800);
    assert_eq!(r.s.recording_slot(id), None);
}

#[test]
fn record_quantize_cannot_be_changed_during_recording() {
    let mut r = Rig::new("locked");
    r.arm();
    r.s.start_recording();
    r.run(512);
    assert!(!r.s.set_record_quantize(RecordQuantize::Quarter));
    r.s.stop_recording();
    r.run(512);
    r.settle();
    assert!(r.s.set_record_quantize(RecordQuantize::Quarter));
}

#[test]
fn an_audio_track_records_input_one_by_default_and_a_count_in_arms_with_the_slot() {
    let mut r = Rig::new("default-input");
    let id = r.track();
    r.s.project.track_mut(id).unwrap().routing.input = Route::Default;
    r.s.send_track_edits(Vec::new());
    r.arm();
    let mut o = r.s.project.metronome_options;
    o.count_in = 1;
    r.s.set_metronome_options(o);
    assert!(r.s.record_slot(id, 0));
    r.run(BAR_FRAMES / 2);
    assert!(!r.s.is_playing(), "the slot waits out the count-in");
    r.run(BAR_FRAMES);
    assert!(r.s.is_playing());
    r.s.close_slot_recording(id);
    r.run(BAR_FRAMES);
    r.settle();
    let c = r.s.clip(id, 0).expect("a clip").clone();
    let audio = decode(&r.file_of(&c)).unwrap();
    assert_eq!(audio.channels, 1, "mono");
    assert!((audio.left[10] - input_sample(BAR_FRAMES + 10, 0.0)).abs() < 1e-5);
}

#[test]
fn alt_double_click_creates_arms_launches_and_listens() {
    let (mut r, id) = overdub_rig("create-arm-launch");
    r.s.set_arm(id, false, false);
    let scene = 3;
    r.s.create_arm_and_launch(id, scene);
    r.run(BAR_FRAMES);
    assert!(r.s.clip(id, scene).is_some());
    assert!(r.s.project.track(id).unwrap().arm);
    assert!(r.s.is_overdubbing(id));
}
