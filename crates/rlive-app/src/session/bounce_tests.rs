//! Bounce, Freeze, Flatten and Consolidate: what ends up in the files and on the tracks.

use super::render::{from_tick, launch_clip, Render, Rendered};
use super::*;
use rlive_core::{ClipParam, EnvTarget, PPQ};
use rlive_engine::{create_engine, Engine};
use rlive_project::{
    ArrangementClip, Clip, ClipContent, ClipEnvelope, Envelope, NoteData, Route, TakeLane, Warp,
    SEND_OFF_DB,
};
use rlive_testkit::assert_no_alloc;
use std::sync::atomic::{AtomicUsize, Ordering};

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;
const COMPRESSOR_LOOKAHEAD: u32 = 9;
/// Exactly one beat at 120 BPM.
const BEAT_FRAMES: usize = 24_000;

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Dir(PathBuf);

impl Dir {
    fn new() -> Self {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("rlive-bounce-{}-{n}", std::process::id()));
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

/// A sample that is different everywhere, so anything out of place shows up.
fn write_source(path: &Path, seconds: f64) {
    let n = (seconds * SR) as usize;
    let left: Vec<f32> = (0..n)
        .map(|i| {
            let t = i as f32 / SR as f32;
            0.4 * (t * 311.0 * std::f32::consts::TAU).sin()
                + 0.2 * (t * 977.0 * std::f32::consts::TAU).sin()
        })
        .collect();
    let right: Vec<f32> = left.iter().map(|s| s * 0.75).collect();
    rlive_devices::write_wav(path, &left, &right, SR as u32).unwrap();
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

fn empty_session() -> (Engine, Session) {
    let (engine, handle) = create_engine(SR, BLOCK);
    let s = Session::new(handle, Project::empty(), PluginCache::default());
    (engine, s)
}

fn audio_track(s: &mut Session, name: &str, color: [u8; 3]) -> TrackId {
    let id = s.add_track(TrackKind::Audio, None);
    if let Some(t) = s.project.track_mut(id) {
        t.name = name.into();
        t.color = color;
    }
    id
}

/// Drains what the session sent, so what's rendered next sees it.
fn settle(engine: &mut Engine, s: &mut Session) {
    let (mut l, mut r) = ([0.0; BLOCK], [0.0; BLOCK]);
    for _ in 0..3 {
        s.idle();
        engine.process_stereo(&mut l, &mut r);
    }
}

/// The Set as it would come out of the Main output.
fn mix(s: &mut Session, start: u64, ticks: u64) -> Rendered {
    s.render_now(Render {
        project: s.project.clone(),
        setup: from_tick(start),
        start,
        ticks,
        tail: false,
        rate: None,
    })
    .unwrap()
}

fn launched(s: &mut Session, track: TrackId, slot: usize, ticks: u64) -> Rendered {
    s.render_now(Render {
        project: s.project.clone(),
        setup: launch_clip(track, slot),
        start: 0,
        ticks,
        tail: false,
        rate: None,
    })
    .unwrap()
}

fn peak(v: &[f32]) -> f32 {
    v.iter().fold(0.0f32, |a, x| a.max(x.abs()))
}

fn loudest_difference(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "the same length");
    a.iter()
        .zip(b)
        .fold(0.0f32, |m, (x, y)| m.max((x - y).abs()))
}

/// [`loudest_difference`] leaving out the anti-click ramps every clip plays at its edges, which a
/// clip of the bounced audio plays again (manual p. 995).
fn away_from_edges(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    loudest_difference(&a[128..n - 128], &b[128..n - 128])
}

/// Every sample equal, down to the bit.
fn assert_same(a: &[f32], b: &[f32]) {
    assert_eq!(a.len(), b.len(), "the same length");
    let at = a
        .iter()
        .zip(b)
        .position(|(x, y)| x.to_bits() != y.to_bits());
    assert_eq!(
        at.map(|i| (i, a[i], b[i])),
        None,
        "the first sample that differs"
    );
}

fn clip_file(clip: &Clip) -> PathBuf {
    match &clip.content {
        ClipContent::Audio { file, .. } => file.path.clone(),
        ClipContent::Midi { .. } => panic!("expected audio"),
    }
}

fn delay(s: &mut Session, id: TrackId) {
    s.add_builtin(ChainRef::Track(id), "rlive.delay");
}

/// A compressor with its 10 ms of lookahead, which delays what comes after it.
fn latent(s: &mut Session, id: TrackId) -> DeviceId {
    s.add_builtin(ChainRef::Track(id), "rlive.compressor");
    let d = s.devices(ChainRef::Track(id)).last().unwrap().id;
    s.set_param(ChainRef::Track(id), d, COMPRESSOR_LOOKAHEAD, 2.0);
    d
}

fn with_clip(s: &mut Session, dir: &Dir, id: TrackId, start_beat: u64, beats: u64) {
    let source = dir.file("source.wav");
    if !source.exists() {
        write_source(&source, 16.0);
    }
    s.samples.wait_for([source.as_path()]);
    s.arr_place(
        id,
        ArrangementClip::new(audio_clip(&source, beats), start_beat * PPQ),
    );
}

// ------------------------------------------------------------- freeze

#[test]
fn a_frozen_track_sounds_identical_bit_for_bit_with_its_latency() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let frozen = audio_track(&mut s, "Frozen", [1, 2, 3]);
    let other = audio_track(&mut s, "Other", [3, 2, 1]);
    delay(&mut s, frozen);
    latent(&mut s, frozen);
    with_clip(&mut s, &dir, frozen, 0, 2);
    with_clip(&mut s, &dir, frozen, 4, 2);
    with_clip(&mut s, &dir, other, 0, 4);
    settle(&mut engine, &mut s);
    let ticks = 8 * PPQ;
    let before = mix(&mut s, 0, ticks);
    assert!(
        s.measure_timing().own_latency(0) > 0,
        "the device is latent"
    );

    s.freeze_track(frozen).unwrap();
    s.finish_render().unwrap();
    settle(&mut engine, &mut s);
    assert_eq!(
        s.measure_timing().own_latency(0),
        0,
        "frozen tracks are on time"
    );
    let after = mix(&mut s, 0, ticks);
    assert_eq!(before.frames(), after.frames());
    assert_same(&before.left, &after.left);
    assert_same(&before.right, &after.right);
}

#[test]
fn a_frozen_track_plays_on_the_live_engine_without_its_devices() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let id = audio_track(&mut s, "Frozen", [1, 2, 3]);
    latent(&mut s, id);
    with_clip(&mut s, &dir, id, 0, 4);
    s.freeze_track(id).unwrap();
    s.finish_render().unwrap();
    settle(&mut engine, &mut s);
    s.seek(0.0);
    s.play();
    settle(&mut engine, &mut s);
    let (mut l, mut r) = ([0.0; BLOCK], [0.0; BLOCK]);
    for _ in 0..20 {
        engine.process_stereo(&mut l, &mut r);
    }
    assert!(peak(&l) > 0.05, "the frozen clip plays");
}

