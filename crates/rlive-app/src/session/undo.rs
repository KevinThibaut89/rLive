//! Undo and redo.

use super::{ChainRef, Session};
use rlive_core::{DeviceId, TrackId};
use rlive_project::history::{Snapshot, TrackSnap};
use rlive_project::{Device, DeviceKind, Track};
use std::collections::HashSet;

impl Session {
    // ------------------------------------------------------------- undo

    /// Records that an undoable edit happened.
    pub(super) fn mark_edit(&mut self, label: &'static str) {
        self.dirty = true;
        self.history.pending.get_or_insert(label);
    }

    /// Records a change that undo doesn't cover.
    pub(super) fn touch(&mut self) {
        self.dirty = true;
        self.history.untracked_dirty = true;
    }

    /// Keeps a removed device, with its plugin state, in case undo brings it back.
    pub(super) fn park_device(&mut self, d: &mut Device) {
        if let DeviceKind::Plugin { state, .. } = &mut d.kind {
            if let Some(h) = self.runtimes.get_mut(&d.id).and_then(|r| r.plugin.as_mut()) {
                if let Some(s) = h.save_state() {
                    *state = Some(s);
                }
            }
        }
        self.history.parked_devices.insert(d.id, d.clone());
    }

    /// Turns finished edits into an undo step; call once per frame. While
    /// `busy` (a drag or text entry in progress) edits accumulate into one step.
    pub fn commit_edits(&mut self, busy: bool) {
        if busy {
            self.history.was_busy = true;
            return;
        }
        let finished = std::mem::take(&mut self.history.was_busy);
        if self.history.pending.is_none() && !finished {
            return;
        }
        let label = self.history.pending.take().unwrap_or("Edit");
        if self.history.commit(&self.project, label) {
            self.dirty = true;
        }
    }

    fn after_restore(&mut self) {
        self.history.pending = None;
        self.history.was_busy = false;
        self.dirty = self.history.untracked_dirty || !self.history.at_saved();
    }

    pub fn undo(&mut self) -> bool {
        if self.is_recording() {
            return false;
        }
        self.commit_edits(false);
        let Some(target) = self.history.undo_target() else {
            return false;
        };
        self.restore(&target);
        self.after_restore();
        true
    }

    pub fn redo(&mut self) -> bool {
        if self.is_recording() {
            return false;
        }
        self.commit_edits(false);
        let Some(target) = self.history.redo_target() else {
            return false;
        };
        self.restore(&target);
        self.after_restore();
        true
    }

    /// Brings the project to `target` by applying only what differs, so
    /// everything else keeps playing and no unaffected plugin is touched.
    fn restore(&mut self, target: &Snapshot) {
        self.project.scenes = (*target.scenes).clone();
        let keep: HashSet<TrackId> = target.tracks.iter().map(|t| t.id).collect();
        let gone: Vec<TrackId> = self
            .project
            .tracks
            .iter()
            .map(|t| t.id)
            .filter(|id| !keep.contains(id))
            .collect();
        for id in gone {
            self.delete_track(id);
        }
        for (i, s) in target.tracks.iter().enumerate() {
            if self.project.track(s.id).is_none() {
                let t = self.track_from_snapshot(s);
                self.install_track(t, i);
            }
        }
        for (i, s) in target.tracks.iter().enumerate() {
            if self.project.track_index(s.id) != Some(i) {
                self.move_track(s.id, i);
            }
        }
        for s in &target.tracks {
            self.restore_track(s);
        }
        self.restore_chain(ChainRef::Master, &target.master_devices);
        self.project.arrangement = target.arrangement.clone();
        self.sync_loop();
        self.project.normalize();
        self.update_midi_targets();
    }

    fn track_from_snapshot(&self, s: &TrackSnap) -> Track {
        let mut t = Track::new(&s.name, s.kind, s.color, s.clips.len());
        t.id = s.id;
        if let Some(m) = self.history.parked_mix.get(&s.id) {
            t.volume_db = m.volume_db;
            t.pan = m.pan;
            t.mute = m.mute;
            t.solo = m.solo;
            t.arm = m.arm;
        }
        t.devices = s
            .device_ids
            .iter()
            .filter_map(|id| self.history.parked_devices.get(id).cloned())
            .collect();
        t.clips = s.clips.iter().map(|c| c.as_deref().cloned()).collect();
        t.arrangement = s.lane.iter().map(|c| (**c).clone()).collect();
        t
    }

    fn restore_track(&mut self, s: &TrackSnap) {
        let Some(t) = self.project.track_mut(s.id) else {
            return;
        };
        t.name.clone_from(&s.name);
        t.color = s.color;
        let slots = t.clips.len().max(s.clips.len());
        let mut changed = Vec::new();
        for i in 0..slots {
            let want = s.clips.get(i).and_then(|c| c.as_deref());
            if t.clips.get(i).and_then(Option::as_ref) != want {
                changed.push(i);
            }
        }
        t.clips = s.clips.iter().map(|c| c.as_deref().cloned()).collect();
        let lane_changed = t.arrangement.len() != s.lane.len()
            || t.arrangement.iter().zip(&s.lane).any(|(a, b)| *a != **b);
        if lane_changed {
            t.arrangement = s.lane.iter().map(|c| (**c).clone()).collect();
        }
        self.restore_chain(ChainRef::Track(s.id), &s.device_ids);
        for slot in changed {
            self.sync_clip(s.id, slot);
        }
        if lane_changed {
            self.sync_lane(s.id);
        }
    }

    fn restore_chain(&mut self, chain: ChainRef, ids: &[DeviceId]) {
        let current: Vec<DeviceId> = self.devices(chain).iter().map(|d| d.id).collect();
        for id in current.iter().filter(|id| !ids.contains(id)) {
            self.remove_device(chain, *id);
        }
        for (i, id) in ids.iter().enumerate() {
            if !current.contains(id) {
                if let Some(d) = self.history.parked_devices.get(id).cloned() {
                    self.add_device(chain, Some(i), d);
                }
            }
        }
        for (i, id) in ids.iter().enumerate() {
            if self.devices(chain).get(i).map(|d| d.id) != Some(*id) {
                self.move_device(chain, *id, i);
            }
        }
    }
}
