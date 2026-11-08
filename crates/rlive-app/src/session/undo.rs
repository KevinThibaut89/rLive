//! Undo and redo.
//!
//! Restoring applies only what differs from the target, so everything else keeps playing and no
//! unaffected plugin is touched. Parameter and mixer values go down the parameter path; the
//! engine objects themselves are only touched for what actually came, went or changed kind.

use super::{ChainRef, Session};
use rlive_core::{ChainId, DeviceId, TrackId};
use rlive_engine::{mix_cell, EngineCommand, TransportCmd};
use rlive_project::history::{
    ChainSnap, DeviceSnap, ParkedDevice, ParkedTrack, Snapshot, TrackSnap,
};
use rlive_project::{Chain, Device, DeviceKind, Project, Track};
use std::collections::HashSet;
use std::mem::take;
use std::sync::Arc;

/// Frames to wait after the last parameter a device reported before making a step of them.
pub(super) const SETTLE_FRAMES: u32 = 30;

impl Session {
    // ------------------------------------------------------------- undo

    /// Records that an undoable edit happened.
    pub(super) fn mark_edit(&mut self, label: &'static str) {
        self.dirty = true;
        self.history.pending.get_or_insert(label);
        self.history.settling = 0;
    }

    /// Records that a device changed a parameter by itself.
    pub(super) fn note_reported_edit(&mut self) {
        self.dirty = true;
        self.history.pending.get_or_insert("Change Parameter");
        self.history.settling = SETTLE_FRAMES;
    }

    /// Records a change that undo doesn't cover.
    pub(super) fn touch(&mut self) {
        self.dirty = true;
        self.history.untracked_dirty = true;
    }

    /// Keeps what undo can't restore of a device that is going away, and of anything in it.
    pub(super) fn park_device(&mut self, d: &mut Device) {
        let runtimes = &mut self.runtimes;
        let parked = &mut self.history.parked_devices;
        d.for_each_mut(&mut |d| {
            if let DeviceKind::Plugin { state, .. } = &mut d.kind {
                if let Some(h) = runtimes.get_mut(&d.id).and_then(|r| r.plugin.as_mut()) {
                    if let Some(s) = h.save_state() {
                        *state = Some(s);
                    }
                }
            }
            parked.insert(d.id, ParkedDevice::of(d));
        });
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
        if self.history.settling > 0 && !finished {
            self.history.settling -= 1;
            return;
        }
        self.history.settling = 0;
        let label = self.history.pending.take().unwrap_or("Edit");
        if self.history.commit(&self.project, label) {
            self.dirty = true;
        }
    }

    /// Commits anything still waiting, for an action that needs the history up to date.
    pub(super) fn commit_now(&mut self) {
        self.history.settling = 0;
        self.commit_edits(false);
    }

    fn after_restore(&mut self) {
        self.history.pending = None;
        self.history.was_busy = false;
        self.history.settling = 0;
        self.dirty = self.history.untracked_dirty || !self.history.at_saved();
    }

    pub fn undo(&mut self) -> bool {
        if self.is_recording() {
            return false;
        }
        self.commit_now();
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
        self.commit_now();
        let Some(target) = self.history.redo_target() else {
            return false;
        };
        self.restore(&target);
        self.after_restore();
        true
    }