#[test]
fn swapping_to_and_from_frozen_never_allocates_on_the_audio_thread() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let id = audio_track(&mut s, "T", [1, 2, 3]);
    delay(&mut s, id);
    latent(&mut s, id);
    with_clip(&mut s, &dir, id, 0, 4);
    s.set_clip(id, 0, Some(audio_clip(&dir.file("source.wav"), 2)));
    s.seek(0.0);
    s.play();
    let (mut l, mut r) = ([0.0; BLOCK], [0.0; BLOCK]);
    for _ in 0..10 {
        s.idle();
        engine.process_stereo(&mut l, &mut r);
    }
    s.freeze_track(id).unwrap();
    s.finish_render().unwrap();
    for _ in 0..3 {
        assert_no_alloc(|| engine.process_stereo(&mut l, &mut r));
    }
    assert!(s.is_frozen(id));
    s.unfreeze_track(id);
    for _ in 0..3 {
        assert_no_alloc(|| engine.process_stereo(&mut l, &mut r));
    }
    assert!(!s.is_frozen(id));
    s.idle();
    for _ in 0..3 {
        assert_no_alloc(|| engine.process_stereo(&mut l, &mut r));
    }
}

#[test]
fn frozen_session_clips_are_two_loop_cycles_and_sound_like_the_clip() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let id = audio_track(&mut s, "T", [1, 2, 3]);
    delay(&mut s, id);
    let source = dir.file("source.wav");
    write_source(&source, 8.0);
    s.samples.wait_for([source.as_path()]);
    s.set_clip(id, 0, Some(audio_clip(&source, 2)));
    settle(&mut engine, &mut s);
    let ticks = 4 * PPQ;
    let before = launched(&mut s, id, 0, ticks);

    s.freeze_track(id).unwrap();
    s.finish_render().unwrap();
    let clip = s.clip(id, 0).unwrap();
    let frozen = clip.frozen.as_ref().unwrap();
    assert_eq!(frozen.length, 4 * PPQ, "two cycles");
    assert!(frozen.looped);
    let file = rlive_audio_io::decode(&frozen.file.path).unwrap();
    assert_eq!(
        file.frames(),
        4 * BEAT_FRAMES,
        "exactly two cycles of frames"
    );
    let after = launched(&mut s, id, 0, ticks);
    assert_same(&before.left, &after.left);
    assert_same(&before.right, &after.right);
}

#[test]
fn session_freeze_holds_parameters_where_automation_starts() {
    let (_engine, mut s) = empty_session();
    let id = s.add_track(TrackKind::Midi, None);
    let device = s.devices(ChainRef::Track(id))[0].id;
    let target = EnvTarget::Param(rlive_core::ParamRef {
        owner: rlive_core::ParamOwner::Device(device),
        id: rlive_core::ParamId(0),
    });
    let t = s.project.track_mut(id).unwrap();
    t.automation.push(Envelope {
        target,
        points: vec![
            rlive_project::Breakpoint::new(0.0, 0.25),
            rlive_project::Breakpoint::new(960.0, 0.75),
        ],
    });
    let mut v = s.project.clone();
    super::render::snapshot_parameters(&mut v);
    let t = v.track(id).unwrap();
    assert!(t.automation.is_empty());
    assert_eq!(t.devices[0].params.get(&0), Some(&0.25));
}

#[test]
fn an_arrangement_freeze_keeps_tails_as_temporary_clips() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let id = audio_track(&mut s, "T", [1, 2, 3]);
    delay(&mut s, id);
    with_clip(&mut s, &dir, id, 0, 1);
    with_clip(&mut s, &dir, id, 8, 1);
    s.freeze_track(id).unwrap();
    s.finish_render().unwrap();
    settle(&mut engine, &mut s);
    let t = s.project.track(id).unwrap();
    let tails: Vec<&ArrangementClip> = t
        .arrangement
        .iter()
        .filter(|c| c.clip.frozen_tail)
        .collect();
    assert_eq!(tails.len(), 2, "a tail after each clip with sound past it");
    assert_eq!(tails[0].start, PPQ, "the tail starts where the clip ends");
    assert!(tails[1].start >= 9 * PPQ);
    assert!(t.arrangement.iter().all(|c| c.clip.frozen.is_some()));

    s.unfreeze_track(id);
    let t = s.project.track(id).unwrap();
    assert_eq!(t.arrangement.len(), 2, "the tails disappear");
    assert!(t.arrangement.iter().all(|c| c.clip.frozen.is_none()));
}

#[test]
fn unfreezing_restores_devices_and_clips_exactly() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let id = audio_track(&mut s, "T", [1, 2, 3]);
    delay(&mut s, id);
    latent(&mut s, id);
    with_clip(&mut s, &dir, id, 0, 2);
    s.set_clip(id, 0, Some(audio_clip(&dir.file("source.wav"), 2)));
    settle(&mut engine, &mut s);
    let before = s.project.clone();
    let heard = mix(&mut s, 0, 4 * PPQ);
    s.freeze_track(id).unwrap();
    s.finish_render().unwrap();
    assert_ne!(s.project, before);
    s.unfreeze_track(id);
    settle(&mut engine, &mut s);
    assert_eq!(s.project, before, "exactly what it was");
    let again = mix(&mut s, 0, 4 * PPQ);
    assert_same(&heard.left, &again.left);
}

#[test]
fn frozen_tracks_lock_devices_but_not_mixing_or_clip_moves() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let id = audio_track(&mut s, "T", [1, 2, 3]);
    let d = latent(&mut s, id);
    with_clip(&mut s, &dir, id, 0, 2);
    s.freeze_track(id).unwrap();
    s.finish_render().unwrap();
    settle(&mut engine, &mut s);
    let chain = ChainRef::Track(id);
    let params = s.devices(chain)[0].params.clone();
    s.add_builtin(chain, "rlive.delay");
    assert_eq!(s.devices(chain).len(), 1, "no new devices");
    s.set_param(chain, d, 0, 0.3);
    assert_eq!(s.devices(chain)[0].params, params, "no parameter changes");
    s.set_bypass(chain, d, true);
    assert!(!s.devices(chain)[0].bypass);
    s.remove_device(chain, d);
    assert_eq!(s.devices(chain).len(), 1, "no device removed");

    let t = s.project.track_mut(id).unwrap();
    t.volume_db = -6.0;
    s.sync_mix(id);
    assert_eq!(s.project.track(id).unwrap().volume_db, -6.0);
    let clip = s.project.track(id).unwrap().arrangement[0].id;
    s.arr_move(
        &[ArrMove {
            track: id,
            id: clip,
            to_track: id,
            start: 8 * PPQ,
        }],
        false,
    );
    assert_eq!(s.project.track(id).unwrap().arrangement[0].start, 8 * PPQ);
    assert!(!can_arm(&s.project, id), "a frozen track can't be armed");
}

#[test]
fn trimming_a_frozen_clip_keeps_the_audio_under_it() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let id = audio_track(&mut s, "T", [1, 2, 3]);
    let source = dir.file("source.wav");
    write_source(&source, 16.0);
    s.samples.wait_for([source.as_path()]);
    let mut marked = audio_clip(&source, 4);
    marked.start = PPQ;
    s.arr_place(id, ArrangementClip::new(marked, 0));
    s.freeze_track(id).unwrap();
    s.finish_render().unwrap();
    settle(&mut engine, &mut s);
    let whole = mix(&mut s, 0, 3 * PPQ);
    let clip = s.project.track(id).unwrap().arrangement[0].id;
    s.arr_trim(id, clip, TrimEdge::Left(PPQ));
    settle(&mut engine, &mut s);
    let trimmed = mix(&mut s, 0, 3 * PPQ);
    assert_same(&whole.left[BEAT_FRAMES..], &trimmed.left[BEAT_FRAMES..]);
    assert_eq!(
        peak(&trimmed.left[..BEAT_FRAMES]),
        0.0,
        "nothing before it now"
    );
}

