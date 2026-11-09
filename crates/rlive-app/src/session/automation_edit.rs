//! Automation seen from the lanes: what each track can automate, what its envelopes are, and the
//! edits that clear them or carry them along with moving clips.

use super::automation::CellKey;
use super::{ArrMove, ChainRef, Session};
use rlive_core::param::{chain as chain_controls, mixer, transport};
use rlive_core::param::{EnvTarget, ParamFlags, ParamInfo, ParamOwner, ParamRange, ParamRef};
use rlive_core::{DeviceId, ParamId, TrackId, ACTIVATOR};
use rlive_engine::TrackRef;
use rlive_project::envelope_edit::{copy_range, delete_range, paste_range};
use rlive_project::{Breakpoint, Device, TrackUi};

/// One parameter that can be automated.
#[derive(Debug, Clone)]
pub struct EnvParam {
    pub target: EnvTarget,
    pub info: ParamInfo,
    pub range: ParamRange,
}

/// The parameters of one entry in the Device chooser.
#[derive(Debug, Clone)]
pub struct EnvGroup {
    pub label: String,
    pub device: Option<DeviceId>,
    pub params: Vec<EnvParam>,
}

fn param_target(owner: ParamOwner, id: ParamId) -> EnvTarget {
    EnvTarget::Param(ParamRef { owner, id })
}

/// A track's volume envelope: what its main lane shows until something else is chosen.
pub fn volume_target(chain: ChainRef) -> EnvTarget {
    let owner = match chain {
        ChainRef::Track(t) => ParamOwner::Track(t),
        _ => ParamOwner::Master,
    };
    param_target(owner, mixer::VOLUME)
}

impl Session {
    /// The tempo envelope's scale, which is also how far a mapped MIDI control sweeps it.
    fn tempo_range(&self) -> ParamRange {
        let tempo = &self.project.arrangement.tempo;
        ParamRange {
            min: tempo.min,
            max: tempo.max.max(tempo.min + 1.0),
            skew: 1.0,
            quantized: false,
        }
    }

    fn with_range(&self, owner: ChainRef, target: EnvTarget, info: ParamInfo) -> EnvParam {
        let spec = match target {
            EnvTarget::Param(p) => self
                .layouts
                .get(&owner)
                .and_then(|l| super::automation::cell_of(p, owner, l).and_then(|c| l.spec(c))),
            _ => None,
        };
        let range = spec.map_or_else(|| info.range(), |s| s.range);
        EnvParam {
            target,
            info,
            range,
        }
    }

    /// Everything on a track that can be automated, the mixer first.
    pub fn env_catalog(&self, chain: ChainRef) -> Vec<EnvGroup> {
        let owner = chain.owner();
        let mut groups = vec![self.mixer_group(owner)];
        if let Some(devices) = self.track_devices(owner.into()) {
            self.collect_groups(owner, devices, "", &mut groups);
        }
        groups
    }

    fn mixer_group(&self, owner: ChainRef) -> EnvGroup {
        let mut params = Vec::new();
        match owner {
            ChainRef::Track(t) => {
                for info in mixer::infos() {
                    let id = ParamId(info.id);
                    params.push(self.with_range(
                        owner,
                        param_target(ParamOwner::Track(t), id),
                        info,
                    ));
                }
                for (n, r) in self.project.returns.iter().enumerate() {
                    if r.id == t {
                        continue;
                    }
                    let id = ParamId(mixer::FIRST_SEND.0 + n as u32);
                    let info = ParamInfo::continuous(
                        id.0,
                        &format!("Send {}", (b'A' + n as u8) as char),
                        mixer::VOLUME_MIN_DB,
                        0.0,
                        mixer::VOLUME_MIN_DB,
                        "dB",
                    );
                    params.push(self.with_range(
                        owner,
                        param_target(ParamOwner::Track(t), id),
                        info,
                    ));
                }
            }
            _ => {
                let [volume, ..] = mixer::infos();
                params.push(self.with_range(owner, volume_target(owner), volume));
                let tempo = self.project.tempo;
                let info = ParamInfo::continuous(
                    transport::TEMPO.0,
                    "Song Tempo",
                    self.tempo_range().min,
                    self.tempo_range().max,
                    tempo,
                    "BPM",
                );
                params.push(EnvParam {
                    target: transport::tempo_target(),
                    range: self.tempo_range(),
                    info,
                });
            }
        }
        EnvGroup {
            label: "Mixer".to_string(),
            device: None,
            params,
        }
    }

