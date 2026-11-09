//! Grooves and tuning systems through the Session: Commit, Extract, Retune, Bypass and layouts.

use super::*;
use rlive_core::{GrooveHit, PPQ};
use rlive_engine::{create_engine, Engine};
use rlive_project::{
    Clip, ClipContent, EnvMode, Groove, GroovePattern, NoteData, Track, TrackKind, Tuning,
    TuningLayout, Warp, WarpMarker,
};
use std::path::{Path, PathBuf};

const SIXTEENTH: u64 = PPQ / 4;

fn session() -> (Engine, Session) {
    let (engine, handle) = create_engine(48_000.0, 512);
    let mut p = Project::empty();
    p.tracks.push(Track::new(
        "MIDI",
        TrackKind::Midi,
        [1, 2, 3],
        p.scenes.len(),
    ));
    (engine, Session::new(handle, p, PluginCache::default()))
}

fn track_id(s: &Session) -> TrackId {
    s.project.tracks[0].id
}

fn slot_ref(s: &Session) -> ClipRef {
    ClipRef::slot(track_id(s), 0)
}

fn midi_clip(notes: Vec<NoteData>) -> Clip {
    Clip {
        length: PPQ * 4,
        looped: true,
        content: ClipContent::midi(notes),
        ..Clip::default()
    }
}

fn swing_groove() -> Groove {
    Groove::new(
        "Swing",
        GroovePattern {
            length: PPQ * 4,
            hits: (0..16)
                .map(|i| GrooveHit {
                    tick: (i * SIXTEENTH) as f64 + if i % 2 == 1 { 30.0 } else { 0.0 },
                    velocity: if i % 2 == 0 { 1.0 } else { 0.6 },
                })
                .collect(),
        },
    )
}

fn setup_midi(notes: Vec<NoteData>) -> (Engine, Session, Groove) {
    let (engine, mut s) = session();
    let groove = swing_groove();
    s.project.groove_pool.grooves.push(groove.clone());
    let t = track_id(&s);
    s.set_clip(t, 0, Some(midi_clip(notes)));
    (engine, s, groove)
}

#[test]
fn a_clip_with_a_groove_keeps_its_notes_until_committed() {
    let (_e, mut s, groove) = setup_midi(vec![NoteData::new(SIXTEENTH, 100, 60, 0.5)]);
    let r = slot_ref(&s);
    assert!(s.set_clip_groove(&[r], Some(groove.id)));
    assert_eq!(s.clip_at(r).unwrap().notes()[0].start, SIXTEENTH);

    assert!(s.commit_groove(r));
    let clip = s.clip_at(r).unwrap();
    assert_eq!(clip.notes()[0].start, SIXTEENTH + 30);
    assert_eq!(clip.groove, None, "the chooser goes back to none");
}

#[test]
fn commit_is_undone_in_one_step() {
    let (_e, mut s, groove) = setup_midi(vec![NoteData::new(SIXTEENTH, 100, 60, 0.5)]);
    let r = slot_ref(&s);
    s.set_clip_groove(&[r], Some(groove.id));
    s.commit_now();
    s.commit_groove(r);
    s.commit_now();
    assert!(s.undo());
    let clip = s.clip_at(r).unwrap();
    assert_eq!(clip.notes()[0].start, SIXTEENTH);
    assert_eq!(clip.groove, Some(groove.id));
}

#[test]
fn committing_nothing_does_nothing() {
    let (_e, mut s, _) = setup_midi(vec![]);
    assert!(!s.commit_groove(slot_ref(&s)));
}

#[test]
fn hot_swap_replaces_the_groove_for_every_clip() {
    let (_e, mut s, groove) = setup_midi(vec![NoteData::new(0, 10, 60, 0.5)]);
    let r = slot_ref(&s);
    s.set_clip_groove(&[r], Some(groove.id));
    let replacement = Groove::new("New", GroovePattern::default());
    let new_id = replacement.id;
    s.swap_groove(groove.id, replacement);
    assert_eq!(s.clip_at(r).unwrap().groove, Some(new_id));
    assert!(s.project.groove_pool.get(groove.id).is_none());
    assert!(s.groove_in_use(new_id));
}

#[test]
fn removing_a_groove_frees_its_clips() {
    let (_e, mut s, groove) = setup_midi(vec![NoteData::new(0, 10, 60, 0.5)]);
    let r = slot_ref(&s);
    s.set_clip_groove(&[r], Some(groove.id));
    s.remove_groove(groove.id);
    assert_eq!(s.clip_at(r).unwrap().groove, None);
    assert!(s.project.groove_pool.grooves.is_empty());
    assert!(!s.any_groove_in_use());
}

#[test]
fn global_amount_is_kept_in_range_and_the_control_bar_slider_follows_use() {
    let (_e, mut s, groove) = setup_midi(vec![NoteData::new(0, 10, 60, 0.5)]);
    s.set_groove_amount(2.0);
    assert_eq!(s.project.groove_pool.amount, 1.3);
    assert!(!s.any_groove_in_use());
    let r = slot_ref(&s);
    s.set_clip_groove(&[r], Some(groove.id));
    assert!(s.any_groove_in_use());
}

