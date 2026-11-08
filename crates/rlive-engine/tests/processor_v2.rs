//! The device chain under Processor v2: sample-timed parameters, MIDI effects and resources.
#![allow(clippy::vec_box)]

use rlive_core::{
    DeviceId, MidiEvent, MidiMessage, ParamEvent, ParamId, ParamInfo, ProcessContext, Processor,
    Resource, TrackId, TransportInfo,
};
use rlive_engine::*;
use rlive_testkit::assert_no_alloc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

fn note_on(key: u8) -> MidiMessage {
    MidiMessage::NoteOn {
        channel: 0,
        key,
        velocity: 1.0,
        note_id: 0,
        tuning: 0.0,
    }
}

fn key_of(m: &MidiMessage) -> Option<u8> {
    match m {
        MidiMessage::NoteOn { key, .. } => Some(*key),
        _ => None,
    }
}

/// Records the keys it hears each block.
struct Keys {
    heard: Arc<Mutex<Vec<u8>>>,
}

impl Processor for Keys {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        let mut heard = self.heard.lock().unwrap();
        heard.extend(ctx.events.iter().filter_map(|e| key_of(&e.message)));
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

/// A MIDI effect that transposes every note, or swallows them all.
struct Transpose {
    by: Option<i8>,
}

impl Processor for Transpose {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        let Some(by) = self.by else { return };
        for e in ctx.events {
            let mut m = e.message;
            if let MidiMessage::NoteOn { key, .. } = &mut m {
                *key = key.wrapping_add_signed(by);
            }
            ctx.events_out.push(MidiEvent::new(e.offset, m));
        }
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

/// Writes a note to `events_out` despite not being a MIDI effect.
struct Rogue;

impl Processor for Rogue {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        ctx.events_out.push(MidiEvent::new(0, note_on(1)));
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

struct Chain {
    id: TrackId,
    keys: Arc<Mutex<Vec<u8>>>,
    h: EngineHandle,
    sender: MidiSender,
    engine: Engine,
}

fn slot(p: impl Processor + 'static, midi_effect: bool) -> Box<DeviceSlot> {
    let mut s = DeviceSlot::new(DeviceId::new(), Box::new(p));
    s.midi_effect = midi_effect;
    Box::new(s)
}

/// A track running `first`, then something recording what it hears.
fn chain(first: Vec<Box<DeviceSlot>>) -> Chain {
    let (engine, mut h) = create_engine(SR, BLOCK);
    let id = TrackId::new();
    let keys = Arc::new(Mutex::new(Vec::new()));
    let mut track = Box::new(TrackNode::new(id, BLOCK));
    track.devices.extend(first);
    track.devices.push(slot(
        Keys {
            heard: keys.clone(),
        },
        false,
    ));
    assert!(h
        .send(EngineCommand::graph(GraphOp::AddTrack { index: 0, track }))
        .is_ok());
    let sender = h.take_midi_sender().unwrap();
    let mut c = Chain {
        id,
        keys,
        h,
        sender,
        engine,
    };
    // The engine reads MIDI before commands, so the track has to exist before the first note.
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    c.engine.process_stereo(&mut l, &mut r);
    c
}

impl Chain {
    fn play(&mut self, key: u8) -> Vec<u8> {
        assert!(self.sender.send(self.id, note_on(key)));
        let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
        self.engine.process_stereo(&mut l, &mut r);
        std::mem::take(&mut self.keys.lock().unwrap())
    }
}

#[test]
fn a_midi_effects_output_reaches_the_next_device() {
    let mut c = chain(vec![slot(Transpose { by: Some(12) }, true)]);
    assert_eq!(c.play(60), [72]);
}

#[test]
fn midi_effects_chain_in_order() {
    let mut c = chain(vec![
        slot(Transpose { by: Some(1) }, true),
        slot(Transpose { by: Some(2) }, true),
        slot(Transpose { by: Some(4) }, true),
    ]);
    assert_eq!(c.play(60), [67]);
}

#[test]
fn a_midi_effect_can_swallow_notes() {
    let mut c = chain(vec![slot(Transpose { by: None }, true)]);
    assert_eq!(c.play(60), Vec::<u8>::new());
}

#[test]
fn a_device_that_is_not_a_midi_effect_cannot_change_the_stream() {
    let mut c = chain(vec![slot(Rogue, false)]);
    assert_eq!(c.play(60), [60], "live MIDI flows on as before");
}

#[test]
fn a_bypassed_midi_effect_lets_the_input_through() {
    let mut fx = slot(Transpose { by: Some(12) }, true);
    fx.bypass = true;
    let mut c = chain(vec![fx]);
    assert_eq!(c.play(60), [60]);
}

/// Records every parameter event and each call's length.
struct Params {
    params: Arc<Mutex<Vec<(usize, ParamEvent)>>>,
    calls: usize,
}

impl Processor for Params {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        self.calls += 1;
        let mut log = self.params.lock().unwrap();
        for e in ctx.params {
            log.push((self.calls, *e));
        }
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

fn level_param() -> ParamInfo {
    ParamInfo::continuous(3, "Level", 0.0, 1.0, 0.5, "")
}

/// Sets a device cell from a hook, at given steps.
struct Script {
    steps: Vec<(u32, f64)>,
    cell: CellIx,
}

impl ControlHooks for Script {
    fn automation(&mut self, step: ControlStep, bus: &mut ParamBus) {
        if let Some(&(_, v)) = self.steps.iter().find(|(at, _)| *at == step.index) {
            bus.write_automation(self.cell, v);
        }
    }
}

struct Rig {
    log: Arc<Mutex<Vec<(usize, ParamEvent)>>>,
    id: TrackId,
    activator: CellIx,
    h: EngineHandle,
    engine: Engine,
}

fn params_rig(steps: Vec<(u32, f64)>) -> Rig {
    let (engine, mut h) = create_engine(SR, BLOCK);
    let id = TrackId::new();
    let device = DeviceId::new();
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut track = Box::new(TrackNode::new(id, BLOCK));
    track.devices.push(Box::new(DeviceSlot::new(
        device,
        Box::new(Params {
            params: log.clone(),
            calls: 0,
        }),
    )));
    let mut layout = BusLayout::for_track(&MixSettings::default());
    layout.place_device(device, true, &[level_param()], |p| p.default);
    let cell = layout.device_cell(device, 3).unwrap();
    let activator = layout.activator_cell(device).unwrap();
    track.bus = layout.build();
    track.hooks = Some(Box::new(Script { steps, cell }));
    assert!(h
        .send(EngineCommand::graph(GraphOp::AddTrack { index: 0, track }))
        .is_ok());
    Rig {
        log,
        id,
        activator,
        h,
        engine,
    }
}

impl Rig {
    fn block(&mut self) {
        let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
        self.engine.process_stereo(&mut l, &mut r);
    }
}

fn event(offset: u32, value: f64) -> ParamEvent {
    ParamEvent {
        offset,
        id: ParamId(3),
        value,
    }
}

#[test]
fn changes_arrive_as_timed_events_in_a_single_call() {
    let mut r = params_rig(vec![(2, 0.1), (5, 0.2)]);
    r.block();
    assert_eq!(
        *r.log.lock().unwrap(),
        [
            (1, event(2 * CONTROL_STEP as u32, 0.1)),
            (1, event(5 * CONTROL_STEP as u32, 0.2)),
        ],
        "both in the device's first and only call"
    );
}

#[test]
fn a_device_hears_a_change_made_while_it_is_off_when_it_comes_back() {
    let mut r = params_rig(vec![(3, 0.25)]);
    // Off from the start of this block.
    assert!(r
        .h
        .send(EngineCommand::Param(ParamCmd::Set {
            track: TrackRef::Track(r.id),
            cell: r.activator,
            value: 0.0,
            gesture: true,
        }))
        .is_ok());
    r.block();
    assert!(r.log.lock().unwrap().is_empty(), "skipped while off");
    r.block();
    assert!(r.log.lock().unwrap().is_empty());

    assert!(r
        .h
        .send(EngineCommand::Param(ParamCmd::Set {
            track: TrackRef::Track(r.id),
            cell: r.activator,
            value: 1.0,
            gesture: true,
        }))
        .is_ok());
    r.block();
    assert_eq!(*r.log.lock().unwrap(), [(1, event(0, 0.25))]);
}

#[test]
fn switching_a_device_off_mid_block_stops_it_at_that_step() {
    /// Counts the frames it has processed.
    struct Counter {
        frames: Arc<Mutex<usize>>,
    }
    impl Processor for Counter {
        fn activate(&mut self, _: f64, _: usize) {}
        fn process(&mut self, ctx: &mut ProcessContext) {
            *self.frames.lock().unwrap() += ctx.audio.frames();
        }
        fn set_param(&mut self, _: u32, _: f64) {}
    }
    struct OffAt {
        step: u32,
        cell: CellIx,
    }
    impl ControlHooks for OffAt {
        fn automation(&mut self, step: ControlStep, bus: &mut ParamBus) {
            if step.index == self.step {
                bus.write_automation(self.cell, 0.0);
            }
        }
    }

    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let frames = Arc::new(Mutex::new(0));
    let device = DeviceId::new();
    let id = TrackId::new();
    let mut track = Box::new(TrackNode::new(id, BLOCK));
    track.devices.push(Box::new(DeviceSlot::new(
        device,
        Box::new(Counter {
            frames: frames.clone(),
        }),
    )));
    let mut layout = BusLayout::for_track(&MixSettings::default());
    layout.place_device(device, true, &[], |p| p.default);
    let cell = layout.activator_cell(device).unwrap();
    track.bus = layout.build();
    track.hooks = Some(Box::new(OffAt { step: 3, cell }));
    assert!(h
        .send(EngineCommand::graph(GraphOp::AddTrack { index: 0, track }))
        .is_ok());
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    engine.process_stereo(&mut l, &mut r);
    assert_eq!(*frames.lock().unwrap(), 3 * CONTROL_STEP);
}

#[test]
fn a_device_latency_is_published_after_it_runs() {
    /// Reports a latency that can change as it runs, like a look-ahead setting.
    struct Latent;
    impl Processor for Latent {
        fn activate(&mut self, _: f64, _: usize) {}
        fn process(&mut self, _: &mut ProcessContext) {}
        fn set_param(&mut self, _: u32, _: f64) {}
        fn latency(&self) -> u32 {
            128
        }
    }
    let device = slot(Latent, false);
    let latency = device.latency.clone();
    assert_eq!(latency.load(Ordering::Relaxed), 0);
    let _running = chain(vec![device]);
    assert_eq!(latency.load(Ordering::Relaxed), 128);
}

#[test]
fn the_transport_carries_the_bar_start_and_loop() {
    let seen = Arc::new(Mutex::new(Vec::<TransportInfo>::new()));
    struct Spy(Arc<Mutex<Vec<TransportInfo>>>);
    impl Processor for Spy {
        fn activate(&mut self, _: f64, _: usize) {}
        fn process(&mut self, ctx: &mut ProcessContext) {
            self.0.lock().unwrap().push(*ctx.transport);
        }
        fn set_param(&mut self, _: u32, _: f64) {}
    }
    let mut c = chain(vec![slot(Spy(seen.clone()), false)]);
    let bar = 3840.0;
    for cmd in [
        TransportCmd::SetLoop(Some((bar, 2.0 * bar))),
        TransportCmd::Seek(bar + 100.0),
        TransportCmd::Play,
    ] {
        assert!(c.h.send(EngineCommand::Transport(cmd)).is_ok());
    }
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    c.engine.process_stereo(&mut l, &mut r);
    let t = *seen.lock().unwrap().last().unwrap();
    assert_eq!(t.bar_start, bar);
    assert_eq!(t.loop_range, Some((bar, 2.0 * bar)));
}

/// Takes a shared `u64` and hands back what it had.
struct Holder {
    held: Option<Resource>,
    value: Arc<AtomicU64>,
}

impl Processor for Holder {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, _: &mut ProcessContext) {}
    fn set_param(&mut self, _: u32, _: f64) {}
    fn set_resource(&mut self, _: u32, resource: Resource) -> Option<Resource> {
        self.value
            .store(*resource.downcast_ref::<u64>().unwrap(), Ordering::Relaxed);
        self.held.replace(resource)
    }
}

#[test]
fn a_replaced_resource_is_retired_and_never_freed_on_the_audio_thread() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let id = TrackId::new();
    let device = DeviceId::new();
    let value = Arc::new(AtomicU64::new(0));
    let mut track = Box::new(TrackNode::new(id, BLOCK));
    track.devices.push(Box::new(DeviceSlot::new(
        device,
        Box::new(Holder {
            held: None,
            value: value.clone(),
        }),
    )));
    assert!(h
        .send(EngineCommand::graph(GraphOp::AddTrack { index: 0, track }))
        .is_ok());
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    engine.process_stereo(&mut l, &mut r);

