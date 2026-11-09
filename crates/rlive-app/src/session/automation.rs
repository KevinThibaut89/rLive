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

use super::build::clip_curve;
use super::devices::mixer_cell_of;
use super::{build, ChainRef, Session};
use rlive_core::param::mixer;
use rlive_core::param::{transport, EnvTarget, ParamOwner, ParamRef, ACTIVATOR};
use rlive_core::{ChainId, DeviceId, ParamRange, TrackId};
use rlive_engine::shared::NO_SLOT;
use rlive_engine::{
    AutomationLane, BusLayout, CellIx, CellState, CellTarget, EngineCommand, GraphOp, MacroRoute,
    ParamCmd, RecordCmd, TrackRef, TrackRoutes, TransportCmd,
};
use rlive_engine::{ClipEnvAction, ClipLane, ClipRoutes};
use rlive_project::clip_envelope::{self, modulation_range, SessionAutomationIn};
use rlive_project::{ArrangementClip, Breakpoint, Clip, Device, DeviceKind, EnvMode, Envelope};
use std::collections::{HashMap, HashSet};
use std::mem::take;
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// Compiles a track's envelopes and Macro mappings for the engine, addressed to the cells of its
/// layout. Envelopes for something without a cell (yet) are left out and kept in the project.
pub(super) fn routes_for(
    envelopes: &[Envelope],
    devices: &[Device],
    owner: ChainRef,
    layout: &BusLayout,
    clips: (&[Option<Clip>], &[ArrangementClip]),
    mods: impl FnOnce(&HashSet<CellIx>) -> Vec<rlive_engine::ModRoute>,
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
    let macro_cells: HashSet<CellIx> = macros.iter().map(|m| m.target()).collect();
    let session = clips
        .0
        .iter()
        .enumerate()
        .filter_map(|(slot, clip)| {
            let clip = clip.as_ref()?;
            Some((slot as u32, clip_lanes(clip, owner, layout, true)))
        })
        .collect();
    let lane = clips
        .1
        .iter()
        .map(|c| (c.start, clip_lanes(&c.clip, owner, layout, false)))
        .collect();
    Arc::new(
        TrackRoutes::new(lanes)
            .with_macros(macros)
            .with_mods(mods(&macro_cells))
            .with_clips(session, lane),
    )
}

