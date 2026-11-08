//! Automation Arm, recording gestures into envelopes, overrides and Re-Enable.

use super::*;
use rlive_core::param::{mixer, EnvTarget, ParamOwner, ParamRef, ACTIVATOR};
use rlive_core::{ParamId, PPQ};
use rlive_engine::{create_engine, mix_cell, Engine};
use rlive_project::{Breakpoint, Clip, ClipEnvelope, EnvMode, Envelope};

fn session() -> (Engine, Session) {
    let (engine, handle) = create_engine(48_000.0, 512);
    (
        engine,
        Session::new(handle, Project::new_default(), PluginCache::default()),
    )
}

fn render(engine: &mut Engine, s: &mut Session, frames: usize) {
    let frames = frames.next_multiple_of(512);
    rlive_testkit::render_chunks(frames, 512, |l, r| {
        engine.process_stereo(l, r);
        s.idle();
    });
}

/// One frame of the app: the mouse's state, then the undo commit, as the UI loop does them.
fn frame(s: &mut Session, mouse_down: bool) {
    s.pointer_frame(mouse_down);
    let busy = mouse_down || s.is_recording();
    s.commit_edits(busy);
}

fn volume_target(track: TrackId) -> EnvTarget {
    EnvTarget::Param(ParamRef {
        owner: ParamOwner::Track(track),
        id: mixer::VOLUME,
    })
}

fn device_target(device: DeviceId, id: u32) -> EnvTarget {
    EnvTarget::Param(ParamRef {
        owner: ParamOwner::Device(device),
        id: ParamId(id),
    })
}

/// A track's first device and a continuous parameter of it.
fn knob(s: &Session, t: TrackId) -> (DeviceId, u32) {
    let d = s.devices(ChainRef::Track(t))[0].id;
    let id = s.runtimes[&d]
        .params
        .iter()
        .find(|p| matches!(p.kind, rlive_core::ParamKind::Continuous { skew } if skew == 1.0))
        .unwrap()
        .id;
    (d, id)
}

fn envelope_of(s: &Session, track: TrackId, target: EnvTarget) -> Option<Envelope> {
    s.project
        .track(track)?
        .automation
        .iter()
        .find(|e| e.target == target)
        .cloned()
}

fn points(e: &Envelope) -> Vec<(f64, f64)> {
    e.points.iter().map(|p| (p.time, p.value)).collect()
}

fn range_of(s: &Session, t: TrackId, cell: CellIx) -> rlive_core::ParamRange {
    s.layouts[&ChainRef::Track(t)].spec(cell).unwrap().range
}

/// Starts recording and plays long enough for the engine to be in the pass.
fn start_pass(engine: &mut Engine, s: &mut Session) {
    s.set_automation_arm(true);
    assert!(s.start_recording());
    render(engine, s, 512);
}

fn tick_now(s: &Session) -> f64 {
    s.engine.shared.tick.load()
}

fn stop_pass(engine: &mut Engine, s: &mut Session) {
    s.stop_recording();
    render(engine, s, 512);
    frame(s, false);
}

#[test]
fn automation_arm_alone_is_enough_to_record() {
    let (mut engine, mut s) = session();
    assert!(!s.start_recording(), "nothing armed, nothing to record");
    s.set_automation_arm(true);
    assert!(s.automation_arm());
    assert!(s.start_recording());
    render(&mut engine, &mut s, 512);
    assert!(s.is_recording());
    stop_pass(&mut engine, &mut s);
    assert!(!s.is_recording());
    assert!(
        s.project.tracks.iter().all(|t| t.automation.is_empty()),
        "moving nothing records nothing"
    );
}