#[test]
fn extracting_a_clip_makes_a_groove_of_what_plays() {
    let (_e, mut s, _) = setup_midi(vec![
        NoteData::new(0, 10, 60, 1.0),
        NoteData::new(SIXTEENTH + 25, 10, 60, 0.5),
    ]);
    let id = s.extract_groove(slot_ref(&s)).unwrap();
    let g = s.project.groove_pool.get(id).unwrap();
    assert_eq!(g.pattern.length, PPQ * 4);
    assert_eq!(g.pattern.hits.len(), 2);
    assert_eq!(g.pattern.hits[1].tick, (SIXTEENTH + 25) as f64);
    // The new groove plays the clip's feel back onto a straight clip.
    let map = g.map(1.0);
    assert_eq!(
        map.place(SIXTEENTH as f64, 0.5, 0.0).0,
        (SIXTEENTH + 25) as f64
    );
}

#[test]
fn a_groove_becomes_a_clip_for_editing() {
    let (_e, mut s, groove) = setup_midi(vec![]);
    let t = track_id(&s);
    let empty_slot = 1;
    s.project.track_mut(t).unwrap().clips.resize(4, None);
    assert!(s.groove_to_clip(groove.id, t, empty_slot));
    let c = s.clip(t, empty_slot).unwrap();
    assert_eq!(c.notes().len(), 16);
    assert_eq!(c.notes()[1].start, SIXTEENTH + 30);
    assert_eq!(c.notes()[1].velocity, 0.6);
}

/// A warped clip of `seconds` of audio over `length` ticks, with a real file behind it.
fn wav_file(dir: &Path, name: &str, frames: usize) -> PathBuf {
    let left: Vec<f32> = (0..frames).map(|i| (i as f32 * 0.03).sin()).collect();
    let path = dir.join(name);
    rlive_devices::write_wav(&path, &left, &left, 48_000).unwrap();
    path
}

