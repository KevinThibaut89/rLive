//! What a plug-in is on top of a device: its Configure Mode panel, X-Y field and preset bank,
//! and its window (manual pp. 481-488).

use super::{ChainRef, ClipRef, Session};
use rlive_core::param::{plugin_sidechain, EnvTarget, ParamOwner, ParamRef};
use rlive_core::{DeviceId, ParamId, PluginFormat};
use rlive_engine::TrackRef;
use rlive_plugin_host::vst3;
use rlive_project::{
    Device, DeviceKind, EnvMode, MapTarget, PluginBank, PluginPanel, PLUGIN_BANK_SIZE,
};
use std::collections::HashSet;
use std::path::Path;

/// A plug-in with more parameters than this opens with an empty panel, to be configured.
pub const MAX_DEFAULT_PANEL: usize = 64;

/// How often to check whether a temporary chooser entry has been put to use.
const SETTLE_FRAMES: u32 = 8;

/// Where a parameter is in use, so that taking it off the panel can warn about losing it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ParamUsage {
    pub automation: bool,
    pub clip_envelope: bool,
    pub midi_or_key: bool,
    pub macro_mapped: bool,
}

impl ParamUsage {
    pub fn any(&self) -> bool {
        self.automation || self.clip_envelope || self.midi_or_key || self.macro_mapped
    }
}

/// The Plug-Ins Settings for windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PluginWindowPrefs {
    /// Open a plug-in's window when it is loaded from the browser.
    pub auto_open: bool,
    /// Keep the others open when opening one.
    pub multiple: bool,
    /// Only show the windows of the selected track.
    pub auto_hide: bool,
}

/// What the session remembers about plug-in windows and panels between edits.
#[derive(Debug, Default)]
pub struct PluginUi {
    pub prefs: PluginWindowPrefs,
    /// The Hide Plug-In Windows command.
    pub windows_hidden: bool,
    /// The plug-in whose panel is being configured.
    pub configuring: Option<DeviceId>,
    /// Moved in plug-in editors while recording, for the panel once recording stops.
    recorded: HashSet<(DeviceId, u32)>,
    /// Counts frames, so the check for a temporary entry in use needn't happen on every one.
    settle_tick: u32,
    /// What a plug-in's window moved last, for Macro Map Mode, which the UI runs.
    last_touch: Option<(ChainRef, ParamRef)>,
    selected: Option<ChainRef>,
}

/// The parameters of a plug-in that can go on its panel, in the plug-in's order.
fn panel_candidates(params: &[rlive_core::ParamInfo]) -> impl Iterator<Item = u32> + '_ {
    params
        .iter()
        .filter(|p| {
            !plugin_sidechain::contains(p.id)
                && !p.flags.contains(rlive_core::ParamFlags::HIDDEN)
                && !p.flags.contains(rlive_core::ParamFlags::READ_ONLY)
        })
        .map(|p| p.id)
}

/// What a plug-in's panel shows, in order, given everything it publishes.
pub fn panel_params(panel: &PluginPanel, params: &[rlive_core::ParamInfo]) -> Vec<u32> {
    match &panel.params {
        Some(list) => {
            let known: HashSet<u32> = panel_candidates(params).collect();
            list.iter()
                .copied()
                .filter(|id| known.contains(id))
                .collect()
        }
        None => {
            let all: Vec<u32> = panel_candidates(params).collect();
            if all.len() > MAX_DEFAULT_PANEL {
                Vec::new()
            } else {
                all
            }
        }
    }
}

impl Session {
    fn plugin_kind(&self, chain: ChainRef, id: DeviceId) -> Option<&DeviceKind> {
        let d = self.chain(chain)?.iter().find(|d| d.id == id)?;
        matches!(d.kind, DeviceKind::Plugin { .. }).then_some(&d.kind)
    }

    fn plugin_panel(&self, chain: ChainRef, id: DeviceId) -> Option<&PluginPanel> {
        match self.plugin_kind(chain, id)? {
            DeviceKind::Plugin { panel, .. } => Some(panel),
            _ => None,
        }
    }

