//! Group tracks, return tracks and sends.

use super::{ChainRef, Session};
use rlive_core::param::{mixer, EnvTarget, ParamOwner, ParamRef};
use rlive_core::{ParamId, TrackId};
use rlive_engine::{mix_cell, GraphOp, MAX_SENDS};
use rlive_project::{Track, TrackDelay, TrackKind, TrackSend, PALETTE};

/// The edits that take the engine from the order `current` to `target`, which hold the same
/// tracks. Everything else in the engine's list sits after them and is left where it is.
pub(super) fn reorder_ops(mut current: Vec<TrackId>, target: &[TrackId]) -> Vec<GraphOp> {
    let mut ops = Vec::new();
    for (i, want) in target.iter().enumerate() {
        if current[i] == *want {
            continue;
        }
        let Some(j) = current.iter().position(|id| id == want) else {
            continue;
        };
        let id = current.remove(j);
        current.insert(i, id);
        ops.push(GraphOp::MoveTrack {
            track: *want,
            to: i,
        });
    }
    ops
}

impl Session {
    /// Puts tracks into a new group, nesting any groups among them. Returns the new group, or
    /// `None` if nothing could be grouped. Members keep any custom Audio To.
    pub fn group_tracks(&mut self, ids: &[TrackId]) -> Option<TrackId> {
        // Only clip and group tracks can be grouped, and a track goes with its group.
        let chosen: Vec<TrackId> = self
            .project
            .tracks
            .iter()
            .filter(|t| ids.contains(&t.id))
            .map(|t| t.id)
            .collect();
        let roots: Vec<TrackId> = chosen
            .iter()
            .copied()
            .filter(|id| {
                !self
                    .project
                    .ancestors(*id)
                    .iter()
                    .any(|a| chosen.contains(a))
            })
            .collect();
        let first = *roots.first()?;
        let shared = self.project.parent_group(first);
        let parent = shared.filter(|p| {
            roots
                .iter()
                .all(|id| self.project.parent_group(*id) == Some(*p))
        });
        let index = self.project.track_index(first)?;
        let scenes = self.project.scenes.len();
        let n = self.project.tracks.len() + 1;
        let mut group = Track::new(
            &format!("{n} Group"),
            TrackKind::Group,
            self.project.next_color(),
            scenes,
        );
        group.group = parent;
        let group_id = group.id;

        // Everything moving goes together, right after the new group, in track order.
        let moving: Vec<TrackId> = self
            .project
            .tracks
            .iter()
            .filter(|t| {
                roots
                    .iter()
                    .any(|r| self.project.track_block(*r).contains(&t.id))
            })
            .map(|t| t.id)
            .collect();
        let mut order: Vec<TrackId> = self
            .project
            .tracks
            .iter()
            .map(|t| t.id)
            .filter(|id| !moving.contains(id))
            .collect();
        let before = self.project.tracks[..index]
            .iter()
            .filter(|t| !moving.contains(&t.id))
            .count();
        order.insert(before, group_id);
        for (i, id) in moving.iter().enumerate() {
            order.insert(before + 1 + i, *id);
        }

        let node = self.build_node(&group);
        let mut ops = vec![GraphOp::AddTrack {
            index: before,
            track: node,
        }];
        let mut current: Vec<TrackId> = self.project.tracks.iter().map(|t| t.id).collect();
        current.insert(before, group_id);
        ops.extend(reorder_ops(current, &order));
        self.project.tracks.insert(before, group);
        for root in &roots {
            if let Some(t) = self.project.track_mut(*root) {
                t.group = Some(group_id);
            }
        }
        self.project
            .tracks
            .sort_by_cached_key(|t| order.iter().position(|id| *id == t.id));
        self.send_track_edits(ops);
        self.mark_edit("Group Tracks");
        Some(group_id)
    }

    /// Reverts a group into its tracks, which take its place and its own group, if any.
    pub fn ungroup(&mut self, group: TrackId) {
        if !self.project.track(group).is_some_and(Track::is_group) {
            return;
        }
        let parent = self.project.parent_group(group);
        for child in self.project.group_children(group) {
            if let Some(t) = self.project.track_mut(child) {
                t.group = parent;
            }
        }
        if let Some(op) = self.forget_track(group) {
            self.send_track_edits(vec![op]);
        }
        self.mark_edit("Ungroup Tracks");
    }

    /// Shows or hides what is inside a group. View state, so undo leaves it alone.
    pub fn set_folded(&mut self, group: TrackId, folded: bool) {
        if let Some(t) = self.project.track_mut(group).filter(|t| t.is_group()) {
            t.ui.folded = folded;
            self.touch();
        }
    }

