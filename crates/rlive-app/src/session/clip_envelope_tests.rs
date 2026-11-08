//! Clip envelopes from the Session: reaching the engine, undo, Linked and Unlinked, recording
//! into a playing clip, and clips as templates.

use super::*;
use rlive_core::param::{mixer, ClipParam, EnvTarget, ParamOwner, ParamRef};
use rlive_core::{ParamId, PPQ};
use rlive_engine::{create_engine, mix_cell, Engine};
use rlive_project::clip_envelope::SessionAutomationIn;
use rlive_project::warp_edit::WarpOp;
use rlive_project::{Breakpoint, Clip, ClipContent, EnvLoop, EnvMode, Warp, WarpMarker, WarpMode};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;
const BAR: u64 = 4 * PPQ;
static NEXT: AtomicUsize = AtomicUsize::new(0);

fn session() -> (Engine, Session) {
    let (engine, handle) = create_engine(SR, BLOCK);
    (
        engine,
        Session::new(handle, Project::new_default(), PluginCache::default()),
    )
}

fn render(engine: &mut Engine, s: &mut Session, frames: usize) -> Vec<f32> {
    let frames = frames.next_multiple_of(BLOCK);
    let (l, _) = rlive_testkit::render_chunks(frames, BLOCK, |l, r| {
        engine.process_stereo(l, r);
        s.idle();
    });
    l
}

fn frame(s: &mut Session, mouse_down: bool) {
    s.pointer_frame(mouse_down);
    s.commit_edits(mouse_down);
}

fn volume(track: TrackId) -> EnvTarget {
    EnvTarget::Param(ParamRef {
        owner: ParamOwner::Track(track),
        id: mixer::VOLUME,
    })
}

fn device_knob(s: &Session, t: TrackId) -> (DeviceId, u32) {
    let d = s.devices(ChainRef::Track(t))[0].id;
    let id = s.runtimes[&d]
        .params
        .iter()
        .find(|p| matches!(p.kind, rlive_core::ParamKind::Continuous { skew } if skew == 1.0))
        .unwrap()
        .id;
    (d, id)
}

fn device_target(device: DeviceId, id: u32) -> EnvTarget {
    EnvTarget::Param(ParamRef {
        owner: ParamOwner::Device(device),
        id: ParamId(id),
    })
}

fn points(points: &[(f64, f64)]) -> Vec<Breakpoint> {
    points.iter().map(|&(t, v)| Breakpoint::new(t, v)).collect()
}

fn midi_clip(s: &Session) -> Clip {
    Clip::new_midi("env", [1, 2, 3], 2, s.project.time_signature)
}

/// Puts a MIDI clip in slot 0 of the first track and plays it.
fn launched(engine: &mut Engine, s: &mut Session) -> (TrackId, ClipRef) {
    let t = s.project.tracks[0].id;
    let clip = midi_clip(s);
    s.set_clip(t, 0, Some(clip));
    s.commit_edits(false);
    s.launch_clip(t, 0);
    render(engine, s, BLOCK);
    (t, ClipRef::slot(t, 0))
}

#[test]
fn a_modulation_envelope_reaches_the_track_and_undo_takes_it_back() {
    let (mut engine, mut s) = session();
    let (t, cref) = launched(&mut engine, &mut s);
    let chain = ChainRef::Track(t);
    s.set_clip_envelope(cref, volume(t), EnvMode::Modulation, points(&[(0.0, 0.5)]));
    s.commit_edits(false);
    render(&mut engine, &mut s, 4 * BLOCK);
    assert!(s.cell_state(chain, mix_cell::VOLUME).modulated());
    let moved = s.modulated_cell_norm(chain, mix_cell::VOLUME).unwrap();
    assert!((moved - 0.5 * 0.941_176).abs() < 0.01, "{moved}");

    assert!(s.undo());
    render(&mut engine, &mut s, 4 * BLOCK);
    assert!(!s.cell_state(chain, mix_cell::VOLUME).modulated(), "undone");
    assert!(s.clip_at(cref).unwrap().envelopes.is_empty());

    assert!(s.redo());
    render(&mut engine, &mut s, 4 * BLOCK);
    assert!(s.cell_state(chain, mix_cell::VOLUME).modulated(), "redone");
}

