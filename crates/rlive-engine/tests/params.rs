//! The parameter bus end to end: resolving inside blocks, gain timing and modulation.

use rlive_core::param::mixer;
use rlive_core::{BlockStep, DeviceId, ModMode, ParamInfo, ProcessContext, Processor, TrackId};
use rlive_engine::*;
use rlive_testkit::assert_no_alloc;
use std::sync::{Arc, Mutex};

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

/// Adds a constant to the buffer and records what happens to it, in frames since it started.
struct Probe {
    level: f32,
    frames: u64,
    log: Arc<Mutex<Vec<Seen>>>,
}

#[derive(Debug, Clone, PartialEq)]
enum Seen {
    Block {
        frame: u64,
        len: usize,
        sample_pos: u64,
    },
    Param {
        frame: u64,
        id: u32,
        value: f64,
    },
    Reset,
}

impl Processor for Probe {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        let sample_pos = ctx.transport.sample_pos;
        ctx.for_each_step(|step| match step {
            BlockStep::Audio(audio) => {
                let len = audio.left.len();
                self.log.lock().unwrap().push(Seen::Block {
                    frame: self.frames,
                    len,
                    sample_pos,
                });
                for s in audio.left.iter_mut().chain(audio.right.iter_mut()) {
                    *s += self.level;
                }
                self.frames += len as u64;
            }
            BlockStep::Param { id, value } => {
                self.log.lock().unwrap().push(Seen::Param {
                    frame: self.frames,
                    id,
                    value,
                });
                if id == 0 {
                    self.level = value as f32;
                }
            }
            BlockStep::Midi(_) => {}
        });
    }
    fn set_param(&mut self, _: u32, _: f64) {}
    fn reset(&mut self) {
        self.log.lock().unwrap().push(Seen::Reset);
    }
}

fn level_param() -> ParamInfo {
    ParamInfo::continuous(0, "Level", 0.0, 1.0, 0.5, "")
}

/// A track with one probe device, its cells placed in the track's bus.
struct Rig {
    id: TrackId,
    device: DeviceId,
    cell: CellIx,
    activator: CellIx,
    log: Arc<Mutex<Vec<Seen>>>,
}

fn rig(hooks: Option<Box<dyn ControlHooks>>, level: f32) -> (Rig, Box<TrackNode>) {
    let id = TrackId::new();
    let device = DeviceId::new();
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut track = Box::new(TrackNode::new(id, BLOCK));
    let probe = Probe {
        level,
        frames: 0,
        log: log.clone(),
    };
    track
        .devices
        .push(Box::new(DeviceSlot::new(device, Box::new(probe))));
    let mut layout = BusLayout::for_track(&MixSettings::default());
    let params = [level_param()];
    layout.place_device(device, true, &params, |_| level as f64);
    let cell = layout.device_cell(device, 0).unwrap();
    let activator = layout.activator_cell(device).unwrap();
    track.bus = layout.build();
    track.hooks = hooks;
    (
        Rig {
            id,
            device,
            cell,
            activator,
            log,
        },
        track,
    )
}

fn add_track(h: &mut EngineHandle, track: Box<TrackNode>) {
    assert!(h
        .send(EngineCommand::graph(GraphOp::AddTrack { index: 0, track }))
        .is_ok());
}

fn set_cell(h: &mut EngineHandle, track: TrackId, cell: CellIx, value: f64) {
    assert!(h
        .send(EngineCommand::Param(ParamCmd::Set {
            track: TrackRef::Track(track),
            cell,
            value,
            gesture: true,
        }))
        .is_ok());
}

/// Writes one cell at one control step of one block, with whichever hook it is told to use.
struct AtStep {
    block: u32,
    index: u32,
    now: u32,
    cell: CellIx,
    value: f64,
    kind: Kind,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Macro,
    Automation,
}

impl AtStep {
    fn due(&mut self, step: ControlStep) -> bool {
        if step.index == 0 && self.now != u32::MAX {
            self.now += 1;
        }
        self.now == self.block && step.index == self.index
    }
}

impl ControlHooks for AtStep {
    fn macros(&mut self, step: ControlStep, bus: &mut ParamBus) {
        if self.kind == Kind::Macro && self.due(step) {
            bus.write_macro(self.cell, self.value);
        }
    }
    fn automation(&mut self, step: ControlStep, bus: &mut ParamBus) {
        if self.kind == Kind::Automation && self.due(step) {
            bus.write_automation(self.cell, self.value);
        }
    }
}