#[test]
fn only_tracks_with_clips_can_be_frozen() {
    let (_engine, mut s) = empty_session();
    let group = s.add_track(TrackKind::Group, None);
    assert!(!s.can_freeze(group));
    let empty = s.add_track(TrackKind::Midi, None);
    assert!(!s.can_freeze(empty));
    let ret = s.add_return().unwrap();
    assert!(!s.can_freeze(ret));
    assert!(s.freeze_track(empty).is_err());
}

#[test]
fn a_frozen_clip_moved_to_an_audio_track_becomes_audio() {
    let (mut engine, mut s) = empty_session();
    let midi = s.add_track(TrackKind::Midi, None);
    let mut clip = Clip::new_midi("Notes", [1, 2, 3], 1, Default::default());
    clip.notes_mut()
        .unwrap()
        .push(NoteData::new(0, PPQ, 60, 0.9));
    s.arr_place(midi, ArrangementClip::new(clip, 0));
    let audio = audio_track(&mut s, "Audio", [4, 5, 6]);
    s.freeze_track(midi).unwrap();
    s.finish_render().unwrap();
    settle(&mut engine, &mut s);
    let frozen = s.project.track(midi).unwrap().arrangement[0].id;
    s.arr_move(
        &[ArrMove {
            track: midi,
            id: frozen,
            to_track: audio,
            start: 0,
        }],
        true,
    );
    let on_audio = &s.project.track(audio).unwrap().arrangement[0];
    assert!(on_audio.is_audio(), "the frozen sound, as a clip of audio");
    assert!(on_audio.clip.frozen.is_none());
}

#[test]
fn a_frozen_clip_can_start_a_new_frozen_track() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let id = audio_track(&mut s, "T", [1, 2, 3]);
    let d = latent(&mut s, id);
    with_clip(&mut s, &dir, id, 0, 4);
    s.freeze_track(id).unwrap();
    s.finish_render().unwrap();
    settle(&mut engine, &mut s);
    let clip = s.project.track(id).unwrap().arrangement[0].id;
    let new = s
        .frozen_clip_to_new_track(
            ClipRef::Arr {
                track: id,
                id: clip,
            },
            Some((PPQ, 2 * PPQ)),
        )
        .unwrap();
    let t = s.project.track(new).unwrap();
    assert!(t.is_frozen());
    assert_eq!(t.arrangement.len(), 1, "only the selected part");
    assert_eq!(t.arrangement[0].start, PPQ);
    assert_eq!(t.arrangement[0].length, PPQ);
    assert_eq!(t.devices.len(), 1);
    assert_ne!(t.devices[0].id, d, "devices of its own");
}

#[test]
fn hardware_in_the_chain_freezes_no_faster_than_the_music() {
    let dir = Dir::new();
    let (_engine, mut s) = empty_session();
    let id = audio_track(&mut s, "T", [1, 2, 3]);
    s.add_builtin(ChainRef::Track(id), "rlive.external_audio_effect");
    with_clip(&mut s, &dir, id, 0, 4);
    s.freeze_track(id).unwrap();
    let status = s.render_status().unwrap();
    assert!(status.realtime);
    assert_eq!(status.fraction, 0.0);
    // A slice of a millisecond at most hears a millisecond's worth.
    assert!(s.step_render(std::time::Duration::from_millis(1)));
    let done = s.render_status().unwrap().fraction;
    assert!(done < 0.01, "real time, not offline speed: {done}");
    std::thread::sleep(std::time::Duration::from_millis(60));
    s.step_render(std::time::Duration::from_millis(20));
    let later = s.render_status().unwrap().fraction;
    assert!(later > done, "and it keeps going: {later}");
    s.finish_render().unwrap();
    assert!(s.project.track(id).unwrap().freeze.unwrap().realtime);
    let path = s.project.track(id).unwrap().arrangement[0]
        .clip
        .frozen
        .as_ref()
        .unwrap()
        .file
        .path
        .clone();
    assert!(path.exists());
    s.unfreeze_track(id);
    let song = dir.file("song.rlive");
    s.save(&song).unwrap();
    assert!(!path.exists(), "gone once nothing can bring it back");
}

#[test]
fn flatten_turns_the_frozen_sound_into_the_tracks_clips() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let id = audio_track(&mut s, "T", [1, 2, 3]);
    delay(&mut s, id);
    with_clip(&mut s, &dir, id, 0, 2);
    settle(&mut engine, &mut s);
    s.freeze_track(id).unwrap();
    s.finish_render().unwrap();
    settle(&mut engine, &mut s);
    let frozen_sound = mix(&mut s, 0, 2 * PPQ);
    let frozen = s.project.track(id).unwrap().arrangement[0]
        .clip
        .frozen
        .clone()
        .unwrap();
    s.flatten_track(id).unwrap();
    settle(&mut engine, &mut s);
    let t = s.project.track(id).unwrap();
    assert_eq!(t.kind, TrackKind::Audio);
    assert!(t.devices.is_empty());
    assert!(!t.is_frozen());
    assert!(t.arrangement.iter().all(|c| c.is_audio()));
    let flat = &t.arrangement[0];
    assert_eq!(
        clip_file(&flat.clip),
        frozen.file.path,
        "the frozen file itself"
    );
    assert!(flat.clip.frozen.is_none());
    let ClipContent::Audio { warp, .. } = &flat.clip.content else {
        panic!("audio");
    };
    assert_eq!(
        warp.markers, frozen.markers,
        "it plays what the frozen clip played"
    );
    let flat_sound = mix(&mut s, 0, 2 * PPQ);
    let difference = away_from_edges(&frozen_sound.left, &flat_sound.left);
    assert!(
        difference < 1e-3,
        "Beats is neutral at this tempo: {difference}"
    );
}

#[test]
fn freeze_files_go_to_processed_freeze_at_32_bit() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let song = dir.file("song.rlive");
    s.save(&song).unwrap();
    let id = audio_track(&mut s, "T", [1, 2, 3]);
    with_clip(&mut s, &dir, id, 0, 2);
    s.freeze_track(id).unwrap();
    s.finish_render().unwrap();
    settle(&mut engine, &mut s);
    let frozen = s.project.track(id).unwrap().arrangement[0]
        .clip
        .frozen
        .as_ref()
        .unwrap()
        .file
        .path
        .clone();
    assert!(frozen.starts_with(dir.file("Samples/Processed/Freeze")));
    let bytes = std::fs::read(&frozen).unwrap();
    assert_eq!(u16::from_le_bytes([bytes[20], bytes[21]]), 3, "float");
    assert_eq!(u16::from_le_bytes([bytes[34], bytes[35]]), 32, "32 bits");
}

