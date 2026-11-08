//! The device tree and the render plan: racks, transactions and random edits while playing.
#![allow(clippy::vec_box)]

use rlive_core::{BlockStep, ChainId, DeviceId, ParamInfo, ProcessContext, Processor, TrackId};
use rlive_engine::*;
use rlive_testkit::assert_no_alloc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

/// A processor made of arithmetic, so a graph's output is known exactly.
struct Fx {
    gain: f32,
    add: f32,
    alive: Option<Arc<AtomicI64>>,
}

impl Fx {
    fn new(gain: f32, add: f32) -> Self {
        Self {
            gain,
            add,
            alive: None,
        }
    }

    fn counted(gain: f32, add: f32, alive: &Arc<AtomicI64>) -> Self {
        alive.fetch_add(1, Ordering::SeqCst);
        Self {
            gain,
            add,
            alive: Some(alive.clone()),
        }
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        if let Some(alive) = &self.alive {
            alive.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

impl Processor for Fx {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        for s in ctx.audio.left.iter_mut().chain(ctx.audio.right.iter_mut()) {
            *s = *s * self.gain + self.add;
        }
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

fn fx(gain: f32, add: f32) -> Box<DeviceSlot> {
    Box::new(DeviceSlot::new(
        DeviceId::new(),
        Box::new(Fx::new(gain, add)),
    ))
}

fn chain_of(devices: Vec<Box<DeviceSlot>>) -> Box<ChainNode> {
    let mut chain = ChainNode::new(ChainId::new(), BLOCK);
    chain.devices.extend(devices);
    Box::new(chain)
}

fn rack_of(kind: RackKind, chains: Vec<Box<ChainNode>>) -> Box<DeviceSlot> {
    let mut rack = RackNode::new(kind);
    rack.chains.extend(chains);
    Box::new(DeviceSlot::rack(DeviceId::new(), Box::new(rack)))
}

fn track_of(id: TrackId, devices: Vec<Box<DeviceSlot>>) -> Box<TrackNode> {
    let mut track = Box::new(TrackNode::new(id, BLOCK));
    track.devices.extend(devices);
    track
}

fn send(h: &mut EngineHandle, cmd: EngineCommand) {
    assert!(h.send(cmd).is_ok(), "command queue full");
}

fn add_track(h: &mut EngineHandle, track: Box<TrackNode>) {
    send(
        h,
        EngineCommand::graph(GraphOp::AddTrack { index: 0, track }),
    );
}

fn render(engine: &mut Engine) -> (Vec<f32>, Vec<f32>) {
    rlive_testkit::render_chunks(BLOCK, BLOCK, |l, r| engine.process_stereo(l, r))
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() <= 1e-3 * (1.0 + b.abs())
}

// ------------------------------------------------------------------ racks

#[test]
fn nested_racks_run_chains_in_parallel_and_devices_in_order() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    // 0.1 → [ [×2 → [ [+0.25 → ×3] [×0.5] ]] [+1] ]
    let inner = rack_of(
        RackKind::Audio,
        vec![
            chain_of(vec![fx(1.0, 0.25), fx(3.0, 0.0)]),
            chain_of(vec![fx(0.5, 0.0)]),
        ],
    );
    let outer = rack_of(
        RackKind::Audio,
        vec![
            chain_of(vec![fx(2.0, 0.0), inner]),
            chain_of(vec![fx(1.0, 1.0)]),
        ],
    );
    add_track(&mut h, track_of(TrackId::new(), vec![fx(1.0, 0.1), outer]));

    let (l, r) = render(&mut engine);
    let x = 0.1f32;
    let a = x * 2.0;
    let expected = ((a + 0.25) * 3.0 + a * 0.5) + (x + 1.0);
    assert!(
        close(l[BLOCK - 1], expected),
        "{} vs {expected}",
        l[BLOCK - 1]
    );
    assert_eq!(l, r);
    assert!(l.iter().all(|s| *s == l[0]), "the same on every frame");
}

#[test]
fn order_inside_a_nested_chain_matters() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let rack = rack_of(
        RackKind::Audio,
        vec![chain_of(vec![fx(2.0, 0.0), fx(1.0, 0.5)])],
    );
    let id = TrackId::new();
    let device = {
        let DeviceBody::Rack(r) = &rack.body else {
            unreachable!()
        };
        (rack.id, r.chains[0].id, r.chains[0].devices[0].id)
    };
    add_track(&mut h, track_of(id, vec![fx(1.0, 0.25), rack]));
    let (l, _) = render(&mut engine);
    assert_eq!(l[0], 0.25 * 2.0 + 0.5);

    let nested = ChainAddr::track(id).child(device.0, device.1).unwrap();
    send(
        &mut h,
        EngineCommand::graph(GraphOp::MoveDevice {
            target: nested,
            device: device.2,
            to: 1,
        }),
    );
    let (l, _) = render(&mut engine);
    assert_eq!(l[0], (0.25 + 0.5) * 2.0);
}

#[test]
fn a_rack_with_no_chains_passes_audio_through() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let rack = rack_of(RackKind::Audio, Vec::new());
    add_track(&mut h, track_of(TrackId::new(), vec![fx(1.0, 0.3), rack]));
    assert_eq!(render(&mut engine).0[0], 0.3);
}

#[test]
fn an_empty_chain_is_a_dry_path() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let rack = rack_of(
        RackKind::Audio,
        vec![chain_of(Vec::new()), chain_of(vec![fx(0.0, 0.0)])],
    );
    add_track(&mut h, track_of(TrackId::new(), vec![fx(1.0, 0.3), rack]));
    assert_eq!(render(&mut engine).0[0], 0.3);
}

