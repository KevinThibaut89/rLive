//! Adding, removing and reordering tracks, devices and chains.
//!
//! Every structural edit travels in a [`GraphTxn`], applied whole between two blocks. A block can
//! never see a track without its plan, or a device without its parameter cells.

use crate::graph::{ChainAddr, RenderPlan, TrackRef, MAX_RACK_DEPTH};
use crate::params::{ParamBus, TrackRoutes};
use crate::rack::ChainNode;
use crate::track::bind_chain;
use crate::{retire, DeviceSlot, Engine, Garbage, TrackNode};
use rlive_core::{ChainId, DeviceId, Resource, TrackId};
use std::sync::Arc;

pub enum GraphOp {
    AddTrack {
        index: usize,
        track: Box<TrackNode>,
    },
    RemoveTrack(TrackId),
    MoveTrack {
        track: TrackId,
        to: usize,
    },
    InsertDevice {
        target: ChainAddr,
        index: usize,
        device: Box<DeviceSlot>,
    },
    RemoveDevice {
        target: ChainAddr,
        device: DeviceId,
    },
    MoveDevice {
        target: ChainAddr,
        device: DeviceId,
        to: usize,
    },
    /// Move a device, with everything in it, to another chain of the same track. Nesting a device
    /// into a rack and ungrouping one are both this.
    TransferDevice {
        from: ChainAddr,
        device: DeviceId,
        to: ChainAddr,
        index: usize,
    },
    /// Add a chain to the rack `device` found in the chain at `target`.
    InsertChain {
        target: ChainAddr,
        device: DeviceId,
        index: usize,
        chain: Box<ChainNode>,
    },
    RemoveChain {
        target: ChainAddr,
        device: DeviceId,
        chain: ChainId,
    },
    /// Give a chain owner a new parameter bus, after its layout changed. Cells that kept their
    /// owner keep their live state.
    SwapBus {
        target: TrackRef,
        bus: Box<ParamBus>,
    },
    /// Replace what feeds a track's cells, for when an edit moved the cells its lanes address.
    SetRoutes {
        target: TrackRef,
        routes: Arc<TrackRoutes>,
    },
    /// Hand a device a shared resource, such as a sample. Playing voices carry on. What it
    /// replaces is retired, never freed here.
    SetResource {
        target: ChainAddr,
        device: DeviceId,
        slot: u32,
        resource: Resource,
    },
    /// Replace the whole session (project load / sample-rate change).
    ReplaceAll {
        tracks: Box<Vec<Box<TrackNode>>>,
        master: Box<TrackNode>,
    },
}

/// A set of structural edits that take effect together, and the plan they end up with.
///
/// Built on the UI thread and retired through [`Garbage`], so the audio thread neither allocates
/// nor frees to apply it. Whatever an edit puts in the graph must have its capacity reserved
/// already: a full chain refuses a device rather than grow.
pub struct GraphTxn {
    ops: Vec<GraphOp>,
    /// The plan for after these edits. `None` keeps the current one, unless the list of tracks
    /// changes, when the engine goes back to summing every track to the Master.
    plan: Option<RenderPlan>,
    /// Holds a replaced plan when `plan` is still taken up by one the engine refused.
    replaced: Option<RenderPlan>,
}

impl GraphTxn {
    /// Call on the UI thread.
    pub fn new(ops: Vec<GraphOp>, plan: Option<RenderPlan>) -> Box<Self> {
        Box::new(Self {
            ops,
            plan,
            replaced: None,
        })
    }
}

/// What an edit changed, beyond the node it touched.
#[derive(Default)]
struct Applied {
    /// Tracks came, went or changed order, so plan indices no longer mean what they did.
    tracks: bool,
}

impl Engine {
    pub(crate) fn handle_graph(&mut self, mut txn: Box<GraphTxn>) {
        let mut applied = Applied::default();
        for op in txn.ops.drain(..) {
            self.apply_graph_op(op, &mut applied);
        }
        self.install_plan(&mut txn, &applied);
        // Drained, it still owns its buffer, and now the plan this one replaced.
        retire(&mut self.garbage, Garbage::Spent(txn));
    }

