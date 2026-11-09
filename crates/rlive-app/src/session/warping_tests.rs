//! Warping clips from the Session down to what the engine plays.

use super::*;
use rlive_core::PPQ;
use rlive_engine::{create_engine, Engine};
use rlive_project::warp_edit::WarpOp;
use rlive_project::{Clip, ClipContent, Warp, WarpMarker, WarpMode};
use std::sync::atomic::{AtomicUsize, Ordering};

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;
const BAR: u64 = 4 * PPQ;

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Dir(PathBuf);

impl Dir {
    fn new() -> Self {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("rlive-warp-{}-{n}", std::process::id()));
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

fn session() -> (Engine, Session) {
    let (engine, handle) = create_engine(SR, BLOCK);
    let s = Session::new(handle, Project::new_default(), PluginCache::default());
    (engine, s)
}

/// Writes `seconds` of a short burst at each moment in `hits`.
fn write_hits(path: &Path, seconds: f64, hits: &[f64]) {
    let mut left = vec![0.0f32; (seconds * SR) as usize];
    for t in hits {
        let at = (t * SR) as usize;
        for (i, s) in left[at..].iter_mut().take(3000).enumerate() {
            *s += (-(i as f32) / 500.0).exp() * if i % 2 == 0 { 0.7 } else { -0.7 };
        }
    }
    rlive_devices::write_wav(path, &left, &left, SR as u32).unwrap();
}

fn warped_clip(path: &Path, seconds: f64, bars: u64) -> Clip {
    let mut content = ClipContent::audio(path, 0.0);
    if let ClipContent::Audio { warp, .. } = &mut content {
        *warp = Warp {
            enabled: true,
            markers: vec![
                WarpMarker {
                    seconds: 0.0,
                    tick: 0.0,
                },
                WarpMarker {
                    seconds,
                    tick: (bars * BAR) as f64,
                },
            ],
            ..Warp::default()
        };
    }
    Clip {
        name: "Loop".into(),
        color: [1, 2, 3],
        length: bars * BAR,
        looped: true,
        content,
        ..Clip::default()
    }
}

fn arrive(s: &mut Session) {
    s.samples.wait_idle();
    s.idle();
}

fn warp_of(s: &Session, r: ClipRef) -> Warp {
    match &s.clip_at(r).unwrap().content {
        ClipContent::Audio { warp, .. } => warp.clone(),
        ClipContent::Midi { .. } => panic!("not audio"),
    }
}

fn with_clip(hits: &[f64]) -> (Engine, Session, ClipRef, Dir) {
    let dir = Dir::new();
    let path = dir.file("a.wav");
    write_hits(&path, 4.0, hits);
    let (engine, mut s) = session();
    let t = s.project.tracks[1].id;
    s.set_clip(t, 0, Some(warped_clip(&path, 4.0, 2)));
    arrive(&mut s);
    s.commit_edits(false);
    (engine, s, ClipRef::slot(t, 0), dir)
}

#[test]
fn a_marker_edit_is_one_undo_step_and_undoes_cleanly() {
    let (_e, mut s, r, _dir) = with_clip(&[0.5, 1.0, 2.0]);
    let before = warp_of(&s, r);
    assert!(s.apply_warp_op(
        &[r],
        &WarpOp::Add {
            seconds: 1.0,
            tick: 2.0 * PPQ as f64
        }
    ));
    s.commit_edits(false);
    assert_eq!(warp_of(&s, r).markers.len(), 3);
    assert!(s.undo());
    assert_eq!(warp_of(&s, r), before);
    assert!(s.redo());
    assert_eq!(warp_of(&s, r).markers.len(), 3);
}

#[test]
fn a_drag_of_many_frames_is_one_undo_step() {
    let (_e, mut s, r, _dir) = with_clip(&[0.5]);
    s.apply_warp_op(
        &[r],
        &WarpOp::Add {
            seconds: 1.0,
            tick: 2.0 * PPQ as f64,
        },
    );
    s.commit_edits(false);
    let before = warp_of(&s, r);
    let mut tick = 2.0 * PPQ as f64;
    for step in 0..10 {
        let to = 2.0 * PPQ as f64 + step as f64 * 10.0;
        s.apply_warp_op(&[r], &WarpOp::Move { tick, to });
        tick = to;
        s.commit_edits(true);
    }
    s.commit_edits(false);
    assert!(s.undo());
    assert_eq!(warp_of(&s, r), before);
}

#[test]
fn warp_edits_reach_clips_of_the_same_length_only() {
    let dir = Dir::new();
    let (a, b, c) = (dir.file("a.wav"), dir.file("b.wav"), dir.file("c.wav"));
    write_hits(&a, 2.0, &[0.5]);
    write_hits(&b, 2.0, &[0.5]);
    write_hits(&c, 4.0, &[0.5]);
    let (_e, mut s) = session();
    let tracks: Vec<_> = s.project.tracks.iter().map(|t| t.id).collect();
    for (i, (path, seconds, bars)) in [(&a, 2.0, 1), (&b, 2.0, 1), (&c, 4.0, 2)]
        .into_iter()
        .enumerate()
    {
        s.set_clip(tracks[i], 0, Some(warped_clip(path, seconds, bars)));
    }
    arrive(&mut s);
    s.commit_edits(false);
    let refs: Vec<ClipRef> = (0..3).map(|i| ClipRef::slot(tracks[i], 0)).collect();
    let group = s.warp_group(refs[0], &refs);
    assert_eq!(
        group,
        vec![refs[0], refs[1]],
        "same file length and loop length"
    );

    s.apply_warp_op(
        &group,
        &WarpOp::Add {
            seconds: 0.5,
            tick: PPQ as f64,
        },
    );
    s.commit_edits(false);
    assert_eq!(warp_of(&s, refs[0]).markers.len(), 3);
    assert_eq!(warp_of(&s, refs[1]).markers.len(), 3);
    assert_eq!(warp_of(&s, refs[2]).markers.len(), 2);
    assert!(s.undo());
    assert_eq!(warp_of(&s, refs[1]).markers.len(), 2, "one step for both");
}

#[test]
fn turning_warp_on_assumes_an_even_length_loop() {
    let dir = Dir::new();
    let path = dir.file("a.wav");
    write_hits(&path, 4.0, &[0.5]);
    let (_e, mut s) = session();
    let t = s.project.tracks[1].id;
    let mut clip = warped_clip(&path, 4.0, 2);
    if let ClipContent::Audio { warp, .. } = &mut clip.content {
        *warp = Warp::default();
    }
    clip.length = 8 * PPQ;
    s.set_clip(t, 0, Some(clip));
    arrive(&mut s);
    let r = ClipRef::slot(t, 0);
    assert!(s.set_warp_enabled(&[r], true));
    let w = warp_of(&s, r);
    assert_eq!(w.markers.len(), 2);
    assert_eq!(w.markers[1].tick, (2 * BAR) as f64, "two bars at 120 BPM");
    assert_eq!(rlive_project::warp_edit::clip_bpm(&w), Some(120.0));
    assert_eq!(s.clip_at(r).unwrap().length, 2 * BAR);
}

#[test]
fn saved_settings_come_back_on_new_clips_of_the_file() {
    let (_e, mut s, r, dir) = with_clip(&[0.5, 1.0]);
    let path = dir.file("a.wav");
    s.apply_warp_op(
        &[r],
        &WarpOp::Add {
            seconds: 1.0,
            tick: 2.0 * PPQ as f64,
        },
    );
    s.set_clip_gain(&[r], -6.0);
    s.save_clip_with_sample(r).unwrap();
    let t = r.track();
    let clip = s.audio_clip_from_file(&path, t).unwrap();
    let ClipContent::Audio { warp, gain_db, .. } = &clip.content else {
        panic!("audio");
    };
    assert_eq!(warp.markers.len(), 3);
    assert_eq!(*gain_db, -6.0);
    assert_eq!(clip.length, 2 * BAR);
}

#[test]
fn short_samples_follow_the_setting() {
    let dir = Dir::new();
    let long_hit = dir.file("loop.wav");
    let short_hit = dir.file("hit.wav");
    write_hits(&long_hit, 4.0, &[0.5]);
    write_hits(&short_hit, 0.3, &[0.0]);
    let (_e, mut s) = session();
    let t = s.project.tracks[1].id;

    let enabled = |c: &Clip| matches!(&c.content, ClipContent::Audio { warp, .. } if warp.enabled);

    // Auto: something under a second is a one-shot, anything else a loop.
    let c = s.audio_clip_from_file(&short_hit, t).unwrap();
    assert!(!enabled(&c) && !c.looped);
    let c = s.audio_clip_from_file(&long_hit, t).unwrap();
    assert!(enabled(&c) && c.looped);
    assert_eq!(c.length, 2 * BAR);

    s.warp_defaults.short_samples = ShortSampleMode::WarpedOneShot;
    s.warp_defaults.mode = WarpMode::RePitch;
    let c = s.audio_clip_from_file(&long_hit, t).unwrap();
    assert!(enabled(&c) && !c.looped);
    let ClipContent::Audio { warp, .. } = &c.content else {
        panic!("audio");
    };
    assert_eq!(warp.mode, WarpMode::RePitch);

    s.warp_defaults.short_samples = ShortSampleMode::UnwarpedOneShot;
    let c = s.audio_clip_from_file(&long_hit, t).unwrap();
    assert!(!enabled(&c) && !c.looped);
    s.samples.wait_idle();
}

#[test]
fn a_warped_clip_follows_the_set_tempo_and_transients_are_shown() {
    let hits = [0.5, 1.5, 2.5];
    let (mut engine, mut s, r, _dir) = with_clip(&hits);
    let view = s.warp_view(r).unwrap();
    assert_eq!(view.transients.len(), hits.len(), "{:?}", view.transients);
    s.set_tempo(90.0);
    s.launch_clip(r.track(), 0);
    let mut out = Vec::new();
    for _ in 0..(6.0 * SR) as usize / BLOCK {
        let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
        engine.process_stereo(&mut l, &mut r);
        s.idle();
        out.extend_from_slice(&l);
    }
    let quiet_for = (SR * 0.2) as usize;
    let top = out.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    let mut found = Vec::new();
    let mut silent = quiet_for;
    for (i, x) in out.iter().enumerate() {
        if x.abs() > top * 0.25 {
            if silent >= quiet_for {
                found.push(i);
            }
            silent = 0;
        } else {
            silent += 1;
        }
    }
    let launch_lag = found[0] as f64 - hits[0] * SR * 120.0 / 90.0;
    assert!(found.len() >= hits.len(), "{found:?}");
    for (i, hit) in hits.iter().enumerate() {
        let want = hit * SR * 120.0 / 90.0 + launch_lag;
        assert!(
            (found[i] as f64 - want).abs() < 200.0,
            "hit {i}: heard {} wanted {want}",
            found[i]
        );
    }
}

#[test]
fn a_clip_without_a_file_tells_the_tab_nothing() {
    let (_e, s) = session();
    let t = s.project.tracks[1].id;
    assert!(s.warp_view(ClipRef::slot(t, 0)).is_none());
}

/// Writes a click on every beat, louder on the first of each bar.
fn write_groove(path: &Path, seconds: f64, bpm: f64) {
    let mut left = vec![0.0f32; (seconds * SR) as usize];
    let beat = 60.0 / bpm * SR;
    let mut i = 0usize;
    while (i as f64 * beat) < left.len() as f64 {
        let at = (i as f64 * beat) as usize;
        let level = if i.is_multiple_of(4) { 0.8 } else { 0.4 };
        for (j, s) in left[at..].iter_mut().take(2_000).enumerate() {
            *s += (-(j as f32) / 300.0).exp() * if j % 2 == 0 { level } else { -level };
        }
        i += 1;
    }
    rlive_devices::write_wav(path, &left, &left, SR as u32).unwrap();
}

fn audio_of(c: &Clip) -> &Warp {
    let ClipContent::Audio { warp, .. } = &c.content else {
        panic!("audio");
    };
    warp
}

#[test]
fn a_long_sample_is_warped_on_its_bars_when_its_beats_arrive() {
    let dir = Dir::new();
    let path = dir.file("song.wav");
    write_groove(&path, 70.0, 125.0);
    let (_e, mut s) = session();
    assert!(s.warp_defaults.auto_warp_long, "on by default");
    let t = s.project.tracks[1].id;
    let c = s.audio_clip_from_file(&path, t).unwrap();
    assert!(!audio_of(&c).enabled, "nothing to go on yet");
    s.project.tracks[1].place_clip(rlive_project::ArrangementClip::new(c, 0));
    s.samples.wait_idle();
    s.poll_samples();
    let placed = &s.project.tracks[1].arrangement[0].clip;
    let warp = audio_of(placed);
    assert!(warp.enabled, "warped once tracked");
    assert_eq!(warp.mode, WarpMode::Beats);
    let bars = 4.0 * 60.0 / 125.0;
    assert!(warp.markers.len() > 30, "{} markers", warp.markers.len());
    for pair in warp.markers.windows(2).take(warp.markers.len() - 2) {
        assert_eq!(pair[1].tick - pair[0].tick, BAR as f64);
        let gap = pair[1].seconds - pair[0].seconds;
        assert!((gap - bars).abs() < 0.03, "{gap} seconds in a bar");
    }
    assert!(!placed.looped);
    let last = warp.markers.last().unwrap();
    assert!((last.seconds - 70.0).abs() < 1e-6);
    // Stored with the sample: a later session gets it without tracking again.
    let print = rlive_audio_io::fingerprint(&path).unwrap();
    let stored = rlive_audio_io::analysis::read(&path, &print).unwrap();
    assert!(stored.extras.beats().is_some(), "not in the sidecar");
    let (_e2, mut fresh) = session();
    fresh.samples.request(&path);
    fresh.samples.wait_idle();
    fresh.poll_samples();
    let bpm = fresh.samples.beats(&path).expect("from the sidecar").bpm;
    assert!((bpm - 125.0).abs() < 1.0, "{bpm}");
}

#[test]
fn clips_from_an_earlier_session_are_not_warped_when_their_beats_arrive() {
    let dir = Dir::new();
    let path = dir.file("song.wav");
    write_groove(&path, 62.0, 120.0);
    let (_e, mut s) = session();
    // A clip that was saved unwarped, as a Set being opened would have it.
    let t = s.project.tracks[1].id;
    s.warp_defaults.auto_warp_long = false;
    let c = s.audio_clip_from_file(&path, t).unwrap();
    s.warp_defaults.auto_warp_long = true;
    s.project.tracks[1].place_clip(rlive_project::ArrangementClip::new(c, 0));
    s.samples.wait_idle();
    s.poll_samples();
    assert!(s.samples.beats(&path).is_some(), "tracked all the same");
    assert!(!audio_of(&s.project.tracks[1].arrangement[0].clip).enabled);
}

#[test]
fn auto_warp_off_leaves_long_samples_alone() {
    let dir = Dir::new();
    let path = dir.file("song.wav");
    write_groove(&path, 62.0, 120.0);
    let (_e, mut s) = session();
    s.warp_defaults.auto_warp_long = false;
    let mut config = crate::settings::Settings::default()
        .samples
        .loader_config(false);
    config.cache.folder = dir.file("cache");
    s.samples.configure(config);
    let t = s.project.tracks[1].id;
    let c = s.audio_clip_from_file(&path, t).unwrap();
    s.project.tracks[1].place_clip(rlive_project::ArrangementClip::new(c, 0));
    s.samples.wait_idle();
    s.poll_samples();
    assert!(!audio_of(&s.project.tracks[1].arrangement[0].clip).enabled);
    assert!(s.samples.beats(&path).is_none(), "not analysed");
}

fn leader_track(
    s: &mut Session,
    start: u64,
    bars: u64,
    bpm: f64,
) -> (TrackId, rlive_core::ArrClipId) {
    let seconds = bars as f64 * 4.0 * 60.0 / bpm;
    let mut clip = warped_clip(Path::new("/nowhere/leader.wav"), seconds, bars);
    clip.length = bars * BAR;
    let placed = rlive_project::ArrangementClip::new(clip, start);
    let id = placed.id;
    let track = s.project.tracks[1].id;
    s.project.tracks[1].place_clip(placed);
    (track, id)
}

#[test]
fn a_leader_clip_sets_the_set_tempo_and_locks_the_field() {
    let (_e, mut s) = session();
    let before = s.project.tempo;
    let (track, id) = leader_track(&mut s, BAR, 2, 90.0);
    assert!(s.arr_can_lead(&[(track, id)]));
    assert!(!s.tempo_is_led());
    s.arr_set_tempo_leader(&[(track, id)], true);
    assert!(s.tempo_is_led() && s.arr_is_leading(&[(track, id)]));
    let maps = build::time_maps(&s.project);
    assert!((maps.tempo.bpm_at(1.5 * BAR as f64) - 90.0).abs() < 1e-6);
    assert!((maps.tempo.bpm_at(0.5 * BAR as f64) - before).abs() < 1e-6);
    // The tempo field and the Main track's envelope belong to the clip now.
    s.set_tempo(200.0);
    assert_eq!(s.project.tempo, before);
    let target = rlive_core::param::transport::tempo_target();
    assert!(!s.env_points(ChainRef::Master, target).is_empty());
    s.set_envelope(ChainRef::Master, target, Vec::new());
    assert!(!s.env_points(ChainRef::Master, target).is_empty());
    assert!(s.project.arrangement.tempo.points.is_empty());
}

#[test]
fn unfollowing_keeps_the_tempo_and_frees_the_envelope() {
    let (_e, mut s) = session();
    let (track, id) = leader_track(&mut s, 0, 2, 96.0);
    s.arr_set_tempo_leader(&[(track, id)], true);
    let target = rlive_core::param::transport::tempo_target();
    let generated = s.env_points(ChainRef::Master, target).to_vec();
    assert!(s.unfollow_tempo_automation());
    assert!(!s.tempo_is_led());
    assert_eq!(s.project.arrangement.tempo.points, generated);
    s.set_envelope(ChainRef::Master, target, Vec::new());
    assert!(s.project.arrangement.tempo.points.is_empty(), "editable");
}

#[test]
fn undo_takes_back_a_leader() {
    let (_e, mut s) = session();
    let (track, id) = leader_track(&mut s, 0, 2, 96.0);
    s.commit_now();
    s.arr_set_tempo_leader(&[(track, id)], true);
    s.commit_now();
    assert!(s.tempo_is_led());
    assert!(s.undo());
    assert!(!s.tempo_is_led());
    let target = rlive_core::param::transport::tempo_target();
    assert!(s.env_points(ChainRef::Master, target).is_empty());
}