/// A clip's envelopes for the parameters of its track. An Arrangement clip only has modulation:
/// a Session clip's automation is what it is because it's launched (manual p. 538).
fn clip_lanes(clip: &Clip, owner: ChainRef, layout: &BusLayout, session: bool) -> ClipRoutes {
    ClipRoutes::new(
        clip.envelopes
            .iter()
            .filter_map(|e| {
                let EnvTarget::Param(target) = e.envelope.target else {
                    return None;
                };
                let cell = cell_of(target, owner, layout)?;
                let spec = layout.spec(cell)?;
                let (range, action) = match e.mode {
                    EnvMode::Automation if !session => return None,
                    EnvMode::Automation => (spec.range, ClipEnvAction::Automate),
                    EnvMode::Modulation => (
                        modulation_range(spec.mod_mode).0,
                        ClipEnvAction::Modulate(spec.mod_mode),
                    ),
                };
                Some(ClipLane::new(cell, clip_curve(e, clip, range)?, action))
            })
            .collect(),
    )
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
pub(super) struct CellKey {
    pub track: TrackRef,
    pub cell: CellIx,
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
    /// The Session slot this goes into, when it isn't the Arrangement. Points are then in the
    /// clip's own time.
    slot: Option<usize>,
}

impl Stroke {
    fn starting(key: CellKey, before: f64, tick: f64, value: f64) -> Self {
        Self {
            key,
            before,
            points: vec![Breakpoint::new(tick, value)],
            slot: None,
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
    /// The last control the user moved, as its track and cell, for the automation lanes to show.
    touched: Option<(ChainRef, CellIx)>,
    /// Session Record: gestures go into the Session clips playing.
    session_record: bool,
    /// Which tracks Session Record records on.
    session_in: SessionAutomationIn,
    /// Which tracks the engine currently has recording into their Session clips.
    session_tracks: HashSet<TrackId>,
    /// Tracks the engine has clip envelopes for, so it hears when the last of them goes.
    clip_routed: HashSet<TrackId>,
}

impl AutomationRecorder {
    pub(super) fn take_touched(&mut self) -> Option<(ChainRef, CellIx)> {
        self.touched.take()
    }

    /// Forget the pass in progress, as its parameters may no longer exist.
    pub(super) fn abandon(&mut self) {
        self.gestures.clear();
        self.open.clear();
        self.done.clear();
        self.wrote = false;
        self.session_tracks.clear();
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
    /// user has overridden that. Modulation doesn't move the needle; it's drawn beside it.
    pub fn automated_cell_value(&self, chain: ChainRef, cell: CellIx) -> Option<f64> {
        let layout = self.layouts.get(&chain.owner())?;
        let state = layout.state(cell);
        if !state.automated() || state.overridden() {
            return None;
        }
        let norm = layout.source(cell)?;
        Some(layout.spec(cell)?.range.from_normalized(norm as f64))
    }

    /// Where modulation has taken a control, normalized, when something is modulating it. Mixer
    /// controls draw this beside their needle (manual pp. 539-541).
    pub fn modulated_cell_norm(&self, chain: ChainRef, cell: CellIx) -> Option<f32> {
        let layout = self.layouts.get(&chain.owner())?;
        layout
            .state(cell)
            .modulated()
            .then(|| layout.effective(cell))
            .flatten()
    }

    /// The normalized value a control is at, before modulation.
    pub fn source_norm(&self, chain: ChainRef, cell: CellIx) -> Option<f32> {
        self.layouts.get(&chain.owner())?.source(cell)
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

    pub(super) fn envelopes(&self, track: TrackRef) -> Option<&Vec<Envelope>> {
        match track {
            TrackRef::Master => Some(&self.project.master.automation),
            TrackRef::Track(id) => self.project.track(id).map(|t| &t.automation),
        }
    }

    pub(super) fn envelopes_mut(&mut self, track: TrackRef) -> Option<&mut Vec<Envelope>> {
        match track {
            TrackRef::Master => Some(&mut self.project.master.automation),
            TrackRef::Track(id) => self.project.track_mut(id).map(|t| &mut t.automation),
        }
    }

    pub(super) fn track_clips(&self, track: TrackRef) -> (&[Option<Clip>], &[ArrangementClip]) {
        match track {
            TrackRef::Master => (&[], &[]),
            TrackRef::Track(id) => self.project.track(id).map_or((&[], &[]), |t| {
                (t.clips.as_slice(), t.arrangement.as_slice())
            }),
        }
    }

    pub(super) fn track_devices(&self, track: TrackRef) -> Option<&Vec<Device>> {
        match track {
            TrackRef::Master => Some(&self.project.master.devices),
            TrackRef::Track(id) => self.project.track(id).map(|t| &t.devices),
        }
    }

    pub(super) fn compiled_routes(&self, track: TrackRef) -> Option<Arc<TrackRoutes>> {
        let owner = owner_of(track);
        let layout = self.layouts.get(&owner)?;
        let plan = super::modulators::ModPlan::of(&self.project);
        let mut slots = self.mod_slots.clone();
        Some(routes_for(
            self.envelopes(track)?,
            self.track_devices(track)?,
            owner,
            layout,
            self.track_clips(track),
            |macros| plan.routes(owner, layout, &mut slots, macros),
        ))
    }

    /// Whether a clip on this track has an envelope for one of the track's parameters.
    pub(super) fn has_clip_routes(&self, track: TrackId) -> bool {
        let (clips, lane) = self.track_clips(TrackRef::Track(track));
        let any = |c: &Clip| {
            c.envelopes
                .iter()
                .any(|e| !clip_envelope::targets_clip_content(e.envelope.target))
        };
        clips.iter().flatten().any(any) || lane.iter().any(|c| any(&c.clip))
    }

    /// Start knowing which tracks the engine has clip envelopes for, after building them all.
    pub(super) fn note_clip_routes(&mut self) {
        let routed = self
            .project
            .tracks
            .iter()
            .map(|t| t.id)
            .filter(|id| self.has_clip_routes(*id))
            .collect();
        self.automation.clip_routed = routed;
    }

    /// Give the engine a track's routes after its clips changed, when that can have changed them.
    pub(super) fn sync_clip_routes(&mut self, track: TrackId) {
        let has = self.has_clip_routes(track);
        if has || self.automation.clip_routed.contains(&track) {
            if has {
                self.automation.clip_routed.insert(track);
            } else {
                self.automation.clip_routed.remove(&track);
            }
            self.send_routes(TrackRef::Track(track));
        }
    }

    /// Replace a parameter's envelope. No points removes it. This is what editing envelopes goes
    /// through, as one undoable edit.
    pub fn set_envelope(&mut self, chain: ChainRef, target: EnvTarget, points: Vec<Breakpoint>) {
        if target == transport::tempo_target() {
            // What the leaders generate is on show, not for editing.
            if self.project.has_tempo_leader() {
                return;
            }
            if self.project.arrangement.tempo.points != points {
                self.project.arrangement.tempo.points = points;
                self.sync_time_maps();
                self.mark_edit("Edit Automation");
            }
            return;
        }
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

    /// Give the engine the song's tempo and meter as the project has them now.
    pub(super) fn sync_time_maps(&mut self) {
        self.leader_points = self.project.leader_tempo_points();
        let maps = Arc::new(build::time_maps(&self.project));
        self.send(EngineCommand::Transport(TransportCmd::SetTimeMaps(maps)));
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
        self.automation.touched = Some((chain.owner(), cell));
        if self.automation.arm && (self.is_recording() || self.records_clip(chain)) {
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
        if self.automation.arm && (self.is_recording() || self.records_clip(chain)) {
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
            if dragged && self.has_recorded_before(key) {
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
    pub(super) fn lane_of(&self, key: CellKey) -> Option<(EnvTarget, ParamRange)> {
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

    /// Whether the envelope a gesture is recording into already existed, which decides whether
    /// the mouse going up hands the parameter back or latches it (manual pp. 514-515).
    fn has_recorded_before(&self, key: CellKey) -> bool {
        match self.automation.open.get(&key).and_then(|s| s.slot) {
            None => self.has_envelope(key),
            Some(slot) => self.clip_has_automation(key, slot),
        }
    }

    fn clip_has_automation(&self, key: CellKey, slot: usize) -> bool {
        let (TrackRef::Track(track), Some((target, _))) = (key.track, self.lane_of(key)) else {
            return false;
        };
        self.clip(track, slot)
            .and_then(|c| c.envelope(target, EnvMode::Automation))
            .is_some_and(|e| !e.envelope.points.is_empty())
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
        let open = self.take_track_strokes();
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

    // ------------------------------------------------------------- Session automation

    /// Session Record: whether gestures are recorded into the Session clips playing. A track
    /// records if it's armed, or on every track with Record Session Automation In set so.
    pub fn session_automation_record(&self) -> bool {
        self.automation.session_record
    }

    pub fn set_session_automation_record(&mut self, on: bool) {
        if on == self.automation.session_record {
            return;
        }
        self.automation.session_record = on;
        if !on {
            self.finish_session_automation();
        }
        self.sync_session_tracks();
    }

    /// Whether any track that would record Session automation has a Session clip playing.
    pub(super) fn any_clip_takes_automation(&self) -> bool {
        self.project.tracks.iter().any(|t| {
            let eligible = t.arm || self.automation.session_in == SessionAutomationIn::AllTracks;
            eligible
                && self
                    .track_shared
                    .get(&t.id)
                    .is_some_and(|s| s.playing_slot.load(Ordering::Relaxed) != NO_SLOT)
        })
    }

    /// The Record Session Automation In setting.
    pub fn set_session_automation_in(&mut self, tracks: SessionAutomationIn) {
        self.automation.session_in = tracks;
        self.sync_session_tracks();
    }

    /// Whether gestures on this chain are going into its playing Session clip.
    fn records_clip(&self, chain: ChainRef) -> bool {
        match chain.owner() {
            ChainRef::Track(t) => self.automation.session_tracks.contains(&t),
            _ => false,
        }
    }

    /// Which tracks should be recording into their clips right now, as the engine must know.
    pub(super) fn sync_session_tracks(&mut self) {
        let wanted: HashSet<TrackId> = if self.automation.session_record {
            let all = self.automation.session_in == SessionAutomationIn::AllTracks;
            self.project
                .tracks
                .iter()
                .filter(|t| all || t.arm)
                .map(|t| t.id)
                .collect()
        } else {
            HashSet::new()
        };
        let changed: Vec<(TrackId, bool)> = wanted
            .difference(&self.automation.session_tracks)
            .map(|t| (*t, true))
            .chain(
                self.automation
                    .session_tracks
                    .difference(&wanted)
                    .map(|t| (*t, false)),
            )
            .collect();
        for (track, on) in changed {
            self.send(EngineCommand::Record(RecordCmd::SetSessionAutomation {
                track,
                on,
            }));
        }
        self.automation.session_tracks = wanted;
    }

    pub(super) fn clip_automation_moved(
        &mut self,
        track: TrackId,
        slot: usize,
        cell: CellIx,
        (value, before): (f64, f64),
        position: f64,
    ) {
        let key = CellKey {
            track: TrackRef::Track(track),
            cell,
        };
        let wrapped = self.automation.open.get(&key).is_some_and(|s| {
            s.slot != Some(slot) || position < s.points.last().map_or(0.0, |p| p.time)
        });
        let resumed = if wrapped {
            self.end_clip_stroke(key, None)
        } else {
            None
        };
        self.automation
            .open
            .entry(key)
            .and_modify(|s| s.push(position, value))
            .or_insert_with(|| {
                let mut stroke = Stroke::starting(key, resumed.unwrap_or(before), position, value);
                stroke.slot = Some(slot);
                stroke
            });
    }

    pub(super) fn clip_automation_released(&mut self, track: TrackId, cell: CellIx, position: f64) {
        let key = CellKey {
            track: TrackRef::Track(track),
            cell,
        };
        self.end_clip_stroke(key, Some(position));
        self.write_recorded_automation();
    }

    /// Ends a stroke recording into a clip, at `at` or at the end of the clip, and hands it over
    /// to be written. Returns what it was last at.
    fn end_clip_stroke(&mut self, key: CellKey, at: Option<f64>) -> Option<f64> {
        let mut stroke = self.automation.open.remove(&key)?;
        let slot = stroke.slot?;
        let TrackRef::Track(track) = key.track else {
            return None;
        };
        let end = at.or_else(|| self.clip(track, slot).map(|c| c.length as f64));
        if let Some(end) = end {
            stroke.close(end);
        }
        let last = stroke.last_value();
        self.automation.done.push(stroke);
        Some(last)
    }

    /// A pass of a latched control ends at the end of the clip or its loop, which only the
    /// playhead's progress can say. Call often.
    pub(super) fn poll_clip_strokes(&mut self) {
        let keys: Vec<CellKey> = self
            .automation
            .open
            .iter()
            .filter(|(_, s)| s.slot.is_some())
            .map(|(k, _)| *k)
            .collect();
        let mut changed = false;
        for key in keys {
            let TrackRef::Track(track) = key.track else {
                continue;
            };
            let Some(stroke) = self.automation.open.get(&key) else {
                continue;
            };
            let (slot, last) = (stroke.slot, stroke.points.last().map_or(0.0, |p| p.time));
            let (playing, position) = self.track_shared.get(&track).map_or((None, 0.0), |s| {
                let at = s.playing_slot.load(Ordering::Relaxed);
                (
                    (at != NO_SLOT).then_some(at as usize),
                    s.clip_position.load(),
                )
            });
            if playing == slot && position >= last {
                continue;
            }
            // It came round, or the clip isn't playing any more.
            let end = if playing == slot { None } else { Some(last) };
            self.end_clip_stroke(key, end);
            let held = self
                .automation
                .gestures
                .get(&key)
                .is_some_and(|g| g.source == GestureSource::Mouse && !g.latched);
            if !held {
                self.automation.gestures.remove(&key);
                self.send(EngineCommand::Param(ParamCmd::EndGesture {
                    track: key.track,
                    cell: key.cell,
                }));
            }
            changed = true;
        }
        if changed {
            self.write_recorded_automation();
            self.note_automation_pass();
        }
    }

    /// The strokes in progress going into the Arrangement. Those going into a clip keep going.
    fn take_track_strokes(&mut self) -> Vec<Stroke> {
        let mut strokes = Vec::new();
        self.automation.open.retain(|_, s| {
            s.slot.is_some() || {
                strokes.push(std::mem::replace(
                    s,
                    Stroke {
                        key: s.key,
                        before: s.before,
                        points: Vec::new(),
                        slot: None,
                    },
                ));
                false
            }
        });
        strokes
    }

    /// Session Record went off: every gesture ends where the clip is now.
    fn finish_session_automation(&mut self) {
        let keys: Vec<CellKey> = self
            .automation
            .open
            .iter()
            .filter(|(_, s)| s.slot.is_some())
            .map(|(k, _)| *k)
            .collect();
        for key in keys {
            let last = self
                .automation
                .open
                .get(&key)
                .and_then(|s| s.points.last().map(|p| p.time));
            self.end_clip_stroke(key, last);
            self.automation.gestures.remove(&key);
        }
        self.write_recorded_automation();
        self.note_automation_pass();
    }

    fn note_automation_pass(&mut self) {
        if take(&mut self.automation.wrote) {
            self.dirty = true;
            self.history.pending = Some("Record Automation");
            self.history.settling = 0;
        }
    }

    /// A clip that set this parameter stopped counting, leaving it where it was. Everything else
    /// that knows the knob's value has to catch up.
    pub(super) fn clip_automation_ended(&mut self, target: TrackRef, cell: CellIx, value: f64) {
        let owner = owner_of(target);
        let Some(layout) = self.layouts.get_mut(&owner) else {
            return;
        };
        layout.note_value(cell, value);
        let Some(spec) = layout.spec(cell) else {
            return;
        };
        match spec.target {
            CellTarget::Device { device, param } => {
                let chain = self.chain_of_device(device).unwrap_or(owner);
                self.report_device_param(chain, device, param.0, value);
                return;
            }
            CellTarget::Mixer(control) => {
                if let TrackRef::Track(id) = target {
                    let returns: Vec<TrackId> = self.project.returns.iter().map(|r| r.id).collect();
                    if let Some(t) = self.project.track_mut(id) {
                        match control {
                            mixer::VOLUME => t.volume_db = value as f32,
                            mixer::PAN => t.pan = value as f32,
                            c if c >= mixer::FIRST_SEND => {
                                let n = (c.0 - mixer::FIRST_SEND.0) as usize;
                                if let Some(ret) = returns.get(n) {
                                    if let Some(send) = t.sends.iter_mut().find(|s| s.to == *ret) {
                                        send.level_db = value as f32;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                } else if control == mixer::VOLUME {
                    self.project.master.volume_db = value as f32;
                }
            }
            _ => {}
        }
        self.touch();
    }

    /// Recording stopped at `tick`. The whole pass becomes one undo step.
    pub(super) fn finish_automation_pass(&mut self, tick: f64) {
        for mut stroke in self.take_track_strokes() {
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
            if let (Some(slot), TrackRef::Track(track)) = (stroke.slot, stroke.key.track) {
                let Some(clip) = self.clip_mut(track, slot) else {
                    continue;
                };
                if clip.envelope(target, EnvMode::Automation).is_none() {
                    clip.envelopes.push(rlive_project::ClipEnvelope {
                        envelope: Envelope {
                            target,
                            points: Vec::new(),
                        },
                        mode: EnvMode::Automation,
                        loop_region: None,
                    });
                }
                if let Some(e) = clip.envelope_mut(target, EnvMode::Automation) {
                    e.envelope.overwrite(&stroke.points, stroke.before, range);
                }
                self.automation.clip_routed.insert(track);
                if !changed.contains(&stroke.key.track) {
                    changed.push(stroke.key.track);
                }
                continue;
            }
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