fn settle_frames() -> usize {
    SR as usize // long enough for every gain ramp to settle.
}

#[test]
fn control_steps_are_32_frames() {
    assert_eq!(CONTROL_STEP, 32);
}

#[test]
fn a_mid_block_change_reaches_the_device_at_its_control_step() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (r, mut track) = rig(None, 0.5);
    track.hooks = Some(Box::new(AtStep {
        block: 1,
        index: 5,
        now: 0,
        cell: r.cell,
        value: 0.25,
        kind: Kind::Automation,
    }));
    add_track(&mut h, track);
    let (mut l, mut r_buf) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    engine.process_stereo(&mut l, &mut r_buf);

    let offset = 5 * CONTROL_STEP;
    assert_eq!(
        *r.log.lock().unwrap(),
        [
            Seen::Block {
                frame: 0,
                len: offset,
                sample_pos: 0
            },
            Seen::Param {
                frame: offset as u64,
                id: 0,
                value: 0.25
            },
            Seen::Block {
                frame: offset as u64,
                len: BLOCK - offset,
                sample_pos: 0
            },
        ],
        "the transport is stopped so sample_pos doesn't move, but the frames are split"
    );
    assert_eq!(l[offset - 1], 0.5);
    assert_eq!(l[offset], 0.25, "the very first frame of the step is new");
}

#[test]
fn a_chain_is_not_split_while_nothing_changes() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (r, track) = rig(None, 0.5);
    add_track(&mut h, track);
    rlive_testkit::render_chunks(BLOCK * 4, BLOCK, |l, rb| engine.process_stereo(l, rb));
    assert_eq!(r.log.lock().unwrap().len(), 4, "one whole-block call each");
}

#[test]
fn segments_carry_the_transport_and_midi_of_their_own_offsets() {
    use rlive_core::MidiMessage;
    /// Records where notes land in absolute frames, as a processor sees them.
    struct Notes {
        log: Arc<Mutex<Vec<(u64, u8)>>>,
    }
    impl Processor for Notes {
        fn activate(&mut self, _: f64, _: usize) {}
        fn process(&mut self, ctx: &mut ProcessContext) {
            let mut log = self.log.lock().unwrap();
            for e in ctx.events {
                if let MidiMessage::NoteOn { key, .. } = e.message {
                    log.push((ctx.transport.sample_pos + e.offset as u64, key));
                }
            }
        }
        fn set_param(&mut self, _: u32, _: f64) {}
    }

    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let log = Arc::new(Mutex::new(Vec::new()));
    let device = DeviceId::new();
    let id = TrackId::new();
    let mut track = TrackNode::new(id, BLOCK);
    track.devices.push(Box::new(DeviceSlot::new(
        device,
        Box::new(Notes { log: log.clone() }),
    )));
    let mut layout = BusLayout::for_track(&MixSettings::default());
    layout.place_device(device, true, &[level_param()], |p| p.default);
    let cell = layout.device_cell(device, 0).unwrap();
    track.bus = layout.build();
    // At 120 BPM a frame is 0.04 ticks, so these notes sit on frames 25, 150 and 250.
    let notes = [(1, 60), (6, 61), (10, 62)]
        .iter()
        .map(|&(start, key)| Note {
            start,
            length: 1,
            key,
            velocity: 0.8,
            ..Note::default()
        })
        .collect();
    track.set_slot(
        0,
        Some(Arc::new(ClipData::Midi(MidiClip::new(3840, notes, true)))),
    );
    // Splits the chain between the first and second note, at frame 96.
    track.hooks = Some(Box::new(AtStep {
        block: 1,
        index: 3,
        now: 0,
        cell,
        value: 0.1,
        kind: Kind::Automation,
    }));
    add_track(&mut h, Box::new(track));
    assert!(h
        .send(EngineCommand::Launch(LaunchCmd::LaunchClip {
            track: id,
            slot: 0
        }))
        .is_ok());
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    engine.process_stereo(&mut l, &mut r);

    assert_eq!(*log.lock().unwrap(), [(25, 60), (150, 61), (250, 62)]);
}