#[test]
fn a_frozen_set_survives_saving_moving_and_reloading() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let id = audio_track(&mut s, "T", [1, 2, 3]);
    delay(&mut s, id);
    with_clip(&mut s, &dir, id, 0, 2);
    s.set_clip(id, 0, Some(audio_clip(&dir.file("source.wav"), 2)));
    s.freeze_track(id).unwrap();
    s.finish_render().unwrap();
    settle(&mut engine, &mut s);
    let heard = mix(&mut s, 0, 2 * PPQ);

    // Unsaved, it sits in the temporary folder; saving brings it into the Set.
    let project_dir = dir.file("first");
    std::fs::create_dir_all(&project_dir).unwrap();
    let song = project_dir.join("song.rlive");
    s.save(&song).unwrap();
    let file = |s: &Session, song_dir: &Path| {
        let c = &s.project.track(id).unwrap().arrangement[0].clip;
        let f = c.frozen.as_ref().unwrap();
        assert!(f
            .file
            .path
            .starts_with(song_dir.join("Samples/Processed/Freeze")));
        assert!(f.file.relative.is_some(), "relative to the Set");
        f.file.path.clone()
    };
    assert!(file(&s, &project_dir).exists());

    let moved = dir.file("second");
    std::fs::rename(&project_dir, &moved).unwrap();
    s.new_project();
    s.load(&moved.join("song.rlive")).unwrap();
    assert!(file(&s, &moved).exists(), "found from where the Set is now");
    settle(&mut engine, &mut s);
    let again = mix(&mut s, 0, 2 * PPQ);
    assert_same(&heard.left, &again.left);
}

#[test]
fn undo_after_saving_finds_files_that_moved_out_of_the_temporary_folder() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let id = audio_track(&mut s, "T", [1, 2, 3]);
    with_clip(&mut s, &dir, id, 0, 2);
    s.bounce_in_place(id).unwrap();
    s.finish_render().unwrap();
    s.commit_now();
    let song = dir.file("song.rlive");
    s.save(&song).unwrap();
    settle(&mut engine, &mut s);
    s.undo();
    s.redo();
    let c = &s.project.track(id).unwrap().arrangement[0].clip;
    assert!(clip_file(c).starts_with(dir.file("Samples/Processed/Bounce")));
    assert!(clip_file(c).exists());
}

// ------------------------------------------------------------- bounce

#[test]
fn a_track_bounce_is_post_fx_and_pre_mixer_with_the_mixer_carried_over() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let id = audio_track(&mut s, "Keys", [9, 8, 7]);
    delay(&mut s, id);
    with_clip(&mut s, &dir, id, 0, 2);
    settle(&mut engine, &mut s);
    let unity = mix(&mut s, 0, 2 * PPQ);
    let t = s.project.track_mut(id).unwrap();
    t.volume_db = -12.0;
    t.pan = 0.5;
    s.sync_mix(id);
    let song = dir.file("song.rlive");
    s.save(&song).unwrap();

    s.bounce_in_place(id).unwrap();
    s.finish_render().unwrap();
    settle(&mut engine, &mut s);
    let t = s.project.track(id).unwrap();
    assert_eq!(t.kind, TrackKind::Audio);
    assert!(t.devices.is_empty(), "the devices are in the audio");
    assert_eq!((t.volume_db, t.pan), (-12.0, 0.5), "the mixer stays");
    assert_eq!(t.name, "Keys (Bounce)");
    assert_eq!(t.color, [9, 8, 7]);
    let clip = &t.arrangement[0].clip;
    assert_eq!(clip.name, "Loop (Bounce)");
    assert!(clip_file(clip).starts_with(dir.file("Samples/Processed/Bounce")));
    let file = rlive_audio_io::decode(&clip_file(clip)).unwrap();
    assert!(
        loudest_difference(&file.left[..2 * BEAT_FRAMES], &unity.left) < 1e-5,
        "the file is what the devices put out, not what the mixer did to it"
    );
}

#[test]
fn a_bounce_sounds_like_the_track_it_replaces() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let id = audio_track(&mut s, "T", [1, 2, 3]);
    delay(&mut s, id);
    with_clip(&mut s, &dir, id, 0, 4);
    settle(&mut engine, &mut s);
    let ticks = 4 * PPQ;
    let before = mix(&mut s, 0, ticks);
    s.bounce_in_place(id).unwrap();
    s.finish_render().unwrap();
    settle(&mut engine, &mut s);
    let after = mix(&mut s, 0, ticks);
    let difference = away_from_edges(&before.left, &after.left);
    assert!(difference < 1e-3, "difference {difference}");
}

#[test]
fn an_in_place_bounce_commits_both_views_and_keeps_tails() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let id = audio_track(&mut s, "T", [1, 2, 3]);
    delay(&mut s, id);
    with_clip(&mut s, &dir, id, 0, 2);
    s.set_clip(id, 0, Some(audio_clip(&dir.file("source.wav"), 2)));
    s.bounce_in_place(id).unwrap();
    s.finish_render().unwrap();
    settle(&mut engine, &mut s);
    let t = s.project.track(id).unwrap();
    assert!(t.arrangement[0].is_audio());
    assert!(t.arrangement[0].length > 2 * PPQ, "the delay's tail is in");
    let slot = t.clips[0].as_ref().unwrap();
    assert_eq!(slot.length, 2 * PPQ);
    assert_eq!(slot.name, "Loop (Bounce)");
}

#[test]
fn bouncing_a_time_selection_mutes_it_and_makes_a_track_per_source() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let a = audio_track(&mut s, "A", [1, 2, 3]);
    let b = audio_track(&mut s, "B", [3, 2, 1]);
    let empty = audio_track(&mut s, "C", [5, 5, 5]);
    delay(&mut s, a);
    with_clip(&mut s, &dir, a, 0, 4);
    with_clip(&mut s, &dir, b, 0, 4);
    s.bounce_to_new_track(BounceSource::Span {
        tracks: vec![a, b, empty],
        start: PPQ,
        end: 3 * PPQ,
    })
    .unwrap();
    s.finish_render().unwrap();
    settle(&mut engine, &mut s);
    assert_eq!(s.project.tracks.len(), 5, "one more per track with clips");
    let names: Vec<&str> = s.project.tracks.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["A", "A (Bounce)", "B", "B (Bounce)", "C"]);
    let new = &s.project.tracks[1];
    assert_eq!(new.arrangement[0].start, PPQ);
    assert_eq!(new.arrangement[0].length, 2 * PPQ);
    assert_eq!(new.arrangement[0].clip.name, "Loop (Bounce)");
    let source = s.project.track(a).unwrap();
    assert_eq!(source.arrangement.len(), 3, "split at the selection");
    assert!(source.arrangement[0].clip.active);
    assert!(!source.arrangement[1].clip.active, "the middle is muted");
    assert!(source.arrangement[2].clip.active);
}

/// A clip that starts a beat in, and loops over its second bar.
fn marked_clip(path: &Path) -> Clip {
    Clip {
        start: PPQ,
        loop_start: 4 * PPQ,
        ..audio_clip(path, 4)
    }
}

