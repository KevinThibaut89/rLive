//! Automation end to end: envelopes reach the output, and follow the timeline through loops,
//! seeks, overrides and swaps, all without allocating.

use rlive_core::{BlockStep, DeviceId, ParamInfo, ProcessContext, Processor, TrackId, PPQ};
use rlive_engine::*;
use rlive_testkit::assert_no_alloc;
use std::sync::Arc;

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;
/// Ticks per second at the default 120 BPM.
const TICKS_PER_SECOND: f64 = 2.0 * PPQ as f64;

/// Outputs its parameter as a constant, so what a device hears can be read off the audio.
struct Dc {
    level: f32,
}

impl Processor for Dc {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        ctx.for_each_step(|step| match step {
            BlockStep::Audio(audio) => {
                audio.left.fill(self.level);
                audio.right.fill(self.level);
            }
            BlockStep::Param { id: 0, value } => self.level = value as f32,
            _ => {}
        });
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

fn level_param() -> ParamInfo {
    ParamInfo::continuous(0, "Level", 0.0, 1.0, 0.5, "")
}

struct Rig {
    id: TrackId,
    cell: CellIx,
    layout: BusLayout,
    handle: EngineHandle,
    engine: Engine,
}

fn rig(params: &[ParamInfo]) -> Rig {
    let (engine, mut handle) = create_engine(SR, BLOCK);
    let id = TrackId::new();
    let device = DeviceId::new();
    let mut track = Box::new(TrackNode::new(id, BLOCK));
    track.devices.push(Box::new(DeviceSlot::new(
        device,
        Box::new(Dc { level: 0.5 }),
    )));
    let mut layout = BusLayout::for_track(&MixSettings::default());
    layout.place_device(device, true, params, |p| p.default);
    let cell = layout.device_cell(device, 0).unwrap();
    track.bus = layout.build();
    assert!(handle
        .send(EngineCommand::graph(GraphOp::AddTrack { index: 0, track }))
        .is_ok());
    Rig {
        id,
        cell,
        layout,
        handle,
        engine,
    }
}

impl Rig {
    fn send(&mut self, cmd: EngineCommand) {
        assert!(self.handle.send(cmd).is_ok());
    }

    fn lanes(&mut self, lanes: Vec<AutomationLane>) {
        let track = TrackRef::Track(self.id);
        let routes = Arc::new(TrackRoutes::new(lanes));
        self.send(EngineCommand::Param(ParamCmd::SetRoutes { track, routes }));
    }

    fn lane(&self, cell: CellIx, points: &[(f64, f64, f32)]) -> AutomationLane {
        let range = self.layout.spec(cell).unwrap().range;
        AutomationLane::new(cell, range, points.iter().copied()).unwrap()
    }

    fn transport(&mut self, cmd: TransportCmd) {
        self.send(EngineCommand::Transport(cmd));
    }

    fn set(&mut self, cell: CellIx, value: f64, gesture: bool) {
        let track = TrackRef::Track(self.id);
        self.send(EngineCommand::Param(ParamCmd::Set {
            track,
            cell,
            value,
            gesture,
        }));
    }

    fn render(&mut self, frames: usize) -> Vec<f32> {
        let engine = &mut self.engine;
        rlive_testkit::render_chunks(frames, BLOCK, |l, r| engine.process_stereo(l, r)).0
    }

    fn state(&self, cell: CellIx) -> CellState {
        self.layout.state(cell)
    }

