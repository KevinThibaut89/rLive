//! Adding, removing and mixing tracks, and routing live MIDI to them.

use super::routing::reorder_ops;
use super::{mix_of, ChainRef, Session};
use rlive_core::{MidiMessage, TrackId};
use rlive_engine::shared::TrackShared;
use rlive_engine::{mix_cell, EngineCommand, GraphOp, RecordCmd, TrackNode};
use rlive_project::history::ParkedTrack;
use rlive_project::{Route, Track, TrackKind};
use std::collections::HashMap;
use std::sync::Arc;

/// Where a moved track ends up.
#[derive(Clone, Copy)]
enum Place {
    /// At this position among the other tracks, joining any group it lands in.
    Index(usize),
    /// As the last member of a group.
    Into(TrackId),
}

impl Session {
    // ------------------------------------------------------------- tracks

    pub fn add_track(&mut self, kind: TrackKind, index: Option<usize>) -> TrackId {
        if kind == TrackKind::Return {
            return self.add_return().unwrap_or(TrackId(0));
        }
        let n = self.project.tracks.len() + 1;
        let name = match kind {
            TrackKind::Midi => format!("{n} MIDI"),
            TrackKind::Audio => format!("{n} Audio"),
            TrackKind::Group => format!("{n} Group"),
            TrackKind::Return | TrackKind::Master => "Master".to_string(),
        };
        let mut t = Track::new(
            &name,
            kind,
            self.colors.track_color(&self.project),
            self.project.scenes.len(),
        );
        self.apply_track_default(&mut t);
        // A track placed among a group's members joins them.
        let prev = index
            .and_then(|i| i.checked_sub(1))
            .and_then(|i| self.project.tracks.get(i))
            .map(|t| t.id);
        t.group = self.project.group_after(prev);
        let id = t.id;
        let index = index.unwrap_or(self.project.tracks.len());
        self.install_track(t, index);
        self.mark_edit("Add Track");
        id
    }

    /// Instantiates a track's engine side and registers its UI side. The caller puts it in the
    /// project and sends the node.
    pub(super) fn build_node(&mut self, t: &Track) -> Box<TrackNode> {
        let shared = Arc::new(TrackShared::default());
        self.track_shared.insert(t.id, shared.clone());
        let returns: Vec<TrackId> = self.project.returns.iter().map(|r| r.id).collect();
        let mut errors = Vec::new();
        let mut runtimes = HashMap::new();
        let mut layouts = HashMap::new();
        let (mut b, _) = self.builder();
        let node = b.track(
            t,
            &returns,
            shared,
            &mut runtimes,
            &mut layouts,
            &mut errors,
        );
        self.runtimes.extend(runtimes);
        self.layouts.extend(layouts);
        self.messages.extend(errors);
        node
    }

    /// Adds a fully formed track to the project and the engine.
    pub(super) fn install_track(&mut self, t: Track, index: usize) {
        let index = index.min(self.project.tracks.len());
        let node = self.build_node(&t);
        self.project.tracks.insert(index, t);
        self.send_track_edits(vec![GraphOp::AddTrack { index, track: node }]);
    }

    /// Takes a track out of the project and everything mirroring it. Returns the edit that does
    /// the same in the engine; the caller sends it, with whatever else changes alongside.
    pub(super) fn forget_track(&mut self, id: TrackId) -> Option<GraphOp> {
        let mut t = match (self.project.track_index(id), self.project.return_index(id)) {
            (Some(i), _) => self.project.tracks.remove(i),
            (_, Some(i)) => self.project.returns.remove(i),
            _ => return None,
        };
        self.history.parked_tracks.insert(id, ParkedTrack::of(&t));
        for d in &mut t.devices {
            self.park_device(d);
            let mut ids = Vec::new();
            d.for_each(&mut |nested| ids.push(nested.id));
            for nested in ids {
                self.runtimes.remove(&nested);
            }
        }
        self.track_shared.remove(&id);
        self.layouts.remove(&ChainRef::Track(id));
        Some(GraphOp::RemoveTrack(id))
    }

    /// Deletes one track from the engine and project, with no other changes: undo does this.
    pub(super) fn remove_track(&mut self, id: TrackId) {
        if let Some(op) = self.forget_track(id) {
            self.send_track_edits(vec![op]);
            self.update_midi_targets();
        }
    }

