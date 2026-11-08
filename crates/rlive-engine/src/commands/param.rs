//! Parameter changes.

use crate::graph::TrackRef;
use crate::params::{CellIx, TrackRoutes};
use crate::{retire, Engine, EngineEvent, Garbage};
use std::sync::Arc;

pub enum ParamCmd {
    /// Set a parameter's knob, in plain units. `gesture` means the user moved it, which
    /// overrides automation on that parameter.
    Set {
        track: TrackRef,
        cell: CellIx,
        value: f64,
        gesture: bool,
    },
    /// Replace what modulates this track.
    SetRoutes {
        track: TrackRef,
        routes: Arc<TrackRoutes>,
    },
    /// Hand one parameter, or all of a track's, back to automation.
    ReEnable {
        track: TrackRef,
        cell: Option<CellIx>,
    },
    /// The user let go of a parameter while automation records, so its envelope has it again.
    EndGesture { track: TrackRef, cell: CellIx },
}

impl Engine {
    pub(crate) fn handle_param(&mut self, cmd: ParamCmd) {
        match cmd {
            ParamCmd::Set {
                track,
                cell,
                value,
                gesture,
            } => {
                if gesture && self.records_automation() {
                    self.record_gesture(track, cell, value);
                } else if let Some(bus) = self.bus_mut(track) {
                    bus.set_base(cell, value, gesture);
                }
            }
            ParamCmd::SetRoutes { track, routes } => self.set_routes(track, routes),
            ParamCmd::ReEnable { track, cell } => {
                if let Some(bus) = self.bus_mut(track) {
                    bus.re_enable(cell);
                }
            }
            ParamCmd::EndGesture { track, cell } => {
                if let Some(bus) = self.bus_mut(track) {
                    bus.release_touch(cell);
                }
                let tick = self.command_tick;
                let _ = self.events.push(EngineEvent::AutomationReleased {
                    target: track,
                    cell,
                    tick,
                });
            }
        }
    }

    /// Whether a parameter gesture is being recorded as automation right now.
    pub(crate) fn records_automation(&self) -> bool {
        self.automation_arm && self.recording && self.playing
    }

    /// The user moved a parameter while automation records: the knob is heard for now, and the
    /// UI gets told where on the timeline the move happened.
    fn record_gesture(&mut self, track: TrackRef, cell: CellIx, value: f64) {
        let tick = self.command_tick;
        let Some(bus) = self.bus_mut(track) else {
            return;
        };
        let automatable = bus.spec(cell).is_some_and(|s| s.automatable);
        if !automatable {
            bus.set_base(cell, value, true);
            return;
        }
        if let Some(before) = bus.touch_cell(cell, value) {
            let _ = self.events.push(EngineEvent::AutomationMoved {
                target: track,
                cell,
                value,
                before,
                tick,
            });
        }
    }

    /// Replace a track's routes, handing cells no lane drives any more back to their knobs. The
    /// old routes are freed on the UI thread.
    pub(crate) fn set_routes(&mut self, track: TrackRef, routes: Arc<TrackRoutes>) {
        let (slot, bus) = match track {
            TrackRef::Master => (&mut self.master.routes, &mut self.master.bus),
            TrackRef::Track(id) => match self.tracks.iter_mut().find(|t| t.id == id) {
                Some(t) => {
                    let t = &mut **t;
                    (&mut t.routes, &mut t.bus)
                }
                None => {
                    retire(&mut self.garbage, Garbage::Shared(routes));
                    return;
                }
            },
        };
        slot.release_dropped(&routes, bus);
        let old = std::mem::replace(slot, routes);
        retire(&mut self.garbage, Garbage::Shared(old));
    }
}
