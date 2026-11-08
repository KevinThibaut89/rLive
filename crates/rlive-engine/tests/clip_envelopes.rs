//! Clip envelopes that drive parameters, through the whole engine: modulation composing with
//! automation, the relative mixer controls, and Session automation.

use rlive_core::param::ModMode;
use rlive_core::{BlockStep, DeviceId, ParamInfo, ProcessContext, Processor, TrackId, PPQ};
use rlive_engine::*;
use rlive_testkit::assert_no_alloc;
use std::sync::Arc;

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;
const BAR: f64 = 4.0 * PPQ as f64;
/// Ticks per second at the default 120 BPM.
const TICKS_PER_SECOND: f64 = 2.0 * PPQ as f64;

/// Outputs a constant, so the gain of its track can be read off the audio.
struct Dc(f32);

impl Processor for Dc {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        ctx.for_each_step(|step| match step {
            BlockStep::Audio(audio) => {
                audio.left.fill(self.0);
                audio.right.fill(self.0);
            }
            BlockStep::Param { id: 0, value } => self.0 = value as f32,
            _ => {}
        });
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

struct Rig {
    id: TrackId,
    device: DeviceId,
    knob: CellIx,
    layout: BusLayout,
    handle: EngineHandle,
    engine: Engine,
}

fn rig() -> Rig {
    let (engine, mut handle) = create_engine(SR, BLOCK);
    let id = TrackId::new();
    let device = DeviceId::new();
    let mut track = Box::new(TrackNode::new(id, BLOCK));
    track
        .devices
        .push(Box::new(DeviceSlot::new(device, Box::new(Dc(0.5)))));
    let mut layout = BusLayout::for_track(&MixSettings::default());
    let params = [ParamInfo::continuous(0, "Level", 0.0, 1.0, 0.5, "")];
    layout.place_device(device, true, &params, |p| p.default);
    let knob = layout.device_cell(device, 0).unwrap();
    track.bus = layout.build();
    // A long, silent Session clip to launch: the clip's envelopes are all that matter here.
    track.set_slot(0, Some(midi_clip()));
    assert!(handle
        .send(EngineCommand::graph(GraphOp::AddTrack { index: 0, track }))
        .is_ok());
    Rig {
        id,
        device,
        knob,
        layout,
        handle,
        engine,
    }
}

fn midi_clip() -> Arc<ClipData> {
    Arc::new(ClipData::Midi(MidiClip::new(
        8 * BAR as u64,
        Vec::new(),
        true,
    )))
}

fn linear(min: f64, max: f64) -> rlive_core::ParamRange {
    rlive_core::ParamRange {
        min,
        max,
        skew: 1.0,
        quantized: false,
    }
}

fn linked_curve(range: rlive_core::ParamRange, points: &[(f64, f64)], length: f64) -> ClipCurve {
    ClipCurve::new(
        EnvCurve::new(range, points.iter().map(|&(t, v)| (t, v, 0.0))).unwrap(),
        EnvTiming::Linked {
            length,
            looped: true,
        },
    )
}

impl Rig {
    fn send(&mut self, cmd: EngineCommand) {
        assert!(self.handle.send(cmd).is_ok());
    }