    /// Whether anything in a group sits at a scene, so its group slot has launch buttons.
    pub fn group_slot_has_clip(&self, group: TrackId, scene: usize) -> bool {
        self.project
            .group_contents(group)
            .iter()
            .any(|id| self.clip(*id, scene).is_some())
    }

    /// Launches every clip contained in a group at a scene.
    pub fn launch_group_slot(&mut self, group: TrackId, scene: usize) {
        for id in self.clip_tracks_in(group) {
            if self.clip(id, scene).is_some() {
                self.launch_clip(id, scene);
            }
        }
    }

    /// Stops every clip contained in a group.
    pub fn stop_group(&mut self, group: TrackId) {
        for id in self.clip_tracks_in(group) {
            self.stop_track(id);
        }
    }

    fn clip_tracks_in(&self, group: TrackId) -> Vec<TrackId> {
        self.project
            .group_contents(group)
            .into_iter()
            .filter(|id| self.project.track(*id).is_some_and(|t| !t.is_group()))
            .collect()
    }

    /// Paints a group's contained tracks and clips in its colour: Session clips or Arrangement
    /// clips, whichever the current view shows.
    pub fn assign_group_color(&mut self, group: TrackId, session_view: bool) {
        let Some(color) = self.project.track(group).map(|t| t.color) else {
            return;
        };
        for id in self.project.group_contents(group) {
            let Some(t) = self.project.track_mut(id) else {
                continue;
            };
            t.color = color;
            if session_view {
                for c in t.clips.iter_mut().flatten() {
                    c.color = color;
                }
            } else {
                for c in t.arrangement.iter_mut() {
                    c.clip.color = color;
                }
            }
        }
        self.mark_edit("Assign Color");
    }

    /// Adds a return track at the end. Returns `None` once there are as many as a Set can have.
    pub fn add_return(&mut self) -> Option<TrackId> {
        let at = self.project.returns.len();
        if at >= MAX_SENDS {
            return None;
        }
        let letter = (b'A' + at as u8) as char;
        let color = PALETTE[(at * 3 + 2) % PALETTE.len()];
        let t = Track::new(&format!("{letter} Return"), TrackKind::Return, color, 0);
        let id = t.id;
        self.install_return(t, at);
        self.mark_edit("Add Return Track");
        Some(id)
    }

    /// Adds a fully formed return track to the project and the engine.
    pub(super) fn install_return(&mut self, t: Track, index: usize) {
        let index = index.min(self.project.returns.len());
        let node = self.build_node(&t);
        self.project.returns.insert(index, t);
        let engine_index = self.project.tracks.len() + index;
        self.send_track_edits(vec![GraphOp::AddTrack {
            index: engine_index,
            track: node,
        }]);
    }

    /// Deletes a return track, and every send to it. Sends after it move down one.
    pub fn delete_return(&mut self, id: TrackId) {
        let Some(n) = self.project.return_index(id) else {
            return;
        };
        let moved_from = mixer::FIRST_SEND.0 + n as u32;
        for t in self
            .project
            .tracks
            .iter_mut()
            .chain(self.project.returns.iter_mut())
        {
            t.sends.retain(|s| s.to != id);
            let owner = ParamOwner::Track(t.id);
            t.automation.retain_mut(|e| match &mut e.target {
                EnvTarget::Param(ParamRef { owner: o, id: p }) if *o == owner => {
                    if p.0 == moved_from {
                        false
                    } else {
                        if p.0 > moved_from {
                            *p = ParamId(p.0 - 1);
                        }
                        true
                    }
                }
                _ => true,
            });
        }
        if let Some(op) = self.forget_track(id) {
            self.send_track_edits(vec![op]);
        }
        self.push_all_sends();
        self.mark_edit("Delete Return Track");
    }

    /// Pushes every send of every track to the engine, after return tracks changed position.
    pub(super) fn push_all_sends(&mut self) {
        let ids: Vec<TrackId> = self
            .project
            .tracks
            .iter()
            .chain(&self.project.returns)
            .map(|t| t.id)
            .collect();
        for id in ids {
            self.push_sends(id, false);
        }
    }

    /// Whether the sends feeding a return track tap before or after the mixer stage.
    pub fn set_pre_fader_sends(&mut self, ret: TrackId, pre: bool) {
        let Some(n) = self.project.return_index(ret) else {
            return;
        };
        self.project.returns[n].pre_fader_sends = pre;
        self.send_track_edits(Vec::new());
        self.mark_edit("Change Send Pre/Post");
    }

