//! The lanes' edits on a session: the envelope round trip, undo as one step, tempo, clearing and
//! envelopes travelling with clips.

use super::*;
use rlive_core::param::{mixer, transport, EnvTarget, ParamId, ParamOwner, ParamRef};
use rlive_core::PPQ;
use rlive_engine::create_engine;
use rlive_project::{ArrangementClip, Breakpoint, Clip};

fn session() -> (rlive_engine::Engine, Session) {
    let (engine, handle) = create_engine(48_000.0, 512);
    (
        engine,
        Session::new(handle, Project::new_default(), PluginCache::default()),
    )
}

fn frame(s: &mut Session, mouse_down: bool) {
    s.pointer_frame(mouse_down);
    s.commit_edits(mouse_down);
}

fn volume(t: TrackId) -> EnvTarget {
    EnvTarget::Param(ParamRef {
        owner: ParamOwner::Track(t),
        id: mixer::VOLUME,
    })
}

#[test]
fn an_envelope_round_trips_through_set_envelope() {
    let (_e, mut s) = session();
    let t = s.project.tracks[0].id;
    let chain = ChainRef::Track(t);
    let target = volume(t);
    let points = vec![Breakpoint::new(0.0, -12.0), Breakpoint::new(960.0, -3.0)];
    s.set_envelope(chain, target, points.clone());
    assert_eq!(s.env_points(chain, target), points.as_slice());
    assert!(s.is_automated(chain, target));
    s.set_envelope(chain, target, Vec::new());
    assert!(!s.is_automated(chain, target));
}

#[test]
fn a_whole_gesture_is_one_undo_step() {
    let (_e, mut s) = session();
    let t = s.project.tracks[0].id;
    let chain = ChainRef::Track(t);
    let target = volume(t);
    let before = s.env_points(chain, target).to_vec();
    for i in 0..30 {
        s.set_envelope(chain, target, vec![Breakpoint::new(0.0, -i as f64)]);
        frame(&mut s, true);
    }
    frame(&mut s, false);
    assert_eq!(s.env_points(chain, target)[0].value, -29.0);
    assert!(s.undo());
    assert_eq!(s.env_points(chain, target), before.as_slice());
}

#[test]
fn the_tempo_goes_through_the_same_model_and_undoes() {
    let (_e, mut s) = session();
    let target = transport::tempo_target();
    let points = vec![Breakpoint::new(0.0, 100.0), Breakpoint::new(960.0, 140.0)];
    s.set_envelope(ChainRef::Master, target, points.clone());
    frame(&mut s, false);
    assert_eq!(s.project.arrangement.tempo.points, points);
    assert_eq!(s.env_points(ChainRef::Master, target), points.as_slice());
    assert_eq!(s.automated_targets(ChainRef::Master), [target]);
    assert!(s.undo());
    assert!(s.project.arrangement.tempo.points.is_empty());
}

#[test]
fn the_tempo_range_is_the_lane_scale_and_stays_valid() {
    let (_e, mut s) = session();
    s.set_tempo_range(90.0, 180.0);
    let p = s
        .env_param(ChainRef::Master, transport::tempo_target())
        .unwrap();
    assert_eq!((p.range.min, p.range.max), (90.0, 180.0));
    s.set_tempo_range(300.0, 100.0);
    let t = &s.project.arrangement.tempo;
    assert_eq!((t.min, t.max), (300.0, 301.0));
}

#[test]
fn deleting_automation_returns_the_parameter_to_its_default() {
    let (_e, mut s) = session();
    let t = s.project.tracks[0].id;
    let chain = ChainRef::Track(t);
    let target = volume(t);
    s.set_envelope(chain, target, vec![Breakpoint::new(0.0, -20.0)]);
    s.project.track_mut(t).unwrap().volume_db = -30.0;
    s.delete_automation(chain, target);
    assert!(!s.is_automated(chain, target));
    assert_eq!(s.project.track(t).unwrap().volume_db, 0.0);
}

#[test]
fn clearing_an_envelope_leaves_the_parameter_alone() {
    let (_e, mut s) = session();
    let t = s.project.tracks[0].id;
    let chain = ChainRef::Track(t);
    let target = volume(t);
    s.set_envelope(chain, target, vec![Breakpoint::new(0.0, -20.0)]);
    s.project.track_mut(t).unwrap().volume_db = -30.0;
    s.clear_envelope(chain, target);
    assert_eq!(s.project.track(t).unwrap().volume_db, -30.0);
}