#[test]
fn clearing_an_envelope_deletes_it_and_its_unlinking() {
    let (mut engine, mut s) = session();
    let (t, cref) = launched(&mut engine, &mut s);
    s.set_clip_envelope(cref, volume(t), EnvMode::Modulation, points(&[(0.0, 0.5)]));
    s.set_clip_envelope_loop(
        cref,
        volume(t),
        EnvMode::Modulation,
        Some(EnvLoop::new(0, 700)),
    );
    let e = s
        .clip_at(cref)
        .unwrap()
        .envelope(volume(t), EnvMode::Modulation);
    assert_eq!(e.unwrap().loop_region.unwrap().length, 700);
    s.set_clip_envelope(cref, volume(t), EnvMode::Modulation, Vec::new());
    render(&mut engine, &mut s, BLOCK);
    assert!(s.clip_at(cref).unwrap().envelopes.is_empty());
}

#[test]
fn an_arrangement_clip_only_hears_its_modulation_envelopes() {
    let (mut engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let chain = ChainRef::Track(t);
    let (dev, param) = device_knob(&s, t);
    let cell = s.device_cell(chain, dev, param).unwrap();
    let target = device_target(dev, param);
    let placed = rlive_project::ArrangementClip::new(midi_clip(&s), 0);
    let id = placed.id;
    s.arr_place(t, placed);
    let cref = ClipRef::Arr { track: t, id };
    s.set_clip_envelope(cref, target, EnvMode::Automation, points(&[(0.0, 0.123)]));
    s.play();
    render(&mut engine, &mut s, 4 * BLOCK);
    assert!(!s.cell_state(chain, cell).automated(), "ignored");

    s.set_clip_envelope(cref, target, EnvMode::Modulation, points(&[(0.0, -0.5)]));
    render(&mut engine, &mut s, 4 * BLOCK);
    assert!(s.cell_state(chain, cell).modulated(), "modulation counts");
}

#[test]
fn a_session_clips_automation_leaves_the_knob_where_it_stopped() {
    let (mut engine, mut s) = session();
    let (t, cref) = launched(&mut engine, &mut s);
    let chain = ChainRef::Track(t);
    let (dev, param) = device_knob(&s, t);
    let cell = s.device_cell(chain, dev, param).unwrap();
    let target = device_target(dev, param);
    s.set_clip_envelope(cref, target, EnvMode::Automation, points(&[(0.0, 0.4)]));
    render(&mut engine, &mut s, 4 * BLOCK);
    assert!(s.cell_state(chain, cell).automated());

    s.stop_track(t);
    render(&mut engine, &mut s, 100_000);
    assert!(!s.cell_state(chain, cell).automated());
    let stored = s.devices(chain)[0].stored_param(param).unwrap();
    assert!(
        (stored - 0.4).abs() < 1e-9,
        "the project has the knob where the clip left it: {stored}"
    );
}

#[test]
fn volume_automation_stopping_updates_the_mixer() {
    let (mut engine, mut s) = session();
    let (t, cref) = launched(&mut engine, &mut s);
    let chain = ChainRef::Track(t);
    s.set_clip_envelope(
        cref,
        volume(t),
        EnvMode::Automation,
        points(&[(0.0, -18.0)]),
    );
    render(&mut engine, &mut s, 4 * BLOCK);
    assert!(s.cell_state(chain, mix_cell::VOLUME).automated());
    s.stop_track(t);
    render(&mut engine, &mut s, 100_000);
    assert!((s.project.tracks[0].volume_db + 18.0).abs() < 1e-3);
}

struct TempFile(PathBuf);

impl TempFile {
    fn wav(frames: usize) -> Self {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("rlive-clip-env-{}-{n}.wav", std::process::id()));
        let left: Vec<f32> = (0..frames).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        rlive_devices::write_wav(&path, &left, &left, SR as u32).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        std::fs::remove_file(&self.0).ok();
    }
}

fn warped_audio(path: &Path) -> Clip {
    let mut content = ClipContent::audio(path, -6.0);
    if let ClipContent::Audio { warp, .. } = &mut content {
        *warp = Warp {
            enabled: true,
            mode: WarpMode::Beats,
            markers: vec![
                WarpMarker {
                    seconds: 0.0,
                    tick: 0.0,
                },
                WarpMarker {
                    seconds: 2.0,
                    tick: BAR as f64,
                },
            ],
            ..Warp::default()
        };
    }
    Clip {
        name: "a".into(),
        length: BAR,
        content,
        ..Clip::default()
    }
}