    /// Swaps in a new copy of the track with hooks set, keeping its lanes.
    fn set_hooks(&mut self, hooks: Box<dyn ControlHooks>) {
        let device = DeviceId::new();
        let mut track = Box::new(TrackNode::new(self.id, BLOCK));
        track.devices.push(Box::new(DeviceSlot::new(
            device,
            Box::new(Dc { level: 0.5 }),
        )));
        let mut layout = BusLayout::for_track(&MixSettings::default());
        layout.place_device(device, true, &[level_param()], |p| p.default);
        let cell = layout.device_cell(device, 0).unwrap();
        assert_eq!(cell, self.cell);
        track.bus = layout.build();
        track.hooks = Some(hooks);
        let lane = AutomationLane::new(cell, layout.spec(cell).unwrap().range, [(0.0, 0.1, 0.0)]);
        track.routes = Arc::new(TrackRoutes::new(vec![lane.unwrap()]));
        self.send(EngineCommand::graph(GraphOp::RemoveTrack(self.id)));
        self.send(EngineCommand::graph(GraphOp::AddTrack { index: 0, track }));
        self.render(BLOCK);
    }
}

fn close(got: f32, want: f64, tolerance: f64, what: &str) {
    assert!(
        (got as f64 - want).abs() < tolerance,
        "{what}: {got} vs {want}"
    );
}

fn ticks_at(frame: usize) -> f64 {
    frame as f64 / SR * TICKS_PER_SECOND
}

/// The tick of the control step a frame belongs to.
fn step_tick(frame: usize) -> f64 {
    ticks_at(frame / CONTROL_STEP * CONTROL_STEP)
}

#[test]
fn a_device_hears_the_envelope_at_each_control_step() {
    let mut r = rig(&[level_param()]);
    let lane = r.lane(r.cell, &[(0.0, 0.0, 0.0), (3840.0, 1.0, 0.0)]);
    r.lanes(vec![lane]);
    r.transport(TransportCmd::Play);
    let frames = 24_000;
    let out = r.render(frames);
    for (frame, sample) in out.iter().enumerate().skip(BLOCK) {
        close(*sample, step_tick(frame) / 3840.0, 1e-4, "at its step");
    }
}

#[test]
fn the_rendered_gain_follows_the_volume_envelope() {
    let mut r = rig(&[level_param()]);
    // A 24 dB rise over two beats, then held, from the steady device output.
    r.set(r.cell, 1.0, false);
    let lane = r.lane(
        mix_cell::VOLUME,
        &[(0.0, -24.0, 0.0), (2.0 * PPQ as f64, 0.0, 0.0)],
    );
    r.lanes(vec![lane]);
    // Let the gain smoother reach the start of the envelope before playing it.
    r.render(9600);
    r.transport(TransportCmd::Play);
    let out = r.render(60_000);

    let (mut checked, mut last) = (0, 0.0f32);
    for (frame, sample) in out.iter().enumerate().step_by(32) {
        let tick = ticks_at(frame);
        if tick >= 2.0 * PPQ as f64 {
            break;
        }
        let db = -24.0 + 24.0 * tick / (2.0 * PPQ as f64);
        let want = rlive_core::db_to_gain(db as f32);
        close(
            *sample,
            want as f64,
            want as f64 * 0.02 + 0.002,
            "the curve",
        );
        assert!(*sample >= last - 1e-5, "goes up at frame {frame}");
        last = *sample;
        checked += 1;
    }
    assert!(checked > 100);
    close(out[59_999], 1.0, 1e-3, "and holds at the end");
}

#[test]
fn curved_segments_bend_the_way_the_shared_shape_does() {
    let mut r = rig(&[level_param()]);
    let lane = r.lane(r.cell, &[(0.0, 0.0, 1.0), (1920.0, 1.0, 0.0)]);
    r.lanes(vec![lane]);
    r.transport(TransportCmd::Play);
    let out = r.render(48_000);
    let half = 24_000;
    let want = rlive_core::envelope::curve_shape(step_tick(half) / 1920.0, 1.0);
    close(out[half], want, 1e-3, "halfway round the curve");
    assert!(out[half] < 0.2, "a slow start: {}", out[half]);
}

#[test]
fn a_stepped_parameter_holds_until_the_next_point() {
    let wave = ParamInfo::stepped(0, "Wave", &["A", "B", "C", "D"], 0);
    let mut r = rig(&[wave]);
    let lane = r.lane(
        r.cell,
        &[(0.0, 0.0, 0.0), (480.0, 3.0, 0.0), (960.0, 1.0, 0.0)],
    );
    r.lanes(vec![lane]);
    r.transport(TransportCmd::Play);
    let out = r.render(48_000);
    // The probe hears 0, 3, 1; nothing diagonal in between.
    let values: Vec<u32> = out.iter().map(|s| *s as u32).collect();
    let mut changes: Vec<u32> = values.clone();
    changes.dedup();
    assert_eq!(
        changes[..3],
        [0, 3, 1],
        "no diagonal passing through 1 or 2"
    );
    let first_three = values.iter().position(|v| *v == 3).unwrap();
    let expected = 480.0 / TICKS_PER_SECOND * SR;
    assert!(
        (first_three as f64 - expected).abs() <= CONTROL_STEP as f64 + 1.0,
        "{first_three} vs {expected}"
    );
}

#[test]
fn the_envelope_repeats_around_a_loop() {
    let mut r = rig(&[level_param()]);
    let lane = r.lane(r.cell, &[(0.0, 0.0, 0.0), (1920.0, 1.0, 0.0)]);
    r.lanes(vec![lane]);
    r.transport(TransportCmd::SetLoop(Some((0.0, 1920.0))));
    r.transport(TransportCmd::Play);
    let out = r.render(4 * 48_000);
    let lap = 48_000;
    for frame in (1024..lap).step_by(1000) {
        close(out[frame + lap], out[frame] as f64, 1e-3, "pass two");
        close(out[frame + 2 * lap], out[frame] as f64, 1e-3, "pass three");
    }
    assert!(out[lap - 512] > 0.9, "climbs to the end");
    assert!(out[lap + 512] < 0.1, "and starts over");
}

#[test]
fn a_seek_lands_on_the_envelope_there_playing_or_not() {
    let mut r = rig(&[level_param()]);
    let lane = r.lane(r.cell, &[(0.0, 0.0, 0.0), (9600.0, 1.0, 0.0)]);
    r.lanes(vec![lane]);
    // Stopped: the control follows the playhead.
    r.transport(TransportCmd::Seek(4800.0));
    close(*r.render(BLOCK).last().unwrap(), 0.5, 1e-3, "stopped");
    // Playing: it moves straight there and carries on.
    r.transport(TransportCmd::Seek(2400.0));
    r.transport(TransportCmd::Play);
    let out = r.render(4800);
    close(out[0], 0.25, 1e-3, "seek forward");
    close(
        *out.last().unwrap(),
        (2400.0 + 192.0) / 9600.0,
        2e-3,
        "moves on",
    );
    r.transport(TransportCmd::Seek(9600.0));
    close(
        r.render(BLOCK)[0],
        1.0,
        1e-3,
        "past the last point it holds",
    );
}

#[test]
fn a_launched_clip_punches_automation_out_until_back_to_arrangement() {
    let mut r = rig(&[level_param()]);
    let lane = r.lane(r.cell, &[(0.0, 0.0, 0.0), (1920.0, 1.0, 0.0)]);
    r.lanes(vec![lane]);
    r.transport(TransportCmd::SetQuantize(rlive_core::Quantize::None));
    let clip = Arc::new(ClipData::Midi(MidiClip::new(3840, Vec::new(), true)));
    let track = r.id;
    r.send(EngineCommand::Clip(ClipCmd::SetClip {
        track,
        slot: 0,
        clip: Some(clip),
    }));
    r.transport(TransportCmd::Play);
    let before = *r.render(12_000).last().unwrap();
    assert!(before > 0.2, "{before}");

    r.send(EngineCommand::Launch(LaunchCmd::LaunchClip {
        track,
        slot: 0,
    }));
    r.render(BLOCK);
    let held = *r.render(BLOCK).last().unwrap();
    let later = *r.render(24_000).last().unwrap();
    close(later, held as f64, 1e-6, "stops tracking its envelope");
    assert!(
        r.state(r.cell).automated() && !r.state(r.cell).overridden(),
        "punched out is not overridden"
    );

    r.send(EngineCommand::Launch(LaunchCmd::BackToArrangement));
    let resumed = *r.render(BLOCK).last().unwrap();
    assert!(resumed > later + 0.1, "{resumed} vs {later}");
}

#[test]
fn a_gesture_overrides_until_re_enabled() {
    let mut r = rig(&[level_param()]);
    let lane = r.lane(r.cell, &[(0.0, 0.25, 0.0), (9600.0, 0.25, 0.0)]);
    r.lanes(vec![lane]);
    r.transport(TransportCmd::Play);
    close(*r.render(BLOCK).last().unwrap(), 0.25, 1e-6, "automated");
    assert!(r.state(r.cell).automated());
    assert!(!r.state(r.cell).overridden());

    r.set(r.cell, 0.75, false);
    close(
        *r.render(BLOCK).last().unwrap(),
        0.25,
        1e-6,
        "not a gesture",
    );
    assert!(!r.layout.any_overridden());

    r.set(r.cell, 0.75, true);
    close(*r.render(BLOCK).last().unwrap(), 0.75, 1e-6, "overridden");
    assert!(r.state(r.cell).overridden());
    assert!(r.layout.any_overridden());

    let track = TrackRef::Track(r.id);
    r.send(EngineCommand::Param(ParamCmd::ReEnable {
        track,
        cell: None,
    }));
    close(*r.render(BLOCK).last().unwrap(), 0.25, 1e-6, "restored");
    assert!(!r.layout.any_overridden());
    assert!(r.state(r.cell).automated());
}

#[test]
fn re_enabling_one_parameter_leaves_the_others_overridden() {
    let mut r = rig(&[level_param()]);
    let (a, b) = (
        r.lane(r.cell, &[(0.0, 0.2, 0.0)]),
        r.lane(mix_cell::PAN, &[(0.0, 0.0, 0.0)]),
    );
    r.lanes(vec![a, b]);
    r.render(BLOCK);
    r.set(r.cell, 0.9, true);
    r.set(mix_cell::PAN, 0.5, true);
    r.render(BLOCK);
    assert!(r.state(r.cell).overridden());
    assert!(r.state(mix_cell::PAN).overridden());

    let track = TrackRef::Track(r.id);
    r.send(EngineCommand::Param(ParamCmd::ReEnable {
        track,
        cell: Some(r.cell),
    }));
    r.render(BLOCK);
    assert!(!r.state(r.cell).overridden());
    assert!(r.state(mix_cell::PAN).overridden());
    assert!(r.layout.any_overridden(), "the pan still needs its button");
}

#[test]
fn a_gesture_on_an_unautomated_parameter_overrides_nothing() {
    let mut r = rig(&[level_param()]);
    r.render(BLOCK);
    r.set(r.cell, 0.9, true);
    close(*r.render(BLOCK).last().unwrap(), 0.9, 1e-6, "just the knob");
    assert!(!r.state(r.cell).overridden());
    assert!(!r.state(r.cell).automated());
}

#[test]
fn swapping_routes_retires_the_old_and_frees_dropped_cells() {
    let mut r = rig(&[level_param()]);
    r.set(r.cell, 0.4, false);
    let lane = r.lane(r.cell, &[(0.0, 0.9, 0.0)]);
    r.lanes(vec![lane]);
    r.render(BLOCK);
    assert!(r.state(r.cell).automated());
    r.handle.collect_garbage();

    r.lanes(vec![]);
    let frames = r.render(BLOCK);
    close(*frames.last().unwrap(), 0.4, 1e-6, "back at the knob");
    assert!(!r.state(r.cell).automated(), "the LED goes out");
    assert_eq!(r.handle.collect_garbage(), 1, "the routes it replaced");
}

#[test]
fn a_macro_outranks_automation_at_the_same_step() {
    struct Hold(CellIx);
    impl ControlHooks for Hold {
        fn macros(&mut self, _: ControlStep, bus: &mut ParamBus) {
            bus.write_macro(self.0, 0.6);
        }
    }
    let mut r = rig(&[level_param()]);
    let hold = Box::new(Hold(r.cell));
    let lane = r.lane(r.cell, &[(0.0, 0.1, 0.0)]);
    r.lanes(vec![lane]);
    r.render(BLOCK);
    close(
        *r.render(BLOCK).last().unwrap(),
        0.1,
        1e-6,
        "without the Macro",
    );
    r.set_hooks(hold);
    close(
        *r.render(BLOCK).last().unwrap(),
        0.6,
        1e-6,
        "the Macro wins",
    );
}

#[test]
fn gestures_while_recording_are_reported_and_heard() {
    let mut r = rig(&[level_param()]);
    let lane = r.lane(r.cell, &[(0.0, 0.25, 0.0)]);
    r.lanes(vec![lane]);
    r.send(EngineCommand::Record(RecordCmd::SetAutomationArm(true)));
    r.send(EngineCommand::Record(RecordCmd::SetRecording(true)));
    r.transport(TransportCmd::Play);
    close(*r.render(BLOCK).last().unwrap(), 0.25, 1e-6, "automated");

    r.set(r.cell, 0.9, true);
    close(*r.render(BLOCK).last().unwrap(), 0.9, 1e-6, "the hand wins");
    assert!(
        !r.state(r.cell).overridden(),
        "recording replaces the envelope, it doesn't override it"
    );
    let track = TrackRef::Track(r.id);
    let cell = r.cell;
    let moved = r.handle.poll_event().expect("a move");
    assert_eq!(
        moved,
        EngineEvent::AutomationMoved {
            target: track,
            cell,
            value: 0.9,
            before: 0.25,
            tick: ticks_at(BLOCK),
        }
    );

    r.send(EngineCommand::Param(ParamCmd::EndGesture { track, cell }));
    close(*r.render(BLOCK).last().unwrap(), 0.25, 1e-6, "back it goes");
    let released = r.handle.poll_event().expect("a release");
    assert_eq!(
        released,
        EngineEvent::AutomationReleased {
            target: track,
            cell,
            tick: ticks_at(BLOCK * 2),
        }
    );
    r.send(EngineCommand::Record(RecordCmd::SetRecording(false)));
    r.render(BLOCK);
    assert!(matches!(
        r.handle.poll_event(),
        Some(EngineEvent::RecordingStopped { .. })
    ));
}

#[test]
fn outside_a_recording_a_gesture_is_just_an_override() {
    let mut r = rig(&[level_param()]);
    let lane = r.lane(r.cell, &[(0.0, 0.25, 0.0)]);
    r.lanes(vec![lane]);
    r.send(EngineCommand::Record(RecordCmd::SetAutomationArm(true)));
    r.transport(TransportCmd::Play);
    r.render(BLOCK);
    r.set(r.cell, 0.9, true);
    r.render(BLOCK);
    assert!(r.state(r.cell).overridden());
    assert!(r.handle.poll_event().is_none(), "nothing was recorded");
}

#[test]
fn nothing_allocates_as_automation_plays_records_and_swaps() {
    let mut r = rig(&[level_param()]);
    let points: Vec<_> = (0..4096)
        .map(|i| {
            let t = f64::from(i) * 2.0;
            (t, (f64::from(i) * 0.1).sin() * 0.5 + 0.5, 0.25_f32)
        })
        .collect();
    let lane = r.lane(r.cell, &points);
    let volume = r.lane(mix_cell::VOLUME, &[(0.0, -12.0, 0.5), (8000.0, 0.0, 0.0)]);
    r.lanes(vec![lane, volume]);
    r.transport(TransportCmd::SetLoop(Some((0.0, 2048.0))));
    r.transport(TransportCmd::Play);
    r.send(EngineCommand::Record(RecordCmd::SetAutomationArm(true)));
    let (mut l, mut rb) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    r.engine.process_stereo(&mut l, &mut rb);

    let track = TrackRef::Track(r.id);
    let (cell, spare) = (r.cell, r.lane(r.cell, &[(0.0, 0.5, 0.0)]));
    let routes = Arc::new(TrackRoutes::new(vec![spare]));
    for i in 0..400u32 {
        let value = f64::from(i % 10) / 10.0;
        match i % 8 {
            0 => r.send(EngineCommand::Record(RecordCmd::SetRecording(true))),
            1 => r.set(cell, value, true),
            2 => r.send(EngineCommand::Param(ParamCmd::EndGesture { track, cell })),
            3 => r.set(cell, value, true),
            4 => r.send(EngineCommand::Record(RecordCmd::SetRecording(false))),
            5 => r.send(EngineCommand::Param(ParamCmd::SetRoutes {
                track,
                routes: routes.clone(),
            })),
            6 => r.transport(TransportCmd::Seek(f64::from(i) * 7.0)),
            _ => r.send(EngineCommand::Param(ParamCmd::ReEnable {
                track,
                cell: None,
            })),
        }
        assert_no_alloc(|| r.engine.process_stereo(&mut l, &mut rb));
        r.handle.collect_garbage();
    }
}