    fn with_panel(&mut self, chain: ChainRef, id: DeviceId, f: impl FnOnce(&mut PluginPanel)) {
        if let Some(DeviceKind::Plugin { panel, .. }) = self
            .chain_mut(chain)
            .and_then(|v| v.iter_mut().find(|d| d.id == id))
            .map(|d| &mut d.kind)
        {
            f(panel);
        }
    }

    /// The parameters on a plug-in's panel, in order.
    pub fn shown_params(&self, chain: ChainRef, id: DeviceId) -> Vec<u32> {
        let (Some(panel), Some(rt)) = (self.plugin_panel(chain, id), self.runtimes.get(&id)) else {
            return Vec::new();
        };
        panel_params(panel, &rt.params)
    }

    /// Enter or leave Configure Mode for a plug-in.
    pub fn toggle_configure(&mut self, id: DeviceId) {
        let plugins = &mut self.plugin_ui;
        plugins.configuring = (plugins.configuring != Some(id)).then_some(id);
    }

    pub fn is_configuring(&self, id: DeviceId) -> bool {
        self.plugin_ui.configuring == Some(id)
    }

    /// Put a parameter on the panel, after the last one. Does nothing if it's already there.
    pub fn add_to_panel(&mut self, chain: ChainRef, id: DeviceId, param: u32) {
        if plugin_sidechain::contains(param) {
            return;
        }
        let shown = self.shown_params(chain, id);
        if shown.contains(&param) {
            return;
        }
        self.with_panel(chain, id, |panel| {
            let mut list = shown;
            list.push(param);
            panel.params = Some(list);
        });
        self.mark_edit("Configure Plug-In");
    }

    /// Move a parameter on the panel from position `from` to `to`.
    pub fn move_in_panel(&mut self, chain: ChainRef, id: DeviceId, from: usize, to: usize) {
        let mut list = self.shown_params(chain, id);
        if from >= list.len() || from == to {
            return;
        }
        let param = list.remove(from);
        list.insert(to.min(list.len()), param);
        self.with_panel(chain, id, |panel| panel.params = Some(list));
        self.mark_edit("Configure Plug-In");
    }

    /// Where a parameter is in use. Asked before it's taken off the panel.
    pub fn param_usage(&self, chain: ChainRef, id: DeviceId, param: u32) -> ParamUsage {
        let target = ParamRef {
            owner: ParamOwner::Device(id),
            id: ParamId(param),
        };
        let env = EnvTarget::Param(target);
        let track = chain.into();
        let automation = self
            .envelopes(track)
            .is_some_and(|v| v.iter().any(|e| e.target == env));
        let (slots, arrangement) = self.track_clips(track);
        let clip_envelope = slots
            .iter()
            .flatten()
            .chain(arrangement.iter().map(|c| &c.clip))
            .any(|c| c.envelopes.iter().any(|e| e.envelope.target == env));
        let map = MapTarget::Param(target);
        let midi_or_key = self.project.midi_mappings.iter().any(|m| m.target == map)
            || self.project.key_mappings.iter().any(|m| m.target == map);
        ParamUsage {
            automation,
            clip_envelope,
            midi_or_key,
            macro_mapped: self.is_macro_mapped(chain, target),
        }
    }

    /// Whether a parameter can leave the panel. A plug-in with no window of its own has nothing
    /// else to show its parameters with, so its panel can be reordered but not cut down.
    pub fn can_remove_from_panel(&self, id: DeviceId) -> bool {
        self.runtimes
            .get(&id)
            .and_then(|r| r.plugin.as_ref())
            .is_some_and(|h| h.has_editor())
    }