fn wait_for_sample(s: &mut Session, path: &Path) {
    for _ in 0..500 {
        s.samples.request(path);
        s.idle();
        if s.samples.get(path).is_some() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    panic!("the sample never loaded");
}

#[test]
fn linked_envelopes_stay_over_the_audio_and_unlinked_ones_dont() {
    let wav = TempFile::wav(96_000);
    let (_engine, mut s) = session();
    wait_for_sample(&mut s, wav.path());
    let t = s.project.tracks[0].id;
    let mut clip = warped_audio(wav.path());
    let gain = EnvTarget::Clip(ClipParam::Gain);
    let offset = EnvTarget::Clip(ClipParam::SampleOffset);
    clip.set_envelope_points(gain, EnvMode::Modulation, points(&[(1920.0, 0.5)]));
    clip.set_envelope_points(offset, EnvMode::Modulation, points(&[(1920.0, 1.0)]));
    clip.envelope_mut(offset, EnvMode::Modulation)
        .unwrap()
        .loop_region = Some(EnvLoop::new(0, 3840));
    s.set_clip(t, 0, Some(clip));
    s.commit_edits(false);
    let cref = ClipRef::slot(t, 0);
    // Stretch the audio so what was at beat 2 is now at beat 1.
    let cx_added = s.apply_warp_op(
        &[cref],
        &WarpOp::Add {
            seconds: 1.0,
            tick: 1920.0,
        },
    );
    assert!(cx_added);
    s.commit_edits(false);
    assert!(s.apply_warp_op(
        &[cref],
        &WarpOp::Move {
            tick: 1920.0,
            to: 960.0,
        },
    ));
    let c = s.clip_at(cref).unwrap();
    let linked = c.envelope(gain, EnvMode::Modulation).unwrap();
    let unlinked = c.envelope(offset, EnvMode::Modulation).unwrap();
    assert_eq!(
        linked.envelope.points[0].time, 960.0,
        "it stays over the audio"
    );
    assert_eq!(
        unlinked.envelope.points[0].time, 1920.0,
        "it has its own time"
    );
    s.commit_edits(false);
    assert!(s.undo());
    let c = s.clip_at(cref).unwrap();
    assert_eq!(
        c.envelope(gain, EnvMode::Modulation)
            .unwrap()
            .envelope
            .points[0]
            .time,
        1920.0,
        "undo puts it back too"
    );
}

#[test]
fn dropping_a_sample_on_a_clip_keeps_everything_else_about_it() {
    let (a, b) = (TempFile::wav(4_800), TempFile::wav(9_600));
    let (_engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let mut clip = warped_audio(a.path());
    let gain = EnvTarget::Clip(ClipParam::Gain);
    clip.set_envelope_points(gain, EnvMode::Modulation, points(&[(0.0, 0.5)]));
    if let ClipContent::Audio { transpose, .. } = &mut clip.content {
        *transpose = 3;
    }
    let before = clip.clone();
    s.set_clip(t, 0, Some(clip));
    let cref = ClipRef::slot(t, 0);
    assert!(s.replace_clip_sample(cref, b.path()));
    let after = s.clip_at(cref).unwrap();
    assert_eq!(after.envelopes, before.envelopes);
    let ClipContent::Audio {
        file, transpose, ..
    } = &after.content
    else {
        panic!("still audio");
    };
    assert_eq!(*transpose, 3);
    assert_eq!(file.path, b.path());
}

/// Starts the Session automation recording of a clip that's already playing on `t`.
fn start_session_pass(engine: &mut Engine, s: &mut Session) {
    s.set_automation_arm(true);
    s.set_session_automation_record(true);
    render(engine, s, BLOCK);
}

#[test]
fn nothing_records_on_a_track_that_isnt_armed_unless_all_tracks_is_set() {
    let (mut engine, mut s) = session();
    let (t, cref) = launched(&mut engine, &mut s);
    let chain = ChainRef::Track(t);
    let (dev, param) = device_knob(&s, t);
    let target = device_target(dev, param);
    start_session_pass(&mut engine, &mut s);
    s.set_param(chain, dev, param, 0.7);
    render(&mut engine, &mut s, 2 * BLOCK);
    assert!(
        s.clip_at(cref)
            .unwrap()
            .envelope(target, EnvMode::Automation)
            .is_none(),
        "not armed"
    );

    s.set_session_automation_in(SessionAutomationIn::AllTracks);
    render(&mut engine, &mut s, BLOCK);
    s.set_param(chain, dev, param, 0.6);
    render(&mut engine, &mut s, 4 * BLOCK);
    s.set_session_automation_record(false);
    render(&mut engine, &mut s, BLOCK);
    frame(&mut s, false);
    let e = s
        .clip_at(cref)
        .unwrap()
        .envelope(target, EnvMode::Automation);
    assert!(
        e.is_some_and(|e| !e.envelope.points.is_empty()),
        "All Tracks"
    );
}

#[test]
fn an_armed_tracks_gesture_records_into_its_playing_clip_as_one_undo_step() {
    let (mut engine, mut s) = session();
    let (t, cref) = launched(&mut engine, &mut s);
    let chain = ChainRef::Track(t);
    s.project.track_mut(t).unwrap().arm = true;
    let (dev, param) = device_knob(&s, t);
    let target = device_target(dev, param);
    start_session_pass(&mut engine, &mut s);
    let undo_before = s.project.tracks[0].clips.clone();
    s.set_param(chain, dev, param, 0.7);
    render(&mut engine, &mut s, 4 * BLOCK);
    s.set_param(chain, dev, param, 0.2);
    render(&mut engine, &mut s, 4 * BLOCK);
    s.set_session_automation_record(false);
    render(&mut engine, &mut s, BLOCK);
    frame(&mut s, false);
    let e = s
        .clip_at(cref)
        .unwrap()
        .envelope(target, EnvMode::Automation);
    let points: Vec<f64> = e.unwrap().envelope.points.iter().map(|p| p.value).collect();
    assert!(points.contains(&0.7) && points.contains(&0.2), "{points:?}");
    assert!(s.undo());
    assert_eq!(s.project.tracks[0].clips, undo_before);
}

#[test]
fn a_mouse_gesture_is_touch_once_the_clip_has_automation_and_latch_the_first_time() {
    let (mut engine, mut s) = session();
    let (t, cref) = launched(&mut engine, &mut s);
    let chain = ChainRef::Track(t);
    let (dev, param) = device_knob(&s, t);
    let target = device_target(dev, param);
    let cell = s.device_cell(chain, dev, param).unwrap();
    s.project.track_mut(t).unwrap().arm = true;
    start_session_pass(&mut engine, &mut s);

    // First time: released, it holds.
    s.set_param(chain, dev, param, 0.7);
    render(&mut engine, &mut s, BLOCK);
    frame(&mut s, true);
    render(&mut engine, &mut s, BLOCK);
    frame(&mut s, false);
    render(&mut engine, &mut s, BLOCK);
    assert!(!s.session_automation_record() || s.layouts[&chain].base(cell) == Some(0.7));
    // Let it come round and write its first pass.
    render(&mut engine, &mut s, (2.0 * 2.0 * SR) as usize + BLOCK);
    assert!(s
        .clip_at(cref)
        .unwrap()
        .envelope(target, EnvMode::Automation)
        .is_some());

    // Second time through: there's an envelope, so letting go hands the parameter back to it.
    s.set_param(chain, dev, param, 0.3);
    render(&mut engine, &mut s, BLOCK);
    frame(&mut s, true);
    render(&mut engine, &mut s, BLOCK);
    frame(&mut s, false);
    render(&mut engine, &mut s, 4 * BLOCK);
    assert!(s.cell_state(chain, cell).automated());
    let effective = s.effective_norm(chain, dev, param).unwrap();
    assert!(
        (effective - 0.7).abs() < 0.2,
        "back on what it recorded: {effective}"
    );
}

#[test]
fn a_session_record_pass_ends_with_every_gesture() {
    let (mut engine, mut s) = session();
    let (t, cref) = launched(&mut engine, &mut s);
    let chain = ChainRef::Track(t);
    s.project.track_mut(t).unwrap().arm = true;
    let (dev, param) = device_knob(&s, t);
    let target = device_target(dev, param);
    start_session_pass(&mut engine, &mut s);
    s.set_param(chain, dev, param, 0.9);
    render(&mut engine, &mut s, 2 * BLOCK);
    // The clip comes round: a latched value lasts to the end of the loop, not beyond.
    let bar_frames = (2.0 * 2.0 * SR) as usize;
    render(&mut engine, &mut s, bar_frames + 4 * BLOCK);
    let e = s
        .clip_at(cref)
        .unwrap()
        .envelope(target, EnvMode::Automation);
    let e = e.expect("written when it came round");
    assert_eq!(
        e.envelope.points.last().unwrap().time,
        s.clip_at(cref).unwrap().length as f64
    );
    s.set_session_automation_record(false);
    render(&mut engine, &mut s, BLOCK);
    assert!(!s.session_automation_record());
}