#[test]
fn a_mid_block_gain_change_is_sample_accurate() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (r, mut track) = rig(None, 1.0);
    let settle = settle_frames() / BLOCK;
    track.hooks = Some(Box::new(AtStep {
        block: settle as u32 + 1,
        index: 4,
        now: 0,
        cell: mix_cell::VOLUME,
        value: -20.0,
        kind: Kind::Automation,
    }));
    add_track(&mut h, track);
    rlive_testkit::render_chunks(settle * BLOCK, BLOCK, |l, rb| engine.process_stereo(l, rb));

    let (mut l, mut rb) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    engine.process_stereo(&mut l, &mut rb);
    let at = 4 * CONTROL_STEP;
    assert!((l[at - 1] - 1.0).abs() < 1e-6, "{}", l[at - 1]);
    assert!(l[at] < 1.0 - 1e-4, "{}", l[at]);
    assert!(l[at] > 0.99, "smoothed, not stepped: {}", l[at]);
    // The device itself was left alone: no split for a mixer-only change.
    assert_eq!(r.log.lock().unwrap().len(), settle + 1);
}

/// Sends a command to the engine from inside the block, at a given step.
struct SendAt {
    h: Arc<Mutex<EngineHandle>>,
    index: u32,
    send: Option<(TrackRef, CellIx, f64)>,
}

impl ControlHooks for SendAt {
    fn automation(&mut self, step: ControlStep, _: &mut ParamBus) {
        if step.index == self.index {
            if let Some((track, cell, value)) = self.send.take() {
                let cmd = EngineCommand::Param(ParamCmd::Set {
                    track,
                    cell,
                    value,
                    gesture: true,
                });
                assert!(self.h.lock().unwrap().send(cmd).is_ok());
            }
        }
    }
}

#[test]
fn a_set_arriving_mid_block_lands_on_the_next_control_step() {
    let (mut engine, h) = create_engine(SR, BLOCK);
    let h = Arc::new(Mutex::new(h));
    let (r, mut track) = rig(None, 0.5);
    track.hooks = Some(Box::new(SendAt {
        h: h.clone(),
        index: 3,
        send: Some((TrackRef::Track(r.id), r.cell, 0.25)),
    }));
    add_track(&mut h.lock().unwrap(), track);
    let (mut l, mut rb) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    engine.process_stereo(&mut l, &mut rb);
    // Queued during step 3, picked up and delivered at step 4.
    let at = 4 * CONTROL_STEP;
    assert_eq!(l[at - 1], 0.5);
    assert_eq!(l[at], 0.25);
}

#[test]
fn a_set_behind_a_graph_command_waits_for_the_next_block() {
    /// Queues a graph command and a Set during the block, in that order.
    struct GraphThenSet {
        h: Arc<Mutex<EngineHandle>>,
        send: Option<(TrackId, DeviceId, CellIx)>,
    }
    impl ControlHooks for GraphThenSet {
        fn automation(&mut self, step: ControlStep, _: &mut ParamBus) {
            if step.index != 3 {
                return;
            }
            if let Some((track, device, cell)) = self.send.take() {
                let mut h = self.h.lock().unwrap();
                let graph = EngineCommand::graph(GraphOp::MoveDevice {
                    target: ChainAddr::track(track),
                    device,
                    to: 0,
                });
                let set = EngineCommand::Param(ParamCmd::Set {
                    track: TrackRef::Track(track),
                    cell,
                    value: 0.25,
                    gesture: true,
                });
                assert!(h.send(graph).is_ok());
                assert!(h.send(set).is_ok());
            }
        }
    }

    let (mut engine, h) = create_engine(SR, BLOCK);
    let h = Arc::new(Mutex::new(h));
    let (r, mut track) = rig(None, 0.5);
    track.hooks = Some(Box::new(GraphThenSet {
        h: h.clone(),
        send: Some((r.id, r.device, r.cell)),
    }));
    add_track(&mut h.lock().unwrap(), track);
    let (mut l, mut rb) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    engine.process_stereo(&mut l, &mut rb);
    assert!(
        l.iter().all(|s| *s == 0.5),
        "the graph command holds the Set back for this block"
    );
    engine.process_stereo(&mut l, &mut rb);
    assert_eq!(l[0], 0.25, "both ran at the top of the next block");
}