    /// Take a parameter off the panel, along with its automation, clip envelopes and mappings.
    /// Asked for only after [`param_usage`](Self::param_usage) has been offered to the user.
    pub fn remove_from_panel(&mut self, chain: ChainRef, id: DeviceId, param: u32) {
        if !self.can_remove_from_panel(id) {
            return;
        }
        let target = ParamRef {
            owner: ParamOwner::Device(id),
            id: ParamId(param),
        };
        let usage = self.param_usage(chain, id, param);
        let env = EnvTarget::Param(target);
        if usage.automation {
            self.clear_envelope(chain, env);
        }
        if usage.clip_envelope {
            self.remove_clip_envelopes(chain, env);
        }
        if usage.midi_or_key {
            let map = MapTarget::Param(target);
            self.project.midi_mappings.retain(|m| m.target != map);
            self.project.key_mappings.retain(|m| m.target != map);
            self.publish_remote_table();
        }
        if usage.macro_mapped {
            self.unmap_macros_of(chain, target);
        }
        let mut list = self.shown_params(chain, id);
        list.retain(|p| *p != param);
        self.with_panel(chain, id, |panel| {
            panel.params = Some(list);
            for slot in panel.xy.iter_mut().filter(|s| **s == Some(param)) {
                *slot = None;
            }
        });
        self.mark_edit("Configure Plug-In");
    }

    fn remove_clip_envelopes(&mut self, chain: ChainRef, env: EnvTarget) {
        let track = chain.into();
        let refs = self.clips_with_envelope(track, env);
        for (clip, mode) in refs {
            self.set_clip_envelope(clip, env, mode, Vec::new());
        }
    }

    /// The clips of a track holding an envelope for `env`, and which kind it is.
    fn clips_with_envelope(&self, track: TrackRef, env: EnvTarget) -> Vec<(ClipRef, EnvMode)> {
        let TrackRef::Track(id) = track else {
            return Vec::new();
        };
        let (slots, arrangement) = self.track_clips(track);
        let modes_of = |c: &rlive_project::Clip| -> Vec<EnvMode> {
            c.envelopes
                .iter()
                .filter(|e| e.envelope.target == env)
                .map(|e| e.mode)
                .collect()
        };
        let mut out = Vec::new();
        for (slot, clip) in slots.iter().enumerate() {
            if let Some(clip) = clip {
                out.extend(
                    modes_of(clip)
                        .into_iter()
                        .map(|m| (ClipRef::slot(id, slot), m)),
                );
            }
        }
        for a in arrangement {
            out.extend(modes_of(&a.clip).into_iter().map(|m| {
                (
                    ClipRef::Arr {
                        track: id,
                        id: a.id,
                    },
                    m,
                )
            }));
        }
        out
    }

    /// Takes a parameter out of every Macro on its track.
    fn unmap_macros_of(&mut self, chain: ChainRef, target: ParamRef) {
        fn walk(list: &mut [Device], target: ParamRef) {
            for d in list.iter_mut() {
                d.for_each_mut(&mut |d| {
                    if let DeviceKind::Rack { macros, .. } = &mut d.kind {
                        for m in macros.iter_mut() {
                            m.mappings.retain(|map| map.target != target);
                        }
                    }
                });
            }
        }
        let owner = chain.owner();
        if let Some(devices) = self.chain_mut(owner) {
            walk(devices, target);
        }
        self.send_routes(owner.into());
    }

    /// The X or Y parameter of the panel's X-Y field. Choosing one makes it permanent, if it was
    /// only a temporary entry (manual p. 484).
    pub fn set_xy_param(&mut self, chain: ChainRef, id: DeviceId, axis: usize, param: Option<u32>) {
        if axis >= 2 {
            return;
        }
        self.with_panel(chain, id, |panel| panel.xy[axis] = param);
        if let Some(param) = param {
            if self.runtimes.get(&id).and_then(|r| r.touched) == Some(param) {
                self.add_to_panel(chain, id, param);
            }
        }
        self.mark_edit("Change X-Y");
    }

    /// What the plug-in's X-Y field is set to.
    pub fn xy_params(&self, chain: ChainRef, id: DeviceId) -> [Option<u32>; 2] {
        self.plugin_panel(chain, id).map_or([None, None], |p| p.xy)
    }