#[derive(Debug, Clone, PartialEq)]
enum Heard {
    Audio { frame: u64, len: usize },
    Param { frame: u64, value: f64 },
}

/// Adds its level and writes down what it hears and when.
struct Probe {
    frames: u64,
    level: f32,
    log: Arc<Mutex<Vec<Heard>>>,
}

impl Processor for Probe {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        ctx.for_each_step(|step| match step {
            BlockStep::Audio(a) => {
                self.log.lock().unwrap().push(Heard::Audio {
                    frame: self.frames,
                    len: a.left.len(),
                });
                for s in a.left.iter_mut() {
                    *s += self.level;
                }
                self.frames += a.left.len() as u64;
            }
            BlockStep::Param { value, .. } => {
                self.level = value as f32;
                self.log.lock().unwrap().push(Heard::Param {
                    frame: self.frames,
                    value,
                });
            }
            BlockStep::Midi(_) => {}
        });
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

/// Writes a cell's value at one offset, from the track's control hooks.
struct WriteAt {
    cell: CellIx,
    offset: u32,
    value: f64,
}

impl ControlHooks for WriteAt {
    fn automation(&mut self, step: ControlStep, bus: &mut ParamBus) {
        if step.offset == self.offset {
            bus.write_automation(self.cell, self.value);
        }
    }
}

/// A device two racks down, with its cells placed in the track's flat bus.
struct DeepRig {
    track: Box<TrackNode>,
    log: Arc<Mutex<Vec<Heard>>>,
    param: CellIx,
    activator: CellIx,
}

fn deep_rig(level: f32) -> DeepRig {
    let log = Arc::new(Mutex::new(Vec::new()));
    let device = DeviceId::new();
    let probe = Probe {
        frames: 0,
        level,
        log: log.clone(),
    };
    let probe = Box::new(DeviceSlot::new(device, Box::new(probe)));
    let inner = rack_of(RackKind::Audio, vec![chain_of(vec![probe])]);
    let outer = rack_of(RackKind::Audio, vec![chain_of(vec![inner])]);

    let mut layout = BusLayout::for_track(&MixSettings::default());
    let params = [ParamInfo::continuous(
        0,
        "Level",
        0.0,
        1.0,
        level as f64,
        "",
    )];
    layout.place_device(device, true, &params, |p| p.default);
    let param = layout.device_cell(device, 0).unwrap();
    let activator = layout.activator_cell(device).unwrap();
    let mut track = track_of(TrackId::new(), vec![outer]);
    track.bus = layout.build();
    DeepRig {
        track,
        log,
        param,
        activator,
    }
}

#[test]
fn a_parameter_reaches_a_device_two_racks_down_at_its_frame() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let mut rig = deep_rig(0.25);
    rig.track.hooks = Some(Box::new(WriteAt {
        cell: rig.param,
        offset: 96,
        value: 0.75,
    }));
    add_track(&mut h, rig.track);
    let (l, _) = render(&mut engine);
    assert_eq!(
        *rig.log.lock().unwrap(),
        [
            Heard::Audio { frame: 0, len: 96 },
            Heard::Param {
                frame: 96,
                value: 0.75,
            },
            Heard::Audio {
                frame: 96,
                len: BLOCK - 96,
            },
        ]
    );
    assert_eq!(l[95], 0.25);
    assert_eq!(l[96], 0.75);
}

#[test]
fn switching_a_nested_device_off_splits_the_block_at_that_frame() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let mut rig = deep_rig(0.5);
    rig.track.hooks = Some(Box::new(WriteAt {
        cell: rig.activator,
        offset: 64,
        value: 0.0,
    }));
    add_track(&mut h, rig.track);
    let (l, _) = render(&mut engine);
    assert_eq!(l[63], 0.5);
    assert_eq!(l[64], 0.0, "off from exactly this frame");
    assert_eq!(
        *rig.log.lock().unwrap(),
        [Heard::Audio { frame: 0, len: 64 }]
    );
}

/// Reports a parameter change of its own every block.
struct SelfMover;

impl Processor for SelfMover {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        ctx.param_out.push(0, 0.75);
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

#[test]
fn a_nested_device_reports_its_own_parameter_changes_to_its_track() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let device = DeviceId::new();
    let id = TrackId::new();
    let nested = Box::new(DeviceSlot::new(device, Box::new(SelfMover)));
    let rack = rack_of(RackKind::Audio, vec![chain_of(vec![nested])]);
    let mut layout = BusLayout::for_track(&MixSettings::default());
    let params = [ParamInfo::continuous(0, "P", 0.0, 1.0, 0.5, "")];
    layout.place_device(device, true, &params, |p| p.default);
    let mut track = track_of(id, vec![rack]);
    track.bus = layout.build();
    add_track(&mut h, track);
    render(&mut engine);
    assert_eq!(
        h.poll_event(),
        Some(EngineEvent::ParamChanged {
            target: TrackRef::Track(id),
            device,
            param: 0,
            value: 0.75,
        })
    );
    assert_eq!(
        layout.effective(layout.device_cell(device, 0).unwrap()),
        Some(0.75)
    );
}