    /// Deletes a track. A group goes together with everything in it.
    pub fn delete_track(&mut self, id: TrackId) {
        if self.project.track_index(id).is_none() {
            return;
        }
        let block = self.project.track_block(id);
        let ops: Vec<GraphOp> = block.iter().filter_map(|t| self.forget_track(*t)).collect();
        self.project.prune_links();
        self.send_track_edits(ops);
        self.update_midi_targets();
        self.mark_edit("Delete Track");
    }

    /// One track to a new position in the engine and the project. Undo does this.
    pub(super) fn move_track_exact(&mut self, id: TrackId, to: usize) {
        if let Some(i) = self.project.track_index(id) {
            let t = self.project.tracks.remove(i);
            let to = to.min(self.project.tracks.len());
            self.project.tracks.insert(to, t);
            self.send_track_edits(vec![GraphOp::MoveTrack { track: id, to }]);
        }
    }

    /// Moves a track, with everything in it if it is a group, so that it ends up at `to` among
    /// the other tracks. It joins whatever group it lands in, and leaves the one it was in.
    pub fn move_track(&mut self, id: TrackId, to: usize) {
        self.reposition(id, Place::Index(to));
    }

    /// Moves each track of `selection` one place, keeping the gaps between them (manual
    /// p. 1030). `delta` is -1 or 1. Nothing moves if the first one to go is already at the edge.
    pub fn shift_tracks(&mut self, selection: &[TrackId], delta: isize) {
        let project = &self.project;
        let mut chosen: Vec<TrackId> = selection
            .iter()
            .copied()
            .filter(|t| project.track_index(*t).is_some())
            .collect();
        // A group moves with its contents, so a member inside a selected group doesn't also go.
        let groups = chosen.clone();
        chosen.retain(|t| {
            !groups
                .iter()
                .any(|g| g != t && project.track_block(*g).contains(t))
        });
        chosen.sort_by_key(|t| project.track_index(*t));
        chosen.dedup();
        if delta > 0 {
            chosen.reverse();
        }
        let Some(&first) = chosen.first() else {
            return;
        };
        if self.track_shift_target(first, delta).is_none() {
            return;
        }
        for id in chosen {
            if let Some(to) = self.track_shift_target(id, delta) {
                self.move_track(id, to);
            }
        }
        self.mark_edit("Move Track");
    }

    /// Where `move_track` must put a track for it to swap with its neighbour, if it has one.
    fn track_shift_target(&self, id: TrackId, delta: isize) -> Option<usize> {
        let project = &self.project;
        let start = project.track_index(id)?;
        let block = project.track_block(id);
        let parent = project.parent_group(id);
        if delta < 0 {
            let before = project.tracks[..start]
                .iter()
                .rev()
                .find(|t| project.parent_group(t.id) == parent)?;
            project.track_index(before.id)
        } else {
            let next_start = start + block.len();
            let next = project.tracks[next_start..]
                .iter()
                .find(|t| project.parent_group(t.id) == parent)?;
            let at = project.track_index(next.id)?;
            Some(at - block.len() + project.track_block(next.id).len())
        }
    }

    /// Moves a track into a group, as its last member.
    pub fn move_into_group(&mut self, id: TrackId, group: TrackId) {
        let inside = self.project.track_block(id).contains(&group);
        if inside || !self.project.track(group).is_some_and(Track::is_group) {
            return;
        }
        self.reposition(id, Place::Into(group));
    }