    /// A plug-in's own window moved a parameter. In Configure Mode that adds it to the panel, in
    /// a mapping mode it does too and selects it, and otherwise it's offered for a moment in the
    /// choosers (manual pp. 483-484).
    pub(super) fn note_editor_touch(&mut self, chain: ChainRef, id: DeviceId, param: u32) {
        if plugin_sidechain::contains(param) {
            return;
        }
        if let Some(rt) = self.runtimes.get_mut(&id) {
            rt.touched = Some(param);
        }
        if self.is_recording() {
            self.plugin_ui.recorded.insert((id, param));
        }
        let target = ParamRef {
            owner: ParamOwner::Device(id),
            id: ParamId(param),
        };
        self.plugin_ui.last_touch = Some((chain, target));
        if self.is_configuring(id) {
            self.add_to_panel(chain, id, param);
        } else if self.map_mode().is_some() {
            self.add_to_panel(chain, id, param);
            self.select_map_target(Some(MapTarget::Param(target)));
        }
    }

    /// What was just moved in a plug-in's window, if nothing has asked yet.
    pub fn take_last_editor_touch(&mut self) -> Option<(ChainRef, ParamRef)> {
        self.plugin_ui.last_touch.take()
    }

    /// The temporary chooser entry of each plug-in, made permanent by being put to use.
    pub(super) fn settle_plugin_panels(&mut self) {
        let recording = self.is_recording();
        if !recording && !self.plugin_ui.recorded.is_empty() {
            let recorded: Vec<_> = std::mem::take(&mut self.plugin_ui.recorded)
                .into_iter()
                .collect();
            for (id, param) in recorded {
                if let Some(chain) = self.chain_of_device(id) {
                    self.add_to_panel(chain, id, param);
                }
            }
        }
        // That means walking the track's clips, so only some of the time.
        self.plugin_ui.settle_tick = self.plugin_ui.settle_tick.wrapping_add(1);
        if !self.plugin_ui.settle_tick.is_multiple_of(SETTLE_FRAMES) {
            return;
        }
        let touched: Vec<(DeviceId, u32)> = self
            .runtimes
            .iter()
            .filter_map(|(id, r)| Some((*id, r.touched?)))
            .collect();
        for (id, param) in touched {
            let Some(chain) = self.chain_of_device(id) else {
                continue;
            };
            let usage = self.param_usage(chain, id, param);
            if usage.automation || usage.clip_envelope {
                self.add_to_panel(chain, id, param);
                if let Some(rt) = self.runtimes.get_mut(&id) {
                    rt.touched = None;
                }
            }
        }
    }

    #[cfg(test)]
    pub(super) fn note_recorded_param_for_test(&mut self, id: DeviceId, param: u32) {
        self.plugin_ui.recorded.insert((id, param));
    }

    /// An envelope chooser settled on `target`. Picking anything but the temporary entries
    /// there were makes them permanent (manual p. 484).
    pub fn envelope_chooser_picked(&mut self, target: Option<EnvTarget>) {
        let picked = |id: DeviceId, param: u32| match target {
            Some(EnvTarget::Param(r)) => {
                r.owner == ParamOwner::Device(id) && r.id == ParamId(param)
            }
            _ => false,
        };
        let touched: Vec<(DeviceId, u32)> = self
            .runtimes
            .iter()
            .filter_map(|(id, r)| Some((*id, r.touched?)))
            .filter(|(id, param)| !picked(*id, *param))
            .collect();
        for (id, param) in touched {
            if let Some(chain) = self.chain_of_device(id) {
                self.add_to_panel(chain, id, param);
            }
            if let Some(rt) = self.runtimes.get_mut(&id) {
                rt.touched = None;
            }
        }
    }

    // ------------------------------------------------------------- windows

    pub fn set_window_prefs(&mut self, prefs: PluginWindowPrefs) {
        self.plugin_ui.prefs = prefs;
    }