#[test]
fn a_volume_change_is_click_free() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (r, track) = rig(None, 1.0);
    let id = r.id;
    add_track(&mut h, track);
    let settle = settle_frames();
    rlive_testkit::render_chunks(settle, BLOCK, |l, rb| engine.process_stereo(l, rb));
    // Slam the fader down between two host buffers.
    set_cell(&mut h, id, mix_cell::VOLUME, -60.0);
    let (l, _) =
        rlive_testkit::render_chunks(BLOCK * 8, BLOCK, |l, rb| engine.process_stereo(l, rb));
    let biggest_jump = l.windows(2).fold(0.0f32, |m, w| m.max((w[1] - w[0]).abs()));
    assert!(biggest_jump < 0.01, "{biggest_jump}");
    assert!((l[0] - 1.0).abs() < 0.01, "starts from where it was");
    assert!(*l.last().unwrap() < 0.002, "and reaches the new level");
}

#[test]
fn scale_and_additive_modulation_reach_the_output() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (r, mut track) = rig(None, 0.5);
    let cell = r.cell;
    let volume = mixer::infos()[0].clone();
    struct Mod {
        cell: CellIx,
    }
    impl ControlHooks for Mod {
        fn modulation(&mut self, _: ControlStep, bus: &mut ParamBus) {
            // Scale the device level down by half and nudge the fader up a quarter of its range.
            bus.modulate(self.cell, 0.0, 0.5);
            bus.modulate(mix_cell::VOLUME, 0.25, 1.0);
        }
    }
    track.hooks = Some(Box::new(Mod { cell }));
    add_track(&mut h, track);
    let frames = settle_frames();
    let (l, _) = rlive_testkit::render_chunks(frames, BLOCK, |l, rb| engine.process_stereo(l, rb));
    let norm = volume.to_normalized(0.0);
    let db = volume.from_normalized(norm + 0.25);
    let expect = 0.25 * rlive_core::db_to_gain(db as f32);
    assert!(
        (l[frames - 1] - expect).abs() < 1e-4,
        "{} vs {expect}",
        l[frames - 1]
    );
}

#[test]
fn turning_a_device_off_goes_through_its_activator() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (r, track) = rig(None, 0.5);
    let (id, activator) = (r.id, r.activator);
    add_track(&mut h, track);
    let (mut l, mut rb) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    engine.process_stereo(&mut l, &mut rb);
    assert!(l[BLOCK - 1] > 0.0);

    r.log.lock().unwrap().clear();
    set_cell(&mut h, id, activator, 0.0);
    rlive_testkit::render_chunks(2400, BLOCK, |l, rb| engine.process_stereo(l, rb));
    let log = r.log.lock().unwrap();
    assert_eq!(*log, [Seen::Reset], "reset once and then skipped");
    drop(log);

    set_cell(&mut h, id, activator, 1.0);
    let (l, _) = rlive_testkit::render_chunks(BLOCK, BLOCK, |l, rb| engine.process_stereo(l, rb));
    assert!(l[BLOCK - 1] > 0.0, "back on");
}

#[test]
fn soloing_a_track_silences_the_others_at_a_control_step() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (a, mut ta) = rig(None, 0.5);
    let (b, mut tb) = rig(None, 0.25);
    let settle = settle_frames() / BLOCK;
    ta.hooks = None;
    tb.hooks = Some(Box::new(AtStep {
        block: settle as u32 + 1,
        index: 2,
        now: 0,
        cell: mix_cell::SOLO,
        value: 1.0,
        kind: Kind::Macro,
    }));
    add_track(&mut h, ta);
    add_track(&mut h, tb);
    let _ = (&a, &b);
    rlive_testkit::render_chunks(settle * BLOCK, BLOCK, |l, rb| engine.process_stereo(l, rb));
    let (mut l, mut rb) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    engine.process_stereo(&mut l, &mut rb);
    let at = 2 * CONTROL_STEP;
    assert!((l[at - 1] - 0.75).abs() < 1e-3, "{}", l[at - 1]);
    let (l, _) =
        rlive_testkit::render_chunks(BLOCK * 8, BLOCK, |l, rb| engine.process_stereo(l, rb));
    assert!(
        (l[BLOCK * 8 - 1] - 0.25).abs() < 1e-3,
        "{}",
        l[BLOCK * 8 - 1]
    );
}