#[test]
fn a_rack_can_be_switched_off_as_a_whole() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let mut rack = rack_of(RackKind::Audio, vec![chain_of(vec![fx(1.0, 0.5)])]);
    let device = rack.id;
    rack.bypass = false;
    let mut layout = BusLayout::for_track(&MixSettings::default());
    layout.place_device(device, true, &[], |p| p.default);
    let activator = layout.activator_cell(device).unwrap();
    let id = TrackId::new();
    let mut track = track_of(id, vec![fx(1.0, 0.25), rack]);
    track.bus = layout.build();
    add_track(&mut h, track);
    assert_eq!(render(&mut engine).0[0], 0.75);
    send(
        &mut h,
        EngineCommand::Param(ParamCmd::Set {
            track: TrackRef::Track(id),
            cell: activator,
            value: 0.0,
            gesture: true,
        }),
    );
    assert_eq!(
        render(&mut engine).0[0],
        0.25,
        "the rack passes audio through"
    );
}

// ------------------------------------------------------------------ MIDI through racks

fn note_on(key: u8) -> rlive_core::MidiMessage {
    rlive_core::MidiMessage::NoteOn {
        channel: 0,
        key,
        velocity: 1.0,
        note_id: 0,
        tuning: 0.0,
    }
}

/// Passes every note on, moved by `by`.
struct Transpose {
    by: i8,
}

impl Processor for Transpose {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        for e in ctx.events {
            let mut m = e.message;
            if let rlive_core::MidiMessage::NoteOn { key, .. } = &mut m {
                *key = key.wrapping_add_signed(self.by);
            }
            ctx.events_out.push(rlive_core::MidiEvent::new(e.offset, m));
        }
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

/// Writes down every key it hears.
struct Keys(Arc<Mutex<Vec<u8>>>);

impl Processor for Keys {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        for e in ctx.events {
            if let rlive_core::MidiMessage::NoteOn { key, .. } = e.message {
                self.0.lock().unwrap().push(key);
            }
        }
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

fn midi_effect(by: i8) -> Box<DeviceSlot> {
    let mut s = DeviceSlot::new(DeviceId::new(), Box::new(Transpose { by }));
    s.midi_effect = true;
    Box::new(s)
}

fn keys_device() -> (Box<DeviceSlot>, Arc<Mutex<Vec<u8>>>) {
    let heard = Arc::new(Mutex::new(Vec::new()));
    let keys = Box::new(DeviceSlot::new(
        DeviceId::new(),
        Box::new(Keys(heard.clone())),
    ));
    (keys, heard)
}

#[test]
fn every_chain_of_a_rack_hears_the_tracks_midi() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (one, heard_one) = keys_device();
    let (two, heard_two) = keys_device();
    let rack = rack_of(
        RackKind::Instrument,
        vec![chain_of(vec![midi_effect(12), one]), chain_of(vec![two])],
    );
    let id = TrackId::new();
    let mut sender = h.take_midi_sender().unwrap();
    add_track(&mut h, track_of(id, vec![rack]));
    render(&mut engine);
    sender.send(id, note_on(60));
    render(&mut engine);
    assert_eq!(
        *heard_one.lock().unwrap(),
        [72],
        "its chain's MIDI effect ran"
    );
    assert_eq!(
        *heard_two.lock().unwrap(),
        [60],
        "untouched by the other chain"
    );
}

#[test]
fn a_midi_rack_passes_on_what_its_chains_pass_on() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let rack = rack_of(
        RackKind::Midi,
        vec![
            chain_of(vec![midi_effect(1)]),
            chain_of(vec![midi_effect(2)]),
        ],
    );
    let (after, heard) = keys_device();
    let id = TrackId::new();
    let mut sender = h.take_midi_sender().unwrap();
    add_track(&mut h, track_of(id, vec![rack, after]));
    render(&mut engine);
    sender.send(id, note_on(60));
    render(&mut engine);
    let mut keys = heard.lock().unwrap().clone();
    keys.sort_unstable();
    assert_eq!(keys, [61, 62]);
}

// ------------------------------------------------------------------ transactions and plans

fn step(track: u32, inputs: &[u32]) -> Step {
    Step {
        track,
        inputs: inputs.iter().copied().map(BusIn::post_mixer).collect(),
    }
}

/// `tracks` tracks and a Master that hears only those in `routed`, in that order.
fn routed_plan(tracks: u32, routed: &[u32]) -> RenderPlan {
    let mut steps: Vec<Step> = (0..tracks).map(|t| step(t, &[])).collect();
    steps.push(step(MASTER_STEP, routed));
    RenderPlan {
        steps,
        levels: vec![0..tracks, tracks..tracks + 1],
        buses: 0,
        sidechains: Vec::new(),
    }
}

fn dc_track(id: TrackId, level: f32, alive: &Arc<AtomicI64>) -> Box<TrackNode> {
    let device = Box::new(DeviceSlot::new(
        DeviceId::new(),
        Box::new(Fx::counted(1.0, level, alive)),
    ));
    track_of(id, vec![device])
}