    /// Open a plug-in's window. Without Multiple Plug-In Windows, and unless `keep_others`, the
    /// others close.
    pub fn open_plugin_window(&mut self, id: DeviceId, keep_others: bool) -> Result<(), String> {
        if !self.plugin_ui.prefs.multiple && !keep_others {
            self.close_plugin_windows_except(id);
        }
        self.plugin_ui.windows_hidden = false;
        let title = self.window_title(id);
        let rt = self.runtimes.get_mut(&id).ok_or("no such plug-in")?;
        let handle = rt.plugin.as_mut().ok_or("not a plug-in")?;
        if let Some(title) = title {
            handle.set_editor_title(&title);
            rt.window_title = title;
        }
        handle.open_editor().map_err(|e| e.to_string())?;
        rt.window_visible = true;
        Ok(())
    }

    pub fn close_plugin_window(&mut self, id: DeviceId) {
        if let Some(h) = self.runtimes.get_mut(&id).and_then(|r| r.plugin.as_mut()) {
            h.close_editor();
        }
    }

    fn close_plugin_windows_except(&mut self, keep: DeviceId) {
        for (id, rt) in self.runtimes.iter_mut() {
            if *id != keep {
                if let Some(h) = rt.plugin.as_mut() {
                    h.close_editor();
                }
            }
        }
    }

    /// Hide all open plug-in windows, or bring them back.
    pub fn toggle_plugin_windows(&mut self) {
        self.plugin_ui.windows_hidden = !self.plugin_ui.windows_hidden;
        self.apply_window_visibility();
    }

    pub fn plugin_windows_hidden(&self) -> bool {
        self.plugin_ui.windows_hidden
    }

    /// Which chain's plug-in windows Auto-Hide keeps on screen. Call when the selection changes.
    pub fn set_selected_chain(&mut self, chain: Option<ChainRef>) {
        let owner = chain.map(|c| c.owner());
        if self.plugin_ui.selected != owner {
            self.plugin_ui.selected = owner;
            self.apply_window_visibility();
        }
    }

    /// Show or hide open windows to match the settings, and keep their titles up to date.
    pub(super) fn apply_window_visibility(&mut self) {
        let open: Vec<DeviceId> = self
            .runtimes
            .iter()
            .filter(|(_, r)| r.plugin.as_ref().is_some_and(|h| h.is_editor_open()))
            .map(|(id, _)| *id)
            .collect();
        let UiView {
            hidden,
            auto_hide,
            selected,
        } = UiView::of(&self.plugin_ui);
        for id in open {
            let owner = self.chain_of_device(id).map(|c| c.owner());
            let visible = !hidden && (!auto_hide || owner == selected || selected.is_none());
            let title = self.window_title(id);
            if let Some(rt) = self.runtimes.get_mut(&id) {
                if let (Some(title), Some(h)) = (title, rt.plugin.as_mut()) {
                    if title != rt.window_title {
                        h.set_editor_title(&title);
                        rt.window_title = title;
                    }
                }
                if rt.window_visible != visible {
                    rt.window_visible = visible;
                    if let Some(h) = rt.plugin.as_mut() {
                        h.set_editor_visible(visible);
                    }
                }
            }
        }
    }

    /// A window says which track its plug-in is on (manual p. 482).
    fn window_title(&self, id: DeviceId) -> Option<String> {
        let chain = self.chain_of_device(id)?;
        let device = self.chain(chain)?.iter().find(|d| d.id == id)?;
        let track = match chain.owner() {
            ChainRef::Track(t) => self.project.track(t)?.name.clone(),
            _ => "Master".to_string(),
        };
        Some(format!("{track}: {}", device.name))
    }

    // ------------------------------------------------------------- preset bank

    fn with_bank<R>(
        &mut self,
        chain: ChainRef,
        id: DeviceId,
        f: impl FnOnce(&mut PluginBank) -> R,
    ) -> Option<R> {
        match &mut self.chain_mut(chain)?.iter_mut().find(|d| d.id == id)?.kind {
            DeviceKind::Plugin { bank, .. } => Some(f(bank)),
            _ => None,
        }
    }

