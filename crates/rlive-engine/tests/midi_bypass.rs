//! Bypassing a MIDI effect must end the notes it had sounding: the instrument after it can
//! only release what it is told about, and a bypassed effect no longer gets to tell it.
#![allow(clippy::vec_box)]

use rlive_core::{ChainId, DeviceId, MidiMessage, ParamInfo, ProcessContext, Processor, TrackId};
use rlive_devices::devices::b1::{arpeggiator, chord, pitch};
use rlive_engine::*;
use rlive_testkit::assert_no_alloc;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
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

fn off(key: u8) -> MidiMessage {
    MidiMessage::NoteOff {
        channel: 0,
        key,
        velocity: 0.5,
        note_id: 0,
    }
}

/// What the instrument at the end of the chain sees. Notes sound by key, as in the built-in
/// instruments. No locks, because the allocation guard watches the audio thread.
struct Sounding {
    keys: [AtomicI32; 128],
    started: AtomicU32,
    orphans: AtomicU32,
}

impl Sounding {
    fn new() -> Self {
        Self {
            keys: std::array::from_fn(|_| AtomicI32::new(0)),
            started: AtomicU32::new(0),
            orphans: AtomicU32::new(0),
        }
    }

    fn held(&self) -> Vec<u8> {
        (0..128)
            .filter(|k| self.keys[*k as usize].load(Ordering::Relaxed) > 0)
            .collect()
    }

    fn total(&self) -> i32 {
        self.keys.iter().map(|k| k.load(Ordering::Relaxed)).sum()
    }

    fn started(&self) -> u32 {
        self.started.load(Ordering::Relaxed)
    }
}

struct Instrument(Arc<Sounding>);

impl Processor for Instrument {
    fn activate(&mut self, _: f64, _: usize) {}

    fn process(&mut self, ctx: &mut ProcessContext) {
        for e in ctx.events {
            match e.message {
                MidiMessage::NoteOn { key, .. } => {
                    self.0.keys[key as usize].fetch_add(1, Ordering::Relaxed);
                    self.0.started.fetch_add(1, Ordering::Relaxed);
                }
                MidiMessage::NoteOff { key, .. } => {
                    let slot = &self.0.keys[key as usize];
                    if slot.load(Ordering::Relaxed) > 0 {
                        slot.fetch_sub(1, Ordering::Relaxed);
                    } else {
                        self.0.orphans.fetch_add(1, Ordering::Relaxed);
                    }
                }
                _ => {}
            }
        }
    }

    fn set_param(&mut self, _: u32, _: f64) {}
}

fn effect(device: DeviceId, p: Box<dyn Processor>) -> Box<DeviceSlot> {
    let mut s = DeviceSlot::new(device, p);
    s.midi_effect = true;
    Box::new(s)
}

struct Rig {
    id: TrackId,
    activator: CellIx,
    sounding: Arc<Sounding>,
    h: EngineHandle,
    sender: MidiSender,
    engine: Engine,
}

impl Rig {
    /// `devices` come first and hold the bypassed one: its cell's activator is `target`.
    fn new(devices: Vec<Box<DeviceSlot>>, mut layout: BusLayout, target: DeviceId) -> Self {
        let (engine, mut h) = create_engine(SR, BLOCK);
        let id = TrackId::new();
        let activator = layout.activator_cell(target).unwrap();
        let sounding = Arc::new(Sounding::new());
        let mut track = Box::new(TrackNode::new(id, BLOCK));
        track.devices.extend(devices);
        track.devices.push(Box::new(DeviceSlot::new(
            DeviceId::new(),
            Box::new(Instrument(sounding.clone())),
        )));
        track.bus = layout.build();
        assert!(h
            .send(EngineCommand::graph(GraphOp::AddTrack { index: 0, track }))
            .is_ok());
        let sender = h.take_midi_sender().unwrap();
        let mut rig = Self {
            id,
            activator,
            sounding,
            h,
            sender,
            engine,
        };
        rig.blocks(1);
        rig
    }

