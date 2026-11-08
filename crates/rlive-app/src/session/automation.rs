//! Track automation: compiling envelopes for the engine, recording gestures into them, and the
//! state the control bar and knobs show.
//!
//! A pass of recording works like this. While Automation Arm is on and the Arrangement is
//! recording, a gesture on a parameter reaches the engine, which holds the parameter at the knob
//! and tells us where on the timeline it moved ([`EngineEvent::AutomationMoved`]). Moves collect
//! into a [`Stroke`], which ends when the gesture does: at mouse release for a touch, or at the
//! end of the pass or the loop for a latch. Strokes go into the project when the playhead jumps
//! or recording stops, overwriting what was there.
//!
//! [`EngineEvent::AutomationMoved`]: rlive_engine::EngineEvent::AutomationMoved

use super::devices::mixer_cell_of;
use super::{ChainRef, Session};
use rlive_core::param::{EnvTarget, ParamOwner, ParamRef, ACTIVATOR};
use rlive_core::{ChainId, DeviceId, ParamRange, TrackId};
use rlive_engine::{
    AutomationLane, BusLayout, CellIx, CellState, CellTarget, EngineCommand, GraphOp, MacroRoute,
    ParamCmd, RecordCmd, TrackRef, TrackRoutes,
};
use rlive_project::{Breakpoint, Device, DeviceKind, EnvMode, Envelope};
use std::collections::HashMap;
use std::mem::take;
use std::sync::Arc;

/// Compiles a track's envelopes and Macro mappings for the engine, addressed to the cells of its
/// layout. Envelopes for something without a cell (yet) are left out and kept in the project.
pub(super) fn routes_for(
    envelopes: &[Envelope],
    devices: &[Device],
    owner: ChainRef,
    layout: &BusLayout,
) -> Arc<TrackRoutes> {
    let lanes = envelopes
        .iter()
        .filter_map(|e| {
            let EnvTarget::Param(target) = e.target else {
                return None;
            };
            let cell = cell_of(target, owner, layout)?;
            let points = e.points.iter().map(|p| (p.time, p.value, p.curve));
            AutomationLane::new(cell, layout.spec(cell)?.range, points)
        })
        .collect();
    let mut macros = Vec::new();
    collect_macro_routes(devices, owner, layout, &mut macros);
    Arc::new(TrackRoutes::new(lanes).with_macros(macros))
}

/// Every Macro mapping under these devices, outer racks before the racks nested in them.
fn collect_macro_routes(
    devices: &[Device],
    owner: ChainRef,
    layout: &BusLayout,
    out: &mut Vec<MacroRoute>,
) {
    for dev in devices {
        if let DeviceKind::Rack { macros, .. } = &dev.kind {
            for (n, m) in macros.iter().enumerate() {
                let Some(source) = layout.device_cell(dev.id, n as u32) else {
                    continue;
                };
                for map in &m.mappings {
                    let Some(target) = cell_of(map.target, owner, layout) else {
                        continue;
                    };
                    if let Some(spec) = layout.spec(target) {
                        out.push(MacroRoute::new(
                            source, target, spec.range, map.min, map.max,
                        ));
                    }
                }
            }
        }
        for chain in dev.chains() {
            collect_macro_routes(&chain.devices, owner, layout, out);
        }
    }
}

/// The cell of a parameter under `owner`, in O(1). Devices are found through the owner's layout,
/// which knows every device at any depth, so this never has to look for a device's chain.
pub(super) fn cell_of(target: ParamRef, owner: ChainRef, layout: &BusLayout) -> Option<CellIx> {
    match target.owner {
        ParamOwner::Device(device) if target.id == ACTIVATOR => layout.activator_cell(device),
        ParamOwner::Device(device) => layout.device_cell(device, target.id.0),
        ParamOwner::Chain(chain) => layout.chain_cell(chain, target.id),
        ParamOwner::Track(t) if owner == ChainRef::Track(t) => mixer_cell_of(owner, target.id),
        ParamOwner::Master if owner == ChainRef::Master => mixer_cell_of(owner, target.id),
        _ => None,
    }
}