    pub fn plugin_bank(&self, chain: ChainRef, id: DeviceId) -> Option<&PluginBank> {
        match self.plugin_kind(chain, id)? {
            DeviceKind::Plugin { bank, .. } => Some(bank),
            _ => None,
        }
    }

    /// Whether this plug-in has a bank of its own. Only VST does (manual p. 488).
    pub fn has_plugin_bank(&self, chain: ChainRef, id: DeviceId) -> bool {
        matches!(
            self.plugin_kind(chain, id),
            Some(DeviceKind::Plugin {
                format: PluginFormat::Vst3,
                ..
            })
        )
    }

    /// Reads the plug-in's state into the preset being worked in.
    fn store_current_preset(&mut self, chain: ChainRef, id: DeviceId) {
        let state = self
            .runtimes
            .get_mut(&id)
            .and_then(|r| r.plugin.as_mut())
            .and_then(|h| h.save_state());
        if let Some(state) = state {
            self.with_bank(chain, id, |b| {
                let current = b.current;
                b.slot(current).state = Some(state);
            });
        }
    }

    /// Switch to another preset of the bank. All changes so far stay in the one being left.
    pub fn select_plugin_preset(&mut self, chain: ChainRef, id: DeviceId, index: usize) {
        if index >= PLUGIN_BANK_SIZE {
            return;
        }
        self.store_current_preset(chain, id);
        let state = self.with_bank(chain, id, |b| {
            b.current = index;
            b.slot(index).state.clone()
        });
        if let Some(state) = state.flatten() {
            self.load_plugin_state(chain, id, &state);
        }
        self.mark_edit("Change Plug-In Preset");
    }

    /// Replace what the plug-in is doing with `state`, as if its values had always been these.
    fn load_plugin_state(&mut self, chain: ChainRef, id: DeviceId, state: &[u8]) -> bool {
        let Some(h) = self.runtimes.get_mut(&id).and_then(|r| r.plugin.as_mut()) else {
            return false;
        };
        if !h.load_state(state) {
            return false;
        }
        self.after_state_load(chain, id);
        true
    }