#[test]
fn gestures_record_on_every_track_armed_or_not() {
    let (mut engine, mut s) = session();
    assert!(!s.project.tracks.iter().any(|t| t.arm));
    let (a, b) = (s.project.tracks[0].id, s.project.tracks[1].id);
    let (dev, param) = knob(&s, a);
    start_pass(&mut engine, &mut s);

    s.set_param(ChainRef::Track(a), dev, param, 0.7);
    if let Some(t) = s.project.track_mut(b) {
        t.volume_db = -12.0;
    }
    s.sync_mix(b);
    render(&mut engine, &mut s, 4096);
    stop_pass(&mut engine, &mut s);

    assert!(envelope_of(&s, a, device_target(dev, param)).is_some());
    assert!(envelope_of(&s, b, volume_target(b)).is_some());
}

#[test]
fn nothing_is_recorded_without_automation_arm() {
    let (mut engine, mut s) = session();
    let t = s.project.tracks[0].id;
    s.set_arm(t, true, false);
    assert!(s.start_recording());
    render(&mut engine, &mut s, 512);
    let (dev, param) = knob(&s, t);
    s.set_param(ChainRef::Track(t), dev, param, 0.7);
    render(&mut engine, &mut s, 4096);
    stop_pass(&mut engine, &mut s);
    assert!(s.project.track(t).unwrap().automation.is_empty());
}

#[test]
fn solo_does_not_record() {
    let (mut engine, mut s) = session();
    let t = s.project.tracks[0].id;
    start_pass(&mut engine, &mut s);
    if let Some(track) = s.project.track_mut(t) {
        track.solo = true;
    }
    s.sync_mix(t);
    render(&mut engine, &mut s, 2048);
    stop_pass(&mut engine, &mut s);
    assert!(s.project.track(t).unwrap().automation.is_empty());
    assert!(
        s.project.track(t).unwrap().solo,
        "the click still reached the mixer"
    );
}

