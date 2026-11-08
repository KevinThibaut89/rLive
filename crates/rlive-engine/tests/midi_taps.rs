//! MIDI read from another track has to end like any other: what a bypassed MIDI effect or a chain
//! switched off releases is part of the tapped stream.
#![allow(clippy::vec_box)]

use rlive_core::param::chain as controls;
use rlive_core::{ChainId, DeviceId, MidiMessage, ParamInfo, ProcessContext, Processor, TrackId};
use rlive_devices::devices::b1::pitch;
use rlive_engine::*;
use rlive_testkit::assert_no_alloc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

fn on(key: u8) -> MidiMessage {
    MidiMessage::NoteOn {
        channel: 0,
        key,
        velocity: 0.8,
        note_id: 0,
        tuning: 0.0,
    }
}

/// How many notes are held on each key.
struct Sounding([AtomicI32; 128]);

impl Sounding {
    fn new() -> Arc<Self> {
        Arc::new(Self(std::array::from_fn(|_| AtomicI32::new(0))))
    }

    fn held(&self) -> Vec<u8> {
        (0..128)
            .filter(|k| self.0[*k as usize].load(Ordering::Relaxed) > 0)
            .collect()
    }
}

struct Instrument(Arc<Sounding>);

impl Processor for Instrument {
    fn activate(&mut self, _: f64, _: usize) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        for e in ctx.events {
            match e.message {
                MidiMessage::NoteOn { key, .. } => {
                    self.0 .0[key as usize].fetch_add(1, Ordering::Relaxed);
                }
                MidiMessage::NoteOff { key, .. } => {
                    self.0 .0[key as usize].fetch_add(-1, Ordering::Relaxed);
                }
                _ => {}
            }
        }
    }
    fn set_param(&mut self, _: u32, _: f64) {}
}

struct Rig {
    source: TrackId,
    cell: CellIx,
    sender: MidiSender,
    h: EngineHandle,
    engine: Engine,
    sounding: Arc<Sounding>,
}

impl Rig {
    /// Track 0 plays through `devices`, and its MIDI goes on to the instrument on track 1.
    fn new(
        devices: Vec<Box<DeviceSlot>>,
        mut layout: BusLayout,
        cell: impl FnOnce(&BusLayout) -> CellIx,
        target: impl FnOnce(DeviceId) -> Option<DeviceId>,
        gate: InputGate,
    ) -> Self {
        let (engine, mut h) = create_engine(SR, BLOCK);
        let source = TrackId::new();
        let cell = cell(&layout);
        let sounding = Sounding::new();
        let mut a = Box::new(TrackNode::new(source, BLOCK));
        a.devices.extend(devices);
        a.bus = layout.build();
        let mut b = Box::new(TrackNode::new(TrackId::new(), BLOCK));
        let inst = DeviceId::new();
        b.devices.push(Box::new(DeviceSlot::new(
            inst,
            Box::new(Instrument(sounding.clone())),
        )));
        let plan = RenderPlan {
            steps: vec![
                Step {
                    track: 0,
                    inputs: vec![],
                },
                Step {
                    track: 1,
                    inputs: vec![],
                },
                Step {
                    track: 2,
                    inputs: vec![BusIn::post_mixer(0), BusIn::post_mixer(1)],
                },
            ],
            levels: vec![0..1, 1..2, 2..3],
            midi: vec![MidiRoute {
                from: 0,
                to: 1,
                device: target(inst),
                channel: None,
            }],
            io: vec![TrackIo::default(), TrackIo { gate, own_delay: 0 }],
            ..Default::default()
        };
        let txn = GraphTxn::new(
            vec![
                GraphOp::AddTrack { index: 0, track: a },
                GraphOp::AddTrack { index: 1, track: b },
            ],
            Some(plan),
        );
        assert!(h.send(EngineCommand::Graph(txn)).is_ok());
        let sender = h.take_midi_sender().unwrap();
        let mut rig = Self {
            source,
            cell,
            sender,
            h,
            engine,
            sounding,
        };
        rig.blocks(1);
        assert!(rig.engine.has_compiled_plan());
        rig
    }

    fn blocks(&mut self, n: usize) {
        for _ in 0..n {
            let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
            assert_no_alloc(|| self.engine.process_stereo(&mut l, &mut r));
        }
    }

    fn play(&mut self, m: MidiMessage) {
        assert!(self.sender.send(self.source, m));
    }