#[test]
fn the_master_has_a_bus_too() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (_track_rig, track) = rig(None, 1.0);
    add_track(&mut h, track);

    // A probe on the master, with its cells.
    let log = Arc::new(Mutex::new(Vec::new()));
    let device = DeviceId::new();
    let probe = Probe {
        level: 0.0,
        frames: 0,
        log: log.clone(),
    };
    let mut master = Box::new(TrackNode::master(BLOCK));
    master
        .devices
        .push(Box::new(DeviceSlot::new(device, Box::new(probe))));
    let mut layout = BusLayout::for_master(0.0);
    layout.place_device(device, true, &[level_param()], |_| 0.0);
    let cell = layout.device_cell(device, 0).unwrap();
    master.bus = layout.build();
    master.hooks = Some(Box::new(AtStep {
        block: 1,
        index: 3,
        now: 0,
        cell,
        value: 1.0,
        kind: Kind::Automation,
    }));
    assert!(h
        .send(EngineCommand::graph(GraphOp::ReplaceAll {
            tracks: Box::new(Vec::with_capacity(MAX_TRACKS)),
            master,
        }))
        .is_ok());
    let (mut l, mut rb) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    engine.process_stereo(&mut l, &mut rb);
    let offset = 3 * CONTROL_STEP;
    assert_eq!(
        *log.lock().unwrap(),
        [
            Seen::Block {
                frame: 0,
                len: offset,
                sample_pos: 0
            },
            Seen::Param {
                frame: offset as u64,
                id: 0,
                value: 1.0
            },
            Seen::Block {
                frame: offset as u64,
                len: BLOCK - offset,
                sample_pos: 0
            },
        ]
    );

    // Master volume is a cell as well.
    assert!(h
        .send(EngineCommand::Param(ParamCmd::Set {
            track: TrackRef::Master,
            cell: mix_cell::VOLUME,
            value: -96.0,
            gesture: true,
        }))
        .is_ok());
    let (l, _) =
        rlive_testkit::render_chunks(SR as usize, BLOCK, |l, rb| engine.process_stereo(l, rb));
    assert!(l[l.len() - 1].abs() < 1e-3);
}

#[test]
fn a_plugin_changing_a_param_is_reported_and_not_echoed_back() {
    /// Reports one parameter change of its own on every block.
    struct SelfMoving {
        value: f64,
        delivered: Arc<Mutex<Vec<f64>>>,
    }
    impl Processor for SelfMoving {
        fn activate(&mut self, _: f64, _: usize) {}
        fn process(&mut self, ctx: &mut ProcessContext) {
            for e in ctx.params {
                self.delivered.lock().unwrap().push(e.value);
            }
            ctx.param_out.push(0, self.value);
        }
        fn set_param(&mut self, _: u32, _: f64) {}
    }

    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let device = DeviceId::new();
    let id = TrackId::new();
    let delivered = Arc::new(Mutex::new(Vec::new()));
    let mut track = TrackNode::new(id, BLOCK);
    track.devices.push(Box::new(DeviceSlot::new(
        device,
        Box::new(SelfMoving {
            value: 0.75,
            delivered: delivered.clone(),
        }),
    )));
    let mut layout = BusLayout::for_track(&MixSettings::default());
    layout.place_device(device, true, &[level_param()], |p| p.default);
    let cell = layout.device_cell(device, 0).unwrap();
    track.bus = layout.build();
    let shared_cell = layout.effective(cell);
    assert_eq!(shared_cell, Some(0.5));
    add_track(&mut h, Box::new(track));
    rlive_testkit::render_chunks(BLOCK * 4, BLOCK, |l, r| engine.process_stereo(l, r));

    let event = h.poll_event().expect("a change event");
    assert_eq!(
        event,
        EngineEvent::ParamChanged {
            target: TrackRef::Track(id),
            device,
            param: 0,
            value: 0.75,
        }
    );
    assert_eq!(layout.effective(cell), Some(0.75), "the UI can read it");
    assert!(delivered.lock().unwrap().is_empty(), "never sent back");

    // Moving the knob to where the plugin is sends nothing; elsewhere does.
    set_cell(&mut h, id, cell, 0.75);
    rlive_testkit::render_chunks(BLOCK, BLOCK, |l, r| engine.process_stereo(l, r));
    assert!(delivered.lock().unwrap().is_empty());
    set_cell(&mut h, id, cell, 0.2);
    rlive_testkit::render_chunks(BLOCK, BLOCK, |l, r| engine.process_stereo(l, r));
    assert_eq!(*delivered.lock().unwrap(), [0.2]);
}

