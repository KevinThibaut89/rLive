//! Adding, removing and reordering tracks, devices and chains.
//!
//! Every structural edit travels in a [`GraphTxn`], applied whole between two blocks. A block can
//! never see a track without its plan, or a device without its parameter cells.

use crate::delay::{offer_line, StereoLine, TapLines};
use crate::graph::{ChainAddr, RenderPlan, TrackRef, MAX_RACK_DEPTH};
use crate::params::{ParamBus, TrackRoutes};
use crate::rack::{ChainNode, Zones};
use crate::track::bind_chain;
use crate::track::RackOuts;
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
    /// Add a return chain to the rack `device` found in the chain at `target`.
    InsertReturn {
        target: ChainAddr,
        device: DeviceId,
        index: usize,
        chain: Box<ChainNode>,
    },
    /// Remove a chain or a return chain.
    RemoveChain {
        target: ChainAddr,
        device: DeviceId,
        chain: ChainId,
    },
    /// Move a chain of the rack `from_device` to the rack `to_device`, on the same track. Grouping
    /// chains and moving one between racks are both this.
    TransferChain {
        from: ChainAddr,
        from_device: DeviceId,
        chain: ChainId,
        to: ChainAddr,
        to_device: DeviceId,
        index: usize,
    },
    /// Change which notes and Chain Selector values reach a chain, its Drum Rack note mapping
    /// included.
    SetChainZones {
        target: ChainAddr,
        device: DeviceId,
        chain: ChainId,
        zones: Zones,
    },
    /// Whether the Chain Selector filters MIDI other than notes as well.
    SetRackOptions {
        target: ChainAddr,
        device: DeviceId,
        select_filters_ctrl: bool,
    },
    /// Route a return chain to the Set's return track, or back to the rack's own output.
    SetReturnOutput {
        target: ChainAddr,
        device: DeviceId,
        chain: ChainId,
        output: Option<TrackId>,
    },
    /// Hold a rack's summed chains back, to line up with its slowest return chain.
    SetRackDelay {
        target: ChainAddr,
        device: DeviceId,
        delay: u32,
        line: Option<Box<StereoLine>>,
    },
    /// Give a track room to feed return tracks from its racks. A track that has room keeps it.
    SetRackOuts {
        track: TrackId,
        outs: Box<RackOuts>,
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
    /// Give a track delay lines for its taps. Longer ones replace what's there and take over its
    /// past; shorter ones come straight back.
    SetTapLines {
        track: TrackId,
        lines: TapLines,
    },
    /// Hold a chain of the rack `device` back, to line up with its slowest chain. The chain keeps
    /// the line it has if `line` is `None`.
    SetChainDelay {
        target: ChainAddr,
        device: DeviceId,
        chain: ChainId,
        delay: u32,
        line: Option<Box<StereoLine>>,
    },
    /// How late a device is while bypassed, to keep the timing it would have had switched on.
    SetBypassDelay {
        target: ChainAddr,
        device: DeviceId,
        delay: u32,
        line: Option<Box<StereoLine>>,
    },
    /// Give the metronome a line to wait for the tracks in.
    SetClickLine(Box<StereoLine>),
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

/// A delay no longer than `line` can provide.
fn fit_delay(delay: u32, line: Option<&StereoLine>) -> u32 {
    delay.min(line.map_or(0, StereoLine::max_delay))
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
        let fits = |p: &RenderPlan| p.fits(self.tracks.len()) && self.delays_fit(p);
        if txn.plan.as_ref().is_some_and(fits) {
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
            for input in &step.inputs {
                if let Some(t) = self.tracks.get_mut(input.from as usize) {
                    t.want_input(input);
                }
            }
        }
        for t in self.tracks.iter_mut() {
            t.settle_edges();
        }
        self.click.delay.set_target(plan.click_delay);
        self.solo_dirty = true;
    }

    /// Whether every delay a plan asks for has a line long enough to read it from.
    fn delays_fit(&self, plan: &RenderPlan) -> bool {
        let click_fits = plan.click_delay == 0
            || self
                .click
                .line
                .as_ref()
                .is_some_and(|l| l.max_delay() >= plan.click_delay);
        click_fits
            && plan.steps.iter().all(|step| {
                step.inputs.iter().all(|input| {
                    self.tracks
                        .get(input.from as usize)
                        .is_some_and(|t| t.can_delay(input))
                })
            })
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
                    chain.cells = bus.chain_cells(chain.id).unwrap_or(0..0);
                }
                match self.rack_mut(target, device) {
                    Some(rack) if fits && rack.chains.len() < rack.chains.capacity() => {
                        let index = index.min(rack.chains.len());
                        rack.chains.insert(index, chain);
                        rack.invalidate_gates();
                    }
                    _ => retire(&mut self.garbage, Garbage::Boxed(chain)),
                }
            }
            C::InsertReturn {
                target,
                device,
                index,
                mut chain,
            } => {
                let nesting = chain.devices.iter().map(|d| d.nesting()).max().unwrap_or(0);
                let fits = target.path.len() + 1 + nesting <= MAX_RACK_DEPTH;
                if let Some(bus) = self.bus_mut(target.owner) {
                    bind_chain(&mut chain.devices, bus);
                    chain.cells = bus.chain_cells(chain.id).unwrap_or(0..0);
                }
                match self.rack_mut(target, device) {
                    Some(rack) if fits && rack.returns.len() < rack.returns.capacity() => {
                        let index = index.min(rack.returns.len());
                        rack.returns.insert(index, chain);
                        rack.invalidate_gates();
                    }
                    _ => retire(&mut self.garbage, Garbage::Boxed(chain)),
                }
            }
            C::TransferChain {
                from,
                from_device,
                chain,
                to,
                to_device,
                index,
            } => self.transfer_chain(from, from_device, chain, to, to_device, index),
            C::SetChainZones {
                target,
                device,
                chain,
                zones,
            } => {
                if let Some(rack) = self.rack_mut(target, device) {
                    if let Some(c) = rack.chains.iter_mut().find(|c| c.id == chain) {
                        c.zones = zones;
                    }
                    rack.invalidate_gates();
                }
            }
            C::SetRackOptions {
                target,
                device,
                select_filters_ctrl,
            } => {
                if let Some(rack) = self.rack_mut(target, device) {
                    rack.select_filters_ctrl = select_filters_ctrl;
                }
            }
            C::SetReturnOutput {
                target,
                device,
                chain,
                output,
            } => {
                if let Some(rack) = self.rack_mut(target, device) {
                    if let Some(c) = rack.returns.iter_mut().find(|c| c.id == chain) {
                        c.set_output(output);
                    }
                }
            }
            C::SetRackDelay {
                target,
                device,
                delay,
                line,
            } => {
                let spare = match self.rack_mut(target, device) {
                    Some(rack) => {
                        let (slot, dry_delay) = rack.dry_line_mut();
                        let spare = offer_line(slot, line);
                        dry_delay.set_target(fit_delay(delay, slot.as_deref()));
                        spare
                    }
                    None => line,
                };
                self.retire_line(spare);
            }
            C::SetRackOuts { track, outs } => {
                let mut outs = Some(outs);
                if let Some(t) = self.track_mut(track) {
                    let same = t.rack_outs.as_ref().is_some_and(|o| {
                        o.tracks() == outs.as_ref().map_or(&[][..], |n| n.tracks())
                    });
                    if !same {
                        std::mem::swap(&mut t.rack_outs, &mut outs);
                    }
                }
                if let Some(old) = outs {
                    retire(&mut self.garbage, Garbage::Boxed(old));
                }
            }
            C::RemoveChain {
                target,
                device,
                chain,
            } => {
                let removed = self.rack_mut(target, device).and_then(|rack| {
                    let removed = match rack.chains.iter().position(|c| c.id == chain) {
                        Some(i) => Some(rack.chains.remove(i)),
                        None => rack
                            .returns
                            .iter()
                            .position(|c| c.id == chain)
                            .map(|i| rack.returns.remove(i)),
                    };
                    rack.invalidate_gates();
                    removed
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
            C::SetTapLines { track, lines } => {
                let spare = match self.track_mut(track) {
                    Some(t) => t.offer_lines(lines),
                    None => lines,
                };
                self.retire_lines(spare);
            }
            C::SetChainDelay {
                target,
                device,
                chain,
                delay,
                line,
            } => {
                let found = self.rack_mut(target, device).and_then(|rack| {
                    rack.chains
                        .iter_mut()
                        .chain(rack.returns.iter_mut())
                        .find(|c| c.id == chain)
                });
                let spare = match found {
                    Some(c) => {
                        let spare = offer_line(&mut c.line, line);
                        c.delay.set_target(fit_delay(delay, c.line.as_deref()));
                        spare
                    }
                    None => line,
                };
                self.retire_line(spare);
            }
            C::SetBypassDelay {
                target,
                device,
                delay,
                line,
            } => {
                let found = self
                    .chain_mut(target)
                    .and_then(|chain| chain.iter_mut().find(|d| d.id == device));
                let spare = match found {
                    Some(d) => {
                        let spare = offer_line(&mut d.bypass_line, line);
                        d.bypass_delay
                            .set_target(fit_delay(delay, d.bypass_line.as_deref()));
                        spare
                    }
                    None => line,
                };
                self.retire_line(spare);
            }
            C::SetClickLine(line) => {
                let spare = offer_line(&mut self.click.line, Some(line));
                self.retire_line(spare);
            }
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

    fn retire_line(&mut self, line: Option<Box<StereoLine>>) {
        if let Some(line) = line {
            retire(&mut self.garbage, Garbage::Boxed(line));
        }
    }

    fn retire_lines(&mut self, lines: TapLines) {
        self.retire_line(lines.main);
        self.retire_line(lines.pre);
        self.retire_line(lines.post);
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

    /// Move a chain between racks on one track, or leave it where it is if it can't go.
    #[allow(clippy::too_many_arguments)]
    fn transfer_chain(
        &mut self,
        from: ChainAddr,
        from_device: DeviceId,
        chain: ChainId,
        to: ChainAddr,
        to_device: DeviceId,
        index: usize,
    ) {
        if from.owner != to.owner || to.path.steps().iter().any(|s| s.chain == chain) {
            return;
        }
        let Some((position, nesting)) = self.rack_mut(from, from_device).and_then(|rack| {
            let i = rack.chains.iter().position(|c| c.id == chain)?;
            let nesting = rack.chains[i].devices.iter().map(|d| d.nesting()).max();
            Some((i, nesting.unwrap_or(0)))
        }) else {
            return;
        };
        let room = self
            .rack_mut(to, to_device)
            .is_some_and(|rack| rack.chains.len() < rack.chains.capacity());
        if !room || to.path.len() + 1 + nesting > MAX_RACK_DEPTH {
            return;
        }
        let moved = self.rack_mut(from, from_device).map(|rack| {
            let moved = rack.chains.remove(position);
            rack.invalidate_gates();
            moved
        });
        if let (Some(moved), Some(rack)) = (moved, self.rack_mut(to, to_device)) {
            let index = index.min(rack.chains.len());
            rack.chains.insert(index, moved);
            rack.invalidate_gates();
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
