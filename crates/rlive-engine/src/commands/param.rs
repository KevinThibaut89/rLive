//! Parameter changes.

use crate::graph::TrackRef;
use crate::params::{CellIx, TrackRoutes};
use crate::{retire, Engine, Garbage};
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
                if let Some(bus) = self.bus_mut(track) {
                    bus.set_base(cell, value, gesture);
                }
            }
            ParamCmd::SetRoutes { track, routes } => {
                let old = match track {
                    TrackRef::Master => Some(std::mem::replace(&mut self.master.routes, routes)),
                    TrackRef::Track(id) => self
                        .track_mut(id)
                        .map(|t| std::mem::replace(&mut t.routes, routes)),
                };
                if let Some(old) = old {
                    retire(&mut self.garbage, Garbage::Shared(old));
                }
            }
            ParamCmd::ReEnable { track, cell } => {
                if let Some(bus) = self.bus_mut(track) {
                    bus.re_enable(cell);
                }
            }
        }
    }
}