#[test]
fn a_session_bounce_keeps_what_the_markers_play_and_its_loop() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let id = audio_track(&mut s, "T", [1, 2, 3]);
    let source = dir.file("source.wav");
    write_source(&source, 16.0);
    s.samples.wait_for([source.as_path()]);
    s.set_clip(id, 0, Some(marked_clip(&source)));
    settle(&mut engine, &mut s);
    let ticks = 11 * PPQ;
    let before = launched(&mut s, id, 0, ticks);
    s.bounce_in_place(id).unwrap();
    s.finish_render().unwrap();
    settle(&mut engine, &mut s);
    let clip = s.clip(id, 0).unwrap();
    assert_eq!(
        (clip.start, clip.loop_start, clip.length),
        (0, 3 * PPQ, 4 * PPQ),
        "three beats to reach the loop, then the loop"
    );
    let file = rlive_audio_io::decode(&clip_file(clip)).unwrap();
    assert_eq!(file.frames(), 7 * BEAT_FRAMES);
    let after = launched(&mut s, id, 0, ticks);
    let difference = away_from_edges(&before.left, &after.left);
    assert!(difference < 1e-2, "difference {difference}");
}

#[test]
fn a_freeze_keeps_what_the_markers_play_bit_for_bit() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let id = audio_track(&mut s, "T", [1, 2, 3]);
    delay(&mut s, id);
    let source = dir.file("source.wav");
    write_source(&source, 16.0);
    s.samples.wait_for([source.as_path()]);
    s.set_clip(id, 0, Some(marked_clip(&source)));
    let mut marked = marked_clip(&source);
    marked.looped = false;
    marked.end = Some(3 * PPQ);
    s.arr_place(id, ArrangementClip::new(marked, 0));
    settle(&mut engine, &mut s);
    let ticks = 11 * PPQ;
    let session_before = launched(&mut s, id, 0, ticks);
    let lane_before = mix(&mut s, 0, 2 * PPQ);
    s.freeze_track(id).unwrap();
    s.finish_render().unwrap();
    settle(&mut engine, &mut s);
    let frozen = s.clip(id, 0).unwrap().frozen.clone().unwrap();
    assert_eq!(frozen.loop_start, 3 * PPQ, "the way into the loop");
    assert_eq!(frozen.length, 11 * PPQ, "and two cycles of it");
    assert_same(&session_before.left, &launched(&mut s, id, 0, ticks).left);
    assert_same(&lane_before.left, &mix(&mut s, 0, 2 * PPQ).left);
}

#[test]
fn hi_q_reaches_what_a_bounce_and_a_freeze_commit() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let id = audio_track(&mut s, "T", [1, 2, 3]);
    let source = dir.file("other-rate.wav");
    let tone: Vec<f32> = (0..132_300)
        .map(|i| (i as f32 / 44_100.0 * 13_000.0 * std::f32::consts::TAU).sin() * 0.4)
        .collect();
    rlive_devices::write_wav(&source, &tone, &tone, 44_100).unwrap();
    s.samples.wait_for([source.as_path()]);
    let clip = |hi_q: bool| {
        let mut c = audio_clip(&source, 2);
        if let ClipContent::Audio { hi_q: h, .. } = &mut c.content {
            *h = hi_q;
        }
        c
    };
    s.arr_place(id, ArrangementClip::new(clip(true), 0));
    settle(&mut engine, &mut s);
    let with_hi_q = mix(&mut s, 0, 2 * PPQ);
    s.freeze_track(id).unwrap();
    s.finish_render().unwrap();
    settle(&mut engine, &mut s);
    assert_same(&with_hi_q.left, &mix(&mut s, 0, 2 * PPQ).left);
    s.unfreeze_track(id);
    let at = s.project.track(id).unwrap().arrangement[0].id;
    s.arr_delete(&[(id, at)]);
    s.arr_place(id, ArrangementClip::new(clip(false), 0));
    settle(&mut engine, &mut s);
    let without = mix(&mut s, 0, 2 * PPQ);
    assert!(
        loudest_difference(&with_hi_q.left, &without.left) > 1e-3,
        "Hi-Q makes a difference to this file"
    );
}

#[test]
fn a_render_keeps_the_tempo_a_leader_on_another_track_sets() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let source = dir.file("source.wav");
    write_source(&source, 16.0);
    s.samples.wait_for([source.as_path()]);
    let follower = audio_track(&mut s, "F", [1, 2, 3]);
    let lead_track = audio_track(&mut s, "L", [3, 2, 1]);
    s.arr_place(follower, ArrangementClip::new(audio_clip(&source, 4), 0));
    let mut leader = audio_clip(&source, 4);
    if let ClipContent::Audio { warp, .. } = &mut leader.content {
        warp.enabled = true;
        warp.tempo_leader = true;
        warp.markers = vec![
            rlive_project::WarpMarker {
                seconds: 0.0,
                tick: 0.0,
            },
            rlive_project::WarpMarker {
                seconds: 4.0,
                tick: (4 * PPQ) as f64,
            },
        ];
    }
    s.arr_place(lead_track, ArrangementClip::new(leader, 0));
    settle(&mut engine, &mut s);
    assert!(s.project.has_tempo_leader());
    let v = super::render::post_fx_variant(&s.project, &[follower]);
    assert_eq!(
        v.tempo_points_in_force().into_owned(),
        s.project.tempo_points_in_force().into_owned(),
        "the leader's track isn't in the render, but its tempo is"
    );
}

#[test]
fn consolidating_midi_follows_the_clip_markers_and_its_loop() {
    let (mut engine, mut s) = empty_session();
    let id = s.add_track(TrackKind::Midi, None);
    let mut clip = Clip::new_midi("N", [1, 2, 3], 1, Default::default());
    clip.loop_start = 2 * PPQ;
    clip.length = 2 * PPQ;
    clip.start = PPQ;
    let notes = clip.notes_mut().unwrap();
    notes.push(NoteData::new(0, PPQ / 2, 50, 0.9));
    notes.push(NoteData::new(PPQ, PPQ / 2, 60, 0.9));
    notes.push(NoteData::new(2 * PPQ, PPQ / 2, 64, 0.9));
    let placed = ArrangementClip {
        length: 5 * PPQ,
        ..ArrangementClip::new(clip, 0)
    };
    s.arr_place(id, placed);
    s.consolidate(&[id], 0, 5 * PPQ).unwrap();
    s.finish_render().unwrap();
    settle(&mut engine, &mut s);
    let c = &s.project.track(id).unwrap().arrangement[0].clip;
    let got: Vec<(u64, u8)> = c.notes().iter().map(|n| (n.start, n.key)).collect();
    assert_eq!(
        got,
        [(0, 60), (PPQ, 64), (3 * PPQ, 64)],
        "the note before the start marker never plays, the loop's note repeats"
    );
}

#[test]
fn clips_with_no_name_bounce_to_a_bare_suffix() {
    assert_eq!(super::bounce::bounce_name(""), "(Bounce)");
    assert_eq!(super::bounce::bounce_name("Bass"), "Bass (Bounce)");
    assert_eq!(super::bounce::bounce_name("Bass (Bounce)"), "Bass (Bounce)");
}

#[test]
fn a_session_bounce_to_a_new_track_leaves_the_arrangement_side_empty() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let id = audio_track(&mut s, "T", [1, 2, 3]);
    with_clip(&mut s, &dir, id, 0, 2);
    s.set_clip(id, 0, Some(audio_clip(&dir.file("source.wav"), 2)));
    s.bounce_to_new_track(BounceSource::Slots(vec![(id, 0)]))
        .unwrap();
    s.finish_render().unwrap();
    settle(&mut engine, &mut s);
    let new = &s.project.tracks[1];
    assert!(new.arrangement.is_empty(), "nothing on the timeline");
    assert!(new.clips[0].is_some(), "the slot has its audio");
    assert!(!s.clip(id, 0).unwrap().active, "the source is muted");
    assert!(s.project.track(id).unwrap().arrangement[0].clip.active);
}