#[test]
fn the_first_pass_latches_and_holds_after_release() {
    let (mut engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let (dev, param) = knob(&s, t);
    let chain = ChainRef::Track(t);
    let before = s.param_value(chain, dev, &s.runtimes[&dev].params[param as usize].clone());
    start_pass(&mut engine, &mut s);

    let start = tick_now(&s);
    for v in [0.2, 0.3, 0.4] {
        s.set_param(chain, dev, param, v);
        render(&mut engine, &mut s, 512);
        frame(&mut s, true);
    }
    // Let go: on a first pass the value holds to the end of the pass.
    frame(&mut s, false);
    render(&mut engine, &mut s, 4096);
    stop_pass(&mut engine, &mut s);

    let e = envelope_of(&s, t, device_target(dev, param)).expect("recorded");
    let last = e.points.last().unwrap();
    assert_eq!(last.value, 0.4, "held to the end");
    assert!(last.time > start + 100.0, "{last:?}");
    assert_eq!(e.points[0].time, 0.0, "reaches back to the start");
    assert_eq!(
        e.points[0].value, before,
        "and holds what it was before the gesture"
    );
    let moved_at = e.points.iter().find(|p| p.value != before).unwrap().time;
    assert!((moved_at - start).abs() < 21.0, "{moved_at} vs {start}");
}

#[test]
fn a_mouse_is_touch_and_the_envelope_comes_back_on_release() {
    let (mut engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let (dev, param) = knob(&s, t);
    let chain = ChainRef::Track(t);
    let cell = s.device_cell(chain, dev, param).unwrap();
    let target = device_target(dev, param);
    s.project.tracks[0].automation.push(Envelope {
        target,
        points: vec![Breakpoint::new(0.0, 0.25), Breakpoint::new(38400.0, 0.25)],
    });
    s.send_routes(TrackRef::Track(t));
    render(&mut engine, &mut s, 512);
    start_pass(&mut engine, &mut s);

    s.set_param(chain, dev, param, 0.8);
    render(&mut engine, &mut s, 1024);
    assert_eq!(s.layouts[&chain].base(cell), Some(0.8), "the knob is held");
    let touch_start = tick_now(&s);
    frame(&mut s, true);
    render(&mut engine, &mut s, 2048);
    frame(&mut s, false);
    render(&mut engine, &mut s, 1024);
    let effective = s.layouts[&chain].effective(cell).unwrap() as f64;
    let range = range_of(&s, t, cell);
    assert!(
        (range.from_normalized(effective) - 0.25).abs() < 1e-6,
        "the envelope is back as soon as the mouse lets go: {effective}"
    );
    stop_pass(&mut engine, &mut s);

    let e = envelope_of(&s, t, target).unwrap();
    assert!((e.value_at(touch_start, range).unwrap() - 0.8).abs() < 1e-6);
    assert!(
        (e.value_at(38_000.0, range).unwrap() - 0.25).abs() < 1e-6,
        "the rest of the envelope is untouched"
    );
    let last = e.points.last().unwrap();
    assert_eq!((last.time, last.value), (38400.0, 0.25));
    assert!(!s.layouts[&chain].state(cell).overridden());
}

#[test]
fn hardware_is_latch_even_over_an_existing_envelope() {
    let (mut engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let (dev, param) = knob(&s, t);
    let chain = ChainRef::Track(t);
    let cell = s.device_cell(chain, dev, param).unwrap();
    let target = device_target(dev, param);
    s.project.tracks[0].automation.push(Envelope {
        target,
        points: vec![Breakpoint::new(0.0, 0.25), Breakpoint::new(38400.0, 0.25)],
    });
    s.send_routes(TrackRef::Track(t));
    render(&mut engine, &mut s, 512);
    start_pass(&mut engine, &mut s);

    s.note_hardware_gesture(chain, cell);
    s.set_param(chain, dev, param, 0.8);
    render(&mut engine, &mut s, 512);
    // The mouse being up says nothing about a fader on a controller.
    frame(&mut s, false);
    render(&mut engine, &mut s, 4096);
    let range = range_of(&s, t, cell);
    let held = s.layouts[&chain].effective(cell).unwrap() as f64;
    assert!((range.from_normalized(held) - 0.8).abs() < 1e-5, "{held}");
    stop_pass(&mut engine, &mut s);

    let stop = tick_now(&s);
    let e = envelope_of(&s, t, target).unwrap();
    assert!((e.value_at(stop - 50.0, range).unwrap() - 0.8).abs() < 1e-6);
    assert!(
        (e.value_at(stop + 2000.0, range).unwrap() - 0.25).abs() < 1e-6,
        "the end of the pass hands back to what was there"
    );
}

#[test]
fn a_loop_ends_a_latch_and_the_next_lap_overwrites_the_last() {
    let (mut engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let (dev, param) = knob(&s, t);
    let chain = ChainRef::Track(t);
    let cell = s.device_cell(chain, dev, param).unwrap();
    let target = device_target(dev, param);
    s.set_loop(true, 0, PPQ * 2);
    start_pass(&mut engine, &mut s);

    // First lap: a first pass, so it latches to the end of the lap.
    s.set_param(chain, dev, param, 0.9);
    render(&mut engine, &mut s, 1024);
    frame(&mut s, true);
    frame(&mut s, false);
    render(&mut engine, &mut s, 48_000);
    assert!(
        envelope_of(&s, t, target).is_some(),
        "the lap went into the project at the wrap, so the next one plays it"
    );
    assert!(s.is_recording(), "still the same pass");

    // Second lap: now it's overwriting, so the mouse touches.
    s.set_param(chain, dev, param, 0.1);
    render(&mut engine, &mut s, 512);
    frame(&mut s, true);
    render(&mut engine, &mut s, 1024);
    frame(&mut s, false);
    render(&mut engine, &mut s, 48_000);
    stop_pass(&mut engine, &mut s);

    let e = envelope_of(&s, t, target).unwrap();
    let range = range_of(&s, t, cell);
    let low = e.points.iter().any(|p| p.value == 0.1);
    let high = e.points.iter().any(|p| p.value == 0.9);
    assert!(low && high, "{:?}", points(&e));
    assert!((e.value_at(PPQ as f64 * 2.0 - 1.0, range).unwrap() - 0.9).abs() < 1e-3);
}

#[test]
fn recording_thins_what_it_records() {
    let (mut engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let (dev, param) = knob(&s, t);
    let chain = ChainRef::Track(t);
    let target = device_target(dev, param);
    start_pass(&mut engine, &mut s);
    let start = tick_now(&s);
    for i in 0..=100 {
        s.set_param(chain, dev, param, i as f64 / 100.0 * 0.9);
        render(&mut engine, &mut s, 512);
        frame(&mut s, true);
    }
    let end = tick_now(&s);
    frame(&mut s, false);
    stop_pass(&mut engine, &mut s);

    let e = envelope_of(&s, t, target).unwrap();
    assert!(
        e.points.len() <= 6,
        "a steady ramp needs few points: {:?}",
        points(&e)
    );
    let range = range_of(&s, t, s.device_cell(chain, dev, param).unwrap());
    let middle = (start + end) / 2.0;
    let want = (middle - start) / (end - start) * 0.9;
    assert!(
        (e.value_at(middle, range).unwrap() - want).abs() < 0.03,
        "{}",
        e.value_at(middle, range).unwrap()
    );
}

#[test]
fn a_clicked_toggle_latches_since_there_is_no_holding_a_click() {
    let (mut engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let mute = EnvTarget::Param(ParamRef {
        owner: ParamOwner::Track(t),
        id: mixer::MUTE,
    });
    s.project.tracks[0].automation.push(Envelope {
        target: mute,
        points: vec![Breakpoint::new(0.0, 0.0), Breakpoint::new(38400.0, 0.0)],
    });
    s.send_routes(TrackRef::Track(t));
    render(&mut engine, &mut s, 512);
    start_pass(&mut engine, &mut s);
    let click = tick_now(&s);
    if let Some(track) = s.project.track_mut(t) {
        track.mute = true;
    }
    s.sync_mix(t);
    frame(&mut s, false);
    render(&mut engine, &mut s, 2048);
    stop_pass(&mut engine, &mut s);
    let e = envelope_of(&s, t, mute).unwrap();
    let range = range_of(&s, t, mix_cell::MUTE);
    assert_eq!(
        e.value_at(click + 50.0, range),
        Some(1.0),
        "{:?}",
        points(&e)
    );
    assert_eq!(e.value_at(click + 10_000.0, range), Some(0.0));
}

#[test]
fn a_pass_is_one_undo_step() {
    let (mut engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let (dev, param) = knob(&s, t);
    let chain = ChainRef::Track(t);
    let target = device_target(dev, param);
    start_pass(&mut engine, &mut s);
    for v in [0.2, 0.3, 0.4] {
        s.set_param(chain, dev, param, v);
        render(&mut engine, &mut s, 512);
        frame(&mut s, true);
    }
    if let Some(track) = s.project.track_mut(t) {
        track.pan = 0.5;
    }
    s.sync_mix(t);
    render(&mut engine, &mut s, 1024);
    frame(&mut s, false);
    stop_pass(&mut engine, &mut s);
    assert_eq!(s.history.undo_label(), Some("Record Automation"));
    assert!(envelope_of(&s, t, target).is_some());

    assert!(s.undo());
    assert!(
        envelope_of(&s, t, target).is_none(),
        "the whole pass is gone, parameters included"
    );
    render(&mut engine, &mut s, 512);
    let cell = s.device_cell(chain, dev, param).unwrap();
    assert!(!s.layouts[&chain].state(cell).automated(), "and the LED");
    assert!(!s.undo(), "that was the only step");

    assert!(s.redo());
    assert!(envelope_of(&s, t, target).is_some());
    render(&mut engine, &mut s, 512);
    assert!(s.layouts[&chain].state(cell).automated(), "it plays again");
}

#[test]
fn saving_and_loading_keeps_automation_and_it_plays() {
    let (_engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let (dev, param) = knob(&s, t);
    let chain = ChainRef::Track(t);
    let target = device_target(dev, param);
    let mut start = Breakpoint::new(0.0, 0.1);
    start.curve = 0.5;
    s.project.tracks[0].automation.push(Envelope {
        target,
        points: vec![start, Breakpoint::new(960.0, 0.9)],
    });
    let master_volume = EnvTarget::Param(ParamRef {
        owner: ParamOwner::Master,
        id: mixer::VOLUME,
    });
    s.project.master.automation.push(Envelope {
        target: master_volume,
        points: vec![Breakpoint::new(0.0, -6.0)],
    });
    let path = std::env::temp_dir().join(format!("rlive-a2-{}.rlive", std::process::id()));
    s.save(&path).unwrap();

    let (mut engine2, handle) = create_engine(48_000.0, 512);
    let mut loaded = Session::new(handle, Project::new_default(), PluginCache::default());
    loaded.load(&path).unwrap();
    std::fs::remove_file(&path).ok();
    assert_eq!(
        loaded.project.tracks[0].automation,
        s.project.tracks[0].automation
    );
    assert_eq!(
        loaded.project.master.automation,
        s.project.master.automation
    );
    render(&mut engine2, &mut loaded, 1024);
    let cell = loaded.device_cell(chain, dev, param).unwrap();
    assert!(loaded.layouts[&chain].state(cell).automated());
    let master_state = loaded.layouts[&ChainRef::Master].state(mix_cell::VOLUME);
    assert!(master_state.automated());
}

#[test]
fn a_knob_move_overrides_and_re_enable_restores_it() {
    let (mut engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let (dev, param) = knob(&s, t);
    let chain = ChainRef::Track(t);
    let cell = s.device_cell(chain, dev, param).unwrap();
    s.project.tracks[0].automation.push(Envelope {
        target: device_target(dev, param),
        points: vec![Breakpoint::new(0.0, 0.3)],
    });
    s.send_routes(TrackRef::Track(t));
    render(&mut engine, &mut s, 512);
    assert!(!s.any_overridden());

    s.set_param(chain, dev, param, 0.8);
    render(&mut engine, &mut s, 512);
    assert!(s.cell_state(chain, cell).overridden());
    assert!(s.any_overridden(), "the button lights up");

    s.re_enable_cell(chain, cell);
    render(&mut engine, &mut s, 512);
    assert!(!s.cell_state(chain, cell).overridden());
    assert!(!s.any_overridden());

    s.set_param(chain, dev, param, 0.8);
    render(&mut engine, &mut s, 512);
    assert!(s.any_overridden());
    s.re_enable_all();
    render(&mut engine, &mut s, 512);
    assert!(!s.any_overridden(), "the button does them all");
}

#[test]
fn relaunching_a_clip_with_automation_restores_its_parameters() {
    let (mut engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let (dev, param) = knob(&s, t);
    let chain = ChainRef::Track(t);
    let cell = s.device_cell(chain, dev, param).unwrap();
    let target = device_target(dev, param);
    s.project.tracks[0].automation.push(Envelope {
        target,
        points: vec![Breakpoint::new(0.0, 0.3)],
    });
    s.send_routes(TrackRef::Track(t));
    let mut clip = Clip::new_midi("env", [1, 2, 3], 1, s.project.time_signature);
    clip.envelopes.push(ClipEnvelope {
        envelope: Envelope {
            target,
            points: vec![Breakpoint::new(0.0, 0.5)],
        },
        mode: EnvMode::Automation,
        loop_region: None,
    });
    s.set_clip(t, 1, Some(clip));
    render(&mut engine, &mut s, 512);

    s.set_param(chain, dev, param, 0.8);
    render(&mut engine, &mut s, 512);
    assert!(s.cell_state(chain, cell).overridden());

    s.launch_clip(t, 1);
    render(&mut engine, &mut s, 512);
    assert!(!s.cell_state(chain, cell).overridden());
}

#[test]
fn nested_devices_and_activators_get_cells() {
    let (mut engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let mut rack = Device::rack(rlive_project::RackKind::Audio, "Rack");
    let mut chain = rlive_project::Chain::new("A");
    let inner = Device::builtin("rlive.compressor", "Inner");
    let inner_id = inner.id;
    chain.devices.push(inner);
    if let DeviceKind::Rack { chains, .. } = &mut rack.kind {
        chains.push(chain);
    }
    s.add_device(ChainRef::Track(t), None, rack);
    let param = s.runtimes[&inner_id].params[0].id;
    s.project.tracks[0].automation.extend([
        Envelope {
            target: device_target(inner_id, param),
            points: vec![Breakpoint::new(0.0, 0.5)],
        },
        Envelope {
            target: EnvTarget::Param(ParamRef {
                owner: ParamOwner::Device(inner_id),
                id: ACTIVATOR,
            }),
            points: vec![Breakpoint::new(0.0, 1.0)],
        },
        Envelope {
            target: EnvTarget::MidiCtrl(7),
            points: vec![Breakpoint::new(0.0, 1.0)],
        },
    ]);
    s.send_routes(TrackRef::Track(t));
    render(&mut engine, &mut s, 512);
    let owner = ChainRef::Track(t);
    let layout = &s.layouts[&owner];
    let cell = layout.device_cell(inner_id, param).unwrap();
    let activator = layout.activator_cell(inner_id).unwrap();
    assert!(layout.state(cell).automated());
    assert!(layout.state(activator).automated());
}

#[test]
fn removing_a_device_takes_its_envelopes_and_undo_brings_them_back() {
    let (mut engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let (dev, param) = knob(&s, t);
    let chain = ChainRef::Track(t);
    let target = device_target(dev, param);
    s.set_envelope(chain, target, vec![Breakpoint::new(0.0, 0.3)]);
    frame(&mut s, false);
    render(&mut engine, &mut s, 512);

    s.remove_device(chain, dev);
    frame(&mut s, false);
    assert!(envelope_of(&s, t, target).is_none());
    render(&mut engine, &mut s, 512);

    assert!(s.undo());
    assert!(envelope_of(&s, t, target).is_some());
    render(&mut engine, &mut s, 512);
    let cell = s.device_cell(chain, dev, param).unwrap();
    assert!(s.cell_state(chain, cell).automated(), "it plays again");
}

#[test]
fn drawing_an_envelope_is_undoable_and_clearing_it_stops_it() {
    let (mut engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let (dev, param) = knob(&s, t);
    let chain = ChainRef::Track(t);
    let cell = s.device_cell(chain, dev, param).unwrap();
    let target = device_target(dev, param);
    s.set_envelope(chain, target, vec![Breakpoint::new(0.0, 0.3)]);
    frame(&mut s, false);
    render(&mut engine, &mut s, 512);
    assert!(s.cell_state(chain, cell).automated());
    assert_eq!(s.history.undo_label(), Some("Edit Automation"));

    s.set_envelope(chain, target, Vec::new());
    frame(&mut s, false);
    render(&mut engine, &mut s, 512);
    assert!(!s.cell_state(chain, cell).automated());

    assert!(s.undo());
    render(&mut engine, &mut s, 512);
    assert!(s.cell_state(chain, cell).automated());
}

#[test]
fn a_new_bus_brings_routes_along() {
    let (mut engine, mut s) = session();
    let t = s.project.tracks[0].id;
    let (dev, param) = knob(&s, t);
    let chain = ChainRef::Track(t);
    let target = device_target(dev, param);
    s.project.tracks[0].automation.push(Envelope {
        target,
        points: vec![Breakpoint::new(0.0, 0.3)],
    });
    s.send_routes(TrackRef::Track(t));
    render(&mut engine, &mut s, 512);
    // A device swapping its bus must not lose the automation of the others.
    s.add_builtin(chain, "rlive.delay");
    render(&mut engine, &mut s, 512);
    let cell = s.device_cell(chain, dev, param).unwrap();
    assert!(s.cell_state(chain, cell).automated());
}