#[test]
fn a_transaction_takes_effect_whole_between_two_blocks() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let alive = Arc::new(AtomicI64::new(0));
    let (a, b, c) = (TrackId::new(), TrackId::new(), TrackId::new());
    send(
        &mut h,
        EngineCommand::Graph(GraphTxn::new(
            vec![
                GraphOp::AddTrack {
                    index: 0,
                    track: dc_track(a, 0.1, &alive),
                },
                GraphOp::AddTrack {
                    index: 1,
                    track: dc_track(b, 0.2, &alive),
                },
                GraphOp::AddTrack {
                    index: 2,
                    track: dc_track(c, 0.4, &alive),
                },
            ],
            Some(routed_plan(3, &[0, 2])),
        )),
    );
    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    assert!(close(render(&mut engine).0[0], 0.5), "b is not routed");

    // A track goes in at the front and the indices shift. With the old plan the Master would
    // sum the new track and b; with the new tracks but no plan, nothing would add up either.
    let d = TrackId::new();
    send(
        &mut h,
        EngineCommand::Graph(GraphTxn::new(
            vec![GraphOp::AddTrack {
                index: 0,
                track: dc_track(d, 0.8, &alive),
            }],
            Some(routed_plan(4, &[1, 3, 0])),
        )),
    );
    let (l, _) = assert_no_alloc(|| render_in_place(&mut engine));
    assert!(l.iter().all(|s| close(*s, 1.3)), "every frame: {}", l[0]);

    // Taking a track out shifts them back, and the plan moves with it.
    let before = alive.load(Ordering::SeqCst);
    send(
        &mut h,
        EngineCommand::Graph(GraphTxn::new(
            vec![
                GraphOp::RemoveTrack(a),
                GraphOp::MoveTrack { track: b, to: 0 },
            ],
            Some(routed_plan(3, &[2])),
        )),
    );
    let (l, _) = assert_no_alloc(|| render_in_place(&mut engine));
    assert!(l.iter().all(|s| close(*s, 0.4)), "only c: {}", l[0]);
    assert_eq!(
        alive.load(Ordering::SeqCst),
        before,
        "nothing freed by the engine"
    );
    assert_eq!(h.collect_garbage(), 1, "the removed track");
    assert_eq!(
        alive.load(Ordering::SeqCst),
        before - 1,
        "freed on this thread"
    );
}

fn render_in_place(engine: &mut Engine) -> ([f32; BLOCK], [f32; BLOCK]) {
    let (mut l, mut r) = ([0.0; BLOCK], [0.0; BLOCK]);
    engine.process_stereo(&mut l, &mut r);
    (l, r)
}

#[test]
fn without_a_plan_a_changed_list_of_tracks_goes_back_to_summing_them_all() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let alive = Arc::new(AtomicI64::new(0));
    let (a, b) = (TrackId::new(), TrackId::new());
    send(
        &mut h,
        EngineCommand::Graph(GraphTxn::new(
            vec![
                GraphOp::AddTrack {
                    index: 0,
                    track: dc_track(a, 0.1, &alive),
                },
                GraphOp::AddTrack {
                    index: 1,
                    track: dc_track(b, 0.2, &alive),
                },
            ],
            Some(routed_plan(2, &[1])),
        )),
    );
    assert!(close(render(&mut engine).0[0], 0.2));
    send(
        &mut h,
        EngineCommand::graph(GraphOp::AddTrack {
            index: 2,
            track: dc_track(TrackId::new(), 0.4, &alive),
        }),
    );
    assert!(close(render(&mut engine).0[0], 0.7));
}

#[test]
fn an_edit_inside_a_track_keeps_the_plan() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let alive = Arc::new(AtomicI64::new(0));
    let (a, b) = (TrackId::new(), TrackId::new());
    send(
        &mut h,
        EngineCommand::Graph(GraphTxn::new(
            vec![
                GraphOp::AddTrack {
                    index: 0,
                    track: dc_track(a, 0.1, &alive),
                },
                GraphOp::AddTrack {
                    index: 1,
                    track: dc_track(b, 0.2, &alive),
                },
            ],
            Some(routed_plan(2, &[1])),
        )),
    );
    send(
        &mut h,
        EngineCommand::graph(GraphOp::InsertDevice {
            target: ChainAddr::track(b),
            index: 1,
            device: fx(2.0, 0.0),
        }),
    );
    assert!(
        close(render(&mut engine).0[0], 0.4),
        "still only b, doubled"
    );
}

#[test]
fn a_plan_that_does_not_fit_the_tracks_is_refused() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let alive = Arc::new(AtomicI64::new(0));
    send(
        &mut h,
        EngineCommand::Graph(GraphTxn::new(
            vec![GraphOp::AddTrack {
                index: 0,
                track: dc_track(TrackId::new(), 0.5, &alive),
            }],
            Some(routed_plan(5, &[0, 4])),
        )),
    );
    assert!(
        close(render(&mut engine).0[0], 0.5),
        "falls back to every track"
    );
}

#[test]
fn old_nodes_come_back_through_garbage_when_a_rack_goes() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let alive = Arc::new(AtomicI64::new(0));
    let id = TrackId::new();
    let nested = Box::new(DeviceSlot::new(
        DeviceId::new(),
        Box::new(Fx::counted(1.0, 0.1, &alive)),
    ));
    let rack = rack_of(RackKind::Audio, vec![chain_of(vec![nested])]);
    let rack_id = rack.id;
    add_track(&mut h, track_of(id, vec![rack]));
    render(&mut engine);
    h.collect_garbage();
    assert_eq!(alive.load(Ordering::SeqCst), 1);
    send(
        &mut h,
        EngineCommand::graph(GraphOp::RemoveDevice {
            target: ChainAddr::track(id),
            device: rack_id,
        }),
    );
    assert_no_alloc(|| render_in_place(&mut engine));
    assert_eq!(
        alive.load(Ordering::SeqCst),
        1,
        "still held for this thread"
    );
    assert_eq!(h.collect_garbage(), 1);
    assert_eq!(alive.load(Ordering::SeqCst), 0);
}