struct Grouped {
    group: TrackId,
    inside: TrackId,
    routed_out: TrackId,
    ret: TrackId,
}

fn grouped(s: &mut Session, dir: &Dir) -> Grouped {
    let inside = audio_track(s, "In", [1, 1, 1]);
    let routed_out = audio_track(s, "Out", [2, 2, 2]);
    let group = s.group_tracks(&[inside, routed_out]).unwrap();
    let ret = s.add_return().unwrap();
    with_clip(s, dir, inside, 0, 2);
    with_clip(s, dir, routed_out, 0, 2);
    s.set_output(&[routed_out], Route::None);
    s.set_send(inside, ret, 0.0);
    Grouped {
        group,
        inside,
        routed_out,
        ret,
    }
}

#[test]
fn a_group_bounce_is_post_mixer_with_sends_and_without_main_processing() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let g = grouped(&mut s, &dir);
    let t = s.project.track_mut(g.group).unwrap();
    t.volume_db = -6.0;
    s.sync_mix(g.group);
    s.add_builtin(ChainRef::Master, "rlive.utility");
    settle(&mut engine, &mut s);

    let v = super::render::group_variant(&s.project, g.group);
    assert!(
        v.master.devices.is_empty(),
        "the Main track's devices are out"
    );

    // What the Main track's fader does afterwards can't reach the bounce.
    let heard = mix(&mut s, 0, 2 * PPQ);
    s.set_master_volume(-12.0);
    settle(&mut engine, &mut s);
    s.bounce_to_new_track(BounceSource::Span {
        tracks: vec![g.group],
        start: 0,
        end: 2 * PPQ,
    })
    .unwrap();
    s.finish_render().unwrap();
    settle(&mut engine, &mut s);
    let new = s
        .project
        .tracks
        .iter()
        .find(|t| t.id != g.group && t.name.ends_with("(Bounce)"))
        .unwrap();
    assert_eq!(new.volume_db, 0.0, "the group's fader is in the audio");
    let file = rlive_audio_io::decode(&clip_file(&new.arrangement[0].clip)).unwrap();
    // The group's -6 dB and the return's copy of the signal are both in there.
    assert!(away_from_edges(&file.left[..heard.frames()], &heard.left) < 1e-5);
    assert!(peak(&file.left) > 0.1);
    let t = s.project.track(g.inside).unwrap();
    assert!(!t.arrangement[0].clip.active, "the member's clip is muted");
    let with_send = peak(&file.left);
    s.set_send(g.inside, g.ret, SEND_OFF_DB);
    settle(&mut engine, &mut s);
    s.set_clip_active_again(g.inside);
    let without = {
        s.bounce_to_new_track(BounceSource::Span {
            tracks: vec![g.group],
            start: 0,
            end: 2 * PPQ,
        })
        .unwrap();
        s.finish_render().unwrap();
        settle(&mut engine, &mut s);
        let newest = s
            .project
            .tracks
            .iter()
            .find(|t| t.name.ends_with("(Bounce)"))
            .unwrap();
        let file = rlive_audio_io::decode(&clip_file(&newest.arrangement[0].clip)).unwrap();
        peak(&file.left)
    };
    assert!(with_send > without * 1.5, "the send is in the bounce");
}

impl Session {
    fn set_clip_active_again(&mut self, track: TrackId) {
        if let Some(t) = self.project.track_mut(track) {
            for c in &mut t.arrangement {
                c.clip.active = true;
            }
        }
        self.sync_lane(track);
    }
}

#[test]
fn tracks_routed_out_of_a_group_are_left_out_of_its_bounce() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let inside = audio_track(&mut s, "In", [1, 1, 1]);
    let group = s.group_tracks(&[inside]).unwrap();
    with_clip(&mut s, &dir, inside, 0, 2);
    // Everything in the group goes elsewhere.
    s.set_output(&[inside], Route::None);
    settle(&mut engine, &mut s);
    s.bounce_to_new_track(BounceSource::Span {
        tracks: vec![group],
        start: 0,
        end: 2 * PPQ,
    })
    .unwrap();
    s.finish_render().unwrap();
    let new = s
        .project
        .tracks
        .iter()
        .find(|t| t.name.ends_with("(Bounce)"))
        .unwrap();
    let file = rlive_audio_io::decode(&clip_file(&new.arrangement[0].clip)).unwrap();
    assert_eq!(peak(&file.left), 0.0, "nothing reached the Main track");
}

#[test]
fn a_group_in_place_replaces_the_group_and_everything_in_it() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let g = grouped(&mut s, &dir);
    let t = s.project.track_mut(g.group).unwrap();
    t.volume_db = -6.0;
    s.sync_mix(g.group);
    settle(&mut engine, &mut s);
    s.commit_now();
    let before = s.project.clone();
    s.bounce_in_place(g.group).unwrap();
    s.finish_render().unwrap();
    settle(&mut engine, &mut s);
    assert_eq!(s.project.tracks.len(), 1, "the members are gone");
    assert!(s.project.track(g.routed_out).is_none());
    let t = &s.project.tracks[0];
    assert_eq!(t.kind, TrackKind::Audio);
    assert_eq!(t.volume_db, 0.0, "post-mixer, so the fader is at unity");
    assert!(t.arrangement[0].is_audio());
    assert!(t.sends.is_empty());

    s.commit_now();
    assert!(s.undo());
    settle(&mut engine, &mut s);
    assert_eq!(s.project, before, "undo brings the group back");
    assert!(s.project.track(g.inside).is_some());
}

#[test]
fn undo_reverses_a_track_bounce() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let id = audio_track(&mut s, "T", [1, 2, 3]);
    delay(&mut s, id);
    with_clip(&mut s, &dir, id, 0, 2);
    settle(&mut engine, &mut s);
    s.commit_now();
    let before = s.project.clone();
    let heard = mix(&mut s, 0, 2 * PPQ);
    s.bounce_in_place(id).unwrap();
    s.finish_render().unwrap();
    settle(&mut engine, &mut s);
    s.commit_now();
    assert!(s.undo());
    settle(&mut engine, &mut s);
    assert_eq!(s.project, before);
    let again = mix(&mut s, 0, 2 * PPQ);
    assert_same(&heard.left, &again.left);
    assert!(s.redo());
    assert_eq!(s.project.track(id).unwrap().kind, TrackKind::Audio);
}

#[test]
fn undo_reverses_a_freeze_and_redo_refreezes() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let id = audio_track(&mut s, "T", [1, 2, 3]);
    delay(&mut s, id);
    with_clip(&mut s, &dir, id, 0, 2);
    settle(&mut engine, &mut s);
    s.commit_now();
    let before = s.project.clone();
    let heard = mix(&mut s, 0, 2 * PPQ);
    s.freeze_track(id).unwrap();
    s.finish_render().unwrap();
    settle(&mut engine, &mut s);
    s.commit_now();
    assert!(s.undo());
    settle(&mut engine, &mut s);
    assert_eq!(s.project, before);
    assert!(s.redo());
    settle(&mut engine, &mut s);
    assert!(s.is_frozen(id));
    let again = mix(&mut s, 0, 2 * PPQ);
    assert_same(&heard.left, &again.left);
}