    fn collect_groups(
        &self,
        owner: ChainRef,
        devices: &[Device],
        path: &str,
        out: &mut Vec<EnvGroup>,
    ) {
        for d in devices {
            let label = if path.is_empty() {
                d.name.clone()
            } else {
                format!("{path} > {}", d.name)
            };
            let device_owner = ParamOwner::Device(d.id);
            let on = ParamInfo::toggle(ACTIVATOR.0, "Device On", true);
            let mut params =
                vec![self.with_range(owner, param_target(device_owner, ACTIVATOR), on)];
            if let Some(rt) = self.runtimes.get(&d.id) {
                // A plug-in offers only what's on its panel, and what its window moved last.
                let offered = super::plugins::plugin_chooser_params(d, rt);
                for info in &rt.params {
                    // What's already automated stays reachable, on the panel or not.
                    let target = param_target(device_owner, ParamId(info.id));
                    if offered.as_ref().is_some_and(|ids| !ids.contains(&info.id))
                        && !self.is_automated(owner, target)
                    {
                        continue;
                    }
                    let blocked = ParamFlags::HIDDEN.0 | ParamFlags::READ_ONLY.0;
                    if info.flags.contains(ParamFlags::AUTOMATABLE) && info.flags.0 & blocked == 0 {
                        let target = param_target(device_owner, ParamId(info.id));
                        params.push(self.with_range(owner, target, info.clone()));
                    }
                }
            }
            out.push(EnvGroup {
                label: label.clone(),
                device: Some(d.id),
                params,
            });
            for chain in d.chains() {
                let params = chain_controls::infos()
                    .into_iter()
                    .take(4)
                    .map(|info| {
                        let target = param_target(ParamOwner::Chain(chain.id), ParamId(info.id));
                        self.with_range(owner, target, info)
                    })
                    .collect();
                let chain_label = format!("{label} > {}", chain.name);
                out.push(EnvGroup {
                    label: chain_label.clone(),
                    device: None,
                    params,
                });
                self.collect_groups(owner, &chain.devices, &chain_label, out);
            }
        }
    }

    /// One parameter's description, if it (still) exists on this track.
    pub fn env_param(&self, chain: ChainRef, target: EnvTarget) -> Option<EnvParam> {
        self.env_catalog(chain)
            .into_iter()
            .flat_map(|g| g.params)
            .find(|p| p.target == target)
    }

    /// An envelope's points, or nothing if the parameter isn't automated.
    pub fn env_points(&self, chain: ChainRef, target: EnvTarget) -> &[Breakpoint] {
        if target == transport::tempo_target() {
            if !self.leader_points.is_empty() {
                return &self.leader_points;
            }
            return &self.project.arrangement.tempo.points;
        }
        self.envelopes(chain.into())
            .and_then(|e| e.iter().find(|e| e.target == target))
            .map_or(&[], |e| e.points.as_slice())
    }

    pub fn is_automated(&self, chain: ChainRef, target: EnvTarget) -> bool {
        !self.env_points(chain, target).is_empty()
    }

    /// What the dashed line of a parameter without automation shows: its own setting.
    pub fn env_base_value(&self, chain: ChainRef, target: EnvTarget) -> Option<f64> {
        if target == transport::tempo_target() {
            return Some(self.project.tempo);
        }
        let EnvTarget::Param(p) = target else {
            return None;
        };
        let owner = chain.owner();
        let layout = self.layouts.get(&owner)?;
        layout.base(super::automation::cell_of(p, owner, layout)?)
    }

    /// A value as the parameter shows it.
    pub fn env_format(&self, chain: ChainRef, target: EnvTarget, value: f64) -> String {
        let Some(param) = self.env_param(chain, target) else {
            return format!("{value:.2}");
        };
        if let EnvTarget::Param(ParamRef {
            owner: ParamOwner::Device(d),
            ..
        }) = target
        {
            if let Some(rt) = self.runtimes.get(&d) {
                return rt.format_value(&param.info, value);
            }
        }
        param.info.format(value)
    }

