//! Parameter changes.

use crate::graph::TrackRef;
use crate::params::{CellIx, TrackRoutes};
use crate::{retire, Engine, EngineEvent, Garbage};
use rlive_core::TrackId;
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
                if gesture && self.records_automation(track) {
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
                if let Some((track, slot, position)) = self.clip_recording_at(track) {
                    let _ = self.events.push(EngineEvent::ClipAutomationReleased {
                        track,
                        slot,
                        cell,
                        position,
                    });
                    return;
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

    /// Whether a parameter gesture on this track is being recorded as automation right now: into
    /// the Arrangement, or into the Session clip it's playing.
    pub(crate) fn records_automation(&self, track: TrackRef) -> bool {
        self.automation_arm && self.playing && (self.recording || self.records_clip(track))
    }

    /// Whether a gesture on this track goes into its playing Session clip.
    fn records_clip(&self, track: TrackRef) -> bool {
        let TrackRef::Track(id) = track else {
            return false;
        };
        self.tracks
            .iter()
            .find(|t| t.id == id)
            .is_some_and(|t| t.session_automation && t.player.playing.is_some())
    }

    /// Which Session clip a gesture on this track records into, and where it's up to in it. The
    /// Arrangement outranks a Session clip: recording the Session into it gets the changes.
    fn clip_recording_at(&self, track: TrackRef) -> Option<(TrackId, usize, f64)> {
        if self.recording || !self.records_clip(track) {
            return None;
        }
        let TrackRef::Track(id) = track else {
            return None;
        };
        let t = self.tracks.iter().find(|t| t.id == id)?;
        let position = t.player.position(self.command_tick);
        let slot = t.player.playing.as_ref()?.slot;
        Some((id, slot, position))
    }

    /// The user moved a parameter while automation records: the knob is heard for now, and the
    /// UI gets told where on the timeline the move happened.
    fn record_gesture(&mut self, track: TrackRef, cell: CellIx, value: f64) {
        let tick = self.command_tick;
        let clip = self.clip_recording_at(track);
        let Some(bus) = self.bus_mut(track) else {
            return;
        };
        let automatable = bus.spec(cell).is_some_and(|s| s.automatable);
        if !automatable {
            bus.set_base(cell, value, true);
            return;
        }
        let Some(before) = bus.touch_cell(cell, value) else {
            return;
        };
        let event = match clip {
            Some((track, slot, position)) => EngineEvent::ClipAutomationMoved {
                track,
                slot,
                cell,
                value,
                before,
                position,
            },
            None => EngineEvent::AutomationMoved {
                target: track,
                cell,
                value,
                before,
                tick,
            },
        };
        let _ = self.events.push(event);
    }

    /// Replace a track's routes, handing cells no lane drives any more back to their knobs. The
    /// old routes are freed on the UI thread.
    pub(crate) fn set_routes(&mut self, track: TrackRef, routes: Arc<TrackRoutes>) {
        let (slot, bus, clip_key) = match track {
            TrackRef::Master => (
                &mut self.master.routes,
                &mut self.master.bus,
                &mut self.master.clip_key,
            ),
            TrackRef::Track(id) => match self.tracks.iter_mut().find(|t| t.id == id) {
                Some(t) => {
                    let t = &mut **t;
                    (&mut t.routes, &mut t.bus, &mut t.clip_key)
                }
                None => {
                    retire(&mut self.garbage, Garbage::Shared(routes));
                    return;
                }
            },
        };
        slot.release_dropped(&routes, bus);
        if !slot.same_clips(&routes) {
            // What was running stops; the clip's new envelopes take over at the next step.
            *clip_key = None;
            let events = &mut self.events;
            bus.release_clip_envelopes(
                |cell| routes.has_lane(cell),
                |cell, value| {
                    let _ = events.push(EngineEvent::ClipAutomationEnded {
                        target: track,
                        cell,
                        value,
                    });
                },
            );
        }
        let old = std::mem::replace(slot, routes);
        retire(&mut self.garbage, Garbage::Shared(old));
    }
}