// ------------------------------------------------------------- paste bounced

fn bounce_track(s: &mut Session, dir: &Dir) -> TrackId {
    let id = audio_track(s, "Src", [1, 2, 3]);
    delay(s, id);
    with_clip(s, dir, id, 0, 2);
    id
}

#[test]
fn pasting_bounced_audio_renders_the_source_as_it_is_now() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let src = bounce_track(&mut s, &dir);
    let dest = audio_track(&mut s, "Dest", [7, 7, 7]);
    let copied = BounceSource::Span {
        tracks: vec![src],
        start: 0,
        end: 2 * PPQ,
    };
    settle(&mut engine, &mut s);
    s.paste_bounced(&copied, PasteTarget::Main { track: dest, at: 0 })
        .unwrap();
    s.finish_render().unwrap();
    let first = clip_file(&s.project.track(dest).unwrap().arrangement[0].clip);
    let first = rlive_audio_io::decode(&first).unwrap();

    // Changing the source changes what the next paste makes.
    let d = s.devices(ChainRef::Track(src))[0].id;
    s.set_bypass(ChainRef::Track(src), d, true);
    settle(&mut engine, &mut s);
    s.paste_bounced(
        &copied,
        PasteTarget::Main {
            track: dest,
            at: 8 * PPQ,
        },
    )
    .unwrap();
    s.finish_render().unwrap();
    let dest_track = s.project.track(dest).unwrap();
    assert_eq!(dest_track.arrangement.len(), 2);
    assert_eq!(dest_track.arrangement[1].start, 8 * PPQ);
    let second = rlive_audio_io::decode(&clip_file(&dest_track.arrangement[1].clip)).unwrap();
    assert!(loudest_difference(&first.left, &second.left) > 1e-3);
}

#[test]
fn pasting_onto_an_empty_midi_track_converts_it_and_a_used_one_refuses() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let src = bounce_track(&mut s, &dir);
    let midi = s.add_track(TrackKind::Midi, None);
    let copied = BounceSource::Span {
        tracks: vec![src],
        start: 0,
        end: PPQ,
    };
    s.paste_bounced(&copied, PasteTarget::Main { track: midi, at: 0 })
        .unwrap();
    s.finish_render().unwrap();
    settle(&mut engine, &mut s);
    let t = s.project.track(midi).unwrap();
    assert_eq!(t.kind, TrackKind::Audio);
    assert!(t.devices.is_empty(), "the instrument can't take audio");
    assert_eq!(t.arrangement.len(), 1);

    let used = s.add_track(TrackKind::Midi, None);
    s.set_clip(
        used,
        0,
        Some(Clip::new_midi("c", [1, 1, 1], 1, Default::default())),
    );
    assert!(!s.can_paste_bounced(PasteTarget::Main { track: used, at: 0 }));
}

#[test]
fn pasting_can_target_a_take_lane() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let src = bounce_track(&mut s, &dir);
    let dest = audio_track(&mut s, "Dest", [7, 7, 7]);
    s.project
        .track_mut(dest)
        .unwrap()
        .take_lanes
        .push(TakeLane {
            name: "Take 1".into(),
            color: None,
            clips: Vec::new(),
        });
    let copied = BounceSource::Span {
        tracks: vec![src],
        start: 0,
        end: PPQ,
    };
    s.paste_bounced(
        &copied,
        PasteTarget::TakeLane {
            track: dest,
            lane: 0,
            at: 4 * PPQ,
        },
    )
    .unwrap();
    s.finish_render().unwrap();
    settle(&mut engine, &mut s);
    let t = s.project.track(dest).unwrap();
    assert!(t.arrangement.is_empty(), "the main lane is untouched");
    assert_eq!(t.take_lanes[0].clips[0].start, 4 * PPQ);
}

#[test]
fn paste_bounced_goes_unavailable_when_the_source_is_deleted() {
    let dir = Dir::new();
    let (_engine, mut s) = empty_session();
    let src = bounce_track(&mut s, &dir);
    let id = s.project.track(src).unwrap().arrangement[0].id;
    let copied = BounceSource::Clips(vec![(src, id)]);
    assert!(s.bounce_source_exists(&copied));
    s.arr_delete(&[(src, id)]);
    assert!(!s.bounce_source_exists(&copied));
    assert!(s
        .paste_bounced(&copied, PasteTarget::Main { track: src, at: 0 })
        .is_err());
}

// ------------------------------------------------------------- consolidate

fn gain_envelope(clip: &mut Clip) {
    let range = rlive_project::clip_envelope::clip_param_range(ClipParam::Gain);
    let _ = range;
    clip.envelopes.push(ClipEnvelope {
        envelope: Envelope {
            target: EnvTarget::Clip(ClipParam::Gain),
            points: vec![
                rlive_project::Breakpoint::new(0.0, 0.5),
                rlive_project::Breakpoint::new((2 * PPQ) as f64, 0.5),
            ],
        },
        mode: Default::default(),
        loop_region: None,
    });
}

fn with_consolidated(s: &mut Session, dir: &Dir, track: TrackId, adjust: impl Fn(&mut Clip)) {
    let source = dir.file("source.wav");
    if !source.exists() {
        write_source(&source, 16.0);
    }
    s.samples.wait_for([source.as_path()]);
    let mut clip = audio_clip(&source, 2);
    adjust(&mut clip);
    s.arr_place(track, ArrangementClip::new(clip, 0));
}

#[test]
fn consolidating_renders_ahead_of_the_devices_and_normalizes() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let id = audio_track(&mut s, "T", [1, 2, 3]);
    delay(&mut s, id);
    s.project.track_mut(id).unwrap().volume_db = -20.0;
    s.sync_mix(id);
    with_consolidated(&mut s, &dir, id, |c| {
        if let ClipContent::Audio { gain_db, .. } = &mut c.content {
            *gain_db = -6.0;
        }
        gain_envelope(c);
    });
    let song = dir.file("song.rlive");
    s.save(&song).unwrap();
    settle(&mut engine, &mut s);
    s.commit_now();
    let before = s.project.clone();
    s.consolidate(&[id], 0, 2 * PPQ).unwrap();
    s.finish_render().unwrap();
    settle(&mut engine, &mut s);
    let t = s.project.track(id).unwrap();
    assert_eq!(t.arrangement.len(), 1);
    let clip = &t.arrangement[0].clip;
    assert!(clip_file(clip).starts_with(dir.file("Samples/Processed/Consolidate")));
    let ClipContent::Audio { gain_db, .. } = clip.content else {
        panic!("audio");
    };
    let file = rlive_audio_io::decode(&clip_file(clip)).unwrap();
    assert!((peak(&file.left) - 1.0).abs() < 1e-3, "normalized");
    let bytes = std::fs::read(clip_file(clip)).unwrap();
    assert_eq!(
        u16::from_le_bytes([bytes[34], bytes[35]]),
        24,
        "the bit depth in Settings"
    );
    // The source's peak is about 0.6, under -6 dB of gain and a half-gain envelope.
    let expected = 20.0 * (0.6f32 * 10f32.powf(-6.0 / 20.0) * 0.5).log10();
    assert!(
        (gain_db - expected).abs() < 1.0,
        "{gain_db} dB, expected about {expected}"
    );
    assert_eq!(t.volume_db, -20.0, "the mixer isn't touched");
    assert_eq!(t.devices.len(), 1, "the devices stay");
    s.commit_now();
    assert!(s.undo());
    settle(&mut engine, &mut s);
    assert_eq!(s.project, before);
}

