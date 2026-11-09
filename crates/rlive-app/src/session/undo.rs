//! Undo and redo.
//!
//! Restoring applies only what differs from the target, so everything else keeps playing and no
//! unaffected plugin is touched. Parameter and mixer values go down the parameter path; the
//! engine objects themselves are only touched for what actually came, went or changed kind.

use super::{ChainRef, Session};
use rlive_core::{ChainId, DeviceId, TrackId};
use rlive_engine::GraphOp;
use rlive_engine::{mix_cell, EngineCommand, TrackRef, TransportCmd};
use rlive_project::history::{ChainSnap, DeviceSnap, ParkedDevice, Snapshot, TrackSnap};
use rlive_project::{Chain, Device, DeviceKind, Macro, Project, Track};
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
        // Whatever this edit deleted, mappings to it go in the same undo step.
        self.project.prune_mappings();
        self.mods_dirty |= label != "Change Parameter";
        self.history.pending.get_or_insert(label);
        self.history.settling = 0;
        // Leaders' tempo comes from their clips, which any edit can move.
        if self.project.has_tempo_leader() || !self.leader_points.is_empty() {
            self.sync_time_maps();
        }
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
        self.restoring = false;
        self.mods_dirty = true;
        self.record_fingerprints();
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

    /// The Undo History's steps, oldest first. The first, which can't be undone, says how the
    /// history began (manual p. 136).
    pub fn undo_steps(&self) -> Vec<&'static str> {
        let mut steps = self.history.labels();
        if let Some(first) = steps.first_mut() {
            *first = if self.path.is_some() {
                "Open Set"
            } else {
                "New Set"
            };
        }
        steps
    }

    pub fn undo_position(&self) -> usize {
        self.history.position()
    }

    /// Undoes or redoes to a step of the history. Returns whether anything changed.
    pub fn jump_to_step(&mut self, step: usize) -> bool {
        if self.is_recording() {
            return false;
        }
        self.commit_now();
        let Some(target) = self.history.jump_target(step) else {
            return false;
        };
        self.restore(&target);
        self.after_restore();
        true
    }

    /// Brings the project to `target` by applying only what differs.
    fn restore(&mut self, target: &Snapshot) {
        self.restoring = true;
        let scenes_changed = self.project.scenes != *target.scenes;
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
            self.remove_track(id);
        }
        for (i, s) in target.tracks.iter().enumerate() {
            if self.project.track(s.shell.id).is_none() {
                let t = self.track_from_snapshot(s);
                self.install_track(t, i);
            }
        }
        for (i, s) in target.tracks.iter().enumerate() {
            if self.project.track_index(s.shell.id) != Some(i) {
                self.move_track_exact(s.shell.id, i);
            }
        }
        self.restore_returns(&target.returns);
        for s in target.tracks.iter().chain(&target.returns) {
            self.restore_track(s);
        }
        self.restore_master(&target.master);
        let maps_changed = self.project.arrangement.tempo != target.arrangement.tempo
            || self.project.arrangement.signatures != target.arrangement.signatures;
        self.project.arrangement = (*target.arrangement).clone();
        if maps_changed || self.project.has_tempo_leader() || !self.leader_points.is_empty() {
            self.sync_time_maps();
        }
        self.restore_settings(&target.shell);
        if scenes_changed {
            self.sync_scenes();
        }
        self.send_follow_enabled();
        self.sync_loop();
        self.project.normalize();
        self.update_midi_targets();
        // Group, send and return changes all end up here: the sends' positions settle last.
        self.push_all_sends();
        self.send_track_edits(Vec::new());
        let tracks: Vec<TrackId> = self.project.tracks.iter().map(|t| t.id).collect();
        for track in tracks {
            self.send_midi_output(track);
        }
        self.refresh_wiring();
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
        // Freezing changes what a clip plays without changing the clip.
        let frozen_changed = t.is_frozen() != s.shell.is_frozen();
        if frozen_changed {
            changed.extend(0..slots);
        }
        let lane_changed = frozen_changed
            || t.arrangement.len() != s.lane.len()
            || t.arrangement.iter().zip(&s.lane).any(|(a, b)| *a != **b);
        if lane_changed {
            t.arrangement = s.lane.iter().map(|c| (**c).clone()).collect();
        }
        let no_stop = t.no_stop.clone();
        adopt_track_shell(t, &s.shell);
        changed.extend(no_stop.iter().map(|s| *s as usize));
        changed.extend(t.no_stop.iter().map(|s| *s as usize));
        let automation_changed = t.automation != *s.automation;
        t.automation = (*s.automation).clone();
        t.take_lanes = (*s.take_lanes).clone();
        self.push_mix(id, false);
        if frozen_changed {
            let frozen = s.shell.is_frozen();
            self.send_ops(vec![GraphOp::SetFrozen { track: id, frozen }]);
        }
        self.restore_chain(ChainRef::Track(id), &s.devices);
        if automation_changed {
            self.send_routes(TrackRef::Track(id));
        }
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
        let automation_changed = master.automation != *s.automation;
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
        let crossfader = self.project.crossfader.position as f64;
        let cue_db = self.project.cue.volume_db as f64;
        for (cell, value) in [
            (mix_cell::CROSSFADER, crossfader),
            (mix_cell::CUE_VOLUME, cue_db),
        ] {
            let unchanged = self
                .layouts
                .get(&ChainRef::Master)
                .is_some_and(|l| l.base(cell) == Some(value));
            if !unchanged {
                self.set_cell(ChainRef::Master, cell, value, false);
            }
        }
        self.restore_chain(ChainRef::Master, &s.devices);
        if automation_changed {
            self.send_routes(TrackRef::Master);
        }
    }

    /// Brings the list of return tracks to the target: what is gone is removed, what came back
    /// is rebuilt, and the rest moves into place. What is in them follows in `restore_track`.
    fn restore_returns(&mut self, target: &[Arc<TrackSnap>]) {
        let keep: HashSet<TrackId> = target.iter().map(|t| t.shell.id).collect();
        let gone: Vec<TrackId> = self
            .project
            .returns
            .iter()
            .map(|t| t.id)
            .filter(|id| !keep.contains(id))
            .collect();
        for id in gone {
            self.remove_track(id);
        }
        for (i, s) in target.iter().enumerate() {
            if self.project.return_index(s.shell.id).is_none() {
                let t = self.track_from_snapshot(s);
                self.install_return(t, i);
            }
        }
        let tracks = self.project.tracks.len();
        for (i, s) in target.iter().enumerate() {
            if self.project.return_index(s.shell.id) != Some(i) {
                let id = s.shell.id;
                let at = self.project.return_index(id).unwrap_or(i);
                let t = self.project.returns.remove(at);
                self.project.returns.insert(i, t);
                self.send_track_edits(vec![rlive_engine::GraphOp::MoveTrack {
                    track: id,
                    to: tracks + i,
                }]);
            }
        }
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
            cue,
            groove_pool,
            tuning,
            midi_mappings,
            key_mappings,
            surface_locks,
            solo_in_place,
            delay_compensation,
            follow_actions_on,
            reduced_latency_when_monitoring,
            metronome_options,
            record_quantize,
            midi_arrangement_overdub,
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
        p.cue = cue.clone();
        let grooves_changed = p.groove_pool != *groove_pool;
        let tuning_changed = p.tuning != *tuning;
        p.groove_pool = groove_pool.clone();
        p.tuning = tuning.clone();
        p.midi_mappings = midi_mappings.clone();
        p.key_mappings = key_mappings.clone();
        p.surface_locks = surface_locks.clone();
        p.solo_in_place = *solo_in_place;
        p.delay_compensation = *delay_compensation;
        p.follow_actions_on = *follow_actions_on;
        p.reduced_latency_when_monitoring = *reduced_latency_when_monitoring;
        p.metronome_options = *metronome_options;
        p.record_quantize = *record_quantize;
        p.midi_arrangement_overdub = *midi_arrangement_overdub;
        for c in commands {
            self.send(EngineCommand::Transport(c));
        }
        if grooves_changed {
            self.send_grooves();
        }
        if tuning_changed {
            self.send_tuning();
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
        let options_were = match self
            .devices(chain)
            .iter()
            .find(|d| d.id == id)
            .map(|d| &d.kind)
        {
            Some(DeviceKind::Rack {
                select_filters_ctrl,
                ..
            }) => Some(*select_filters_ctrl),
            _ => None,
        };
        let macros_changed = if let DeviceKind::Rack { .. } = &s.shell.kind {
            self.restore_rack(chain, id, s);
            self.restore_macro_values(chain, id, s)
        } else {
            false
        };
        self.adopt_device_shell(chain, s);
        if let DeviceKind::Rack { .. } = &s.shell.kind {
            self.after_rack_restored(chain, id, macros_changed, options_were);
        }
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

    /// Brings a rack's macro knobs to the target. Returns whether its macros differ at all.
    fn restore_macro_values(&mut self, chain: ChainRef, id: DeviceId, s: &DeviceSnap) -> bool {
        let DeviceKind::Rack { macros: want, .. } = &s.shell.kind else {
            return false;
        };
        let Some(DeviceKind::Rack { macros: now, .. }) = self
            .devices(chain)
            .iter()
            .find(|d| d.id == id)
            .map(|d| &d.kind)
        else {
            return false;
        };
        let changed = now != want;
        let current: Vec<f64> = now.iter().map(|m: &Macro| m.value).collect();
        for (n, value) in want.iter().map(|m| m.value).enumerate() {
            if current.get(n).copied().unwrap_or(0.0) != value {
                self.write_param(chain, id, n as u32, value, false);
            }
        }
        changed
    }

    /// Everything a restored rack still has to tell the engine, now its shell matches the target.
    fn after_rack_restored(
        &mut self,
        chain: ChainRef,
        id: DeviceId,
        macros_changed: bool,
        options_were: Option<bool>,
    ) {
        if macros_changed {
            self.send_routes(chain.into());
        }
        let filters = match self
            .devices(chain)
            .iter()
            .find(|d| d.id == id)
            .map(|d| &d.kind)
        {
            Some(DeviceKind::Rack {
                select_filters_ctrl,
                ..
            }) => *select_filters_ctrl,
            _ => false,
        };
        if options_were != Some(filters) {
            self.send_edits(vec![GraphOp::SetRackOptions {
                target: chain.into(),
                device: id,
                select_filters_ctrl: filters,
            }]);
        }
        self.push_rack_mix(chain, id);
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
        let wanted = |c: &ChainId| s.chains.iter().any(|t| t.shell.id == *c);
        let return_wanted = |c: &ChainId| s.returns.iter().any(|t| t.shell.id == *c);
        for c in now_ids.iter().filter(|c| !wanted(c)) {
            self.remove_rack_chain(chain, id, *c);
        }
        for c in now_returns.iter().filter(|c| !return_wanted(c)) {
            self.remove_rack_chain(chain, id, *c);
        }
        for (i, t) in s.chains.iter().enumerate() {
            if !now_ids.contains(&t.shell.id) {
                let new = t.to_chain(&|d| self.parked_of(d));
                self.insert_rack_chain(chain, id, i, new);
            }
        }
        for (i, t) in s.returns.iter().enumerate() {
            if !now_returns.contains(&t.shell.id) {
                let new = t.to_chain(&|d| self.parked_of(d));
                self.insert_return_chain(chain, id, i, new);
            }
        }
        self.restore_chains(chain, id, &s.chains);
        self.restore_chains(chain, id, &s.returns);
        if let Some(DeviceKind::Rack {
            chains, returns, ..
        }) = self
            .chain_mut(chain)
            .and_then(|v| v.iter_mut().find(|d| d.id == id))
            .map(|d| &mut d.kind)
        {
            chains.sort_by_key(|c| s.chains.iter().position(|t| t.shell.id == c.id));
            returns.sort_by_key(|c| s.returns.iter().position(|t| t.shell.id == c.id));
        }
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

    /// Copies a chain's settings, telling the engine about any whose zones or routing changed.
    fn adopt_chain_shell(&mut self, chain: ChainRef, rack: DeviceId, shell: &Chain) {
        let Some(c) = self
            .chain_mut(chain)
            .and_then(|v| v.iter_mut().find(|d| d.id == rack))
            .and_then(|d| d.chains_mut().find(|c| c.id == shell.id))
        else {
            return;
        };
        let zones_changed = c.zones != shell.zones;
        let output_changed = c.mix.output != shell.mix.output;
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
        if zones_changed {
            let zones = super::build::engine_zones(&shell.zones);
            self.send_edits(vec![GraphOp::SetChainZones {
                target: chain.into(),
                device: rack,
                chain: shell.id,
                zones,
            }]);
        }
        if output_changed {
            self.send_track_edits(vec![GraphOp::SetReturnOutput {
                target: chain.into(),
                device: rack,
                chain: shell.id,
                output: shell.mix.output,
            }]);
        }
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
            sidechain,
            mpe,
            compare,
            modulations,
            external,
        } = d;
        *name = s.shell.name.clone();
        *bypass = s.shell.bypass;
        params.clone_from(&s.shell.params);
        *info = s.shell.info.clone();
        *sidechain = s.shell.sidechain;
        *mpe = s.shell.mpe;
        compare.clone_from(&s.shell.compare);
        modulations.clone_from(&s.shell.modulations);
        external.clone_from(&s.shell.external);
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
                    panel,
                    bank: _,
                },
                DeviceKind::Plugin {
                    format: f,
                    plugin_id: p,
                    path: pa,
                    state: _,
                    panel: pn,
                    bank: _,
                },
            ) => {
                *format = *f;
                plugin_id.clone_from(p);
                path.clone_from(pa);
                panel.clone_from(pn);
            }
            (
                DeviceKind::Rack {
                    kind,
                    chains: _,
                    returns: _,
                    macros,
                    visible_macros,
                    select_filters_ctrl,
                    variations,
                },
                DeviceKind::Rack {
                    kind: k,
                    chains: _,
                    returns: _,
                    macros: m,
                    visible_macros: vm,
                    select_filters_ctrl: sf,
                    variations: v,
                },
            ) => {
                *kind = *k;
                macros.clone_from(m);
                *visible_macros = *vm;
                *select_filters_ctrl = *sf;
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