    /// Every parameter on this track with an envelope, tempo included on the Master.
    pub fn automated_targets(&self, chain: ChainRef) -> Vec<EnvTarget> {
        let owner = chain.owner();
        let mut targets: Vec<EnvTarget> = self
            .envelopes(owner.into())
            .map(|e| {
                e.iter()
                    .filter(|e| !e.points.is_empty())
                    .map(|e| e.target)
                    .collect()
            })
            .unwrap_or_default();
        if owner == ChainRef::Master
            && !self.env_points(owner, transport::tempo_target()).is_empty()
        {
            targets.push(transport::tempo_target());
        }
        targets
    }

    /// Clear Envelope: the parameter keeps the value it has.
    pub fn clear_envelope(&mut self, chain: ChainRef, target: EnvTarget) {
        self.set_envelope(chain, target, Vec::new());
    }

    /// Delete Automation: the envelope goes and the parameter returns to its default.
    pub fn delete_automation(&mut self, chain: ChainRef, target: EnvTarget) {
        let default = self.env_param(chain, target).map(|p| p.info.default);
        self.clear_envelope(chain, target);
        if let Some(default) = default {
            self.reset_control(chain, target, default);
        }
    }

    fn reset_control(&mut self, chain: ChainRef, target: EnvTarget, value: f64) {
        let EnvTarget::Param(p) = target else {
            return;
        };
        match p.owner {
            ParamOwner::Device(d) => {
                if let Some(c) = self.chain_of_device(d) {
                    if p.id == ACTIVATOR {
                        self.set_bypass(c, d, value < 0.5);
                    } else {
                        self.set_param(c, d, p.id.0, value);
                    }
                }
            }
            ParamOwner::Track(t) => self.reset_track_control(t, p.id, value),
            ParamOwner::Master if p.id == mixer::VOLUME => {
                self.project.master.volume_db = value as f32;
                self.set_cell(chain, rlive_engine::mix_cell::VOLUME, value, false);
                self.mark_edit("Change Mixer");
            }
            _ => {}
        }
    }

    fn reset_track_control(&mut self, t: TrackId, id: ParamId, value: f64) {
        let Some(track) = self.project.track_mut(t) else {
            return;
        };
        match id {
            mixer::VOLUME => track.volume_db = value as f32,
            mixer::PAN => track.pan = value as f32,
            mixer::MUTE => track.mute = value >= 0.5,
            mixer::SOLO => track.solo = value >= 0.5,
            ParamId(n) if n >= mixer::FIRST_SEND.0 => {
                let ret = self
                    .project
                    .returns
                    .get((n - mixer::FIRST_SEND.0) as usize)
                    .map(|r| r.id);
                if let Some(ret) = ret {
                    self.set_send(t, ret, value as f32);
                }
                return;
            }
            _ => return,
        }
        self.sync_mix(t);
    }

    /// Clear All Envelopes of one device, including what's nested in it.
    pub fn clear_device_envelopes(&mut self, chain: ChainRef, device: DeviceId) {
        let owner = chain.owner();
        let mut ids = Vec::new();
        if let Some(devices) = self.track_devices(owner.into()) {
            let mut find = |d: &Device| {
                if d.id == device {
                    d.for_each(&mut |d| ids.push(d.id));
                }
            };
            for d in devices {
                d.for_each(&mut find);
            }
        }
        for target in self.automated_targets(owner) {
            let in_device = matches!(target, EnvTarget::Param(ParamRef {
                owner: ParamOwner::Device(d), ..
            }) if ids.contains(&d));
            if in_device {
                self.clear_envelope(owner, target);
            }
        }
    }

    /// Clears a track's envelopes, keeping the values the parameters have.
    pub fn clear_track_envelopes(&mut self, chain: ChainRef) {
        let owner = chain.owner();
        for target in self.automated_targets(owner) {
            self.clear_envelope(owner, target);
        }
    }

