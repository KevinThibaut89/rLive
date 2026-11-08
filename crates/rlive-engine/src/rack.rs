//! Racks: devices that hold parallel chains of devices, nested to any depth.
//!
//! A rack is a node of the device tree, not a [`Processor`](rlive_core::Processor), so a device
//! inside one is addressed and timed exactly like a device on a track. Its parameter cells live
//! in the owning track's flat bus. For now the chains only sum: zones, macros, choke and return
//! chains come with the rack runtime.

use crate::track::{run_chain, ChainRun, DeviceSlot, Stretch, MAX_DEVICES, MAX_EVENTS};
use rlive_core::{ChainId, EventQueue, MidiEvent};

/// How many chains a rack can hold.
pub const MAX_CHAINS: usize = 128;

/// Which of Live's rack types this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RackKind {
    Instrument,
    Audio,
    Midi,
    Drum,
}

/// Which notes and Chain Selector values reach a chain. Inactive until the rack runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Zones;

/// One chain of a rack.
pub struct ChainNode {
    pub id: ChainId,
    pub devices: Vec<Box<DeviceSlot>>,
    pub zones: Zones,
    pub(crate) left: Vec<f32>,
    pub(crate) right: Vec<f32>,
    /// The rack's MIDI as this chain hears it.
    pub(crate) events: EventQueue,
    /// Holds what MIDI effects in this chain pass along.
    pub(crate) midi: [EventQueue; 2],
}

impl ChainNode {
    /// Create a chain. Call on the UI thread: this allocates all buffers up front.
    pub fn new(id: ChainId, max_block: usize) -> Self {
        Self {
            id,
            devices: Vec::with_capacity(MAX_DEVICES),
            zones: Zones,
            left: vec![0.0; max_block],
            right: vec![0.0; max_block],
            events: EventQueue::with_capacity(MAX_EVENTS),
            midi: [
                EventQueue::with_capacity(MAX_EVENTS),
                EventQueue::with_capacity(MAX_EVENTS),
            ],
        }
    }
}

/// The runtime of a rack device.
pub struct RackNode {
    pub kind: RackKind,
    pub chains: Vec<Box<ChainNode>>,
    pub returns: Vec<Box<ChainNode>>,
}

impl RackNode {
    /// Create a rack with no chains yet. Call on the UI thread.
    pub fn new(kind: RackKind) -> Self {
        Self {
            kind,
            chains: Vec::with_capacity(MAX_CHAINS),
            returns: Vec::with_capacity(MAX_CHAINS),
        }
    }

    /// Run every chain over a copy of the input and sum them. A rack with no chains is
    /// transparent. A MIDI rack passes on what its chains pass on.
    pub(crate) fn run(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
        events: &[MidiEvent],
        events_out: &mut EventQueue,
        at: &mut Stretch<'_>,
    ) {
        let forward_midi = self.kind == RackKind::Midi;
        if self.chains.is_empty() {
            if forward_midi {
                events_out.extend(events);
            }
            return;
        }
        let frames = left.len();
        for chain in self.chains.iter_mut() {
            let chain = &mut **chain;
            chain.left[..frames].copy_from_slice(left);
            chain.right[..frames].copy_from_slice(right);
            chain.events.clear();
            chain.events.extend(events);
            let held = run_chain(
                &mut chain.devices,
                ChainRun {
                    left: &mut chain.left[..frames],
                    right: &mut chain.right[..frames],
                    events: chain.events.as_slice(),
                    midi: &mut chain.midi,
                    at: &mut *at,
                },
            );
            if forward_midi {
                events_out
                    .extend(held.map_or(chain.events.as_slice(), |q| chain.midi[q].as_slice()));
            }
        }
        left.fill(0.0);
        right.fill(0.0);
        for chain in self.chains.iter() {
            for (out, s) in left.iter_mut().zip(&chain.left[..frames]) {
                *out += s;
            }
            for (out, s) in right.iter_mut().zip(&chain.right[..frames]) {
                *out += s;
            }
        }
    }
}