#[test]
fn a_device_can_move_between_chains_but_not_into_itself() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let id = TrackId::new();
    let inner_chain = ChainId::new();
    let mut chain = ChainNode::new(inner_chain, BLOCK);
    chain.devices.push(fx(1.0, 0.5));
    let rack = rack_of(RackKind::Audio, vec![Box::new(chain)]);
    let rack_id = rack.id;
    add_track(&mut h, track_of(id, vec![rack]));
    render(&mut engine);
    let root = ChainAddr::track(id);
    let inside = root.child(rack_id, inner_chain).unwrap();
    send(
        &mut h,
        EngineCommand::graph(GraphOp::TransferDevice {
            from: root,
            device: rack_id,
            to: inside,
            index: 0,
        }),
    );
    assert_eq!(
        render(&mut engine).0[0],
        0.5,
        "a rack can't go inside itself"
    );
}

#[test]
fn a_rack_too_deep_is_handed_back() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let id = TrackId::new();
    add_track(&mut h, track_of(id, Vec::new()));
    render(&mut engine);
    h.collect_garbage();
    let mut too_deep = fx(1.0, 0.5);
    for _ in 0..=MAX_RACK_DEPTH {
        too_deep = rack_of(RackKind::Audio, vec![chain_of(vec![too_deep])]);
    }
    send(
        &mut h,
        EngineCommand::graph(GraphOp::InsertDevice {
            target: ChainAddr::track(id),
            index: 0,
            device: too_deep,
        }),
    );
    assert_eq!(render(&mut engine).0[0], 0.0, "not inserted");
    assert_eq!(h.collect_garbage(), 1);
}

// ------------------------------------------------------------------ audio I/O

#[test]
fn the_engine_takes_inputs_and_writes_any_number_of_outputs() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    add_track(&mut h, track_of(TrackId::new(), vec![fx(1.0, 0.25)]));
    let input = [1.0f32; BLOCK];
    let (mut a, mut b, mut c) = ([0.0f32; BLOCK], [0.0f32; BLOCK], [9.0f32; BLOCK]);
    let inputs: [&[f32]; 1] = [&input];
    let mut outputs: [&mut [f32]; 3] = [&mut a, &mut b, &mut c];
    engine.process(AudioIo {
        inputs: &inputs,
        outputs: &mut outputs,
    });
    assert_eq!(a[0], 0.25, "inputs don't reach the output yet");
    assert_eq!(b[0], 0.25);
    assert!(c.iter().all(|s| *s == 0.0), "extra channels are silent");
}

#[test]
fn the_shortest_output_sets_how_much_is_rendered() {
    let (mut engine, _h) = create_engine(SR, BLOCK);
    let (mut a, mut b) = ([1.0f32; 10], [1.0f32; 4]);
    let mut outputs: [&mut [f32]; 2] = [&mut a, &mut b];
    engine.process(AudioIo {
        inputs: &[],
        outputs: &mut outputs,
    });
    assert_eq!(a[3], 0.0);
    assert_eq!(a[4], 1.0, "untouched");
}

// ------------------------------------------------------------------ random edits

/// A small, seeded generator, so a failing run can be repeated.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }

    fn range(&mut self, low: f32, high: f32) -> f32 {
        low + (self.next() % 1000) as f32 / 1000.0 * (high - low)
    }
}

/// What the UI side thinks is in the graph, and so what it should sound like.
#[derive(Clone)]
enum Node {
    Proc {
        id: DeviceId,
        gain: f32,
        add: f32,
        on: bool,
    },
    Rack {
        id: DeviceId,
        chains: Vec<Chain>,
        on: bool,
    },
}

#[derive(Clone)]
struct Chain {
    id: ChainId,
    nodes: Vec<Node>,
}

impl Node {
    fn id(&self) -> DeviceId {
        match self {
            Node::Proc { id, .. } | Node::Rack { id, .. } => *id,
        }
    }

    fn eval(&self, x: f32) -> f32 {
        match self {
            Node::Proc { on: false, .. } | Node::Rack { on: false, .. } => x,
            Node::Proc { gain, add, .. } => x * gain + add,
            Node::Rack { chains, .. } if chains.is_empty() => x,
            Node::Rack { chains, .. } => chains.iter().map(|c| eval(&c.nodes, x)).sum(),
        }
    }

    fn depth(&self) -> usize {
        match self {
            Node::Proc { .. } => 0,
            Node::Rack { chains, .. } => {
                1 + chains
                    .iter()
                    .flat_map(|c| &c.nodes)
                    .map(Node::depth)
                    .max()
                    .unwrap_or(0)
            }
        }
    }

    fn ids(&self, out: &mut Vec<DeviceId>) {
        out.push(self.id());
        if let Node::Rack { chains, .. } = self {
            for n in chains.iter().flat_map(|c| &c.nodes) {
                n.ids(out);
            }
        }
    }
}

fn eval(nodes: &[Node], x: f32) -> f32 {
    nodes.iter().fold(x, |x, n| n.eval(x))
}

struct ModelTrack {
    id: TrackId,
    nodes: Vec<Node>,
    layout: BusLayout,
}