#[test]
fn a_bus_swap_keeps_live_values_and_retires_the_old_bus() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (r, track) = rig(None, 0.5);
    let (id, device) = (r.id, r.device);
    let cell = r.cell;
    add_track(&mut h, track);
    set_cell(&mut h, id, cell, 0.8);
    set_cell(&mut h, id, mix_cell::VOLUME, -12.0);
    rlive_testkit::render_chunks(BLOCK, BLOCK, |l, rb| engine.process_stereo(l, rb));

    // Add a device to the layout, which replaces the bus.
    let mut layout = BusLayout::for_track(&MixSettings::default());
    let params = [level_param()];
    layout.place_device(device, true, &params, |p| p.default);
    assert_eq!(layout.device_cell(device, 0), Some(cell));
    let other = DeviceId::new();
    layout.place_device(other, true, &params, |p| p.default);
    assert!(h
        .send(EngineCommand::graph(GraphOp::SwapBus {
            target: TrackRef::Track(id),
            bus: layout.build(),
        }))
        .is_ok());
    let (l, _) =
        rlive_testkit::render_chunks(SR as usize, BLOCK, |l, rb| engine.process_stereo(l, rb));
    assert_eq!(h.collect_garbage(), 1, "the old bus comes back to be freed");
    let gain = rlive_core::db_to_gain(-12.0);
    let (last, expect) = (l[l.len() - 1], 0.8 * gain);
    assert!((last - expect).abs() < 1e-4, "{last} vs {expect}");
    assert_eq!(layout.effective(cell).unwrap(), 0.8, "live value survives");
}

#[test]
fn set_routes_and_re_enable_are_real_time_safe() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (r, track) = rig(None, 0.5);
    let id = r.id;
    add_track(&mut h, track);
    let (mut l, mut rb) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    engine.process_stereo(&mut l, &mut rb);
    assert!(h
        .send(EngineCommand::Param(ParamCmd::SetRoutes {
            track: TrackRef::Track(id),
            routes: Arc::new(TrackRoutes::default()),
        }))
        .is_ok());
    assert!(h
        .send(EngineCommand::Param(ParamCmd::ReEnable {
            track: TrackRef::Track(id),
            cell: None,
        }))
        .is_ok());
    assert_no_alloc(|| engine.process_stereo(&mut l, &mut rb));
    assert_eq!(h.collect_garbage(), 1, "the previous routes");
}

#[test]
fn no_allocation_while_parameters_change() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (r, mut track) = rig(None, 0.5);
    let (id, cell, activator) = (r.id, r.cell, r.activator);
    struct Mod {
        cell: CellIx,
        phase: f32,
    }
    impl ControlHooks for Mod {
        fn macros(&mut self, _: ControlStep, bus: &mut ParamBus) {
            self.phase += 0.1;
            bus.write_macro(self.cell, self.phase.sin() as f64 * 0.5 + 0.5);
        }
        fn automation(&mut self, step: ControlStep, bus: &mut ParamBus) {
            bus.write_automation(mix_cell::PAN, (step.index as f64 * 0.1).sin());
        }
        fn modulation(&mut self, _: ControlStep, bus: &mut ParamBus) {
            bus.modulate(mix_cell::VOLUME, self.phase.cos() * 0.1, 0.9);
        }
    }
    track.hooks = Some(Box::new(Mod { cell, phase: 0.0 }));
    // The probe logs every call; it must not be the thing that allocates.
    r.log.lock().unwrap().reserve(1 << 20);
    add_track(&mut h, track);
    let (mut l, mut rb) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    engine.process_stereo(&mut l, &mut rb);

    for i in 0..64u32 {
        let v = i as f64 / 64.0;
        set_cell(&mut h, id, mix_cell::VOLUME, -12.0 * v);
        set_cell(&mut h, id, cell, v);
        set_cell(&mut h, id, activator, f64::from(i % 2));
        assert_no_alloc(|| engine.process_stereo(&mut l, &mut rb));
    }
}

/// Keeps `MixSettings` and the fader range honest as they evolve.
#[test]
fn mixer_params_match_the_fader() {
    let infos = mixer::infos();
    assert_eq!(infos[0].param_id(), mixer::VOLUME);
    assert_eq!(infos[0].mod_mode, ModMode::Scale);
    assert_eq!(infos[1].param_id(), mixer::PAN);
    assert_eq!(infos[2].param_id(), mixer::MUTE);
    assert_eq!(infos[3].param_id(), mixer::SOLO);
    assert_eq!(mix_cell::VOLUME.0, mixer::VOLUME.0);
    assert_eq!(mix_cell::SOLO.0, mixer::SOLO.0);
}