    /// Brings the project to `target` by applying only what differs.
    fn restore(&mut self, target: &Snapshot) {
        self.project.scenes = (*target.scenes).clone();
        let keep: HashSet<TrackId> = target.tracks.iter().map(|t| t.shell.id).collect();
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
            if self.project.track(s.shell.id).is_none() {
                let t = self.track_from_snapshot(s);
                self.install_track(t, i);
            }
        }
        for (i, s) in target.tracks.iter().enumerate() {
            if self.project.track_index(s.shell.id) != Some(i) {
                self.move_track(s.shell.id, i);
            }
        }
        for s in &target.tracks {
            self.restore_track(s);
        }
        self.restore_master(&target.master);
        self.restore_returns(&target.returns);
        self.project.arrangement = (*target.arrangement).clone();
        self.restore_settings(&target.shell);
        self.sync_loop();
        self.project.normalize();
        self.update_midi_targets();
    }

    /// What undo doesn't cover about a device, as remembered when it left the project.
    fn parked_of(&self, id: DeviceId) -> Option<ParkedDevice> {
        self.history.parked_devices.get(&id).cloned()
    }

    fn track_from_snapshot(&self, s: &TrackSnap) -> Track {
        let parked = self.history.parked_tracks.get(&s.shell.id);
        s.to_track(&|id| self.parked_of(id), parked)
    }

    fn restore_track(&mut self, s: &TrackSnap) {
        let id = s.shell.id;
        let Some(t) = self.project.track_mut(id) else {
            return;
        };
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
        adopt_track_shell(t, &s.shell);
        t.automation = (*s.automation).clone();
        t.take_lanes = (*s.take_lanes).clone();
        self.push_mix(id, false);
        self.restore_chain(ChainRef::Track(id), &s.devices);
        for slot in changed {
            self.sync_clip(id, slot);
        }
        if lane_changed {
            self.sync_lane(id);
        }
    }

    fn restore_master(&mut self, s: &TrackSnap) {
        let master = &mut self.project.master;
        adopt_track_shell(master, &s.shell);
        master.automation = (*s.automation).clone();
        master.take_lanes = (*s.take_lanes).clone();
        let db = self.project.master.volume_db as f64;
        let unchanged = self
            .layouts
            .get(&ChainRef::Master)
            .is_some_and(|l| l.base(mix_cell::VOLUME) == Some(db));
        if !unchanged {
            self.set_cell(ChainRef::Master, mix_cell::VOLUME, db, false);
        }
        self.restore_chain(ChainRef::Master, &s.devices);
    }

    /// Return tracks have no engine side yet, so this only has to get the project right.
    fn restore_returns(&mut self, target: &[Arc<TrackSnap>]) {
        for t in &self.project.returns {
            self.history.parked_tracks.insert(t.id, ParkedTrack::of(t));
            let parked = &mut self.history.parked_devices;
            for d in &t.devices {
                d.for_each(&mut |d| {
                    parked.insert(d.id, ParkedDevice::of(d));
                });
            }
        }
        self.project.returns = target.iter().map(|s| self.track_from_snapshot(s)).collect();
    }

    fn restore_settings(&mut self, shell: &Project) {
        let Project {
            version: _,
            tempo,
            time_signature,
            quantize,
            metronome,
            scenes: _,
            tracks: _,
            master: _,
            arrangement: _,
            returns: _,
            crossfader,
            groove_pool,
            tuning,
            midi_mappings,
            key_mappings,
        } = shell;
        let mut commands = Vec::new();
        let p = &mut self.project;
        if p.tempo != *tempo {
            commands.push(TransportCmd::SetTempo(*tempo));
        }
        if p.time_signature != *time_signature {
            commands.push(TransportCmd::SetTimeSignature(*time_signature));
        }
        if p.quantize != *quantize {
            commands.push(TransportCmd::SetQuantize(*quantize));
        }
        if p.metronome != *metronome {
            commands.push(TransportCmd::SetMetronome(*metronome));
        }
        p.tempo = *tempo;
        p.time_signature = *time_signature;
        p.quantize = *quantize;
        p.metronome = *metronome;
        p.crossfader = *crossfader;
        p.groove_pool = groove_pool.clone();
        p.tuning = tuning.clone();
        p.midi_mappings = midi_mappings.clone();
        p.key_mappings = key_mappings.clone();
        for c in commands {
            self.send(EngineCommand::Transport(c));
        }
    }

    fn restore_chain(&mut self, chain: ChainRef, target: &[Arc<DeviceSnap>]) {
        let current: Vec<DeviceId> = self.devices(chain).iter().map(|d| d.id).collect();
        let wanted = |id: &DeviceId| target.iter().any(|s| s.shell.id == *id);
        for id in current.iter().filter(|id| !wanted(id)) {
            self.remove_device(chain, *id);
        }
        for (i, s) in target.iter().enumerate() {
            if !current.contains(&s.shell.id) {
                let d = s.to_device(&|id| self.parked_of(id));
                self.add_device(chain, Some(i), d);
            }
        }
        for (i, s) in target.iter().enumerate() {
            if self.devices(chain).get(i).map(|d| d.id) != Some(s.shell.id) {
                self.move_device(chain, s.shell.id, i);
            }
        }
        for s in target {
            self.restore_device(chain, s);
        }
    }

    /// Brings one device that exists on both sides to the target.
    fn restore_device(&mut self, chain: ChainRef, s: &DeviceSnap) {
        let id = s.shell.id;
        let Some(cur) = self.devices(chain).iter().find(|d| d.id == id) else {
            return;
        };
        if std::mem::discriminant(&cur.kind) != std::mem::discriminant(&s.shell.kind) {
            let index = self.devices(chain).iter().position(|d| d.id == id);
            self.remove_device(chain, id);
            let d = s.to_device(&|id| self.parked_of(id));
            self.add_device(chain, index, d);
            return;
        }
        let same_type = match (&cur.kind, &s.shell.kind) {
            (DeviceKind::Builtin { type_id: a, .. }, DeviceKind::Builtin { type_id: b, .. }) => {
                a == b
            }
            (
                DeviceKind::Plugin {
                    format: fa,
                    plugin_id: pa,
                    ..
                },
                DeviceKind::Plugin {
                    format: fb,
                    plugin_id: pb,
                    ..
                },
            ) => fa == fb && pa == pb,
            _ => true,
        };
        let (current_params, bypass) = (cur.params.clone(), cur.bypass);
        let rebuild = !same_type || self.restore_builtin(chain, id, s);
        for (&param, &value) in &s.shell.params {
            if current_params.get(&param) != Some(&value) {
                self.write_param(chain, id, param, value, false);
            }
        }
        for &param in current_params.keys() {
            if s.shell.params.contains_key(&param) {
                continue;
            }
            // It hadn't been moved at the target step, so it goes back to where it started.
            let start = self.runtimes.get(&id).and_then(|rt| {
                let default = rt.params.iter().find(|p| p.id == param).map(|p| p.default);
                rt.unmoved.get(&param).copied().or(default)
            });
            if let Some(value) = start {
                self.write_param(chain, id, param, value, false);
            }
        }
        if bypass != s.shell.bypass {
            self.write_bypass(chain, id, s.shell.bypass, false);
        }
        if let DeviceKind::Rack { .. } = &s.shell.kind {
            self.restore_rack(chain, id, s);
        }
        self.adopt_device_shell(chain, s);
        if rebuild {
            self.rebuild_device(chain, id);
        }
    }

    /// Restores a built-in's samples and state. Returns whether its engine side has to be
    /// rebuilt because they changed in a way a resource swap can't express.
    fn restore_builtin(&mut self, chain: ChainRef, id: DeviceId, s: &DeviceSnap) -> bool {
        let Some(cur) = self.devices(chain).iter().find(|d| d.id == id) else {
            return false;
        };
        let (
            DeviceKind::Builtin {
                samples: now,
                state: now_state,
                ..
            },
            DeviceKind::Builtin {
                samples: want,
                state: want_state,
                ..
            },
        ) = (&cur.kind, &s.shell.kind)
        else {
            return false;
        };
        let rebuild = now_state != want_state
            || (0..now.len().max(want.len())).any(|i| {
                let target = want.get(i).and_then(Option::as_ref);
                target.is_none() && now.get(i).and_then(Option::as_ref).is_some()
            });
        if rebuild {
            return true;
        }
        let changes: Vec<_> = want
            .iter()
            .enumerate()
            .filter(|(i, w)| now.get(*i) != Some(*w))
            .filter_map(|(i, w)| Some((i, w.clone()?)))
            .collect();
        for (slot, file) in changes {
            self.write_device_sample(chain, id, slot, file);
        }
        false
    }

    /// Brings the rack's chains and return chains to the target, keeping those that stay.
    fn restore_rack(&mut self, chain: ChainRef, id: DeviceId, s: &DeviceSnap) {
        let Some(DeviceKind::Rack {
            chains: now,
            returns,
            ..
        }) = self
            .devices(chain)
            .iter()
            .find(|d| d.id == id)
            .map(|d| &d.kind)
        else {
            return;
        };
        let now_ids: Vec<ChainId> = now.iter().map(|c| c.id).collect();
        let now_returns: Vec<ChainId> = returns.iter().map(|c| c.id).collect();
        let same_returns = now_returns
            .iter()
            .copied()
            .eq(s.returns.iter().map(|c| c.shell.id));
        let wanted = |c: &ChainId| s.chains.iter().any(|t| t.shell.id == *c);
        for c in now_ids.iter().filter(|c| !wanted(c)) {
            self.remove_rack_chain(chain, id, *c);
        }
        for (i, t) in s.chains.iter().enumerate() {
            if !now_ids.contains(&t.shell.id) {
                let new = t.to_chain(&|d| self.parked_of(d));
                self.insert_rack_chain(chain, id, i, new);
            }
        }
        self.restore_chains(chain, id, &s.chains);
        if let Some(chains) = self.rack_chains_mut(chain, id) {
            chains.sort_by_key(|c| s.chains.iter().position(|t| t.shell.id == c.id));
        }
        if !same_returns {
            // Return chains can't be added on their own yet, so this rack is rebuilt. Its
            // chains already match the target, and a rebuild keeps them.
            let order: Vec<ChainId> = s.returns.iter().map(|c| c.shell.id).collect();
            let added: Vec<Chain> = s
                .returns
                .iter()
                .filter(|t| !now_returns.contains(&t.shell.id))
                .map(|t| t.to_chain(&|d| self.parked_of(d)))
                .collect();
            if let Some(DeviceKind::Rack { returns, .. }) = self
                .chain_mut(chain)
                .and_then(|v| v.iter_mut().find(|d| d.id == id))
                .map(|d| &mut d.kind)
            {
                returns.retain(|c| order.contains(&c.id));
                returns.extend(added);
                returns.sort_by_key(|c| order.iter().position(|id| *id == c.id));
            }
            self.rebuild_device(chain, id);
        }
        self.restore_chains(chain, id, &s.returns);
    }

    /// Restores the contents of chains both sides have.
    fn restore_chains(&mut self, chain: ChainRef, id: DeviceId, target: &[ChainSnap]) {
        for t in target {
            if let Some(child) = chain.child(id, t.shell.id) {
                self.restore_chain(child, &t.devices);
            }
            self.adopt_chain_shell(chain, id, &t.shell);
        }
    }

    /// Copies the parts of a chain that have no engine side yet.
    fn adopt_chain_shell(&mut self, chain: ChainRef, rack: DeviceId, shell: &Chain) {
        let Some(c) = self
            .chain_mut(chain)
            .and_then(|v| v.iter_mut().find(|d| d.id == rack))
            .and_then(|d| d.chains_mut().find(|c| c.id == shell.id))
        else {
            return;
        };
        let Chain {
            id: _,
            name,
            devices: _,
            zones,
            mix,
            color,
            info,
        } = c;
        *name = shell.name.clone();
        *zones = shell.zones;
        *mix = shell.mix.clone();
        *color = shell.color;
        *info = shell.info.clone();
    }

    /// Makes the project's copy of a device match the target, leaving what undo doesn't cover.
    /// The engine has already been told about anything it needs to know.
    fn adopt_device_shell(&mut self, chain: ChainRef, s: &DeviceSnap) {
        let Some(d) = self
            .chain_mut(chain)
            .and_then(|v| v.iter_mut().find(|d| d.id == s.shell.id))
        else {
            return;
        };
        let Device {
            id: _,
            name,
            kind,
            bypass,
            params,
            expanded: _,
            info,
        } = d;
        *name = s.shell.name.clone();
        *bypass = s.shell.bypass;
        params.clone_from(&s.shell.params);
        *info = s.shell.info.clone();
        match (kind, &s.shell.kind) {
            (
                DeviceKind::Builtin {
                    type_id,
                    samples,
                    state,
                },
                DeviceKind::Builtin {
                    type_id: t,
                    samples: sm,
                    state: st,
                },
            ) => {
                type_id.clone_from(t);
                samples.clone_from(sm);
                state.clone_from(st);
            }
            (
                DeviceKind::Plugin {
                    format,
                    plugin_id,
                    path,
                    state: _,
                },
                DeviceKind::Plugin {
                    format: f,
                    plugin_id: p,
                    path: pa,
                    state: _,
                },
            ) => {
                *format = *f;
                plugin_id.clone_from(p);
                path.clone_from(pa);
            }
            (
                DeviceKind::Rack {
                    kind,
                    chains: _,
                    returns: _,
                    macros,
                    variations,
                },
                DeviceKind::Rack {
                    kind: k,
                    chains: _,
                    returns: _,
                    macros: m,
                    variations: v,
                },
            ) => {
                *kind = *k;
                macros.clone_from(m);
                variations.clone_from(v);
            }
            _ => {}
        }
    }
}

/// Copies everything undo covers about a track, leaving its children and view state alone.
fn adopt_track_shell(t: &mut Track, shell: &Track) {
    let mut next = shell.clone();
    next.arm = t.arm;
    next.ui = take(&mut t.ui);
    next.devices = take(&mut t.devices);
    next.clips = take(&mut t.clips);
    next.arrangement = take(&mut t.arrangement);
    next.automation = take(&mut t.automation);
    next.take_lanes = take(&mut t.take_lanes);
    *t = next;
}
