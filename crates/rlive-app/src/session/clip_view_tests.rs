//! Clip View editing, from the session's side: markers through the whole engine, Set during
//! playback, Reverse and Crop, multi-clip properties, and how edits reach a running clip.

use super::*;
use rlive_core::{Quantize, TimeSignature, PPQ};
use rlive_devices::devices::b1::common::SCALES;
use rlive_engine::{create_engine, Engine};
use rlive_project::clip_edit as edit;
use rlive_project::{
    ArrangementClip, Clip, ClipContent, ClipScale, LaunchMode, MidiProgram, NoteData, Track,
};

const SR: f64 = 48_000.0;
const BEAT_FRAMES: usize = 24_000;
const BEAT: u64 = PPQ;
const MAX_BLOCK: usize = 512;

struct Rig {
    engine: Engine,
    s: Session,
    dir: PathBuf,
}

impl Drop for Rig {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

/// Writes a file whose beats are constant levels: 0.1 for the first, 0.2 for the second and on.
fn stepped_file(dir: &Path, name: &str, beats: usize) -> PathBuf {
    let left: Vec<f32> = (0..beats * BEAT_FRAMES)
        .map(|i| (i / BEAT_FRAMES + 1) as f32 * 0.1)
        .collect();
    let path = dir.join(name);
    rlive_devices::write_wav(&path, &left, &left, SR as u32).unwrap();
    path
}

fn audio_clip(path: &Path, length: u64, looped: bool) -> Clip {
    Clip {
        name: "Steps".into(),
        color: [4, 5, 6],
        length,
        looped,
        content: ClipContent::audio(path, 0.0),
        ..Clip::default()
    }
}

impl Rig {
    fn new(name: &str, make: impl FnOnce(&Path, &mut Project)) -> Self {
        let dir =
            std::env::temp_dir().join(format!("rlive-clipview-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut p = Project::empty();
        p.tempo = 120.0;
        p.time_signature = TimeSignature::default();
        p.quantize = Quantize::None;
        make(&dir, &mut p);
        let (engine, handle) = create_engine(SR, MAX_BLOCK);
        let s = Session::new(handle, p, PluginCache::default());
        Self { engine, s, dir }
    }

    /// Renders `beats` of audio and returns the left channel.
    fn play(&mut self, beats: f64) -> Vec<f32> {
        let frames = (beats * BEAT_FRAMES as f64) as usize;
        let (mut l, mut r) = (vec![0.0f32; 256], vec![0.0f32; 256]);
        let mut left = Vec::with_capacity(frames);
        while left.len() < frames {
            let n = 256.min(frames - left.len());
            self.engine.process_stereo(&mut l[..n], &mut r[..n]);
            self.s.idle();
            left.extend_from_slice(&l[..n]);
        }
        left
    }

    fn track(&self) -> TrackId {
        self.s.project.tracks[0].id
    }

    fn slot(&self) -> ClipRef {
        ClipRef::slot(self.track(), 0)
    }

    fn clip(&self) -> &Clip {
        self.s.clip_at(self.slot()).unwrap()
    }

    fn file(&self) -> PathBuf {
        match &self.clip().content {
            ClipContent::Audio { file, .. } => file.path.clone(),
            ClipContent::Midi { .. } => panic!("an audio clip"),
        }
    }
}

fn audio_track(p: &mut Project, clip: Clip) {
    let scenes = p.scenes.len();
    let mut t = Track::new("Audio", TrackKind::Audio, [1, 2, 3], scenes);
    t.clips[0] = Some(clip);
    p.tracks = vec![t];
}

/// Looks at a rendered take halfway through each beat.
fn beats_of(left: &[f32]) -> Vec<f32> {
    (0..left.len() / BEAT_FRAMES)
        .map(|b| left[b * BEAT_FRAMES + BEAT_FRAMES / 2])
        .collect()
}

fn near(got: Vec<f32>, want: &[f32]) {
    assert_eq!(got.len(), want.len(), "{got:?}");
    for (g, w) in got.iter().zip(want) {
        assert!((g - w).abs() < 1e-2, "{got:?} vs {want:?}");
    }
}

fn midi_track(p: &mut Project, clip: Clip) {
    let scenes = p.scenes.len();
    let mut t = Track::new("Keys", TrackKind::Midi, [1, 2, 3], scenes);
    t.clips[0] = Some(clip);
    p.tracks = vec![t];
}

fn one_note_clip() -> Clip {
    let mut c = Clip::new_midi("Notes", [1, 1, 1], 1, TimeSignature::default());
    c.notes_mut().unwrap().push(NoteData::new(0, BEAT, 60, 0.8));
    c
}

#[test]
fn a_session_audio_clip_plays_from_its_start_marker_into_its_loop() {
    let mut r = Rig::new("session-audio", |dir, p| {
        let path = stepped_file(dir, "a.wav", 4);
        let mut c = audio_clip(&path, 2 * BEAT, true);
        c.start = BEAT;
        c.loop_start = 2 * BEAT;
        audio_track(p, c);
    });
    let t = r.track();
    r.s.launch_clip(t, 0);
    let left = r.play(6.0);
    near(beats_of(&left), &[0.2, 0.3, 0.4, 0.3, 0.4, 0.3]);
}

#[test]
fn an_arrangement_audio_clip_plays_from_its_start_marker_into_its_loop() {
    let mut r = Rig::new("arr-audio", |dir, p| {
        let path = stepped_file(dir, "a.wav", 4);
        let mut c = audio_clip(&path, 2 * BEAT, true);
        c.start = BEAT;
        c.loop_start = 2 * BEAT;
        let mut placed = ArrangementClip::new(c, BEAT);
        placed.length = 5 * BEAT;
        audio_track(p, audio_clip(&path, BEAT, true));
        p.tracks[0].clips[0] = None;
        p.tracks[0].arrangement.push(placed);
    });
    assert_eq!(r.s.project.tracks[0].arrangement[0].offset, BEAT);
    r.s.play();
    let left = r.play(6.0);
    near(beats_of(&left), &[0.0, 0.2, 0.3, 0.4, 0.3, 0.4]);
}

#[test]
fn an_unlooped_arrangement_clip_stops_at_its_end_marker() {
    let mut r = Rig::new("arr-end", |dir, p| {
        let path = stepped_file(dir, "a.wav", 4);
        let mut c = audio_clip(&path, 4 * BEAT, false);
        c.end = Some(2 * BEAT);
        let mut placed = ArrangementClip::new(c, 0);
        placed.length = 4 * BEAT;
        audio_track(p, audio_clip(&path, BEAT, true));
        p.tracks[0].clips[0] = None;
        p.tracks[0].arrangement.push(placed);
    });
    r.s.play();
    let left = r.play(3.0);
    near(beats_of(&left), &[0.1, 0.2, 0.0]);
}

#[test]
fn moving_a_timeline_clips_start_marker_moves_what_plays_at_its_left_edge() {
    let mut r = Rig::new("arr-start", |dir, p| {
        let path = stepped_file(dir, "a.wav", 4);
        let placed = ArrangementClip::new(audio_clip(&path, 4 * BEAT, false), 0);
        audio_track(p, audio_clip(&path, BEAT, true));
        p.tracks[0].clips[0] = None;
        p.tracks[0].arrangement.push(placed);
    });
    let t = r.track();
    let id = r.s.project.tracks[0].arrangement[0].id;
    let cref = ClipRef::Arr { track: t, id };
    r.s.edit_clips(&[cref], "Start", |c| edit::set_start(c, 2 * BEAT));
    let a = &r.s.project.tracks[0].arrangement[0];
    assert_eq!(
        (a.offset, a.clip.start, a.run_offset()),
        (2 * BEAT, 2 * BEAT, 0)
    );
    r.s.play();
    let left = r.play(2.0);
    near(beats_of(&left), &[0.3, 0.4]);
}

#[test]
fn a_midi_clips_markers_reach_the_engine() {
    let mut r = Rig::new("midi", |_, p| {
        let mut c = one_note_clip();
        c.start = BEAT;
        c.loop_start = 2 * BEAT;
        c.notes_mut()
            .unwrap()
            .push(NoteData::new(2 * BEAT, BEAT, 62, 0.8));
        midi_track(p, c);
    });
    let (mut b, project) = r.s.builder();
    let clip = project.tracks[0].clips[0].clone().unwrap();
    let data = b.clip(&clip).unwrap();
    let region = data.region();
    assert_eq!(region.start, BEAT as f64);
    assert_eq!(region.loop_start, 2.0 * BEAT as f64);
    let rlive_engine::ClipData::Midi(m) = data.content() else {
        panic!("midi");
    };
    assert_eq!(m.notes.len(), 2, "notes before the loop are kept");
}

#[test]
fn a_deactivated_clip_stays_out_of_the_engine() {
    let mut r = Rig::new("active", |dir, p| {
        let path = stepped_file(dir, "a.wav", 2);
        audio_track(p, audio_clip(&path, 2 * BEAT, true));
    });
    let t = r.track();
    let slot = r.slot();
    r.s.set_clips_active(&[slot], false);
    r.s.launch_clip(t, 0);
    r.s.play();
    let left = r.play(1.0);
    assert!(left.iter().all(|s| *s == 0.0));
    r.s.set_clips_active(&[slot], true);
    r.s.launch_clip(t, 0);
    let left = r.play(1.0);
    assert!(left.iter().any(|s| *s > 0.05));
}

#[test]
fn set_rounds_to_the_global_quantization_while_playing() {
    let mut r = Rig::new("set", |dir, p| {
        let path = stepped_file(dir, "a.wav", 4);
        let mut c = audio_clip(&path, 4 * BEAT, false);
        if let ClipContent::Audio { warp, .. } = &mut c.content {
            warp.enabled = true;
        }
        audio_track(p, c);
    });
    let t = r.track();
    r.s.launch_clip(t, 0);
    r.s.play();
    let slot = r.slot();
    assert_eq!(
        r.s.set_marker_at_playhead(slot, SetMarker::Start),
        None,
        "not playing yet"
    );
    r.play(1.3);
    r.s.set_quantize(Quantize::Beat);
    assert_eq!(
        r.s.set_marker_at_playhead(slot, SetMarker::Start),
        Some(BEAT)
    );
    assert_eq!(r.clip().start, BEAT);
    r.s.set_quantize(Quantize::None);
    let at = r.s.set_marker_at_playhead(slot, SetMarker::End).unwrap();
    assert!(at > BEAT && at < 2 * BEAT, "{at}");
    assert_eq!(r.clip().end, Some(at));
}

#[test]
fn the_loop_set_buttons_capture_a_loop_on_the_fly() {
    let mut r = Rig::new("loop-set", |_, p| {
        let mut c = one_note_clip();
        c.looped = false;
        c.length = 8 * BEAT;
        midi_track(p, c);
    });
    let t = r.track();
    r.s.launch_clip(t, 0);
    r.s.play();
    r.play(2.1);
    r.s.set_quantize(Quantize::Beat);
    let slot = r.slot();
    r.s.set_marker_at_playhead(slot, SetMarker::LoopPosition);
    assert!(r.clip().looped, "engages the loop");
    assert_eq!(r.clip().loop_start, 2 * BEAT);
    r.play(1.0);
    r.s.set_marker_at_playhead(slot, SetMarker::LoopLength);
    assert_eq!(r.clip().length, BEAT, "the loop now ends at the playhead");
}

#[test]
fn set_length_with_the_loop_off_keeps_its_length() {
    let mut r = Rig::new("loop-off", |_, p| {
        let mut c = one_note_clip();
        c.looped = false;
        c.length = 2 * BEAT;
        c.end = Some(8 * BEAT);
        midi_track(p, c);
    });
    let t = r.track();
    r.s.launch_clip(t, 0);
    r.s.play();
    r.play(3.0);
    r.s.set_quantize(Quantize::Beat);
    let slot = r.slot();
    r.s.set_marker_at_playhead(slot, SetMarker::LoopLength);
    let c = r.clip();
    assert!(c.looped);
    assert_eq!(c.length, 2 * BEAT, "its preset length");
    assert_eq!(c.loop_end(), 3 * BEAT);
}

#[test]
fn reversing_makes_a_file_and_going_back_costs_nothing() {
    let mut r = Rig::new("reverse", |dir, p| {
        let path = stepped_file(dir, "a.wav", 4);
        let mut c = audio_clip(&path, BEAT, true);
        c.start = BEAT;
        c.loop_start = BEAT;
        if let ClipContent::Audio { warp, .. } = &mut c.content {
            warp.enabled = true;
            warp.markers = vec![
                rlive_project::WarpMarker {
                    seconds: 0.0,
                    tick: 0.0,
                },
                rlive_project::WarpMarker {
                    seconds: 0.5,
                    tick: BEAT as f64,
                },
                rlive_project::WarpMarker {
                    seconds: 2.0,
                    tick: 4.0 * BEAT as f64,
                },
            ];
        }
        audio_track(p, c);
    });
    r.s.record.temp_folder = Some(r.dir.clone());
    let slot = r.slot();
    let original = r.file();
    assert!(r.s.reverse_clip(slot));
    assert!(r.s.clip_job_running());
    r.s.finish_clip_jobs();
    let reversed = r.file();
    assert_ne!(reversed, original);
    assert!(
        reversed.starts_with(r.dir.join("Processed").join("Reverse")),
        "{}",
        reversed.display()
    );
    let audio = rlive_audio_io::decode::decode(&reversed).unwrap();
    assert!(
        (audio.left[0] - 0.4).abs() < 1e-3,
        "the end is now the start"
    );
    assert!((audio.left[BEAT_FRAMES * 4 - 1] - 0.1).abs() < 1e-3);

    let c = r.clip().clone();
    let axis = 4 * BEAT;
    assert_eq!(c.loop_start, axis - 2 * BEAT, "the region is flipped");
    assert_eq!(c.start, axis - 2 * BEAT);
    assert!(matches!(
        c.content,
        ClipContent::Audio { reverse: true, .. }
    ));
    if let ClipContent::Audio { warp, .. } = &c.content {
        assert_eq!(
            warp.markers[1].tick,
            3.0 * BEAT as f64,
            "marker stays on its audio"
        );
        assert_eq!(warp.markers[1].seconds, 1.5);
    }

    // Instant the second time, in either direction.
    assert!(r.s.reverse_clip(slot));
    assert!(
        !r.s.clip_job_running(),
        "no job for a file already reversed"
    );
    assert_eq!(r.file(), original);
    assert_eq!(r.clip().start, BEAT, "the markers are back");
    assert!(r.s.reverse_clip(slot));
    assert!(!r.s.clip_job_running());
    assert_eq!(r.file(), reversed);

    assert!(r.s.undo());
    assert_eq!(r.file(), original);
}

#[test]
fn saving_moves_a_reversed_file_into_the_set() {
    let mut r = Rig::new("reverse-save", |dir, p| {
        let path = stepped_file(dir, "a.wav", 2);
        audio_track(p, audio_clip(&path, BEAT, true));
    });
    r.s.record.temp_folder = Some(r.dir.join("tmp"));
    let slot = r.slot();
    r.s.reverse_clip(slot);
    r.s.finish_clip_jobs();
    let set = r.dir.join("set").join("Test.rlive");
    std::fs::create_dir_all(set.parent().unwrap()).unwrap();
    r.s.save(&set).unwrap();
    let file = r.file();
    assert!(file.starts_with(
        r.dir
            .join("set")
            .join("Samples")
            .join("Processed")
            .join("Reverse")
    ));
    assert!(file.exists());
}

#[test]
fn crop_writes_what_plays_and_keeps_the_markers_on_the_audio() {
    let mut r = Rig::new("crop", |dir, p| {
        let path = stepped_file(dir, "a.wav", 4);
        let mut c = audio_clip(&path, BEAT, true);
        c.start = BEAT;
        c.loop_start = 2 * BEAT;
        audio_track(p, c);
    });
    r.s.record.temp_folder = Some(r.dir.clone());
    let slot = r.slot();
    assert!(r.s.crop_clip(slot, None));
    r.s.finish_clip_jobs();
    let file = r.file();
    assert!(file.starts_with(r.dir.join("Processed").join("Crop")));
    let audio = rlive_audio_io::decode::decode(&file).unwrap();
    assert_eq!(
        audio.frames(),
        2 * BEAT_FRAMES,
        "from the start marker to the loop end"
    );
    assert!((audio.left[0] - 0.2).abs() < 1e-3);
    let c = r.clip();
    assert_eq!((c.start, c.loop_start, c.length), (0, BEAT, BEAT));
    let t = r.track();
    r.s.launch_clip(t, 0);
    let left = r.play(3.0);
    near(beats_of(&left), &[0.2, 0.3, 0.3]);
}

#[test]
fn a_midi_crop_drops_what_is_outside_and_undoes() {
    let mut r = Rig::new("crop-midi", |_, p| {
        let mut c = one_note_clip();
        c.notes_mut()
            .unwrap()
            .push(NoteData::new(2 * BEAT, BEAT, 62, 0.8));
        c.loop_start = 2 * BEAT;
        c.start = 2 * BEAT;
        c.length = BEAT;
        midi_track(p, c);
    });
    r.s.commit_now();
    let slot = r.slot();
    assert!(r.s.crop_clip(slot, None));
    r.s.commit_now();
    let keys: Vec<(u64, u8)> = r.clip().notes().iter().map(|n| (n.start, n.key)).collect();
    assert_eq!(keys, vec![(0, 62)]);
    assert!(r.s.undo());
    assert_eq!(r.clip().notes().len(), 2);
}

fn two_clips(gains: [f32; 2], transposes: [i32; 2]) -> Rig {
    Rig::new("multi", move |dir, p| {
        let path = stepped_file(dir, "a.wav", 2);
        let scenes = p.scenes.len().max(2);
        p.scenes.resize(scenes, p.scenes[0].clone());
        let mut t = Track::new("Audio", TrackKind::Audio, [1, 2, 3], scenes);
        for (i, g) in gains.iter().enumerate() {
            let mut c = audio_clip(&path, BEAT, true);
            if let ClipContent::Audio {
                gain_db, transpose, ..
            } = &mut c.content
            {
                *gain_db = *g;
                *transpose = transposes[i];
            }
            t.clips[i] = Some(c);
        }
        p.tracks = vec![t];
    })
}

#[test]
fn multi_clip_properties_are_ranges_over_what_is_shared() {
    let r = two_clips([-3.0, -9.0], [2, 2]);
    let refs = [ClipRef::slot(r.track(), 0), ClipRef::slot(r.track(), 1)];
    let m = r.s.multi_clip(&refs);
    assert_eq!(m.count, 2);
    assert!(m.all_audio && !m.all_midi);
    assert_eq!(m.gain_db, Some((-9.0, -3.0)));
    assert_eq!(m.transpose, Some((2, 2)), "one value: they agree");
    assert_eq!(m.active, Some((true, true)));
}

#[test]
fn a_range_slider_collapses_at_its_extremes() {
    let mut r = two_clips([-3.0, -9.0], [0, 0]);
    let refs = [ClipRef::slot(r.track(), 0), ClipRef::slot(r.track(), 1)];
    r.s.shift_clip_gain(&refs, 6.0);
    assert_eq!(
        r.s.multi_clip(&refs).gain_db,
        Some((-3.0, 3.0)),
        "both moved"
    );
    r.s.shift_clip_gain(&refs, 100.0);
    assert_eq!(
        r.s.multi_clip(&refs).gain_db,
        Some((24.0, 24.0)),
        "now identical"
    );
}

#[test]
fn a_mixed_selection_only_shows_what_is_common() {
    let mut r = Rig::new("mixed", |dir, p| {
        let path = stepped_file(dir, "a.wav", 2);
        let scenes = p.scenes.len();
        let mut a = Track::new("Audio", TrackKind::Audio, [1, 2, 3], scenes);
        a.clips[0] = Some(audio_clip(&path, BEAT, true));
        let mut m = Track::new("Keys", TrackKind::Midi, [1, 2, 3], scenes);
        m.clips[0] = Some(one_note_clip());
        p.tracks = vec![a, m];
    });
    let refs: Vec<ClipRef> =
        r.s.project
            .tracks
            .iter()
            .map(|t| ClipRef::slot(t.id, 0))
            .collect();
    let m = r.s.multi_clip(&refs);
    assert!(!m.all_audio && !m.all_midi);
    assert_eq!(m.gain_db, None);
    r.s.color_clips(&refs, [9, 9, 9]);
    assert!(refs
        .iter()
        .all(|c| r.s.clip_at(*c).unwrap().color == [9, 9, 9]));
}

#[test]
fn a_clips_scale_reaches_what_the_engine_hands_to_devices() {
    let mut r = Rig::new("scale", |_, p| midi_track(p, one_note_clip()));
    let slot = r.slot();
    let scale = ClipScale {
        enabled: true,
        root: 2,
        name: SCALES[1].0.to_string(),
    };
    r.s.set_clip_scale(&[slot], scale);
    let clip = r.clip().clone();
    let (mut b, _) = r.s.builder();
    let info = b.clip(&clip).unwrap().scale().unwrap();
    assert_eq!((info.root, info.mask), (2, SCALES[1].1));
}

#[test]
fn clip_update_rate_holds_an_edit_until_its_grid() {
    let mut r = Rig::new("update", |dir, p| {
        let path = stepped_file(dir, "a.wav", 4);
        audio_track(p, audio_clip(&path, 4 * BEAT, true));
    });
    let t = r.track();
    r.s.set_clip_update_rate(BEAT);
    r.s.launch_clip(t, 0);
    r.s.play();
    r.play(0.25);
    let slot = r.slot();
    // Twice as loud, partway through the beat.
    r.s.edit_clips(&[slot], "Gain", |c| {
        if let ClipContent::Audio { gain_db, .. } = &mut c.content {
            *gain_db = 20.0 * 2.0f32.log10();
        }
    });
    let left = r.play(1.25);
    let at = |beats: f64| left[((beats - 0.25) * BEAT_FRAMES as f64) as usize];
    assert!((at(0.5) - 0.1).abs() < 1e-3, "not yet: {}", at(0.5));
    assert!((at(1.2) - 0.4).abs() < 1e-3, "after the grid: {}", at(1.2));
}

#[test]
fn clip_update_rate_of_none_applies_at_once() {
    let mut r = Rig::new("update-none", |dir, p| {
        let path = stepped_file(dir, "a.wav", 4);
        audio_track(p, audio_clip(&path, 4 * BEAT, true));
    });
    let t = r.track();
    r.s.launch_clip(t, 0);
    r.s.play();
    r.play(0.25);
    let slot = r.slot();
    r.s.edit_clips(&[slot], "Gain", |c| {
        if let ClipContent::Audio { gain_db, .. } = &mut c.content {
            *gain_db = 20.0 * 2.0f32.log10();
        }
    });
    let left = r.play(0.5);
    assert!((left[BEAT_FRAMES / 4] - 0.2).abs() < 1e-3);
}

#[test]
fn clip_update_rate_holds_an_edit_on_the_timeline_too() {
    let mut r = Rig::new("update-lane", |dir, p| {
        let path = stepped_file(dir, "a.wav", 4);
        let placed = ArrangementClip::new(audio_clip(&path, 4 * BEAT, false), 0);
        audio_track(p, audio_clip(&path, BEAT, true));
        p.tracks[0].clips[0] = None;
        p.tracks[0].arrangement.push(placed);
    });
    let t = r.track();
    let id = r.s.project.tracks[0].arrangement[0].id;
    r.s.set_clip_update_rate(BEAT);
    r.s.play();
    r.play(0.25);
    r.s.edit_clips(&[ClipRef::Arr { track: t, id }], "Gain", |c| {
        if let ClipContent::Audio { gain_db, .. } = &mut c.content {
            *gain_db = 20.0 * 2.0f32.log10();
        }
    });
    let left = r.play(1.25);
    let at = |beats: f64| left[((beats - 0.25) * BEAT_FRAMES as f64) as usize];
    assert!((at(0.5) - 0.1).abs() < 1e-3, "{}", at(0.5));
    assert!((at(1.2) - 0.4).abs() < 1e-3, "{}", at(1.2));
}

#[test]
fn every_property_edit_is_one_undo() {
    let mut r = Rig::new("undo", |_, p| midi_track(p, one_note_clip()));
    r.s.commit_now();
    let slot = r.slot();
    let before = r.clip().clone();
    r.s.set_marker(slot, SetMarker::Start, BEAT);
    r.s.commit_now();
    r.s.set_clip_signature(
        &[slot],
        Some(TimeSignature {
            numerator: 3,
            denominator: 4,
        }),
    );
    r.s.commit_now();
    r.s.set_clip_program(
        &[slot],
        MidiProgram {
            bank: 1,
            sub_bank: 2,
            program: 3,
        },
    );
    r.s.commit_now();
    r.s.set_clips_active(&[slot], false);
    r.s.commit_now();
    assert!(!r.clip().active);
    for _ in 0..4 {
        assert!(r.s.undo());
    }
    assert_eq!(r.clip(), &before);
    assert!(r.s.redo());
    assert_eq!(r.clip().start, BEAT);
}

#[test]
fn duplicate_loop_is_undoable_and_shifts_following_notes() {
    let mut r = Rig::new("dup", |_, p| {
        let mut c = one_note_clip();
        c.length = BEAT;
        c.notes_mut()
            .unwrap()
            .push(NoteData::new(BEAT, BEAT, 70, 0.8));
        midi_track(p, c);
    });
    r.s.commit_now();
    let slot = r.slot();
    assert!(r.s.duplicate_loop(slot));
    r.s.commit_now();
    assert_eq!(r.clip().length, 2 * BEAT);
    let mut at: Vec<u64> = r.clip().notes().iter().map(|n| n.start).collect();
    at.sort_unstable();
    assert_eq!(at, vec![0, BEAT, 2 * BEAT], "the following note moved on");
    assert!(r.s.undo());
    assert_eq!(r.clip().length, BEAT);
}

#[test]
fn replacing_a_sample_keeps_markers_only_for_the_same_length() {
    let mut r = Rig::new("replace", |dir, p| {
        let path = stepped_file(dir, "a.wav", 4);
        let mut c = audio_clip(&path, 4 * BEAT, true);
        if let ClipContent::Audio { warp, gain_db, .. } = &mut c.content {
            warp.enabled = true;
            *gain_db = -5.0;
            warp.markers = rlive_project::warp_edit::loop_markers(2.0, 1, 4 * BEAT);
        }
        audio_track(p, c);
        stepped_file(dir, "same.wav", 4);
        stepped_file(dir, "other.wav", 3);
    });
    let slot = r.slot();
    let markers = |r: &Rig| match &r.clip().content {
        ClipContent::Audio { warp, .. } => warp.markers.clone(),
        ClipContent::Midi { .. } => Vec::new(),
    };
    let before = markers(&r);
    let same = r.dir.join("same.wav");
    assert!(r.s.replace_clip_sample(slot, &same));
    assert_eq!(markers(&r), before, "same length: kept");
    let other = r.dir.join("other.wav");
    assert!(r.s.replace_clip_sample(slot, &other));
    assert_ne!(markers(&r), before, "different length: gone");
    assert!(matches!(
        r.clip().content,
        ClipContent::Audio { gain_db, .. } if gain_db == -5.0
    ));
}

#[test]
fn an_external_edit_needs_playback_stopped_and_reloads_the_file() {
    let mut r = Rig::new("external", |dir, p| {
        let path = stepped_file(dir, "a.wav", 2);
        let mut c = audio_clip(&path, 2 * BEAT, true);
        if let ClipContent::Audio { warp, .. } = &mut c.content {
            warp.enabled = true;
            warp.markers = rlive_project::warp_edit::loop_markers(1.0, 1, 4 * BEAT);
        }
        audio_track(p, c);
    });
    let slot = r.slot();
    r.s.play();
    r.play(0.1);
    assert!(
        r.s.edit_sample_externally(slot).is_err(),
        "not while playing"
    );
    r.s.stop();
    r.play(0.05);
    assert!(
        r.s.edit_sample_externally(slot).is_err(),
        "no editor chosen"
    );
    let file = r.file();
    r.s.watch_external(&file);
    // The same length: the markers stay.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    stepped_file(&r.dir, "a.wav", 2);
    let settle = |r: &mut Rig| {
        r.s.reload_changed_files();
        r.s.samples.wait_for([r.file().as_path()]);
        r.s.idle();
    };
    settle(&mut r);
    let has_markers = |r: &Rig| {
        matches!(&r.clip().content,
        ClipContent::Audio { warp, .. } if !warp.markers.is_empty())
    };
    assert!(has_markers(&r));
    std::thread::sleep(std::time::Duration::from_millis(1100));
    stepped_file(&r.dir, "a.wav", 3);
    settle(&mut r);
    assert!(!has_markers(&r), "a different length loses them");
    assert_eq!(
        r.s.samples.get(&file).unwrap().len(),
        3 * BEAT_FRAMES,
        "the new audio is what plays"
    );
}

#[test]
fn a_new_clip_takes_the_launch_mode_from_the_settings() {
    let mut r = Rig::new("defaults", |dir, p| {
        stepped_file(dir, "a.wav", 2);
        midi_track(p, one_note_clip());
    });
    r.s.launch_defaults.mode = LaunchMode::Toggle;
    let sig = r.s.project.time_signature;
    assert_eq!(
        r.s.new_midi_clip([1, 1, 1], sig).launch.mode,
        LaunchMode::Toggle
    );
    r.s.clip_defaults.create_fades = false;
    let t = r.track();
    let c = r.s.audio_clip_from_file(&r.dir.join("a.wav"), t).unwrap();
    assert_eq!(c.launch.mode, LaunchMode::Toggle);
    assert!(matches!(
        c.content,
        ClipContent::Audio {
            edge_fade: false,
            ..
        }
    ));
}

#[test]
fn moving_the_brace_auditions_an_earlier_pass_of_a_loop_recording() {
    let mut r = Rig::new("passes", |dir, p| {
        // Three passes of a beat each, the last one on the timeline.
        let path = stepped_file(dir, "take.wav", 3);
        let mut c = audio_clip(&path, BEAT, false);
        c.start = 2 * BEAT;
        c.loop_start = 2 * BEAT;
        if let ClipContent::Audio { warp, .. } = &mut c.content {
            warp.enabled = true;
            warp.markers = rlive_project::warp_edit::loop_markers(1.5, 3, BEAT);
        }
        let placed = ArrangementClip {
            id: rlive_core::ArrClipId::new(),
            start: 0,
            length: BEAT,
            offset: 2 * BEAT,
            clip: c,
        };
        audio_track(p, audio_clip(&path, BEAT, true));
        p.tracks[0].clips[0] = None;
        p.tracks[0].arrangement.push(placed);
    });
    r.s.play();
    near(beats_of(&r.play(1.0)), &[0.3]);
    r.s.stop();
    r.play(0.05);
    let t = r.track();
    let id = r.s.project.tracks[0].arrangement[0].id;
    let cref = ClipRef::Arr { track: t, id };
    r.s.edit_clips(&[cref], "Move Loop", |c| edit::move_loop(c, -(BEAT as i64)));
    let a = &r.s.project.tracks[0].arrangement[0];
    assert_eq!(a.clip.loop_start, BEAT);
    assert_eq!(a.offset, BEAT, "the region went with it");
    r.s.seek(0.0);
    r.s.play();
    near(beats_of(&r.play(1.0)), &[0.2]);
}

#[test]
fn session_record_records_automation_into_playing_clips_when_armed() {
    let mut r = Rig::new("session-record", |_, p| midi_track(p, one_note_clip()));
    let t = r.track();
    r.s.launch_clip(t, 0);
    r.play(0.2);
    // Nothing armed: Session Record has no automation to start.
    r.s.set_automation_arm(true);
    r.s.project.track_mut(t).unwrap().arm = true;
    r.s.session_record(0);
    assert!(r.s.session_automation_record());
    r.play(0.05);
    // Again: stops, and leaves it playing.
    r.s.session_record(0);
    assert!(!r.s.session_automation_record());
    r.play(0.1);
    assert_eq!(
        r.s.track_shared[&t]
            .playing_slot
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    // Stop ends it too.
    r.s.session_record(0);
    assert!(r.s.session_automation_record());
    r.s.stop();
    assert!(!r.s.session_automation_record());
}

#[test]
fn session_record_without_automation_arm_keeps_its_old_jobs() {
    let mut r = Rig::new("session-record-plain", |_, p| {
        midi_track(p, one_note_clip())
    });
    let t = r.track();
    r.s.launch_clip(t, 0);
    r.play(0.2);
    r.s.project.track_mut(t).unwrap().arm = true;
    r.s.session_record(0);
    assert!(!r.s.session_automation_record());
    r.play(0.05);
    assert!(r.s.is_overdubbing(t), "it overdubs, as before");
}

#[test]
fn hot_swapping_a_groove_replaces_it_in_the_pool_and_for_every_clip() {
    use rlive_core::GrooveId;
    let mut r = Rig::new("groove", |_, p| {
        midi_track(p, one_note_clip());
        let scenes = p.scenes.len();
        p.tracks[0].clips.resize(scenes.max(2), None);
    });
    let groove = |name: &str| rlive_project::Groove {
        id: GrooveId::new(),
        name: name.to_string(),
        file: None,
        base: 240,
        quantize: 0.0,
        timing: 0.0,
        random: 0.0,
        velocity: 0.0,
        pattern: Default::default(),
    };
    let slot = r.slot();
    let first = groove("Swing");
    let first_id = first.id;
    r.s.project.groove_pool.grooves.push(first);
    assert!(r.s.set_clip_groove(&[slot], Some(first_id)));
    let second = groove("Shuffle");
    let second_id = second.id;
    assert!(r.s.hot_swap_groove(slot, second));
    assert_eq!(r.clip().groove, Some(second_id));
    assert_eq!(r.s.project.groove_pool.grooves.len(), 1);
    assert_eq!(r.s.project.groove_pool.grooves[0].name, "Shuffle");
}

#[test]
fn hi_q_only_matters_when_the_file_needs_resampling() {
    let mut r = Rig::new("hiq", |dir, p| {
        stepped_file(dir, "a.wav", 1);
        midi_track(p, one_note_clip());
    });
    let native = r.dir.join("a.wav");
    let other = r.dir.join("b.wav");
    rlive_devices::write_wav(&other, &[0.5; 4410], &[0.5; 4410], 44_100).unwrap();
    r.s.samples.wait_for([native.as_path(), other.as_path()]);
    let (mut b, _) = r.s.builder();
    let clip = |p: &Path, hi_q: bool| {
        let mut c = audio_clip(p, BEAT, true);
        if let ClipContent::Audio { hi_q: h, .. } = &mut c.content {
            *h = hi_q;
        }
        c
    };
    assert!(
        !b.clip(&clip(&native, true)).unwrap().hi_q(),
        "nothing to resample"
    );
    assert!(b.clip(&clip(&other, true)).unwrap().hi_q());
    assert!(!b.clip(&clip(&other, false)).unwrap().hi_q());
}

#[test]
fn a_session_clip_gets_an_edge_fade_and_a_timeline_clip_does_not() {
    let mut r = Rig::new("fade", |dir, p| {
        let path = stepped_file(dir, "a.wav", 4);
        let mut c = audio_clip(&path, 4 * BEAT, true);
        c.start = BEAT;
        audio_track(p, c);
    });
    let clip = r.clip().clone();
    let (mut b, _) = r.s.builder();
    assert!(b.clip(&clip).unwrap().edge_fade());
    let lane = b.lane(&[ArrangementClip::new(clip, 0)]);
    assert!(!lane.clips[0].data.edge_fade());
}

#[test]
fn assigning_the_track_colour_only_touches_that_views_clips() {
    let mut r = Rig::new("colour", |dir, p| {
        let path = stepped_file(dir, "a.wav", 2);
        let scenes = p.scenes.len();
        let mut t = Track::new("Audio", TrackKind::Audio, [10, 20, 30], scenes);
        let mut session_clip = audio_clip(&path, BEAT, true);
        session_clip.color = [1, 1, 1];
        t.clips[0] = Some(session_clip);
        let mut timeline = audio_clip(&path, BEAT, true);
        timeline.color = [2, 2, 2];
        t.arrangement.push(ArrangementClip::new(timeline, 0));
        p.tracks = vec![t];
    });
    let t = r.track();
    assert!(r.s.assign_track_color(t, false));
    assert_eq!(r.clip().color, [10, 20, 30]);
    assert_eq!(r.s.project.tracks[0].arrangement[0].clip.color, [2, 2, 2]);
    assert!(r.s.assign_track_color(t, true));
    assert_eq!(
        r.s.project.tracks[0].arrangement[0].clip.color,
        [10, 20, 30]
    );
}

#[test]
fn unwarped_audio_cannot_be_looped_but_warped_audio_can() {
    let mut r = Rig::new("loop-warp", |dir, p| {
        let path = stepped_file(dir, "a.wav", 2);
        audio_track(p, audio_clip(&path, BEAT, false));
    });
    let slot = r.slot();
    assert!(!r.s.set_clip_looped(&[slot], true));
    assert!(!r.clip().looped);
    r.s.set_warp_enabled(&[slot], true);
    assert!(r.s.set_clip_looped(&[slot], true));
    assert!(r.clip().looped);
}

#[test]
fn an_audio_clips_scale_reaches_devices_too() {
    let mut r = Rig::new("scale-audio", |dir, p| {
        let path = stepped_file(dir, "a.wav", 1);
        audio_track(p, audio_clip(&path, BEAT, true));
    });
    let slot = r.slot();
    r.s.set_clip_scale(
        &[slot],
        ClipScale {
            enabled: true,
            root: 7,
            name: "Major".into(),
        },
    );
    let clip = r.clip().clone();
    let (mut b, _) = r.s.builder();
    assert_eq!(b.clip(&clip).unwrap().scale().unwrap().root, 7);
}

#[test]
fn a_split_and_time_commands_keep_what_a_marked_clip_plays() {
    let mut r = Rig::new("split", |dir, p| {
        let path = stepped_file(dir, "a.wav", 4);
        let mut c = audio_clip(&path, 2 * BEAT, true);
        c.start = BEAT;
        c.loop_start = 2 * BEAT;
        let mut placed = ArrangementClip::new(c, 0);
        placed.length = 5 * BEAT;
        audio_track(p, audio_clip(&path, BEAT, true));
        p.tracks[0].clips[0] = None;
        p.tracks[0].arrangement.push(placed);
    });
    let want = [0.2, 0.3, 0.4, 0.3, 0.4];
    r.s.play();
    near(beats_of(&r.play(5.0)), &want);
    r.s.stop();
    r.play(0.05);
    let t = r.track();
    // One cut in the run-in, one in the loop.
    r.s.arr_split(&[t], &[BEAT, 3 * BEAT]);
    assert_eq!(r.s.project.tracks[0].arrangement.len(), 3);
    r.s.seek(0.0);
    r.s.play();
    near(beats_of(&r.play(5.0)), &want);
    r.s.stop();
    r.play(0.05);
    r.s.insert_silence(0, BEAT);
    r.s.seek(0.0);
    r.s.play();
    near(beats_of(&r.play(6.0)), &[0.0, 0.2, 0.3, 0.4, 0.3, 0.4]);
}

#[test]
fn moving_an_end_marker_limits_a_crossfade_into_the_next_clip() {
    let mut r = Rig::new("settle", |dir, p| {
        let path = stepped_file(dir, "a.wav", 4);
        let a = ArrangementClip {
            length: 2 * BEAT,
            ..ArrangementClip::new(audio_clip(&path, 4 * BEAT, false), 0)
        };
        let b = ArrangementClip {
            offset: BEAT,
            length: 2 * BEAT,
            ..ArrangementClip::new(audio_clip(&path, 4 * BEAT, false), 2 * BEAT)
        };
        audio_track(p, audio_clip(&path, BEAT, true));
        p.tracks[0].clips[0] = None;
        p.tracks[0].arrangement.extend([a, b]);
    });
    let t = r.track();
    let (a, b) = {
        let lane = &r.s.project.tracks[0].arrangement;
        (lane[0].id, lane[1].id)
    };
    r.s.arr_crossfade(t, a, BEAT / 2, BEAT / 2, 0.0);
    let fades = r.s.project.tracks[0].arrangement[0].clip.fades;
    assert_eq!(fades.out_tail, BEAT / 2);
    let _ = b;
    // Nothing plays past the end marker any more, so there's nothing to crossfade with.
    r.s.edit_clips(&[ClipRef::Arr { track: t, id: a }], "End", |c| {
        edit::set_end(c, 2 * BEAT);
    });
    let lane = &r.s.project.tracks[0].arrangement;
    assert_eq!(lane[0].clip.fades.out_tail, 0);
    assert_eq!(lane[1].clip.fades.fade_in, 0, "its other half goes with it");
}

fn scene_rig(name: &str, make: impl FnOnce(&Path, &mut Vec<Option<Clip>>) + 'static) -> Rig {
    Rig::new(name, move |dir, p| {
        let scenes = p.scenes.len().max(2);
        p.scenes.resize(scenes, p.scenes[0].clone());
        let mut t = Track::new("Audio", TrackKind::Audio, [1, 2, 3], scenes);
        make(dir, &mut t.clips);
        p.tracks = vec![t];
    })
}

/// A warped loop of a four-beat file, with Warp's own markers at its ends.
fn warped_loop(path: &Path) -> Clip {
    let mut c = audio_clip(path, 4 * BEAT, true);
    if let ClipContent::Audio { warp, .. } = &mut c.content {
        warp.enabled = true;
        warp.markers = rlive_project::warp_edit::loop_markers(2.0, 1, 4 * BEAT);
    }
    c
}

#[test]
fn a_clip_with_a_moved_start_marker_takes_over_in_place_with_legato() {
    let mut r = scene_rig("legato", |dir, clips| {
        let a = stepped_file(dir, "a.wav", 4);
        let b = dir.join("b.wav");
        let left: Vec<f32> = (0..4 * BEAT_FRAMES)
            .map(|i| (i / BEAT_FRAMES + 6) as f32 * 0.1)
            .collect();
        rlive_devices::write_wav(&b, &left, &left, SR as u32).unwrap();
        clips[0] = Some(warped_loop(&a));
        let mut moved = warped_loop(&b);
        moved.start = BEAT;
        moved.launch.legato = true;
        clips[1] = Some(moved);
    });
    let t = r.track();
    r.s.launch_clip(t, 0);
    r.play(2.25);
    r.s.launch_clip(t, 1);
    let left = r.play(2.5);
    let at = |beat: f64| left[((beat - 2.25) * BEAT_FRAMES as f64) as usize];
    // Still the third beat of the content, not its start marker's.
    assert!((at(2.5) - 0.8).abs() < 0.02, "{}", at(2.5));
    assert!((at(3.5) - 0.9).abs() < 0.02, "{}", at(3.5));
    // Round the loop from its own start.
    assert!((at(4.5) - 0.6).abs() < 0.02, "{}", at(4.5));
}

#[test]
fn a_linked_follow_action_fires_when_a_moved_clip_has_played_its_first_pass() {
    use rlive_core::{FollowAction, FollowActions};
    let mut r = scene_rig("follow", |dir, clips| {
        let a = stepped_file(dir, "a.wav", 4);
        let mut first = warped_loop(&a);
        first.start = BEAT;
        first.launch.follow = FollowActions {
            enabled: true,
            action_a: FollowAction::Next,
            chance_a: 1.0,
            chance_b: 0.0,
            linked: true,
            multiplier: 1,
            ..FollowActions::default()
        };
        clips[0] = Some(first);
        let mut second = warped_loop(&a);
        second.start = 2 * BEAT;
        clips[1] = Some(second);
    });
    let t = r.track();
    r.s.launch_clip(t, 0);
    let left = r.play(5.0);
    // Three beats of the first, from its marker, then the second from its own.
    near(beats_of(&left), &[0.2, 0.3, 0.4, 0.3, 0.4]);
}