fn audio_setup(name: &str) -> (Engine, Session, Groove, PathBuf) {
    let dir = std::env::temp_dir().join(format!("rlive-a14-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (engine, mut s) = session();
    let groove = swing_groove();
    s.project.groove_pool.grooves.push(groove.clone());
    let path = wav_file(&dir, "take.wav", 96_000);
    s.samples.wait_for([path.as_path()]);
    let t = track_id(&s);
    let tid = t;
    let mut content = ClipContent::audio(&path, 0.0);
    if let ClipContent::Audio { warp, .. } = &mut content {
        *warp = Warp {
            enabled: true,
            markers: vec![
                WarpMarker {
                    seconds: 0.0,
                    tick: 0.0,
                },
                WarpMarker {
                    seconds: 2.0,
                    tick: (PPQ * 4) as f64,
                },
            ],
            ..Warp::default()
        };
    }
    let clip = Clip {
        length: PPQ * 4,
        looped: true,
        content,
        ..Clip::default()
    };
    s.set_clip(tid, 0, Some(clip));
    (engine, s, groove, dir)
}

#[test]
fn committing_audio_writes_warp_markers_and_a_volume_envelope() {
    let (_e, mut s, groove, dir) = audio_setup("commit");
    let r = slot_ref(&s);
    s.project.groove_pool.get_mut(groove.id).unwrap().velocity = 100.0;
    assert!(s.set_clip_groove(&[r], Some(groove.id)));
    assert!(s.commit_groove(r));
    let clip = s.clip_at(r).unwrap();
    let ClipContent::Audio { warp, .. } = &clip.content else {
        panic!()
    };
    // The grid's second sixteenth has moved late by the groove's offset.
    let moved = warp
        .markers
        .iter()
        .find(|m| (m.seconds - 2.0 * 240.0 / 3840.0).abs() < 1e-6)
        .unwrap();
    assert!(
        (moved.tick - (SIXTEENTH + 30) as f64).abs() < 1e-6,
        "{moved:?}"
    );
    assert!(warp.markers.len() > 2);
    assert_eq!(clip.groove, None);
    let volume = clip
        .envelopes
        .iter()
        .find(|e| matches!(e.envelope.target, rlive_core::EnvTarget::Clip(_)));
    let volume = volume.expect("a volume envelope");
    assert_eq!(volume.mode, EnvMode::Modulation);
    assert!(volume.envelope.points.len() > 2);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn committing_audio_with_negative_velocity_leaves_the_volume_alone() {
    let (_e, mut s, groove, dir) = audio_setup("negative");
    let r = slot_ref(&s);
    s.project.groove_pool.get_mut(groove.id).unwrap().velocity = -50.0;
    s.set_clip_groove(&[r], Some(groove.id));
    s.commit_groove(r);
    assert!(s.clip_at(r).unwrap().envelopes.is_empty());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_unwarped_clip_cannot_be_committed() {
    let (_e, mut s, groove, dir) = audio_setup("unwarped");
    let r = slot_ref(&s);
    if let ClipContent::Audio { warp, .. } = &mut s.clip_at_mut(r).unwrap().content {
        warp.enabled = false;
    }
    s.set_clip_groove(&[r], Some(groove.id));
    assert!(!s.commit_groove(r));
    std::fs::remove_dir_all(&dir).ok();
}

fn edo(n: usize) -> Tuning {
    Tuning::new(
        format!("{n} EDO"),
        "",
        (1..=n).map(|i| i as f64 * 1200.0 / n as f64).collect(),
    )
}

fn drum_track(s: &mut Session) -> TrackId {
    let mut t = Track::new("Drums", TrackKind::Midi, [1, 2, 3], s.project.scenes.len());
    t.devices.push(rlive_project::Device::rack(
        rlive_project::RackKind::Drum,
        "Drums",
    ));
    let id = t.id;
    s.project.tracks.push(t);
    id
}

#[test]
fn loading_a_tuning_leaves_notes_where_they_are() {
    let (_e, mut s, _) = setup_midi(vec![NoteData::new(0, 10, 67, 0.5)]);
    s.load_tuning(edo(24), false);
    assert_eq!(s.clip(track_id(&s), 0).unwrap().notes()[0].key, 67);
    assert!(s.track_tuning(track_id(&s)).is_some());
}

#[test]
fn retuning_keeps_the_pitch_and_is_undone() {
    let (_e, mut s, _) = setup_midi(vec![NoteData::new(0, 10, 67, 0.5)]);
    let t = track_id(&s);
    let before = rlive_project::tuning::equal_hz(67);
    s.commit_now();
    let tuning = edo(24);
    s.load_tuning(tuning.clone(), true);
    s.commit_now();
    let key = s.clip(t, 0).unwrap().notes()[0].key;
    let error = 1200.0 * (tuning.key_hz(key) / before).log2();
    assert!(error.abs() <= 25.0, "{error} cents");
    assert_ne!(key, 67, "the new key isn't where it started");
    assert!(s.undo());
    assert_eq!(s.clip(t, 0).unwrap().notes()[0].key, 67);
    assert!(s.tuning().is_none());
}

#[test]
fn removing_the_tuning_goes_back_to_twelve_notes() {
    let (_e, mut s, _) = setup_midi(vec![]);
    s.load_tuning(edo(19), false);
    s.clear_tuning(false);
    assert!(s.tuning().is_none());
}

#[test]
fn bypass_and_drum_racks_keep_a_track_out_of_the_tuning() {
    let (_e, mut s, _) = setup_midi(vec![]);
    s.load_tuning(edo(19), false);
    let t = track_id(&s);
    assert!(!s.track_bypasses_tuning(t));
    s.set_bypass_tuning(t, true);
    assert!(s.track_bypasses_tuning(t));
    assert!(s.track_tuning(t).is_none());
    s.set_bypass_tuning(t, false);

    let drums = drum_track(&mut s);
    assert!(s.track_bypasses_tuning(drums));
}

#[test]
fn retune_leaves_bypassed_tracks_alone() {
    let (_e, mut s, _) = setup_midi(vec![NoteData::new(0, 10, 67, 0.5)]);
    let t = track_id(&s);
    s.set_bypass_tuning(t, true);
    s.load_tuning(edo(24), true);
    assert_eq!(s.clip(t, 0).unwrap().notes()[0].key, 67);
}

#[test]
fn layouts_and_mpe_settings_are_saved_with_the_set() {
    let (_e, mut s, _) = setup_midi(vec![]);
    let t = track_id(&s);
    s.set_tuning_layout(t, TuningLayout::White);
    s.set_track_mpe_out(
        t,
        Some(rlive_core::MpeOutput::MultiChannel { first: 2, last: 6 }),
    );
    s.load_tuning(edo(19), false);
    let text = s.project.to_ron().unwrap();
    let back = Project::from_ron(&text).unwrap();
    assert_eq!(back.tracks[0].tuning_layout, TuningLayout::White);
    assert_eq!(
        back.tracks[0].mpe_out,
        Some(rlive_core::MpeOutput::MultiChannel { first: 2, last: 6 })
    );
    assert_eq!(back.tuning.unwrap().steps(), 19);
}

#[test]
fn a_set_with_a_groove_saves_and_reloads_its_pattern() {
    let (_e, s, groove) = setup_midi(vec![]);
    let text = s.project.to_ron().unwrap();
    let back = Project::from_ron(&text).unwrap();
    assert_eq!(back.groove_pool.grooves[0].pattern, groove.pattern);
}

#[test]
fn an_mpe_controller_input_is_locked_to_all_channels() {
    let (_e, mut s, _) = setup_midi(vec![]);
    let t = track_id(&s);
    let route = rlive_project::Route::External(rlive_project::ExternalRoute::Midi {
        port: Some("Seaboard".into()),
        channel: None,
    });
    s.set_input(&[t], route, rlive_project::Tap::default());
    let hw = AudioHardware::default();
    assert_eq!(s.input_channels(t, &hw).len(), 17);
    s.mpe_ports.insert("Seaboard".into());
    assert_eq!(s.input_channels(t, &hw).len(), 1);
}