impl ModelTrack {
    fn nodes_at(&mut self, addr: ChainAddr) -> &mut Vec<Node> {
        let mut nodes = &mut self.nodes;
        for step in addr.path.steps() {
            let rack = nodes.iter_mut().find(|n| n.id() == step.rack).unwrap();
            let Node::Rack { chains, .. } = rack else {
                unreachable!()
            };
            nodes = &mut chains
                .iter_mut()
                .find(|c| c.id == step.chain)
                .unwrap()
                .nodes;
        }
        nodes
    }

    /// Every chain of this track, with the room left for nesting below it.
    fn chains(&self) -> Vec<ChainAddr> {
        fn walk(nodes: &[Node], addr: ChainAddr, out: &mut Vec<ChainAddr>) {
            out.push(addr);
            for n in nodes {
                if let Node::Rack { id, chains, .. } = n {
                    for c in chains {
                        walk(&c.nodes, addr.child(*id, c.id).unwrap(), out);
                    }
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.nodes, ChainAddr::track(self.id), &mut out);
        out
    }
}

struct Fuzz {
    rng: Rng,
    tracks: Vec<ModelTrack>,
    alive: Arc<AtomicI64>,
}

const MAX_MODEL_TRACKS: usize = 6;
const MAX_MODEL_DEVICES: usize = 6;

impl Fuzz {
    fn proc_node(&mut self) -> (Node, Box<DeviceSlot>) {
        let id = DeviceId::new();
        let (gain, add) = (self.rng.range(0.25, 1.1), self.rng.range(0.0, 0.2));
        let node = Node::Proc {
            id,
            gain,
            add,
            on: true,
        };
        let slot = DeviceSlot::new(id, Box::new(Fx::counted(gain, add, &self.alive)));
        (node, Box::new(slot))
    }

    fn place(track: &mut ModelTrack, node: &Node) {
        let mut ids = Vec::new();
        node.ids(&mut ids);
        for id in ids {
            let params = [ParamInfo::continuous(0, "P", 0.0, 1.0, 0.5, "")];
            track.layout.place_device(id, true, &params, |p| p.default);
        }
    }

    fn swap_bus(track: &mut ModelTrack) -> GraphOp {
        GraphOp::SwapBus {
            target: TrackRef::Track(track.id),
            bus: track.layout.build(),
        }
    }

    fn new_track(&mut self) -> (ModelTrack, Box<TrackNode>) {
        let id = TrackId::new();
        let mut track = Box::new(TrackNode::new(id, BLOCK));
        let mut model = ModelTrack {
            id,
            nodes: Vec::new(),
            layout: BusLayout::for_track(&MixSettings::default()),
        };
        let (node, slot) = self.proc_node();
        track.devices.push(slot);
        Self::place(&mut model, &node);
        model.nodes.push(node);
        track.bus = model.layout.build();
        (model, track)
    }

    fn expected(&self) -> f32 {
        self.tracks.iter().map(|t| eval(&t.nodes, 0.0)).sum()
    }

    fn plan(&mut self) -> Option<RenderPlan> {
        self.rng.chance(50).then(|| {
            let n = self.tracks.len() as u32;
            routed_plan(n, &(0..n).collect::<Vec<_>>())
        })
    }

    /// One random edit, as the operations that perform it.
    fn edit(&mut self) -> Vec<GraphOp> {
        match self.rng.below(12) {
            0 if self.tracks.len() < MAX_MODEL_TRACKS => {
                let (model, node) = self.new_track();
                let index = self.rng.below(self.tracks.len() + 1);
                self.tracks.insert(index, model);
                vec![GraphOp::AddTrack { index, track: node }]
            }
            1 if self.tracks.len() > 1 => {
                let at = self.rng.below(self.tracks.len());
                let gone = self.tracks.remove(at);
                vec![GraphOp::RemoveTrack(gone.id)]
            }
            2 if self.tracks.len() > 1 => {
                let at = self.rng.below(self.tracks.len());
                let to = self.rng.below(self.tracks.len());
                let track = self.tracks.remove(at);
                let id = track.id;
                self.tracks.insert(to, track);
                vec![GraphOp::MoveTrack { track: id, to }]
            }
            3 => self.replace_all(),
            _ => self.edit_chain(),
        }
    }

    fn replace_all(&mut self) -> Vec<GraphOp> {
        let n = self.rng.below(3) + 1;
        let (mut models, mut tracks) = (Vec::new(), Vec::with_capacity(MAX_TRACKS));
        for _ in 0..n {
            let (model, track) = self.new_track();
            models.push(model);
            tracks.push(track);
        }
        self.tracks = models;
        vec![GraphOp::ReplaceAll {
            tracks: Box::new(tracks),
            master: Box::new(MasterNode::new(BLOCK)),
        }]
    }

    fn edit_chain(&mut self) -> Vec<GraphOp> {
        let t = self.rng.below(self.tracks.len());
        let chains = self.tracks[t].chains();
        let addr = chains[self.rng.below(chains.len())];
        let len = self.tracks[t].nodes_at(addr).len();
        match self.rng.below(12) {
            0 | 1 if len < MAX_MODEL_DEVICES => self.insert_at(t, addr),
            2 if len > 0 => self.remove_at(t, addr),
            3 if len > 1 => {
                let nodes = self.tracks[t].nodes_at(addr);
                let from = (addr.path.len() + len - 1) % len;
                let device = nodes[from].id();
                let node = nodes.remove(from);
                let to = (from + 1) % len;
                nodes.insert(to, node);
                vec![GraphOp::MoveDevice {
                    target: addr,
                    device,
                    to,
                }]
            }
            4 | 5 if len > 0 => self.nest_at(t, addr),
            6 => self.ungroup_at(t, addr),
            7 if len > 0 => self.transfer_from(t, addr, &chains),
            8 => self.add_chain_at(t, addr),
            9 => self.remove_chain_at(t, addr),
            _ => self.switch_at(t, addr),
        }
    }

    fn insert_at(&mut self, t: usize, addr: ChainAddr) -> Vec<GraphOp> {
        let (node, slot) = self.proc_node();
        let index = self.rng.below(self.tracks[t].nodes_at(addr).len() + 1);
        Self::place(&mut self.tracks[t], &node);
        self.tracks[t].nodes_at(addr).insert(index, node);
        let insert = GraphOp::InsertDevice {
            target: addr,
            index,
            device: slot,
        };
        let swap = Self::swap_bus(&mut self.tracks[t]);
        if self.rng.chance(50) {
            vec![swap, insert]
        } else {
            vec![insert, swap]
        }
    }

    fn remove_at(&mut self, t: usize, addr: ChainAddr) -> Vec<GraphOp> {
        let index = self.rng.below(self.tracks[t].nodes_at(addr).len());
        let node = self.tracks[t].nodes_at(addr).remove(index);
        let mut ids = Vec::new();
        node.ids(&mut ids);
        for id in ids {
            self.tracks[t].layout.remove_device(id);
        }
        vec![GraphOp::RemoveDevice {
            target: addr,
            device: node.id(),
        }]
    }

    /// Wrap one device in a new rack with a single chain.
    fn nest_at(&mut self, t: usize, addr: ChainAddr) -> Vec<GraphOp> {
        let index = self.rng.below(self.tracks[t].nodes_at(addr).len());
        let inner_depth = self.tracks[t].nodes_at(addr)[index].depth();
        if addr.path.len() + 1 + inner_depth > MAX_RACK_DEPTH {
            return Vec::new();
        }
        let (rack, chain) = (DeviceId::new(), ChainId::new());
        let wrapped = self.tracks[t].nodes_at(addr).remove(index);
        let device = wrapped.id();
        let inner = addr.child(rack, chain).unwrap();
        let node = Node::Rack {
            id: rack,
            chains: vec![Chain {
                id: chain,
                nodes: vec![wrapped],
            }],
            on: true,
        };
        Self::place(
            &mut self.tracks[t],
            &Node::Rack {
                id: rack,
                chains: Vec::new(),
                on: true,
            },
        );
        self.tracks[t].nodes_at(addr).insert(index, node);
        let swap = Self::swap_bus(&mut self.tracks[t]);
        let slot = DeviceSlot::rack(rack, Box::new(RackNode::new(RackKind::Audio)));
        vec![
            GraphOp::InsertDevice {
                target: addr,
                index,
                device: Box::new(slot),
            },
            GraphOp::InsertChain {
                target: addr,
                device: rack,
                index: 0,
                chain: Box::new(ChainNode::new(chain, BLOCK)),
            },
            GraphOp::TransferDevice {
                from: addr,
                device,
                to: inner,
                index: 0,
            },
            swap,
        ]
    }

    /// Replace a rack in this chain with the devices of all its chains.
    fn ungroup_at(&mut self, t: usize, addr: ChainAddr) -> Vec<GraphOp> {
        let nodes = self.tracks[t].nodes_at(addr);
        let Some(index) = nodes.iter().position(|n| matches!(n, Node::Rack { .. })) else {
            return Vec::new();
        };
        let Node::Rack { id, chains, .. } = nodes.remove(index) else {
            unreachable!()
        };
        self.tracks[t].layout.remove_device(id);
        let mut ops = Vec::new();
        let mut at = index;
        for chain in chains {
            let from = addr.child(id, chain.id).unwrap();
            for node in chain.nodes {
                ops.push(GraphOp::TransferDevice {
                    from,
                    device: node.id(),
                    to: addr,
                    index: at,
                });
                self.tracks[t].nodes_at(addr).insert(at, node);
                at += 1;
            }
        }
        ops.push(GraphOp::RemoveDevice {
            target: addr,
            device: id,
        });
        ops
    }

    /// Give a rack in this chain another chain, with a device already in it.
    fn add_chain_at(&mut self, t: usize, addr: ChainAddr) -> Vec<GraphOp> {
        let rack = self.tracks[t]
            .nodes_at(addr)
            .iter()
            .find(|n| matches!(n, Node::Rack { .. }))
            .map(Node::id);
        let Some(device) = rack.filter(|_| addr.path.len() < MAX_RACK_DEPTH) else {
            return Vec::new();
        };
        let (node, slot) = self.proc_node();
        let id = ChainId::new();
        let mut chain = ChainNode::new(id, BLOCK);
        chain.devices.push(slot);
        Self::place(&mut self.tracks[t], &node);
        let nodes = self.tracks[t].nodes_at(addr);
        let rack = nodes.iter_mut().find(|n| n.id() == device);
        let Some(Node::Rack { chains, .. }) = rack else {
            unreachable!()
        };
        let index = self.rng.below(chains.len() + 1);
        chains.insert(
            index,
            Chain {
                id,
                nodes: vec![node],
            },
        );
        let swap = Self::swap_bus(&mut self.tracks[t]);
        vec![
            swap,
            GraphOp::InsertChain {
                target: addr,
                device,
                index,
                chain: Box::new(chain),
            },
        ]
    }

    fn remove_chain_at(&mut self, t: usize, addr: ChainAddr) -> Vec<GraphOp> {
        let nodes = self.tracks[t].nodes_at(addr);
        let rack = nodes.iter_mut().find_map(|n| match n {
            Node::Rack { id, chains, .. } if !chains.is_empty() => Some((*id, chains)),
            _ => None,
        });
        let Some((device, chains)) = rack else {
            return Vec::new();
        };
        let index = self.rng.below(chains.len());
        let gone = chains.remove(index);
        let mut ids = Vec::new();
        for n in &gone.nodes {
            n.ids(&mut ids);
        }
        for id in ids {
            self.tracks[t].layout.remove_device(id);
        }
        vec![GraphOp::RemoveChain {
            target: addr,
            device,
            chain: gone.id,
        }]
    }

    /// Move a device into any chain of the same track that isn't under it.
    fn transfer_from(&mut self, t: usize, addr: ChainAddr, chains: &[ChainAddr]) -> Vec<GraphOp> {
        let index = self.rng.below(self.tracks[t].nodes_at(addr).len());
        let (device, depth) = {
            let node = &self.tracks[t].nodes_at(addr)[index];
            (node.id(), node.depth())
        };
        let to = chains[self.rng.below(chains.len())];
        let inside = to.path.steps().iter().any(|s| s.rack == device);
        let full = to != addr && self.tracks[t].nodes_at(to).len() >= MAX_MODEL_DEVICES;
        if inside || full || to.path.len() + depth > MAX_RACK_DEPTH {
            return Vec::new();
        }
        let node = self.tracks[t].nodes_at(addr).remove(index);
        let at = self.rng.below(self.tracks[t].nodes_at(to).len() + 1);
        self.tracks[t].nodes_at(to).insert(at, node);
        vec![GraphOp::TransferDevice {
            from: addr,
            device,
            to,
            index: at,
        }]
    }

    /// Turn a device on or off. This one is a parameter change, not an edit.
    fn switch_at(&mut self, t: usize, addr: ChainAddr) -> Vec<GraphOp> {
        let nodes = self.tracks[t].nodes_at(addr);
        if nodes.is_empty() {
            return Vec::new();
        }
        let index = self.rng.below(nodes.len());
        let (Node::Proc { on, .. } | Node::Rack { on, .. }) = &mut nodes[index];
        *on = !*on;
        Vec::new()
    }

    fn switches(&mut self) -> Vec<EngineCommand> {
        fn walk(owner: TrackRef, nodes: &[Node], layout: &BusLayout, out: &mut Vec<EngineCommand>) {
            for n in nodes {
                if let Some(cell) = layout.activator_cell(n.id()) {
                    let (Node::Proc { on, .. } | Node::Rack { on, .. }) = n;
                    out.push(EngineCommand::Param(ParamCmd::Set {
                        track: owner,
                        cell,
                        value: *on as u8 as f64,
                        gesture: false,
                    }));
                }
                if let Node::Rack { chains, .. } = n {
                    for c in chains {
                        walk(owner, &c.nodes, layout, out);
                    }
                }
            }
        }
        let mut out = Vec::new();
        for t in &self.tracks {
            walk(TrackRef::Track(t.id), &t.nodes, &t.layout, &mut out);
        }
        out
    }
}

fn fuzz(seed: u64, edits: usize) {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let alive = Arc::new(AtomicI64::new(0));
    let mut f = Fuzz {
        rng: Rng(seed),
        tracks: Vec::new(),
        alive: alive.clone(),
    };
    let (model, track) = f.new_track();
    f.tracks.push(model);
    send(
        &mut h,
        EngineCommand::graph(GraphOp::AddTrack { index: 0, track }),
    );
    send(&mut h, EngineCommand::Transport(TransportCmd::Play));
    let mut buffers = ([0.0f32; BLOCK], [0.0f32; BLOCK]);

    for edit in 0..edits {
        let ops = f.edit();
        let plan = f.plan();
        send(&mut h, EngineCommand::Graph(GraphTxn::new(ops, plan)));
        // Cells are placed and every toggle sent, so a device that was off before stays off.
        for cmd in f.switches() {
            send(&mut h, cmd);
        }
        let expected = f.expected();
        let (l, r) = (&mut buffers.0, &mut buffers.1);
        assert_no_alloc(|| engine.process_stereo(l, r));
        for s in l.iter().chain(r.iter()) {
            assert!(s.is_finite(), "seed {seed}, edit {edit}");
            assert!(
                close(*s, expected),
                "seed {seed}, edit {edit}: {s} vs {expected}"
            );
        }
        h.collect_garbage();
    }

    drop(f);
    drop(engine);
    h.collect_garbage();
    drop(h);
    assert_eq!(alive.load(Ordering::SeqCst), 0, "seed {seed}: no leaks");
}

#[test]
fn random_edits_while_playing_stay_correct_and_off_the_heap() {
    for seed in [0x9E37_79B9_7F4A_7C15, 1, 77, 0xDEAD_BEEF, 2024, 31337] {
        fuzz(seed, 1000);
    }
}