    fn blocks(&mut self, n: usize) {
        for _ in 0..n {
            let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
            assert_no_alloc(|| self.engine.process_stereo(&mut l, &mut r));
        }
    }

    fn send(&mut self, m: MidiMessage) {
        assert!(self.sender.send(self.id, m));
    }

    fn set_on(&mut self, on: bool) {
        assert!(self
            .h
            .send(EngineCommand::Param(ParamCmd::Set {
                track: TrackRef::Track(self.id),
                cell: self.activator,
                value: if on { 1.0 } else { 0.0 },
                gesture: true,
            }))
            .is_ok());
    }
}

fn layout_for(devices: &[(DeviceId, Vec<ParamInfo>)]) -> BusLayout {
    let mut layout = BusLayout::for_track(&MixSettings::default());
    for (id, params) in devices {
        layout.place_device(*id, true, params, |p| p.default);
    }
    layout
}

fn pitch_up(semitones: f64) -> (DeviceId, Box<dyn Processor>, Vec<ParamInfo>) {
    let mut p = pitch::Pitch::default();
    p.set_param(pitch::PITCH, semitones);
    (DeviceId::new(), Box::new(p), pitch::params())
}

#[test]
fn bypassing_pitch_releases_the_transposed_note_at_the_instrument() {
    let (id, p, params) = pitch_up(7.0);
    let mut r = Rig::new(vec![effect(id, p)], layout_for(&[(id, params)]), id);

    r.send(on(60));
    r.blocks(2);
    assert_eq!(r.sounding.held(), [67], "the transposed note is playing");

    r.set_on(false);
    r.blocks(1);
    assert_eq!(
        r.sounding.total(),
        0,
        "bypass released it at the instrument"
    );

    // The key comes up while bypassed: the raw note-off finds nothing to release.
    r.send(off(60));
    r.blocks(1);
    assert_eq!(r.sounding.total(), 0);

    // Back on: it works exactly as before, including the release.
    r.set_on(true);
    r.blocks(1);
    r.send(on(60));
    r.blocks(1);
    assert_eq!(r.sounding.held(), [67]);
    r.send(off(60));
    r.blocks(1);
    assert_eq!(r.sounding.total(), 0, "released through the device again");
}

#[test]
fn a_note_sounding_through_a_bypassed_effect_still_ends_after_it_returns() {
    let (id, p, params) = pitch_up(7.0);
    let mut r = Rig::new(vec![effect(id, p)], layout_for(&[(id, params)]), id);

    r.set_on(false);
    r.blocks(1);
    r.send(on(60));
    r.blocks(1);
    assert_eq!(
        r.sounding.held(),
        [60],
        "passes straight through while bypassed"
    );

    r.set_on(true);
    r.blocks(1);
    r.send(off(60));
    r.blocks(1);
    assert_eq!(
        r.sounding.total(),
        0,
        "its note-off still reaches the instrument"
    );
}

#[test]
fn bypassing_an_arpeggiator_stops_its_notes_and_starts_none() {
    let id = DeviceId::new();
    let mut a = arpeggiator::Arpeggiator::with_seed(1);
    a.set_param(arpeggiator::GATE, 200.0);
    let params = arpeggiator::params();
    let mut r = Rig::new(
        vec![effect(id, Box::new(a))],
        layout_for(&[(id, params)]),
        id,
    );

    r.send(on(60));
    r.send(on(64));
    r.send(on(67));
    r.blocks(200);
    assert!(r.sounding.total() > 0, "the arpeggio is playing");

    r.set_on(false);
    r.blocks(1);
    assert_eq!(r.sounding.total(), 0, "every generated note ended");
    let started = r.sounding.started();
    r.blocks(200);
    assert_eq!(r.sounding.started(), started, "and no more follow");

    r.send(off(60));
    r.send(off(64));
    r.send(off(67));
    r.blocks(1);
    assert_eq!(r.sounding.total(), 0);

    r.set_on(true);
    r.blocks(1);
    r.send(on(72));
    r.blocks(2);
    assert_eq!(r.sounding.held(), [72], "plays again after coming back");
    r.send(off(72));
    r.blocks(2_000);
    assert_eq!(r.sounding.total(), 0, "and stops again");
}