#[test]
fn clear_all_reaches_every_track_and_the_tempo() {
    let (_e, mut s) = session();
    let t = s.project.tracks[0].id;
    s.set_envelope(
        ChainRef::Track(t),
        volume(t),
        vec![Breakpoint::new(0.0, -1.0)],
    );
    let tempo = transport::tempo_target();
    s.set_envelope(ChainRef::Master, tempo, vec![Breakpoint::new(0.0, 90.0)]);
    s.clear_all_envelopes();
    assert!(s.project.tracks[0].automation.is_empty());
    assert!(s.project.arrangement.tempo.points.is_empty());
}

#[test]
fn clear_all_of_a_device_leaves_the_mixer() {
    let (_e, mut s) = session();
    let t = s.project.tracks[0].id;
    let chain = ChainRef::Track(t);
    let d = s.devices(chain)[0].id;
    let target = EnvTarget::Param(ParamRef {
        owner: ParamOwner::Device(d),
        id: ParamId(s.runtimes[&d].params[0].id),
    });
    s.set_envelope(chain, target, vec![Breakpoint::new(0.0, 0.5)]);
    s.set_envelope(chain, volume(t), vec![Breakpoint::new(0.0, -1.0)]);
    s.clear_device_envelopes(chain, d);
    assert!(!s.is_automated(chain, target));
    assert!(s.is_automated(chain, volume(t)));
}

#[test]
fn the_catalog_lists_the_mixer_then_the_devices() {
    let (_e, s) = session();
    let t = s.project.tracks[0].id;
    let groups = s.env_catalog(ChainRef::Track(t));
    assert_eq!(groups[0].label, "Mixer");
    assert_eq!(groups[0].params[0].target, volume(t));
    assert!(groups.len() >= 2, "the default track has a device");
    assert!(groups[1].device.is_some());
    let master = s.env_catalog(ChainRef::Master);
    assert!(master[0]
        .params
        .iter()
        .any(|p| p.target == transport::tempo_target()));
}

fn track_with_clip(s: &mut Session) -> (TrackId, rlive_core::ArrClipId) {
    let t = s.project.tracks[0].id;
    let sig = s.project.time_signature;
    let placed = ArrangementClip::new(Clip::new_midi("C", [1, 1, 1], 1, sig), 0);
    let id = placed.id;
    s.arr_place(t, placed);
    (t, id)
}

#[test]
fn moving_a_clip_takes_its_automation_along() {
    let (_e, mut s) = session();
    let (t, id) = track_with_clip(&mut s);
    let chain = ChainRef::Track(t);
    let bar = (4 * PPQ) as f64;
    s.set_envelope(
        chain,
        volume(t),
        vec![Breakpoint::new(0.0, -6.0), Breakpoint::new(bar, -6.0)],
    );
    let to = 8 * PPQ;
    s.arr_move(
        &[ArrMove {
            track: t,
            id,
            to_track: t,
            start: to,
        }],
        false,
    );
    let points = s.env_points(chain, volume(t));
    assert!(points
        .iter()
        .any(|p| p.time == to as f64 && p.value == -6.0));
    assert!(points
        .iter()
        .any(|p| p.time == to as f64 + bar && p.value == -6.0));
}

#[test]
fn locked_envelopes_stay_on_the_timeline() {
    let (_e, mut s) = session();
    let (t, id) = track_with_clip(&mut s);
    let chain = ChainRef::Track(t);
    let points = vec![Breakpoint::new(0.0, -6.0), Breakpoint::new(960.0, -3.0)];
    s.set_envelope(chain, volume(t), points.clone());
    s.set_lock_envelopes(true);
    s.arr_move(
        &[ArrMove {
            track: t,
            id,
            to_track: t,
            start: 8 * PPQ,
        }],
        false,
    );
    assert_eq!(s.env_points(chain, volume(t)), points.as_slice());
}

#[test]
fn moving_a_control_says_which_one_it_was() {
    let (_e, mut s) = session();
    let t = s.project.tracks[0].id;
    let chain = ChainRef::Track(t);
    let d = s.devices(chain)[0].id;
    let id = s.runtimes[&d].params[0].id;
    s.set_param(chain, d, id, s.runtimes[&d].params[0].default);
    let (shown, target) = s.take_touched_control().unwrap();
    assert_eq!(shown, chain);
    assert_eq!(
        target,
        EnvTarget::Param(ParamRef {
            owner: ParamOwner::Device(d),
            id: ParamId(id),
        })
    );
    assert!(s.take_touched_control().is_none());
}
