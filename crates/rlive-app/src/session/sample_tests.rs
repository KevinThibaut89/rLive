//! Samples loading in the background while the session carries on.

use super::*;
use rlive_audio_io::SampleStatus;
use rlive_core::{MidiMessage, PPQ};
use rlive_engine::{create_engine, Engine};
use rlive_project::{ArrangementClip, Clip, ClipContent};
use std::sync::atomic::{AtomicUsize, Ordering};

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Dir(PathBuf);

impl Dir {
    fn new() -> Self {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("rlive-async-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn write_tone(path: &Path, seconds: f64, rate: u32, hz: f32) {
    let n = (seconds * rate as f64) as usize;
    let tone: Vec<f32> = (0..n)
        .map(|i| 0.5 * (i as f32 / rate as f32 * hz * std::f32::consts::TAU).sin())
        .collect();
    rlive_devices::write_wav(path, &tone, &tone, rate).unwrap();
}

fn audio_clip(path: &Path, beats: u64) -> Clip {
    Clip {
        name: "Loop".into(),
        color: [4, 5, 6],
        length: beats * PPQ,
        looped: true,
        content: ClipContent::audio(path, 0.0),
        ..Clip::default()
    }
}

fn session() -> (Engine, Session) {
    let (engine, handle) = create_engine(SR, BLOCK);
    let s = Session::new(handle, Project::new_default(), PluginCache::default());
    (engine, s)
}

fn audio_track(s: &mut Session) -> TrackId {
    s.add_track(TrackKind::Audio, None)
}

/// Blocks of audio, without letting the session pick up anything that finished loading.
fn render_only(engine: &mut Engine, blocks: usize) -> f32 {
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    let mut loudest = 0.0f32;
    for _ in 0..blocks {
        engine.process_stereo(&mut l, &mut r);
        loudest = loudest.max(l.iter().fold(0.0, |a, x| a.max(x.abs())));
    }
    loudest
}

/// Waits for every load to finish and picks up the results, as a UI frame would.
fn arrive(s: &mut Session) {
    s.samples.wait_idle();
    s.idle();
}

/// The first event out of the engine is the queued commands being run; this flushes those.
fn flush(engine: &mut Engine) {
    render_only(engine, 1);
}

#[test]
fn a_lane_clip_is_silent_until_its_sample_arrives_then_plays_without_allocating() {
    let dir = Dir::new();
    let path = dir.file("tone.wav");
    write_tone(&path, 2.0, SR as u32, 440.0);
    let (mut engine, mut s) = session();
    let t = audio_track(&mut s);
    s.arr_place(t, ArrangementClip::new(audio_clip(&path, 8), 0));
    assert!(matches!(
        s.samples.status(&path),
        Some(SampleStatus::Loading)
    ));
    s.play();
    assert_eq!(render_only(&mut engine, 8), 0.0, "no data, no sound");

    s.samples.wait_idle();
    s.idle();
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    // The whole swap, from the command being picked up to the first sound, on the audio side.
    rlive_testkit::assert_no_alloc(|| engine.process_stereo(&mut l, &mut r));
    assert!(render_only(&mut engine, 8) > 0.1, "the clip plays now");
}

#[test]
fn a_session_clip_cannot_be_launched_until_its_sample_arrives() {
    let dir = Dir::new();
    let (slow, playing) = (dir.file("slow.wav"), dir.file("playing.wav"));
    write_tone(&slow, 2.0, SR as u32, 440.0);
    write_tone(&playing, 2.0, SR as u32, 220.0);
    let (mut engine, mut s) = session();
    let t = audio_track(&mut s);
    s.set_clip(t, 0, Some(audio_clip(&playing, 8)));
    s.samples.wait_idle();
    s.idle();
    s.launch_clip(t, 0);
    assert!(render_only(&mut engine, 8) > 0.1);

    s.set_clip(t, 1, Some(audio_clip(&slow, 8)));
    assert!(s.clip_waiting_for_sample(t, 1));
    s.launch_clip(t, 1);
    assert!(render_only(&mut engine, 8) > 0.1, "the old clip carries on");

    s.samples.wait_idle();
    s.idle();
    assert!(!s.clip_waiting_for_sample(t, 1));
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    rlive_testkit::assert_no_alloc(|| engine.process_stereo(&mut l, &mut r));
    s.launch_clip(t, 1);
    assert!(render_only(&mut engine, 2) > 0.1);
}

#[test]
fn a_scene_launch_leaves_a_track_whose_clip_is_not_ready_alone() {
    let dir = Dir::new();
    let (a, b, c) = (dir.file("a.wav"), dir.file("b.wav"), dir.file("c.wav"));
    write_tone(&a, 2.0, SR as u32, 440.0);
    write_tone(&b, 2.0, SR as u32, 660.0);
    write_tone(&c, 2.0, SR as u32, 880.0);
    let (mut engine, mut s) = session();
    let (ta, tb) = (audio_track(&mut s), audio_track(&mut s));
    s.set_clip(ta, 0, Some(audio_clip(&a, 8)));
    s.set_clip(tb, 0, Some(audio_clip(&b, 8)));
    arrive(&mut s);
    s.launch_scene(0);
    assert!(render_only(&mut engine, 4) > 0.1);
    s.set_clip(tb, 1, Some(audio_clip(&c, 8)));
    assert!(s.clip_waiting_for_sample(tb, 1));

    s.launch_scene(1);
    assert!(
        render_only(&mut engine, 8) > 0.1,
        "tb keeps playing; a stop would have silenced it"
    );
    arrive(&mut s);
}

#[test]
fn loading_a_project_returns_before_its_samples_are_decoded() {
    let dir = Dir::new();
    let sample = dir.file("tone.wav");
    write_tone(&sample, 2.0, SR as u32, 440.0);
    let project = dir.file("set.rlive");
    let (mut engine, mut s) = session();
    let t = audio_track(&mut s);
    s.arr_place(t, ArrangementClip::new(audio_clip(&sample, 8), 0));
    s.save(&project).unwrap();
    arrive(&mut s);

    let (mut engine2, handle) = create_engine(SR, BLOCK);
    let mut fresh = Session::new(handle, Project::new_default(), PluginCache::default());
    fresh.load(&project).unwrap();
    assert!(
        fresh.samples.is_loading(&sample),
        "load returned with the sample still loading"
    );
    fresh.play();
    assert_eq!(render_only(&mut engine2, 8), 0.0);

    fresh.samples.wait_idle();
    fresh.idle();
    assert!(render_only(&mut engine2, 8) > 0.1);
    flush(&mut engine);
}

#[test]
fn loaded_file_references_are_filled_in_without_dirtying_the_project() {
    let dir = Dir::new();
    let sample = dir.file("tone.wav");
    write_tone(&sample, 0.5, SR as u32, 440.0);
    let project = dir.file("set.rlive");
    let (_engine, mut s) = session();
    let t = audio_track(&mut s);
    s.set_clip(t, 0, Some(audio_clip(&sample, 4)));
    s.save(&project).unwrap();
    arrive(&mut s);

    let (_engine2, handle) = create_engine(SR, BLOCK);
    let mut fresh = Session::new(handle, Project::new_default(), PluginCache::default());
    fresh.load(&project).unwrap();
    arrive(&mut fresh);
    let track = fresh.project.track(t).unwrap();
    let Some(ClipContent::Audio { file, .. }) = track.clips[0].as_ref().map(|c| &c.content) else {
        panic!("an audio clip");
    };
    assert_eq!(file.size, Some(std::fs::metadata(&sample).unwrap().len()));
    let hash = file.hash.as_ref().unwrap();
    assert!(hash.starts_with("blake3:"), "{hash}");
    assert_eq!(
        fresh.samples.fingerprint(&sample).unwrap().hash,
        *hash,
        "the hash of the file"
    );
    assert!(!fresh.dirty, "learning a file's hash isn't an edit");
    assert!(fresh.history.undo_label().is_none(), "nor an undo step");
}

#[test]
fn a_clip_made_from_a_file_that_already_loaded_gets_its_size_and_hash_on_save() {
    let dir = Dir::new();
    let sample = dir.file("tone.wav");
    write_tone(&sample, 0.5, SR as u32, 440.0);
    let (_engine, mut s) = session();
    let t = audio_track(&mut s);
    s.set_clip(t, 0, Some(audio_clip(&sample, 4)));
    arrive(&mut s);
    let again = s.audio_clip_from_file(&sample, t).unwrap();
    s.set_clip(t, 1, Some(again));
    s.save(&dir.file("set.rlive")).unwrap();
    let Some(ClipContent::Audio { file, .. }) = s.clip(t, 1).map(|c| &c.content) else {
        panic!("an audio clip");
    };
    assert!(file.size.is_some() && file.hash.is_some());
}

#[test]
fn a_new_project_tries_files_that_failed_before_again() {
    let dir = Dir::new();
    let sample = dir.file("late.wav");
    let (_engine, mut s) = session();
    let t = audio_track(&mut s);
    s.set_clip(t, 0, Some(audio_clip(&sample, 4)));
    arrive(&mut s);
    assert!(matches!(
        s.samples.status(&sample),
        Some(SampleStatus::Failed(_))
    ));
    write_tone(&sample, 0.5, SR as u32, 440.0);
    let saved = dir.file("set.rlive");
    s.save(&saved).unwrap();
    s.load(&saved).unwrap();
    arrive(&mut s);
    assert!(s.samples.is_ready(&sample));
}

#[test]
fn a_device_gets_its_sample_when_it_arrives() {
    let dir = Dir::new();
    let sample = dir.file("tone.wav");
    write_tone(&sample, 2.0, SR as u32, 440.0);
    let project = dir.file("set.rlive");
    let (_engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let chain = ChainRef::Track(t);
    s.add_builtin(chain, "rlive.sampler");
    let dev = s.devices(chain)[1].id;
    s.set_device_sample(chain, dev, sample.clone());
    arrive(&mut s);
    s.save(&project).unwrap();

    let (mut engine, handle) = create_engine(SR, BLOCK);
    let mut fresh = Session::new(handle, Project::new_default(), PluginCache::default());
    let mut midi = fresh.engine.take_midi_sender().unwrap();
    fresh.load(&project).unwrap();
    let press = |midi: &mut rlive_engine::MidiSender| {
        assert!(midi.send(
            t,
            MidiMessage::NoteOn {
                channel: 0,
                key: 60,
                velocity: 1.0,
                note_id: 0,
                tuning: 0.0,
            }
        ));
    };
    press(&mut midi);
    assert_eq!(render_only(&mut engine, 8), 0.0, "no sample, no sound");
    fresh.samples.wait_idle();
    fresh.idle();
    press(&mut midi);
    assert!(render_only(&mut engine, 8) > 0.1);
}

#[test]
fn a_device_keeps_its_sample_while_the_next_one_loads() {
    let dir = Dir::new();
    let (a, b) = (dir.file("a.wav"), dir.file("b.wav"));
    write_tone(&a, 2.0, SR as u32, 440.0);
    write_tone(&b, 2.0, SR as u32, 880.0);
    let (_engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let chain = ChainRef::Track(t);
    s.add_builtin(chain, "rlive.sampler");
    let dev = s.devices(chain)[1].id;
    s.set_device_sample(chain, dev, a.clone());
    arrive(&mut s);
    assert_eq!(s.devices(chain)[1].sample(0).unwrap().path, a);

    s.set_device_sample(chain, dev, b.clone());
    assert_eq!(
        s.devices(chain)[1].sample(0).unwrap().path,
        a,
        "unchanged until the new one is ready"
    );
    arrive(&mut s);
    assert_eq!(s.devices(chain)[1].sample(0).unwrap().path, b);
}

#[test]
fn a_missing_file_is_reported_once_and_breaks_nothing() {
    let dir = Dir::new();
    let (mut engine, mut s) = session();
    let t = audio_track(&mut s);
    let gone = dir.file("gone.wav");
    s.arr_place(t, ArrangementClip::new(audio_clip(&gone, 8), 0));
    s.set_clip(t, 0, Some(audio_clip(&gone, 8)));
    arrive(&mut s);
    let about_it: Vec<_> = s
        .messages
        .iter()
        .filter(|m| m.contains("gone.wav"))
        .collect();
    assert_eq!(about_it.len(), 1, "{:?}", s.messages);
    s.play();
    s.launch_clip(t, 0);
    assert_eq!(render_only(&mut engine, 4), 0.0);
}

#[test]
fn an_undecodable_file_is_reported() {
    let dir = Dir::new();
    let junk = dir.file("junk.wav");
    std::fs::write(&junk, vec![7u8; 4096]).unwrap();
    let (_engine, mut s) = session();
    let t = audio_track(&mut s);
    s.set_clip(t, 0, Some(audio_clip(&junk, 4)));
    arrive(&mut s);
    assert!(s.messages.iter().any(|m| m.contains("junk.wav")));
    assert!(matches!(
        s.samples.status(&junk),
        Some(SampleStatus::Failed(_))
    ));
}

#[test]
fn a_clip_from_a_missing_file_is_refused_at_once() {
    let dir = Dir::new();
    let (_engine, mut s) = session();
    let t = audio_track(&mut s);
    assert!(s.audio_clip_from_file(&dir.file("nope.wav"), t).is_err());
}

#[test]
fn a_clip_length_comes_from_the_header_while_the_file_still_loads() {
    let dir = Dir::new();
    let path = dir.file("tone.wav");
    write_tone(&path, 2.0, SR as u32, 440.0);
    let (_engine, mut s) = session();
    let t = audio_track(&mut s);
    s.set_tempo(120.0);
    let clip = s.audio_clip_from_file(&path, t).unwrap();
    assert_eq!(clip.length, 4 * PPQ, "2 s at 120 BPM");
    assert_eq!(clip.name, "tone");
    assert!(s.samples.is_loading(&path), "it started loading");
    arrive(&mut s);
}

#[test]
fn an_export_waits_for_samples_that_are_still_loading() {
    let dir = Dir::new();
    let sample = dir.file("tone.wav");
    write_tone(&sample, 2.0, SR as u32, 440.0);
    let (_engine, mut s) = session();
    let t = audio_track(&mut s);
    s.arr_place(t, ArrangementClip::new(audio_clip(&sample, 4), 0));
    assert!(s.samples.is_loading(&sample));
    let out = dir.file("bounce.wav");
    s.export_arrangement(&out).unwrap();
    let rendered = rlive_devices::load_wav(&out).unwrap();
    assert!(
        rendered.left.iter().any(|x| x.abs() > 0.1),
        "the sample is in the export"
    );
}

#[test]
fn an_export_reports_a_sample_it_could_not_load() {
    let dir = Dir::new();
    let (_engine, mut s) = session();
    let t = audio_track(&mut s);
    s.arr_place(
        t,
        ArrangementClip::new(audio_clip(&dir.file("gone.wav"), 4), 0),
    );
    s.export_arrangement(&dir.file("bounce.wav")).unwrap();
    assert!(s.messages.iter().any(|m| m.contains("gone.wav")));
}

#[test]
fn a_clip_at_another_sample_rate_plays_at_its_recorded_pitch() {
    let dir = Dir::new();
    let path = dir.file("tone.wav");
    write_tone(&path, 2.0, 24_000, 1_000.0);
    let (mut engine, mut s) = session();
    let t = audio_track(&mut s);
    s.arr_place(t, ArrangementClip::new(audio_clip(&path, 2), 0));
    s.play();
    arrive(&mut s);
    let frames = BLOCK * 24;
    let (mut l, mut r) = (vec![0.0; frames], vec![0.0; frames]);
    for (a, b) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)) {
        engine.process_stereo(a, b);
    }
    let body = &l[BLOCK * 4..];
    assert!(rlive_testkit::tone_amplitude(body, 1_000.0, SR) > 0.3);
    assert!(rlive_testkit::tone_amplitude(body, 2_000.0, SR) < 0.02);
    assert!(rlive_testkit::tone_amplitude(body, 500.0, SR) < 0.02);
}

#[test]
fn a_compressed_sample_plays_through_the_whole_path() {
    let dir = Dir::new();
    let source = dir.file("tone.ogg");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../rlive-audio-io/tests/fixtures/stereo44.ogg"),
        &source,
    )
    .unwrap();
    let (mut engine, mut s) = session();
    s.samples.configure(rlive_audio_io::LoaderConfig {
        auto_warp_long: true,
        create_analysis_files: true,
        cache: rlive_audio_io::CacheConfig {
            folder: dir.file("cache"),
            minimum_free_space: 0,
            maximum_size: None,
        },
    });
    let t = audio_track(&mut s);
    s.arr_place(t, ArrangementClip::new(audio_clip(&source, 1), 0));
    s.play();
    arrive(&mut s);
    assert!(render_only(&mut engine, 4) > 0.1);
}

#[test]
fn creating_a_session_with_a_project_has_its_samples_ready() {
    let dir = Dir::new();
    let sample = dir.file("tone.wav");
    write_tone(&sample, 2.0, SR as u32, 440.0);
    let mut p = Project::empty();
    let mut track = rlive_project::Track::new("A", TrackKind::Audio, [1, 2, 3], p.scenes.len());
    track
        .arrangement
        .push(ArrangementClip::new(audio_clip(&sample, 8), 0));
    let id = track.id;
    p.tracks = vec![track];
    let (mut engine, handle) = create_engine(SR, BLOCK);
    let mut s = Session::new(handle, p, PluginCache::default());
    assert!(s.samples.is_ready(&sample));
    s.play();
    assert!(render_only(&mut engine, 8) > 0.1, "playable from the start");
    assert!(s.project.track(id).is_some());
}
