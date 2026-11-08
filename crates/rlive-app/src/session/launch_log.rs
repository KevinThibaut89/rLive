//! Arrangement Record's log of the Session: what's launched becomes Arrangement clips, in the
//! right tracks and places, without any new audio (manual p. 197).

use super::Session;
use rlive_core::{TimeSignature, TrackId};
use rlive_project::{ArrangementClip, Breakpoint, Clip, SignatureMarker};
use std::collections::HashMap;

/// A tick as the whole tick it starts on.
pub(super) fn tick_u64(tick: f64) -> u64 {
    tick.max(0.0).round() as u64
}

struct OpenClip {
    slot: usize,
    /// Where on the timeline this stretch began.
    start: u64,
    /// The song tick of the clip's position 0.
    origin: f64,
    clip: Clip,
}

#[derive(Default)]
pub(super) struct LaunchLog {
    from: f64,
    open: HashMap<TrackId, OpenClip>,
    wrote: bool,
}

impl Session {
    pub(super) fn start_launch_log(&mut self, from: f64) {
        self.launch_log = LaunchLog {
            from,
            open: HashMap::new(),
            wrote: false,
        };
    }

    /// `track` now plays `slot`, or nothing, from `at`.
    pub(super) fn session_playback(
        &mut self,
        track: TrackId,
        slot: Option<usize>,
        at: f64,
        start_tick: f64,
    ) {
        if !self.is_recording() {
            return;
        }
        let at = at.max(self.launch_log.from);
        self.close_logged(track, at);
        let Some(slot) = slot else {
            return;
        };
        let Some(clip) = self.clip(track, slot).cloned() else {
            return;
        };
        self.launch_log.open.insert(
            track,
            OpenClip {
                slot,
                start: tick_u64(at),
                origin: start_tick,
                clip,
            },
        );
    }

    fn close_logged(&mut self, track: TrackId, at: f64) {
        let Some(open) = self.launch_log.open.remove(&track) else {
            return;
        };
        let end = tick_u64(at);
        if end <= open.start {
            return;
        }
        let mut offset = (open.start as f64 - open.origin).max(0.0).round() as u64;
        if open.clip.looped && open.clip.length > 0 && open.clip.region().is_whole() {
            offset %= open.clip.length;
        }
        let placed = ArrangementClip {
            id: rlive_core::ArrClipId::new(),
            start: open.start,
            length: end - open.start,
            offset,
            clip: open.clip,
        };
        if let Some(t) = self.project.track_mut(track) {
            t.place_clip(placed);
            t.settle_fades();
        }
        self.sync_lane(track);
        self.launch_log.wrote = true;
    }

    /// Recording is over: what's still playing goes in up to here.
    pub(super) fn close_launch_log(&mut self, tick: f64) {
        let open: Vec<TrackId> = self.launch_log.open.keys().copied().collect();
        for track in open {
            self.close_logged(track, tick);
        }
        if std::mem::take(&mut self.launch_log.wrote) {
            self.mark_edit("Record Session");
        }
    }

    /// The playhead jumped while recording: what's playing goes on from the new place.
    pub(super) fn launch_log_jumped(&mut self, from: f64, to: f64) {
        let tracks: Vec<TrackId> = self.launch_log.open.keys().copied().collect();
        for track in tracks {
            let Some(open) = self.launch_log.open.get(&track) else {
                continue;
            };
            let (slot, origin, clip) = (open.slot, open.origin, open.clip.clone());
            self.close_logged(track, from);
            self.launch_log.open.insert(
                track,
                OpenClip {
                    slot,
                    start: tick_u64(to),
                    origin: origin + (to - from),
                    clip,
                },
            );
        }
    }

    /// A clip being logged changed: the Arrangement gets a new stretch with the new properties.
    pub(super) fn clip_edited_while_logging(&mut self, track: TrackId, slot: usize) {
        let Some(open) = self.launch_log.open.get(&track).filter(|o| o.slot == slot) else {
            return;
        };
        let origin = open.origin;
        let same = self.clip(track, slot).is_some_and(|c| *c == open.clip);
        if same {
            return;
        }
        let now = self.engine.shared.tick.load();
        self.close_logged(track, now);
        if let Some(clip) = self.clip(track, slot).cloned() {
            self.launch_log.open.insert(
                track,
                OpenClip {
                    slot,
                    start: tick_u64(now),
                    origin,
                    clip,
                },
            );
        }
    }

    /// A launched scene changed the tempo while recording. It's a step in the tempo envelope.
    pub(super) fn log_tempo(&mut self, tick: f64, was: f64, bpm: f64) {
        let points = &mut self.project.arrangement.tempo.points;
        if points.is_empty() {
            points.push(Breakpoint {
                time: 0.0,
                value: was,
                curve: 0.0,
            });
        }
        points.retain(|p| p.time < tick);
        points.push(Breakpoint {
            time: tick,
            value: was,
            curve: 0.0,
        });
        points.push(Breakpoint {
            time: tick,
            value: bpm,
            curve: 0.0,
        });
        self.launch_log.wrote = true;
        self.sync_time_maps();
    }

    /// A launched scene changed the meter while recording. It's a signature marker.
    pub(super) fn log_signature(&mut self, tick: u64, signature: TimeSignature) {
        let markers = &mut self.project.arrangement.signatures;
        markers.retain(|m| m.tick != tick);
        markers.push(SignatureMarker { tick, signature });
        markers.sort_by_key(|m| m.tick);
        self.launch_log.wrote = true;
        self.sync_time_maps();
    }
}