    /// After a state load the plug-in's own values are right and the project's stale.
    fn after_state_load(&mut self, chain: ChainRef, id: DeviceId) {
        if let Some(d) = self
            .chain_mut(chain)
            .and_then(|v| v.iter_mut().find(|d| d.id == id))
        {
            d.params.retain(|p, _| plugin_sidechain::contains(*p));
        }
        let values: Vec<(u32, f64)> = self
            .runtimes
            .get_mut(&id)
            .map(|rt| {
                if let Some(h) = rt.plugin.as_mut() {
                    rt.params = h.params();
                    if rt.sidechain_input {
                        rt.params
                            .extend(rlive_plugin_host::keyed::sidechain_params());
                    }
                }
                rt.params
                    .iter()
                    .filter(|p| !plugin_sidechain::contains(p.id))
                    .filter_map(|p| {
                        let h = rt.plugin.as_ref()?;
                        Some((p.id, h.param_value(p.id)?))
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.refit_device_cells(chain, id);
        if let Some(l) = self.layouts.get_mut(&chain.owner()) {
            for (param, value) in values {
                if let Some(cell) = l.device_cell(id, param) {
                    l.note_value(cell, value);
                }
            }
        }
    }

    /// Load one of the presets an Audio Unit brought with it.
    pub fn load_unit_preset(&mut self, chain: ChainRef, id: DeviceId, index: usize) -> bool {
        let Some(rt) = self.runtimes.get_mut(&id) else {
            return false;
        };
        let (Some(preset), Some(h)) = (rt.presets.get(index).cloned(), rt.plugin.as_mut()) else {
            return false;
        };
        if !h.load_preset(&preset) {
            return false;
        }
        self.after_state_load(chain, id);
        self.mark_edit("Load Plug-In Preset");
        true
    }

    /// Name one of the bank's presets.
    pub fn rename_plugin_preset(
        &mut self,
        chain: ChainRef,
        id: DeviceId,
        index: usize,
        name: String,
    ) {
        if index >= PLUGIN_BANK_SIZE {
            return;
        }
        self.with_bank(chain, id, |b| b.slot(index).name = name);
        self.mark_edit("Rename Plug-In Preset");
    }

    /// Write the current preset to a `.vstpreset` file.
    pub fn save_plugin_preset(
        &mut self,
        chain: ChainRef,
        id: DeviceId,
        path: &Path,
    ) -> Result<(), String> {
        let class_id = match self.plugin_kind(chain, id) {
            Some(DeviceKind::Plugin { plugin_id, .. }) => plugin_id.clone(),
            _ => return Err("not a plug-in".into()),
        };
        let state = self
            .runtimes
            .get_mut(&id)
            .and_then(|r| r.plugin.as_mut())
            .and_then(|h| h.save_state())
            .ok_or("the plug-in has no state to save")?;
        let file = vst3::encode_preset(&class_id, &state)
            .ok_or_else(|| "this plug-in's state isn't a VST3 one".to_string())?;
        std::fs::write(path, file).map_err(|e| e.to_string())
    }

    /// Load a `.vstpreset` file into the current preset.
    pub fn load_plugin_preset(
        &mut self,
        chain: ChainRef,
        id: DeviceId,
        path: &Path,
    ) -> Result<(), String> {
        let class_id = match self.plugin_kind(chain, id) {
            Some(DeviceKind::Plugin { plugin_id, .. }) => plugin_id.clone(),
            _ => return Err("not a plug-in".into()),
        };
        let data = std::fs::read(path).map_err(|e| e.to_string())?;
        let (file_class, state) =
            vst3::decode_preset(&data).ok_or_else(|| "not a VST3 preset file".to_string())?;
        if !file_class.eq_ignore_ascii_case(&class_id) {
            return Err("that preset is for another plug-in".into());
        }
        if !self.load_plugin_state(chain, id, &state) {
            return Err("the plug-in refused that preset".into());
        }
        self.mark_edit("Load Plug-In Preset");
        Ok(())
    }

    /// Write the whole bank to a file, every preset's state included.
    pub fn save_plugin_bank(
        &mut self,
        chain: ChainRef,
        id: DeviceId,
        path: &Path,
    ) -> Result<(), String> {
        self.store_current_preset(chain, id);
        let bank = self.plugin_bank(chain, id).ok_or("not a plug-in")?;
        let text = bank.to_text();
        std::fs::write(path, text).map_err(|e| e.to_string())
    }

    /// Replace the bank with one from a file, and switch to its current preset.
    pub fn load_plugin_bank(
        &mut self,
        chain: ChainRef,
        id: DeviceId,
        path: &Path,
    ) -> Result<(), String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let bank = PluginBank::from_text(&text)?;
        let state = bank.presets.get(bank.current).and_then(|p| p.state.clone());
        self.with_bank(chain, id, |b| *b = bank)
            .ok_or("not a plug-in")?;
        if let Some(state) = state {
            self.load_plugin_state(chain, id, &state);
        }
        self.mark_edit("Load Plug-In Bank");
        Ok(())
    }
}

struct UiView {
    hidden: bool,
    auto_hide: bool,
    selected: Option<ChainRef>,
}

impl UiView {
    fn of(ui: &PluginUi) -> Self {
        Self {
            hidden: ui.windows_hidden,
            auto_hide: ui.prefs.auto_hide,
            selected: ui.selected,
        }
    }
}

/// What a plug-in offers in the envelope choosers, or `None` for any other device.
pub(super) fn plugin_chooser_params(d: &Device, rt: &super::DeviceRuntime) -> Option<HashSet<u32>> {
    let DeviceKind::Plugin { panel, .. } = &d.kind else {
        return None;
    };
    let mut ids: HashSet<u32> = panel_params(panel, &rt.params).into_iter().collect();
    ids.extend(rt.touched);
    ids.extend(
        rt.params
            .iter()
            .map(|p| p.id)
            .filter(|id| plugin_sidechain::contains(*id)),
    );
    Some(ids)
}