    fn routes(&mut self, lanes: Vec<AutomationLane>, session: Vec<(u32, ClipRoutes)>) {
        let track = TrackRef::Track(self.id);
        let routes = Arc::new(TrackRoutes::new(lanes).with_clips(session, Vec::new()));
        self.send(EngineCommand::Param(ParamCmd::SetRoutes { track, routes }));
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

    fn launch(&mut self) {
        self.send(EngineCommand::Launch(LaunchCmd::LaunchClip {
            track: self.id,
            slot: 0,
        }));
    }

    fn play(&mut self) {
        self.send(EngineCommand::Transport(TransportCmd::Play));
    }

    fn render(&mut self, frames: usize) -> Vec<f32> {
        let engine = &mut self.engine;
        rlive_testkit::render_chunks(frames, BLOCK, |l, r| engine.process_stereo(l, r)).0
    }

    fn render_until(&mut self, tick: f64) {
        let frames = (tick / TICKS_PER_SECOND * SR).round() as usize;
        self.render(frames.next_multiple_of(BLOCK));
    }

    fn effective(&self, cell: CellIx) -> f32 {
        self.layout.effective(cell).unwrap()
    }

    fn state(&self, cell: CellIx) -> CellState {
        self.layout.state(cell)
    }

    fn poll_ended(&mut self) -> Vec<(CellIx, f64)> {
        let mut out = Vec::new();
        while let Some(ev) = self.handle.poll_event() {
            if let EngineEvent::ClipAutomationEnded { cell, value, .. } = ev {
                out.push((cell, value));
            }
        }
        out
    }
}

fn modulation(
    cell: CellIx,
    mode: ModMode,
    points: &[(f64, f64)],
    range: rlive_core::ParamRange,
) -> ClipLane {
    ClipLane::new(
        cell,
        linked_curve(range, points, 8.0 * BAR),
        ClipEnvAction::Modulate(mode),
    )
}

/// p. 537: a fade-out automation, and a modulation rising over it. The crescendo turns into the
/// fade where the two meet, and the automation is the ceiling all along.
#[test]
fn automation_is_absolute_and_modulation_works_inside_it() {
    let mut r = rig();
    let cell = mix_cell::VOLUME;
    let range = r.layout.spec(cell).unwrap().range;
    // The clip's automation falls from the top of the fader to the bottom over four bars.
    let fade = ClipLane::new(
        cell,
        linked_curve(range, &[(0.0, 6.0), (4.0 * BAR, -96.0)], 8.0 * BAR),
        ClipEnvAction::Automate,
    );
    let lift = modulation(
        cell,
        ModMode::Scale,
        &[(0.0, 0.0), (4.0 * BAR, 1.0)],
        linear(0.0, 1.0),
    );
    r.routes(Vec::new(), vec![(0, ClipRoutes::new(vec![fade, lift]))]);
    r.play();
    r.launch();
    r.render(BLOCK);
    let launch_tick = 0.0; // Nothing was playing, so it launches straight away.
    let mut seen = Vec::new();
    for q in 1..=15 {
        let at = launch_tick + q as f64 * BAR / 4.0;
        let frames = (BAR / 4.0 / TICKS_PER_SECOND * SR).round() as usize;
        r.render(frames.next_multiple_of(BLOCK));
        let auto_pos = 1.0 - (at / (4.0 * BAR)).min(1.0);
        seen.push((at, r.effective(cell) as f64, auto_pos));
    }
    let levels: Vec<f64> = seen.iter().map(|&(_, e, _)| e).collect();
    let peak = levels
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .unwrap()
        .0;
    assert!(
        peak > 0 && peak < levels.len() - 1,
        "a crescendo into the fade: {levels:?}"
    );
    assert!(
        levels[..peak].windows(2).all(|w| w[0] <= w[1]),
        "{levels:?}"
    );
    assert!(
        levels[peak..].windows(2).all(|w| w[0] >= w[1]),
        "{levels:?}"
    );
    for &(at, effective, auto_pos) in &seen {
        assert!(
            effective <= auto_pos + 0.01,
            "at {at}: {effective} over {auto_pos}"
        );
    }
    assert!(r.state(cell).modulated());
}

#[test]
fn the_modulation_is_a_share_of_where_automation_puts_the_control() {
    let mut r = rig();
    let cell = mix_cell::VOLUME;
    let range = r.layout.spec(cell).unwrap().range;
    let fade = ClipLane::new(
        cell,
        linked_curve(range, &[(0.0, -45.0)], 8.0 * BAR),
        ClipEnvAction::Automate,
    );
    let half = modulation(cell, ModMode::Scale, &[(0.0, 0.5)], linear(0.0, 1.0));
    r.routes(Vec::new(), vec![(0, ClipRoutes::new(vec![fade, half]))]);
    r.play();
    r.launch();
    r.render_until(1000.0);
    let knob = range.to_normalized(-45.0);
    let effective = r.effective(cell) as f64;
    assert!(
        (effective - knob * 0.5).abs() < 0.01,
        "{effective} vs {knob}"
    );
    assert!(r.state(cell).modulated());
    assert!(!r.state(cell).overridden());
}

#[test]
fn the_send_modulation_can_only_turn_a_send_down() {
    let mut r = rig();
    let cell = mix_cell::send(0);
    r.set(cell, -24.0, false);
    let quarter = modulation(cell, ModMode::Scale, &[(0.0, 0.25)], linear(0.0, 1.0));
    r.routes(Vec::new(), vec![(0, ClipRoutes::new(vec![quarter]))]);
    r.play();
    r.launch();
    r.render_until(1000.0);
    let range = r.layout.spec(cell).unwrap().range;
    let want = range.to_normalized(-24.0) * 0.25;
    assert!((r.effective(cell) as f64 - want).abs() < 0.01);
}

fn pan_effect(knob: f64, modulation_amount: f64) -> f64 {
    let mut r = rig();
    let cell = mix_cell::PAN;
    r.set(cell, knob, false);
    let lane = modulation(
        cell,
        ModMode::Intensity,
        &[(0.0, modulation_amount)],
        linear(-1.0, 1.0),
    );
    r.routes(Vec::new(), vec![(0, ClipRoutes::new(vec![lane]))]);
    r.play();
    r.launch();
    r.render_until(1000.0);
    let range = r.layout.spec(cell).unwrap().range;
    range.from_normalized(r.effective(cell) as f64)
}

#[test]
fn pan_modulation_reaches_full_range_from_the_centre_and_nothing_from_hard_over() {
    assert!((pan_effect(0.0, 1.0) - 1.0).abs() < 1e-3);
    assert!((pan_effect(0.0, -1.0) + 1.0).abs() < 1e-3);
    assert!((pan_effect(0.0, 0.5) - 0.5).abs() < 1e-3);
    // Half left: half the reach either way.
    assert!((pan_effect(-0.5, 1.0) - 0.0).abs() < 1e-3);
    assert!((pan_effect(-0.5, -1.0) + 1.0).abs() < 1e-3);
    // Hard left: no effect at all.
    assert!((pan_effect(-1.0, 1.0) + 1.0).abs() < 1e-3);
    assert!((pan_effect(1.0, -1.0) - 1.0).abs() < 1e-3);
}

#[test]
fn a_device_control_is_modulated_relative_to_its_knob() {
    let mut r = rig();
    let knob = r.knob;
    r.set(knob, 0.3, false);
    let up = modulation(knob, ModMode::Additive, &[(0.0, 0.25)], linear(-1.0, 1.0));
    r.routes(Vec::new(), vec![(0, ClipRoutes::new(vec![up]))]);
    r.play();
    r.launch();
    let out = r.render(8 * BLOCK);
    let last = *out.last().unwrap() as f64;
    assert!((last - 0.55).abs() < 1e-3, "0.3 + 0.25 is {last}");
    // Moving the knob moves what's modulated, the same distance.
    r.set(knob, 0.6, true);
    let out = r.render(4 * BLOCK);
    assert!((*out.last().unwrap() as f64 - 0.85).abs() < 1e-3);
    assert!(!r.state(knob).automated(), "a knob, not automation");
}

#[test]
fn session_automation_plays_while_launched_and_is_overridden_then_relaunched() {
    let mut r = rig();
    let knob = r.knob;
    let lane = ClipLane::new(
        knob,
        linked_curve(linear(0.0, 1.0), &[(0.0, 0.25)], 8.0 * BAR),
        ClipEnvAction::Automate,
    );
    r.routes(Vec::new(), vec![(0, ClipRoutes::new(vec![lane]))]);
    r.play();
    r.launch();
    let out = r.render(8 * BLOCK);
    assert!(
        (*out.last().unwrap() as f64 - 0.25).abs() < 1e-3,
        "automated"
    );
    assert!(r.state(knob).automated());

    r.set(knob, 0.8, true);
    let out = r.render(4 * BLOCK);
    assert!(
        (*out.last().unwrap() as f64 - 0.8).abs() < 1e-3,
        "overridden"
    );
    assert!(r.state(knob).overridden());

    let track = TrackRef::Track(r.id);
    r.send(EngineCommand::Param(ParamCmd::ReEnable {
        track,
        cell: Some(knob),
    }));
    let out = r.render(4 * BLOCK);
    assert!((*out.last().unwrap() as f64 - 0.25).abs() < 1e-3, "back");
    assert!(!r.state(knob).overridden());
}

#[test]
fn a_session_clip_stopping_leaves_its_automation_where_it_was() {
    let mut r = rig();
    let knob = r.knob;
    let lane = ClipLane::new(
        knob,
        linked_curve(linear(0.0, 1.0), &[(0.0, 0.25)], 8.0 * BAR),
        ClipEnvAction::Automate,
    );
    r.routes(Vec::new(), vec![(0, ClipRoutes::new(vec![lane]))]);
    r.play();
    r.launch();
    r.render((2.0 * BAR / TICKS_PER_SECOND * SR) as usize);
    assert!(r.state(knob).automated());

    r.send(EngineCommand::Launch(LaunchCmd::StopTrack(r.id)));
    let out = r.render((1.5 * BAR / TICKS_PER_SECOND * SR) as usize);
    assert!(
        (*out.last().unwrap() as f64 - 0.25).abs() < 1e-3,
        "it stays"
    );
    assert!(!r.state(knob).automated(), "and it's the knob's again");
    let ended = r.poll_ended();
    assert!(
        ended
            .iter()
            .any(|(c, v)| *c == knob && (*v - 0.25).abs() < 1e-6),
        "{ended:?}"
    );

    r.set(knob, 0.7, true);
    let out = r.render(4 * BLOCK);
    assert!((*out.last().unwrap() as f64 - 0.7).abs() < 1e-3);
    assert!(!r.state(knob).overridden(), "nothing to override");
}

#[test]
fn modulation_stops_with_its_clip() {
    let mut r = rig();
    let cell = mix_cell::VOLUME;
    let half = modulation(cell, ModMode::Scale, &[(0.0, 0.25)], linear(0.0, 1.0));
    r.routes(Vec::new(), vec![(0, ClipRoutes::new(vec![half]))]);
    r.play();
    r.launch();
    r.render((2.0 * BAR / TICKS_PER_SECOND * SR) as usize);
    assert!(r.state(cell).modulated());
    r.send(EngineCommand::Launch(LaunchCmd::StopTrack(r.id)));
    r.render((1.5 * BAR / TICKS_PER_SECOND * SR) as usize);
    assert!(!r.state(cell).modulated());
    let range = r.layout.spec(cell).unwrap().range;
    assert_eq!(r.effective(cell), range.to_normalized(0.0) as f32);
}

#[test]
fn clip_envelopes_never_allocate() {
    let mut r = rig();
    let knob = r.knob;
    let cell = mix_cell::VOLUME;
    let auto = ClipLane::new(
        knob,
        linked_curve(linear(0.0, 1.0), &[(0.0, 0.25), (BAR, 0.75)], 2.0 * BAR),
        ClipEnvAction::Automate,
    );
    let scale = modulation(
        cell,
        ModMode::Scale,
        &[(0.0, 0.25), (BAR, 1.0)],
        linear(0.0, 1.0),
    );
    r.routes(Vec::new(), vec![(0, ClipRoutes::new(vec![auto, scale]))]);
    r.play();
    r.launch();
    r.render(4 * BLOCK);
    let (mut left, mut right) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    for _ in 0..(3.0 * BAR / TICKS_PER_SECOND * SR / BLOCK as f64) as usize {
        assert_no_alloc(|| r.engine.process_stereo(&mut left, &mut right));
    }
    r.send(EngineCommand::Launch(LaunchCmd::StopTrack(r.id)));
    for _ in 0..(2.0 * BAR / TICKS_PER_SECOND * SR / BLOCK as f64) as usize {
        assert_no_alloc(|| r.engine.process_stereo(&mut left, &mut right));
    }
}

/// An Arrangement clip modulates what the track's automation lane sets.
#[test]
fn arrangement_clip_modulation_composes_with_the_lane() {
    let mut r = rig();
    let cell = mix_cell::VOLUME;
    let range = r.layout.spec(cell).unwrap().range;
    let lane = AutomationLane::new(cell, range, [(0.0, -45.0, 0.0)]).unwrap();
    let half = ClipRoutes::new(vec![modulation(
        cell,
        ModMode::Scale,
        &[(0.0, 0.5)],
        linear(0.0, 1.0),
    )]);
    let track = TrackRef::Track(r.id);
    let routes = Arc::new(TrackRoutes::new(vec![lane]).with_clips(Vec::new(), vec![(0, half)]));
    r.send(EngineCommand::Param(ParamCmd::SetRoutes { track, routes }));
    r.send(EngineCommand::Clip(ClipCmd::SetLane {
        track: r.id,
        lane: Arc::new(ArrangementLane {
            clips: vec![LaneClip {
                start: 0,
                end: 4 * BAR as u64,
                offset: 0,
                data: midi_clip(),
                fades: Default::default(),
            }],
        }),
    }));
    r.play();
    r.render_until(1000.0);
    let knob = range.to_normalized(-45.0);
    assert!((r.effective(cell) as f64 - knob * 0.5).abs() < 0.01);
    assert!(r.state(cell).automated() && r.state(cell).modulated());
    // Past the clip it's just the lane.
    r.render_until(4.0 * BAR + 1000.0);
    assert!((r.effective(cell) as f64 - knob).abs() < 0.01);
    assert!(!r.state(cell).modulated());
}

/// The device id is what a target addresses; the engine just needs it to have been placed.
#[test]
fn the_cell_is_found_by_the_device_it_belongs_to() {
    let r = rig();
    assert_eq!(r.layout.device_cell(r.device, 0), Some(r.knob));
}