    /// Runs after all the edits, so the plan is checked against the tracks it will run over.
    fn install_plan(&mut self, txn: &mut GraphTxn, applied: &Applied) {
        if txn.plan.as_ref().is_some_and(|p| p.fits(self.tracks.len())) {
            std::mem::swap(&mut self.plans.compiled, &mut txn.plan);
        } else if applied.tracks || txn.plan.is_some() {
            txn.replaced = self.plans.compiled.take();
            self.plans.flat.make_flat(self.tracks.len());
        } else {
            return;
        }
        self.configure_tracks();
    }

    /// Tell every track what the running plan wants of it, and have solo worked out again.
    fn configure_tracks(&mut self) {
        let plan = self.plans.active();
        for t in self.tracks.iter_mut() {
            t.plan_changed();
        }
        for step in &plan.steps {
            for input in step.inputs.iter().filter(|i| i.gain.is_some()) {
                if let Some(t) = self.tracks.get_mut(input.from as usize) {
                    t.want_send(&input.tap);
                }
            }
        }
        self.solo_dirty = true;
    }

    fn apply_graph_op(&mut self, op: GraphOp, applied: &mut Applied) {
        use GraphOp as C;
        match op {
            C::AddTrack { index, mut track } => {
                if self.tracks.len() < self.tracks.capacity() {
                    bind_chain(&mut track.devices, &track.bus);
                    track.stretch.prepare(self.sample_rate);
                    let idx = index.min(self.tracks.len());
                    self.tracks.insert(idx, track);
                    applied.tracks = true;
                } else {
                    retire(&mut self.garbage, Garbage::Boxed(track));
                }
            }
            C::RemoveTrack(id) => {
                if let Some(i) = self.tracks.iter().position(|t| t.id == id) {
                    let mut t = self.tracks.remove(i);
                    for d in t.devices.iter_mut() {
                        d.stop_processing();
                    }
                    if let Some(p) = t.player.playing.take() {
                        retire(&mut self.garbage, Garbage::Shared(p.clip));
                    }
                    retire(&mut self.garbage, Garbage::Boxed(t));
                    applied.tracks = true;
                }
            }
            C::MoveTrack { track, to } => {
                if let Some(i) = self.tracks.iter().position(|t| t.id == track) {
                    let t = self.tracks.remove(i);
                    let to = to.min(self.tracks.len());
                    self.tracks.insert(to, t);
                    applied.tracks = true;
                }
            }
            C::InsertDevice {
                target,
                index,
                mut device,
            } => {
                let fits = target.path.len() + device.nesting() <= MAX_RACK_DEPTH;
                if let Some(bus) = self.bus_mut(target.owner) {
                    device.bind_cells(bus);
                }
                match self.chain_mut(target) {
                    Some(chain) if fits && chain.len() < chain.capacity() => {
                        let idx = index.min(chain.len());
                        chain.insert(idx, device);
                    }
                    _ => retire(&mut self.garbage, Garbage::Boxed(device)),
                }
            }
            C::RemoveDevice { target, device } => {
                let removed = self.chain_mut(target).and_then(|chain| {
                    let i = chain.iter().position(|d| d.id == device)?;
                    Some(chain.remove(i))
                });
                if let Some(mut slot) = removed {
                    slot.stop_processing();
                    retire(&mut self.garbage, Garbage::Boxed(slot));
                }
            }
            C::MoveDevice { target, device, to } => {
                if let Some(chain) = self.chain_mut(target) {
                    if let Some(i) = chain.iter().position(|d| d.id == device) {
                        let d = chain.remove(i);
                        let to = to.min(chain.len());
                        chain.insert(to, d);
                    }
                }
            }
            C::TransferDevice {
                from,
                device,
                to,
                index,
            } => self.transfer_device(from, device, to, index),
            C::InsertChain {
                target,
                device,
                index,
                mut chain,
            } => {
                let nesting = chain.devices.iter().map(|d| d.nesting()).max().unwrap_or(0);
                let fits = target.path.len() + 1 + nesting <= MAX_RACK_DEPTH;
                if let Some(bus) = self.bus_mut(target.owner) {
                    bind_chain(&mut chain.devices, bus);
                }
                match self.rack_mut(target, device) {
                    Some(rack) if fits && rack.chains.len() < rack.chains.capacity() => {
                        let index = index.min(rack.chains.len());
                        rack.chains.insert(index, chain);
                    }
                    _ => retire(&mut self.garbage, Garbage::Boxed(chain)),
                }
            }
            C::RemoveChain {
                target,
                device,
                chain,
            } => {
                let removed = self.rack_mut(target, device).and_then(|rack| {
                    let i = rack.chains.iter().position(|c| c.id == chain)?;
                    Some(rack.chains.remove(i))
                });
                if let Some(mut chain) = removed {
                    for d in chain.devices.iter_mut() {
                        d.stop_processing();
                    }
                    retire(&mut self.garbage, Garbage::Boxed(chain));
                }
            }
            C::SwapBus { target, mut bus } => match self.bus_mut(target) {
                Some(current) => {
                    bus.adopt(current);
                    std::mem::swap(current, &mut *bus);
                    retire(&mut self.garbage, Garbage::Boxed(bus));
                    self.rebind(target);
                }
                None => retire(&mut self.garbage, Garbage::Boxed(bus)),
            },
            C::SetRoutes { target, routes } => self.set_routes(target, routes),
            C::SetResource {
                target,
                device,
                slot,
                resource,
            } => {
                let found = self
                    .chain_mut(target)
                    .and_then(|chain| chain.iter_mut().find(|d| d.id == device));
                let retired = match found {
                    Some(d) => d.set_resource(slot, resource),
                    None => Some(resource),
                };
                if let Some(r) = retired {
                    retire(&mut self.garbage, Garbage::Resource(r));
                }
            }
            C::ReplaceAll {
                mut tracks,
                mut master,
            } => {
                for t in self.tracks.iter_mut() {
                    for d in t.devices.iter_mut() {
                        d.stop_processing();
                    }
                    if let Some(p) = t.player.playing.take() {
                        retire(&mut self.garbage, Garbage::Shared(p.clip));
                    }
                }
                for d in self.master.devices.iter_mut() {
                    d.stop_processing();
                }
                for t in tracks.iter_mut() {
                    bind_chain(&mut t.devices, &t.bus);
                    t.stretch.prepare(self.sample_rate);
                }
                bind_chain(&mut master.devices, &master.bus);
                std::mem::swap(&mut self.tracks, &mut *tracks);
                std::mem::swap(&mut self.master, &mut master);
                retire(&mut self.garbage, Garbage::Boxed(tracks));
                retire(&mut self.garbage, Garbage::Boxed(master));
                self.playing = false;
                self.tick = 0.0;
                self.sample_pos = 0;
                self.insert_marker = 0.0;
                self.loop_range = None;
                self.reached_loop_end = false;
                self.recording = false;
                applied.tracks = true;
            }
        }
    }