/// The parameter a cell holds, as an envelope addresses it.
fn param_of(owner: TrackRef, target: CellTarget) -> Option<ParamRef> {
    Some(match target {
        CellTarget::Free => return None,
        CellTarget::Mixer(id) => ParamRef {
            owner: match owner {
                TrackRef::Track(t) => ParamOwner::Track(t),
                TrackRef::Master => ParamOwner::Master,
            },
            id,
        },
        CellTarget::Device { device, param } => ParamRef {
            owner: ParamOwner::Device(device),
            id: param,
        },
        CellTarget::Activator { device } => ParamRef {
            owner: ParamOwner::Device(device),
            id: ACTIVATOR,
        },
        CellTarget::Chain { chain, control } => ParamRef {
            owner: ParamOwner::Chain(chain),
            id: control,
        },
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CellKey {
    track: TrackRef,
    cell: CellIx,
}

/// What a control was moved with, which decides how its gesture ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GestureSource {
    /// Ends at mouse release, with the envelope coming back.
    Mouse,
    /// Holds its value to the end of the pass or the loop.
    Hardware,
}

#[derive(Debug, Clone, Copy)]
struct Gesture {
    source: GestureSource,
    /// The mouse has been seen down while this gesture went on.
    dragged: bool,
    /// It holds its value until the end of the pass or the loop, for all that the mouse is up.
    latched: bool,
}

/// One gesture on one parameter, as it was recorded.
struct Stroke {
    key: CellKey,
    /// What the parameter was sounding at when the stroke started.
    before: f64,
    points: Vec<Breakpoint>,
}

impl Stroke {
    fn starting(key: CellKey, before: f64, tick: f64, value: f64) -> Self {
        Self {
            key,
            before,
            points: vec![Breakpoint::new(tick, value)],
        }
    }

    fn push(&mut self, tick: f64, value: f64) {
        match self.points.last_mut() {
            Some(last) if tick == last.time => last.value = value,
            Some(last) if tick < last.time => {}
            _ => self.points.push(Breakpoint::new(tick, value)),
        }
    }

    fn last_value(&self) -> f64 {
        self.points.last().map_or(self.before, |p| p.value)
    }

    /// The parameter has held its last value until `tick`.
    fn close(&mut self, tick: f64) {
        self.push(tick, self.last_value());
    }
}

/// Automation Arm and the pass being recorded.
#[derive(Default)]
pub(super) struct AutomationRecorder {
    arm: bool,
    /// Parameters the user is moving, or that are holding what they were moved to.
    gestures: HashMap<CellKey, Gesture>,
    open: HashMap<CellKey, Stroke>,
    /// Waiting to go into the project.
    done: Vec<Stroke>,
    /// Something was written to the project in this pass.
    wrote: bool,
}

impl AutomationRecorder {
    /// Forget the pass in progress, as its parameters may no longer exist.
    pub(super) fn abandon(&mut self) {
        self.gestures.clear();
        self.open.clear();
        self.done.clear();
        self.wrote = false;
    }
}

impl Session {
    // ------------------------------------------------------------- arming

    pub fn automation_arm(&self) -> bool {
        self.automation.arm
    }

    /// Automation Arm: while recording, parameter moves are recorded as automation.
    pub fn set_automation_arm(&mut self, on: bool) {
        self.automation.arm = on;
        self.send_automation_arm();
    }

    pub(super) fn send_automation_arm(&mut self) {
        let on = self.automation.arm;
        self.send(EngineCommand::Record(RecordCmd::SetAutomationArm(on)));
    }

    // ------------------------------------------------------------- overriding

    /// Whether the Re-Enable Automation button has something to do.
    pub fn any_overridden(&self) -> bool {
        self.layouts.values().any(|l| l.any_overridden())
    }

    /// Hand every overridden parameter back to its automation.
    pub fn re_enable_all(&mut self) {
        let owners: Vec<ChainRef> = self.layouts.keys().copied().collect();
        for owner in owners {
            self.send(EngineCommand::Param(ParamCmd::ReEnable {
                track: owner.into(),
                cell: None,
            }));
        }
    }

    /// Hand one parameter back to its automation.
    pub fn re_enable_cell(&mut self, chain: ChainRef, cell: CellIx) {
        self.send(EngineCommand::Param(ParamCmd::ReEnable {
            track: chain.into(),
            cell: Some(cell),
        }));
    }

    /// What a parameter's LED should show.
    pub fn cell_state(&self, chain: ChainRef, cell: CellIx) -> CellState {
        self.layouts
            .get(&chain.owner())
            .map_or(CellState::default(), |l| l.state(cell))
    }

    /// The plain value a mixer control should be drawn at: what automation has it at, unless the
    /// user has overridden that.
    pub fn automated_cell_value(&self, chain: ChainRef, cell: CellIx) -> Option<f64> {
        let layout = self.layouts.get(&chain.owner())?;
        let state = layout.state(cell);
        if !state.automated() || state.overridden() {
            return None;
        }
        let norm = layout.effective(cell)?;
        Some(layout.spec(cell)?.range.from_normalized(norm as f64))
    }

    /// Relaunching a clip with automation in it hands that automation back.
    pub(super) fn re_enable_clip_automation(&mut self, track: TrackId, slot: usize) {
        let owner = ChainRef::Track(track);
        let (Some(clip), Some(layout)) = (
            self.project
                .track(track)
                .and_then(|t| t.clips.get(slot))
                .and_then(Option::as_ref),
            self.layouts.get(&owner),
        ) else {
            return;
        };
        let cells: Vec<CellIx> = clip
            .envelopes
            .iter()
            .filter(|e| e.mode == EnvMode::Automation)
            .filter_map(|e| match e.envelope.target {
                EnvTarget::Param(target) => cell_of(target, owner, layout),
                _ => None,
            })
            .collect();
        for cell in cells {
            self.re_enable_cell(owner, cell);
        }
    }

    // ------------------------------------------------------------- compiling

    fn envelopes(&self, track: TrackRef) -> Option<&Vec<Envelope>> {
        match track {
            TrackRef::Master => Some(&self.project.master.automation),
            TrackRef::Track(id) => self.project.track(id).map(|t| &t.automation),
        }
    }

    fn envelopes_mut(&mut self, track: TrackRef) -> Option<&mut Vec<Envelope>> {
        match track {
            TrackRef::Master => Some(&mut self.project.master.automation),
            TrackRef::Track(id) => self.project.track_mut(id).map(|t| &mut t.automation),
        }
    }

    fn track_devices(&self, track: TrackRef) -> Option<&Vec<Device>> {
        match track {
            TrackRef::Master => Some(&self.project.master.devices),
            TrackRef::Track(id) => self.project.track(id).map(|t| &t.devices),
        }
    }

    fn compiled_routes(&self, track: TrackRef) -> Option<Arc<TrackRoutes>> {
        let owner = owner_of(track);
        Some(routes_for(
            self.envelopes(track)?,
            self.track_devices(track)?,
            owner,
            self.layouts.get(&owner)?,
        ))
    }

    /// Replace a parameter's envelope. No points removes it. This is what editing envelopes goes
    /// through, as one undoable edit.
    pub fn set_envelope(&mut self, chain: ChainRef, target: EnvTarget, points: Vec<Breakpoint>) {
        let track: TrackRef = chain.into();
        let Some(envelopes) = self.envelopes_mut(track) else {
            return;
        };
        match envelopes.iter().position(|e| e.target == target) {
            Some(at) if points.is_empty() => {
                envelopes.remove(at);
            }
            Some(at) => envelopes[at].points = points,
            None if points.is_empty() => {}
            None => envelopes.push(Envelope { target, points }),
        }
        self.send_routes(track);
        self.mark_edit("Edit Automation");
    }

    /// Give the engine the routes of a track as the project has them now.
    pub(super) fn send_routes(&mut self, track: TrackRef) {
        if let Some(routes) = self.compiled_routes(track) {
            self.send(EngineCommand::Param(ParamCmd::SetRoutes { track, routes }));
        }
    }

    /// The edits that keep routes matching these edits. A new bus can move cells, and lanes
    /// address cells, so routes have to change in the same breath.
    pub(super) fn routes_for_bus_swaps(&self, ops: &[GraphOp]) -> Vec<GraphOp> {
        let mut swapped: Vec<TrackRef> = Vec::new();
        for op in ops {
            if let GraphOp::SwapBus { target, .. } = op {
                if !swapped.contains(target) {
                    swapped.push(*target);
                }
            }
        }
        swapped
            .into_iter()
            .filter_map(|target| {
                let routes = self.compiled_routes(target)?;
                (!routes.is_empty()).then_some(GraphOp::SetRoutes { target, routes })
            })
            .collect()
    }

    /// Envelopes of devices leaving the project leave with them.
    pub(super) fn forget_device_automation(&mut self, chain: ChainRef, ids: &[DeviceId]) {
        let track: TrackRef = chain.into();
        let Some(envelopes) = self.envelopes_mut(track) else {
            return;
        };
        let before = envelopes.len();
        envelopes.retain(|e| {
            !matches!(e.target, EnvTarget::Param(ParamRef { owner: ParamOwner::Device(d), .. }) if ids.contains(&d))
        });
        if envelopes.len() != before {
            self.send_routes(track);
        }
    }

    /// Envelopes of rack chains leaving the project leave with them.
    pub(super) fn forget_chain_automation(&mut self, chain: ChainRef, ids: &[ChainId]) {
        let track: TrackRef = chain.into();
        let Some(envelopes) = self.envelopes_mut(track) else {
            return;
        };
        let before = envelopes.len();
        envelopes.retain(|e| {
            !matches!(e.target, EnvTarget::Param(ParamRef { owner: ParamOwner::Chain(c), .. }) if ids.contains(&c))
        });
        if envelopes.len() != before {
            self.send_routes(track);
        }
    }

    // ------------------------------------------------------------- gestures

    /// A control is being moved. Call before the value goes to the engine.
    pub(super) fn note_gesture(&mut self, chain: ChainRef, cell: CellIx) {
        if self.automation.arm && self.is_recording() {
            let key = CellKey {
                track: chain.into(),
                cell,
            };
            self.automation.gestures.entry(key).or_insert(Gesture {
                source: GestureSource::Mouse,
                dragged: false,
                latched: false,
            });
        }
    }

    /// The next moves of a control come from a knob or fader on a MIDI controller, so they latch.
    pub fn note_hardware_gesture(&mut self, chain: ChainRef, cell: CellIx) {
        if self.automation.arm && self.is_recording() {
            let key = CellKey {
                track: chain.into(),
                cell,
            };
            self.automation.gestures.insert(
                key,
                Gesture {
                    source: GestureSource::Hardware,
                    dragged: false,
                    latched: true,
                },
            );
        }
    }

    /// Tell recording whether the mouse is down this frame, so a mouse gesture can end with it.
    pub fn pointer_frame(&mut self, down: bool) {
        let mut released = Vec::new();
        for (key, gesture) in self.automation.gestures.iter_mut() {
            if gesture.source != GestureSource::Mouse || gesture.latched {
                continue;
            }
            if down {
                gesture.dragged = true;
            } else {
                released.push((*key, gesture.dragged));
            }
        }
        for (key, dragged) in released {
            if dragged && self.has_envelope(key) {
                self.automation.gestures.remove(&key);
                self.send(EngineCommand::Param(ParamCmd::EndGesture {
                    track: key.track,
                    cell: key.cell,
                }));
            } else if let Some(gesture) = self.automation.gestures.get_mut(&key) {
                // A click has nothing to hold, and a first pass has nothing to go back to.
                gesture.latched = true;
            }
        }
    }

    /// The envelope a parameter would be recorded into, and how its values convert.
    fn lane_of(&self, key: CellKey) -> Option<(EnvTarget, ParamRange)> {
        let spec = self.layouts.get(&owner_of(key.track))?.spec(key.cell)?;
        let target = param_of(key.track, spec.target)?;
        Some((EnvTarget::Param(target), spec.range))
    }

    fn has_envelope(&self, key: CellKey) -> bool {
        let Some((target, _)) = self.lane_of(key) else {
            return false;
        };
        self.envelopes(key.track).is_some_and(|envs| {
            envs.iter()
                .any(|e| e.target == target && !e.points.is_empty())
        })
    }

    pub(super) fn automation_moved(
        &mut self,
        track: TrackRef,
        cell: CellIx,
        value: f64,
        before: f64,
        tick: f64,
    ) {
        let key = CellKey { track, cell };
        self.automation
            .open
            .entry(key)
            .and_modify(|s| s.push(tick, value))
            .or_insert_with(|| Stroke::starting(key, before, tick, value));
    }

    pub(super) fn automation_released(&mut self, track: TrackRef, cell: CellIx, tick: f64) {
        let key = CellKey { track, cell };
        if let Some(mut stroke) = self.automation.open.remove(&key) {
            stroke.close(tick);
            self.automation.done.push(stroke);
        }
    }

    /// The playhead jumped: each stroke ends where it left. A latch is over; a mouse still down
    /// goes on recording where the playhead landed.
    pub(super) fn automation_jumped(&mut self, from: f64, to: f64) {
        let open: Vec<Stroke> = take(&mut self.automation.open).into_values().collect();
        for mut stroke in open {
            stroke.close(from);
            let key = stroke.key;
            let held = self
                .automation
                .gestures
                .get(&key)
                .is_some_and(|g| g.source == GestureSource::Mouse && !g.latched);
            if held {
                let value = stroke.last_value();
                let resumed = Stroke::starting(key, value, to, value);
                self.automation.open.insert(key, resumed);
            } else {
                self.automation.gestures.remove(&key);
                self.send(EngineCommand::Param(ParamCmd::EndGesture {
                    track: key.track,
                    cell: key.cell,
                }));
            }
            self.automation.done.push(stroke);
        }
        self.write_recorded_automation();
    }

    /// Recording stopped at `tick`. The whole pass becomes one undo step.
    pub(super) fn finish_automation_pass(&mut self, tick: f64) {
        for (_, mut stroke) in take(&mut self.automation.open) {
            stroke.close(tick);
            self.automation.done.push(stroke);
        }
        self.automation.gestures.clear();
        self.write_recorded_automation();
        if take(&mut self.automation.wrote) {
            self.dirty = true;
            self.history.pending = Some("Record Automation");
            self.history.settling = 0;
        }
    }

    /// Move finished strokes into the project, and let the engine hear them.
    fn write_recorded_automation(&mut self) {
        let mut changed: Vec<TrackRef> = Vec::new();
        for stroke in take(&mut self.automation.done) {
            let Some((target, range)) = self.lane_of(stroke.key) else {
                continue;
            };
            let Some(envelopes) = self.envelopes_mut(stroke.key.track) else {
                continue;
            };
            let at = match envelopes.iter().position(|e| e.target == target) {
                Some(at) => at,
                None => {
                    envelopes.push(Envelope {
                        target,
                        points: Vec::new(),
                    });
                    envelopes.len() - 1
                }
            };
            envelopes[at].overwrite(&stroke.points, stroke.before, range);
            if !changed.contains(&stroke.key.track) {
                changed.push(stroke.key.track);
            }
        }
        self.automation.wrote |= !changed.is_empty();
        for track in changed {
            self.send_routes(track);
        }
    }
}

fn owner_of(track: TrackRef) -> ChainRef {
    match track {
        TrackRef::Track(t) => ChainRef::Track(t),
        TrackRef::Master => ChainRef::Master,
    }
}