#[test]
fn bypassing_a_chord_releases_all_of_its_notes() {
    let id = DeviceId::new();
    let mut c = chord::Chord::with_seed(1);
    c.set_param(chord::FIRST_SHIFT, 4.0);
    c.set_param(chord::FIRST_SHIFT + 1, 7.0);
    let params = chord::params();
    let mut r = Rig::new(
        vec![effect(id, Box::new(c))],
        layout_for(&[(id, params)]),
        id,
    );

    r.send(on(60));
    r.blocks(2);
    assert_eq!(r.sounding.held(), [60, 64, 67]);
    r.set_on(false);
    r.blocks(1);
    assert_eq!(r.sounding.total(), 0);
}

#[test]
fn the_rest_of_the_midi_stream_still_arrives_on_the_block_of_the_bypass() {
    let (id, p, params) = pitch_up(7.0);
    let mut r = Rig::new(vec![effect(id, p)], layout_for(&[(id, params)]), id);

    r.send(on(60));
    r.blocks(1);
    r.set_on(false);
    // In the very block the effect goes away.
    r.send(on(50));
    r.blocks(1);
    assert_eq!(
        r.sounding.held(),
        [50],
        "the old note is gone, the new one came straight through"
    );
}

/// A MIDI rack with one chain holding `device`, followed in the track by the instrument.
fn in_midi_rack(device: Box<DeviceSlot>) -> (DeviceId, Box<DeviceSlot>) {
    let mut chain = Box::new(ChainNode::new(ChainId::new(), BLOCK));
    chain.devices.push(device);
    let mut rack = Box::new(RackNode::new(RackKind::Midi));
    rack.chains.push(chain);
    let rack_id = DeviceId::new();
    (rack_id, Box::new(DeviceSlot::rack(rack_id, rack)))
}

#[test]
fn bypassing_an_effect_nested_in_a_rack_releases_its_notes() {
    let (id, p, params) = pitch_up(7.0);
    let (rack_id, rack) = in_midi_rack(effect(id, p));
    let layout = layout_for(&[(rack_id, Vec::new()), (id, params)]);
    let mut r = Rig::new(vec![rack], layout, id);

    r.send(on(60));
    r.blocks(2);
    assert_eq!(r.sounding.held(), [67]);
    r.set_on(false);
    r.blocks(1);
    assert_eq!(r.sounding.total(), 0, "the rack passed the note-off on");

    r.set_on(true);
    r.blocks(1);
    r.send(on(60));
    r.blocks(1);
    assert_eq!(r.sounding.held(), [67], "works once it is back");
}

#[test]
fn bypassing_a_midi_rack_releases_what_its_devices_had_sounding() {
    let id = DeviceId::new();
    let mut c = chord::Chord::with_seed(1);
    c.set_param(chord::FIRST_SHIFT, 7.0);
    let (rack_id, rack) = in_midi_rack(effect(id, Box::new(c)));
    let layout = layout_for(&[(rack_id, Vec::new()), (id, chord::params())]);
    let mut r = Rig::new(vec![rack], layout, rack_id);

    r.send(on(60));
    r.blocks(2);
    assert_eq!(r.sounding.held(), [60, 67]);
    r.set_on(false);
    r.blocks(1);
    assert_eq!(r.sounding.total(), 0, "the whole rack went quiet");

    r.set_on(true);
    r.blocks(1);
    r.send(on(62));
    r.blocks(1);
    assert_eq!(r.sounding.held(), [62, 69]);
}
