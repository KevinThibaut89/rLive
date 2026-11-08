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