    /// Clear All Envelopes: all the automation in the Set.
    pub fn clear_all_envelopes(&mut self) {
        let mut chains: Vec<ChainRef> = self
            .project
            .tracks
            .iter()
            .chain(&self.project.returns)
            .map(|t| ChainRef::Track(t.id))
            .collect();
        chains.push(ChainRef::Master);
        for chain in chains {
            self.clear_track_envelopes(chain);
        }
    }

    /// The range the tempo envelope is drawn in. It is kept at least one BPM wide.
    pub fn set_tempo_range(&mut self, min: f64, max: f64) {
        let min = min.clamp(20.0, 998.0);
        let max = max.clamp(min + 1.0, 999.0);
        let tempo = &mut self.project.arrangement.tempo;
        if (tempo.min, tempo.max) != (min, max) {
            (tempo.min, tempo.max) = (min, max);
            self.mark_edit("Change Tempo Range");
        }
    }

    pub fn set_lock_envelopes(&mut self, on: bool) {
        if self.project.arrangement.lock_envelopes != on {
            self.project.arrangement.lock_envelopes = on;
            self.mark_edit("Lock Envelopes");
        }
    }

    /// The lane layout of a track, which undo leaves alone.
    pub fn track_ui(&self, chain: ChainRef) -> Option<&TrackUi> {
        match chain.owner() {
            ChainRef::Track(t) => self.project.track(t).map(|t| &t.ui),
            _ => Some(&self.project.master.ui),
        }
    }

    pub fn edit_track_ui(&mut self, chain: ChainRef, edit: impl FnOnce(&mut TrackUi)) {
        let ui = match chain.owner() {
            ChainRef::Track(t) => self.project.track_mut(t).map(|t| &mut t.ui),
            _ => Some(&mut self.project.master.ui),
        };
        if let Some(ui) = ui {
            edit(ui);
            self.touch();
        }
    }

    /// The control the user moved last, for the automation lanes to show.
    pub fn take_touched_control(&mut self) -> Option<(ChainRef, EnvTarget)> {
        let (chain, cell) = self.automation.take_touched()?;
        let key = CellKey {
            track: chain.into(),
            cell,
        };
        self.lane_of(key).map(|(target, _)| (chain, target))
    }

    /// Unless envelopes are locked, the automation under moved clips goes with them. On a move
    /// to another track the parameters aren't the same ones, so it stays behind.
    pub(super) fn carry_automation(&mut self, moves: &[(ArrMove, f64, f64)], copy: bool) {
        if self.project.arrangement.lock_envelopes {
            return;
        }
        let mut tracks: Vec<TrackId> = Vec::new();
        for (m, ..) in moves {
            if m.track == m.to_track && !tracks.contains(&m.track) {
                tracks.push(m.track);
            }
        }
        for track in tracks {
            let spans: Vec<(f64, f64, f64)> = moves
                .iter()
                .filter(|(m, ..)| m.track == track && m.to_track == track)
                .filter(|&&(m, start, _)| m.start as f64 != start)
                .map(|&(m, start, end)| (start, end, m.start as f64))
                .collect();
            self.carry_on_track(track, &spans, copy);
        }
    }

    fn carry_on_track(&mut self, track: TrackId, spans: &[(f64, f64, f64)], copy: bool) {
        let chain = ChainRef::Track(track);
        let mut changed = false;
        for target in self.automated_targets(chain) {
            let Some(param) = self.env_param(chain, target) else {
                continue;
            };
            let range = param.range;
            let original = self.env_points(chain, target).to_vec();
            let before = self
                .env_base_value(chain, target)
                .unwrap_or(param.info.default);
            let clips: Vec<_> = spans
                .iter()
                .map(|&(start, end, _)| copy_range(&original, start, end, range))
                .collect();
            let mut points = original.clone();
            if !copy {
                for &(start, end, _) in spans {
                    points = delete_range(&points, start, end, range);
                }
            }
            for (clip, &(.., to)) in clips.iter().zip(spans) {
                points = paste_range(&points, clip, to, before, range);
            }
            if points != original {
                if let Some(e) = self
                    .envelopes_mut(TrackRef::Track(track))
                    .and_then(|e| e.iter_mut().find(|e| e.target == target))
                {
                    e.points = points;
                    changed = true;
                }
            }
        }
        if changed {
            self.send_routes(TrackRef::Track(track));
        }
    }
}