    /// Takes a track's block out, puts it back where asked among the rest and tells the engine.
    fn reposition(&mut self, id: TrackId, place: Place) {
        let Some(start) = self.project.track_index(id) else {
            return;
        };
        let block = self.project.track_block(id);
        let rest: Vec<usize> = (0..self.project.tracks.len())
            .filter(|i| !block.contains(&self.project.tracks[*i].id))
            .collect();
        let at = match place {
            Place::Index(to) => to.min(rest.len()),
            Place::Into(group) => {
                let members = self.project.track_block(group);
                rest.iter()
                    .rposition(|i| members.contains(&self.project.tracks[*i].id))
                    .map_or(rest.len(), |last| last + 1)
            }
        };
        let before = self
            .project
            .tracks
            .iter()
            .take(start)
            .filter(|t| !block.contains(&t.id))
            .count();
        // Asked to go to where it already is, staying in its place in the list.
        let unchanged = at == before;
        let prev = at.checked_sub(1).map(|i| self.project.tracks[rest[i]].id);
        let new_parent = match place {
            Place::Index(_) => self.project.group_after(prev),
            Place::Into(group) => Some(group),
        };
        let current_parent = self.project.parent_group(id);
        if unchanged && new_parent == current_parent {
            return;
        }
        let mut order: Vec<TrackId> = rest.iter().map(|&i| self.project.tracks[i].id).collect();
        for (i, moved) in block.iter().enumerate() {
            order.insert(at + i, *moved);
        }
        debug_assert_eq!(order.len(), self.project.tracks.len());
        let ops = reorder_ops(self.project.tracks.iter().map(|t| t.id).collect(), &order);
        self.apply_order(&order);
        if let Some(t) = self.project.track_mut(id) {
            t.group = new_parent;
        }
        self.send_track_edits(ops);
        self.mark_edit("Move Track");
    }

    /// Makes `project.tracks` follow `order`.
    fn apply_order(&mut self, order: &[TrackId]) {
        self.project
            .tracks
            .sort_by_cached_key(|t| order.iter().position(|id| *id == t.id));
    }

    /// Push the track's volume/pan/mute/solo to the engine after editing the project.
    pub fn sync_mix(&mut self, id: TrackId) {
        if self.push_mix(id, true) {
            self.mark_edit("Change Mixer");
        }
    }

    /// Sends the cells of a track's mixer that differ from the project. Returns whether the
    /// track exists.
    pub(super) fn push_mix(&mut self, id: TrackId, gesture: bool) -> bool {
        let Some(t) = self.project.track(id) else {
            return false;
        };
        let mix = mix_of(t);
        let chain = ChainRef::Track(id);
        for (cell, value) in [
            (mix_cell::VOLUME, mix.volume_db as f64),
            (mix_cell::PAN, mix.pan as f64),
            (mix_cell::MUTE, mix.mute as u8 as f64),
            (mix_cell::SOLO, mix.solo as u8 as f64),
            (mix_cell::CROSSFADE_ASSIGN, mix.crossfade as f64),
        ] {
            let unchanged = self
                .layouts
                .get(&chain)
                .is_some_and(|l| l.base(cell) == Some(value));
            if !unchanged {
                self.set_cell(chain, cell, value, gesture);
            }
        }
        self.push_sends(id, gesture);
        true
    }

    pub fn set_master_volume(&mut self, db: f32) {
        self.project.master.volume_db = db;
        self.set_cell(ChainRef::Master, mix_cell::VOLUME, db as f64, true);
        self.mark_edit("Change Mixer");
    }

    /// Arms or disarms a track, and any linked with it.
    pub fn set_arm(&mut self, id: TrackId, arm: bool, exclusive: bool) {
        let linked = self.project.linked_with(id);
        for t in self.project.tracks.iter_mut() {
            if linked.contains(&t.id) {
                t.arm = arm && t.routing.input != Route::None;
            } else if exclusive && arm {
                t.arm = false;
            }
        }
        self.update_midi_targets();
        // Auto monitoring follows the Arm button.
        self.send_track_edits(Vec::new());
    }

    /// Armed tracks receive MIDI; if none are armed, the selected track does (set by the UI).
    pub fn update_midi_targets(&mut self) {
        let armed: Vec<TrackId> = self
            .project
            .tracks
            .iter()
            .filter(|t| t.arm)
            .map(|t| t.id)
            .collect();
        *self.midi_targets.lock().unwrap() = armed;
    }

    pub fn midi_in(&mut self, track: TrackId, message: MidiMessage) {
        self.send(EngineCommand::Record(RecordCmd::Midi { track, message }));
    }

    /// MIDI the user performed: audible, and recorded while recording.
    pub fn play_midi(&mut self, track: TrackId, message: MidiMessage) {
        self.send(EngineCommand::Record(RecordCmd::PlayedMidi {
            track,
            message,
            at: std::time::Instant::now(),
        }));
    }
}