    /// The send's level from `track` to a return track, in dB.
    pub fn send_level(&self, track: TrackId, ret: TrackId) -> f32 {
        self.project
            .track(track)
            .map_or(rlive_project::SEND_OFF_DB, |t| t.send_level(ret))
    }

    pub fn set_send(&mut self, track: TrackId, ret: TrackId, db: f32) {
        let Some(n) = self.project.return_index(ret) else {
            return;
        };
        let Some(t) = self.project.track_mut(track) else {
            return;
        };
        match t.sends.iter_mut().find(|s| s.to == ret) {
            Some(s) => s.level_db = db,
            None => {
                let enabled = t.kind != TrackKind::Return;
                t.sends.push(TrackSend {
                    to: ret,
                    level_db: db,
                    enabled,
                });
            }
        }
        self.set_cell(ChainRef::Track(track), mix_cell::send(n), db as f64, true);
        self.mark_edit("Change Send");
    }

    /// Enables or disables one send. A return track's sends can only feed back when enabled.
    pub fn set_send_enabled(&mut self, track: TrackId, ret: TrackId, enabled: bool) {
        if self.project.return_index(ret).is_none() {
            return;
        }
        if let Some(t) = self.project.track_mut(track) {
            match t.sends.iter_mut().find(|s| s.to == ret) {
                Some(s) => s.enabled = enabled,
                None => t.sends.push(TrackSend {
                    to: ret,
                    level_db: rlive_project::SEND_OFF_DB,
                    enabled,
                }),
            }
        }
        self.send_track_edits(Vec::new());
        self.mark_edit("Enable Send");
    }

    /// Enables every send of a return track.
    pub fn enable_all_sends(&mut self, track: TrackId) {
        let returns: Vec<TrackId> = self.project.returns.iter().map(|r| r.id).collect();
        for ret in returns {
            self.set_send_enabled(track, ret, true);
        }
    }

    /// Keep tracks in sync through device and plug-in latency, and allow Track Delay.
    pub fn set_delay_compensation(&mut self, on: bool) {
        self.project.delay_compensation = on;
        self.send_track_edits(Vec::new());
        self.mark_edit("Delay Compensation");
    }

    /// Whether tracks with input monitoring on skip compensation, for the lowest latency.
    pub fn set_reduced_latency_when_monitoring(&mut self, on: bool) {
        self.project.reduced_latency_when_monitoring = on;
        self.send_track_edits(Vec::new());
        self.mark_edit("Reduced Latency When Monitoring");
    }

    /// How much a track's output is delayed or, negative, brought forward.
    pub fn set_track_delay(&mut self, id: TrackId, delay: TrackDelay) {
        let Some(t) = self.project.track_mut(id) else {
            return;
        };
        t.delay = delay;
        self.send_track_edits(Vec::new());
        self.mark_edit("Track Delay");
    }

    /// Whether soloing a clip or group track leaves the return tracks audible.
    pub fn set_solo_in_place(&mut self, on: bool) {
        self.project.solo_in_place = on;
        self.send_track_edits(Vec::new());
        self.mark_edit("Solo in Place");
    }

    /// Sends the send levels of a track that differ from the engine's.
    pub(super) fn push_sends(&mut self, id: TrackId, gesture: bool) {
        let chain = ChainRef::Track(id);
        let levels: Vec<(usize, f64)> = {
            let Some(t) = self.project.track(id) else {
                return;
            };
            self.project
                .returns
                .iter()
                .take(MAX_SENDS)
                .enumerate()
                .map(|(n, r)| (n, t.send_level(r.id) as f64))
                .collect()
        };
        let empty = self.project.returns.len().min(MAX_SENDS);
        for n in empty..MAX_SENDS {
            let unchanged = self.layouts.get(&chain).is_some_and(|l| {
                l.base(mix_cell::send(n)) == Some(rlive_project::SEND_OFF_DB as f64)
            });
            if !unchanged {
                self.set_cell(
                    chain,
                    mix_cell::send(n),
                    rlive_project::SEND_OFF_DB as f64,
                    gesture,
                );
            }
        }
        for (n, db) in levels {
            let cell = mix_cell::send(n);
            let unchanged = self
                .layouts
                .get(&chain)
                .is_some_and(|l| l.base(cell) == Some(db));
            if !unchanged {
                self.set_cell(chain, cell, db, gesture);
            }
        }
    }
}