    /// Move a device between chains of one track, or leave it where it is if it can't go.
    fn transfer_device(&mut self, from: ChainAddr, device: DeviceId, to: ChainAddr, index: usize) {
        // Cells belong to a track's bus, so a device can't change track this way.
        if from.owner != to.owner || to.path.steps().iter().any(|s| s.rack == device) {
            return;
        }
        let Some((position, nesting)) = self.chain_mut(from).and_then(|chain| {
            let i = chain.iter().position(|d| d.id == device)?;
            Some((i, chain[i].nesting()))
        }) else {
            return;
        };
        let room = self
            .chain_mut(to)
            .is_some_and(|chain| from == to || chain.len() < chain.capacity());
        if !room || to.path.len() + nesting > MAX_RACK_DEPTH {
            return;
        }
        let moved = self.chain_mut(from).map(|chain| chain.remove(position));
        if let (Some(moved), Some(chain)) = (moved, self.chain_mut(to)) {
            let index = index.min(chain.len());
            chain.insert(index, moved);
        }
    }

    /// Find the cells of everything under an owner again, after its bus changed.
    fn rebind(&mut self, owner: TrackRef) {
        match owner {
            TrackRef::Master => bind_chain(&mut self.master.devices, &self.master.bus),
            TrackRef::Track(id) => {
                if let Some(t) = self.track_mut(id) {
                    bind_chain(&mut t.devices, &t.bus);
                }
            }
        }
    }
}