    let send_res = |h: &mut EngineHandle, v: u64| {
        assert!(h
            .send(EngineCommand::graph(GraphOp::SetResource {
                target: ChainAddr::track(id),
                device,
                slot: 0,
                resource: Arc::new(v),
            }))
            .is_ok());
    };
    send_res(&mut h, 1);
    assert_no_alloc(|| engine.process_stereo(&mut l, &mut r));
    assert_eq!(value.load(Ordering::Relaxed), 1);
    assert_eq!(h.collect_garbage(), 0, "nothing replaced the first time");

    send_res(&mut h, 2);
    assert_no_alloc(|| engine.process_stereo(&mut l, &mut r));
    assert_eq!(value.load(Ordering::Relaxed), 2);
    assert_eq!(h.collect_garbage(), 1, "the first resource comes back here");
}

#[test]
fn a_resource_for_a_missing_device_comes_back_too() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    assert!(h
        .send(EngineCommand::graph(GraphOp::SetResource {
            target: ChainAddr::MASTER,
            device: DeviceId::new(),
            slot: 0,
            resource: Arc::new(5u32),
        }))
        .is_ok());
    assert_no_alloc(|| engine.process_stereo(&mut l, &mut r));
    assert_eq!(h.collect_garbage(), 1);
}

#[test]
fn a_chain_of_midi_effects_does_not_allocate() {
    let mut c = chain(vec![
        slot(Transpose { by: Some(1) }, true),
        slot(Transpose { by: Some(2) }, true),
    ]);
    c.keys.lock().unwrap().reserve(1024);
    assert_eq!(c.play(60), [63]);
    c.keys.lock().unwrap().reserve(1024);
    assert!(c.sender.send(c.id, note_on(60)));
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    let engine = &mut c.engine;
    assert_no_alloc(|| engine.process_stereo(&mut l, &mut r));
}