    fn switch(&mut self, value: f64) {
        assert!(self
            .h
            .send(EngineCommand::Param(ParamCmd::Set {
                track: TrackRef::Track(self.source),
                cell: self.cell,
                value,
                gesture: true,
            }))
            .is_ok());
    }
}

fn default_strip() -> [f64; controls::CONTROLS] {
    let mut v = [0.0; controls::CONTROLS];
    v[2] = 1.0;
    for s in v.iter_mut().skip(4) {
        *s = SEND_MIN_DB;
    }
    v
}

fn pitch_slot() -> (DeviceId, Box<DeviceSlot>, Vec<ParamInfo>) {
    let mut p = pitch::Pitch::default();
    p.set_param(pitch::PITCH, 7.0);
    let id = DeviceId::new();
    let mut slot = DeviceSlot::new(id, Box::new(p));
    slot.midi_effect = true;
    (id, Box::new(slot), pitch::params())
}

fn bypassing_releases(target: impl FnOnce(DeviceId) -> Option<DeviceId>, gate: InputGate) {
    let (id, slot, params) = pitch_slot();
    let mut layout = BusLayout::for_track(&MixSettings::default());
    layout.place_device(id, true, &params, |p| p.default);
    let mut r = Rig::new(
        vec![slot],
        layout,
        |l| l.activator_cell(id).unwrap(),
        target,
        gate,
    );
    r.play(on(60));
    r.blocks(2);
    assert_eq!(r.sounding.held(), [67], "heard after the effect");
    r.switch(0.0);
    r.blocks(1);
    assert!(r.sounding.held().is_empty(), "the flush reached the tap");
}

#[test]
fn a_tap_addressing_a_device_sees_the_note_offs_of_a_bypassed_effect() {
    bypassing_releases(Some, InputGate::Never);
}

#[test]
fn a_tap_to_track_in_sees_the_note_offs_of_a_bypassed_effect() {
    bypassing_releases(|_| None, InputGate::Always);
}

#[test]
fn a_note_let_in_before_monitoring_closed_still_ends() {
    let (id, slot, params) = pitch_slot();
    let mut layout = BusLayout::for_track(&MixSettings::default());
    layout.place_device(id, true, &params, |p| p.default);
    let mut r = Rig::new(
        vec![slot],
        layout,
        |l| l.activator_cell(id).unwrap(),
        |_| None,
        InputGate::Always,
    );
    r.play(on(60));
    r.blocks(2);
    assert_eq!(r.sounding.held(), [67]);
    // The plan changes under the held note: monitoring shuts, and the note-off comes later.
    let plan = {
        let mut p = r.engine.active_plan().clone();
        p.io[1].gate = InputGate::Never;
        p
    };
    assert!(r
        .h
        .send(EngineCommand::Graph(GraphTxn::new(Vec::new(), Some(plan))))
        .is_ok());
    r.blocks(1);
    r.play(MidiMessage::NoteOff {
        channel: 0,
        key: 60,
        velocity: 0.5,
        note_id: 0,
    });
    r.blocks(1);
    assert!(r.sounding.held().is_empty());
    r.play(on(62));
    r.blocks(1);
    assert!(r.sounding.held().is_empty(), "nothing new gets in");
}

#[test]
fn a_tap_sees_the_note_offs_of_a_chain_switched_off() {
    let (id, slot, _) = pitch_slot();
    let chain_id = ChainId::new();
    let mut chain = Box::new(ChainNode::new(chain_id, BLOCK));
    chain.devices.push(slot);
    let mut rack = Box::new(RackNode::new(RackKind::Midi));
    rack.chains.push(chain);
    let rack_id = DeviceId::new();
    let rack_slot = Box::new(DeviceSlot::rack(rack_id, rack));

    let mut layout = BusLayout::for_track(&MixSettings::default());
    layout.place_device(rack_id, true, &[], |p| p.default);
    layout.place_chain(chain_id, &default_strip());
    layout.place_device(id, true, &pitch::params(), |p| p.default);
    let mut r = Rig::new(
        vec![rack_slot],
        layout,
        |l| l.chain_cell(chain_id, controls::ACTIVATOR).unwrap(),
        Some,
        InputGate::Never,
    );
    r.play(on(60));
    r.blocks(2);
    assert_eq!(r.sounding.held(), [67], "through the chain's effect");
    r.switch(0.0);
    r.blocks(2);
    assert!(r.sounding.held().is_empty(), "the chain let go of its note");
}