#[test]
fn consolidating_a_warped_clip_keeps_its_timing() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let id = audio_track(&mut s, "T", [1, 2, 3]);
    with_consolidated(&mut s, &dir, id, |c| {
        if let ClipContent::Audio { warp, .. } = &mut c.content {
            *warp = Warp {
                enabled: true,
                markers: vec![
                    rlive_project::WarpMarker {
                        seconds: 0.0,
                        tick: 0.0,
                    },
                    rlive_project::WarpMarker {
                        seconds: 1.0,
                        tick: (2 * PPQ) as f64,
                    },
                ],
                mode: rlive_project::WarpMode::RePitch,
                ..Warp::default()
            };
        }
    });
    settle(&mut engine, &mut s);
    let before = mix(&mut s, 0, 2 * PPQ);
    s.consolidate(&[id], 0, 2 * PPQ).unwrap();
    s.finish_render().unwrap();
    settle(&mut engine, &mut s);
    let after = mix(&mut s, 0, 2 * PPQ);
    let difference = away_from_edges(&before.left, &after.left);
    assert!(difference < 5e-3, "difference {difference}");
}

#[test]
fn consolidate_makes_one_clip_per_track_across_a_selection() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let a = audio_track(&mut s, "A", [1, 2, 3]);
    let b = audio_track(&mut s, "B", [3, 2, 1]);
    with_clip(&mut s, &dir, a, 0, 1);
    with_clip(&mut s, &dir, a, 2, 1);
    with_clip(&mut s, &dir, b, 1, 1);
    s.consolidate(&[a, b], 0, 4 * PPQ).unwrap();
    s.finish_render().unwrap();
    settle(&mut engine, &mut s);
    for id in [a, b] {
        let t = s.project.track(id).unwrap();
        assert_eq!(t.arrangement.len(), 1);
        assert_eq!(t.arrangement[0].start, 0);
        assert_eq!(t.arrangement[0].length, 4 * PPQ);
    }
}

#[test]
fn consolidating_midi_clips_joins_their_notes_in_order() {
    let (mut engine, mut s) = empty_session();
    let id = s.add_track(TrackKind::Midi, None);
    let mut first = Clip::new_midi("One", [1, 2, 3], 1, Default::default());
    first
        .notes_mut()
        .unwrap()
        .push(NoteData::new(0, PPQ, 60, 0.9));
    let mut second = Clip::new_midi("Two", [3, 2, 1], 1, Default::default());
    second
        .notes_mut()
        .unwrap()
        .push(NoteData::new(0, PPQ, 64, 0.9));
    let bar = 4 * PPQ;
    s.arr_place(id, ArrangementClip::new(first, 0));
    s.arr_place(id, ArrangementClip::new(second, bar));
    s.consolidate(&[id], 0, 2 * bar).unwrap();
    s.finish_render().unwrap();
    settle(&mut engine, &mut s);
    let t = s.project.track(id).unwrap();
    assert_eq!(t.arrangement.len(), 1);
    let c = &t.arrangement[0].clip;
    assert_eq!(c.notes().len(), 2);
    assert_eq!(c.notes()[1].start, bar);
    assert_eq!(c.notes()[1].key, 64);
    assert_eq!(c.length, 2 * bar);
}

#[test]
fn consolidating_a_cropped_clip_uses_what_is_left() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let id = audio_track(&mut s, "T", [1, 2, 3]);
    with_clip(&mut s, &dir, id, 0, 4);
    s.arr_crop(&[id], PPQ, 3 * PPQ);
    settle(&mut engine, &mut s);
    s.consolidate(&[id], PPQ, 3 * PPQ).unwrap();
    s.finish_render().unwrap();
    settle(&mut engine, &mut s);
    let t = s.project.track(id).unwrap();
    assert_eq!(t.arrangement.len(), 1);
    assert_eq!(
        (t.arrangement[0].start, t.arrangement[0].length),
        (PPQ, 2 * PPQ)
    );
}

#[test]
fn a_frozen_track_consolidates_without_losing_its_frozen_sound() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let id = audio_track(&mut s, "T", [1, 2, 3]);
    delay(&mut s, id);
    with_clip(&mut s, &dir, id, 0, 2);
    with_clip(&mut s, &dir, id, 2, 2);
    s.freeze_track(id).unwrap();
    s.finish_render().unwrap();
    settle(&mut engine, &mut s);
    let heard = mix(&mut s, 0, 4 * PPQ);
    s.consolidate(&[id], 0, 4 * PPQ).unwrap();
    s.finish_render().unwrap();
    settle(&mut engine, &mut s);
    let t = s.project.track(id).unwrap();
    let inside: Vec<_> = t.arrangement.iter().filter(|c| c.start < 4 * PPQ).collect();
    assert_eq!(inside.len(), 1, "one clip");
    assert!(inside[0].clip.frozen.is_some(), "still frozen");
    let again = mix(&mut s, 0, 4 * PPQ);
    assert_same(&heard.left, &again.left);
}

#[test]
fn consolidate_time_to_new_scene_makes_a_clip_per_track_below() {
    let dir = Dir::new();
    let (mut engine, mut s) = empty_session();
    let a = audio_track(&mut s, "A", [1, 2, 3]);
    let b = s.add_track(TrackKind::Midi, None);
    let empty = audio_track(&mut s, "C", [4, 4, 4]);
    with_clip(&mut s, &dir, a, 0, 2);
    let mut midi = Clip::new_midi("N", [1, 1, 1], 1, Default::default());
    midi.notes_mut()
        .unwrap()
        .push(NoteData::new(0, PPQ, 60, 0.9));
    s.arr_place(b, ArrangementClip::new(midi, 0));
    let scenes = s.project.scenes.len();
    s.consolidate_time_to_new_scene(0, 0, 2 * PPQ).unwrap();
    s.finish_render().unwrap();
    settle(&mut engine, &mut s);
    assert_eq!(s.project.scenes.len(), scenes + 1);
    assert!(s.clip(a, 1).unwrap().looped);
    assert!(s.clip(a, 1).is_some_and(|c| !c.is_midi()));
    assert!(s.clip(b, 1).is_some_and(Clip::is_midi));
    assert!(s.clip(empty, 1).is_none());
    let timeline: usize = s.project.tracks.iter().map(|t| t.arrangement.len()).sum();
    assert_eq!(timeline, 2, "the Arrangement isn't changed");
}

#[test]
fn a_running_job_reports_progress_and_can_be_cancelled() {
    let dir = Dir::new();
    let (_engine, mut s) = empty_session();
    let id = audio_track(&mut s, "T", [1, 2, 3]);
    with_clip(&mut s, &dir, id, 0, 16);
    s.prepare_render();
    let before = s.project.clone();
    s.bounce_in_place(id).unwrap();
    assert!(s.bounce_in_place(id).is_err(), "one at a time");
    assert_eq!(s.project, before, "nothing changes until it's done");
    s.step_render(std::time::Duration::from_micros(10));
    assert!(s.render_status().is_some());
    s.cancel_render();
    assert!(s.render_status().is_none());
    assert_eq!(s.project, before);
}
