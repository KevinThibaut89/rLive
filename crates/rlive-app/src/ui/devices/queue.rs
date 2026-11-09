//! What a user did while the device view was drawn. Actions queue up and apply once drawing is
//! done, so drawing only ever reads the session.

use super::super::BrowserItem;
use super::rack::{self, KeyFocus, RackAction};
use super::DeviceViewState;
use crate::session::{ChainRef, InfoTarget, Session};
use rlive_core::{ChainId, DeviceId};
use rlive_project::{Device, DeviceKind};

/// Something to do to a device, once the frame has been drawn.
#[derive(Clone)]
pub(crate) enum Action {
    Remove(DeviceId),
    Move(DeviceId, usize),
    Bypass(DeviceId, bool),
    Fold(DeviceId, bool),
    Param(DeviceId, u32, f64),
    /// Hand one overridden parameter back to its automation.
    ReEnable(DeviceId, u32),
    OpenEditor(DeviceId),
    CloseEditor(DeviceId),
    LoadSample(DeviceId, usize),
    Reload(DeviceId),
    /// A plug-in's MPE Mode and channels.
    SetMpe(DeviceId, Option<rlive_core::MpeOutput>),
    ShowAll(DeviceId),
    /// Open the Info View's editor on this device.
    EditInfo(DeviceId),
    /// Lock a control surface to a device, or let it go.
    LockSurface(usize, Option<DeviceId>),
    /// Open the Info View's editor on something else in a rack.
    EditInfoTarget(InfoTarget),
    /// Something dropped from the browser into a chain.
    Add(BrowserItem),
    /// A new source in the device's own sidechain chooser.
    SetSidechain(DeviceId, Option<rlive_project::SidechainSource>),
    /// Select a device by its title bar; `true` adds to the selection.
    SelectDevice(DeviceId, bool),
    Rename(DeviceId, String),
    /// Turn off or on (`true` off) this device and the rest of its group.
    BypassGroup(DeviceId, bool),
    /// Group the selected devices, or this one, to a new rack; `true` for a Drum Rack.
    Group(DeviceId, bool),
    Ungroup(DeviceId),
    /// Show a device, opening the chains on the way to it.
    Reveal(DeviceId, Vec<(DeviceId, ChainId)>),
    /// Something done to the rack `DeviceId`.
    Rack(DeviceId, RackAction),
    /// A device's own saved state, written by its panel.
    DeviceState(DeviceId, String),
    /// A conversion or sample command from a sample player's panel.
    Sampling(DeviceId, crate::session::SamplingCommand),
}

/// The actions of one frame, in the order they happened.
#[derive(Default)]
pub(crate) struct ActionQueue {
    items: Vec<(ChainRef, Action)>,
}

impl ActionQueue {
    pub fn push(&mut self, chain: ChainRef, action: Action) {
        self.items.push((chain, action));
    }

    /// Apply everything queued. Actions on a device that's gone by now do nothing, like a
    /// parameter queued after the device was deleted in the same frame.
    pub fn apply(self, session: &mut Session, view: &mut DeviceViewState, status: &mut String) {
        for (chain, action) in self.items {
            apply(session, view, status, chain, action);
        }
    }
}

fn apply(
    session: &mut Session,
    view: &mut DeviceViewState,
    status: &mut String,
    chain: ChainRef,
    action: Action,
) {
    match action {
        Action::Remove(id) => session.remove_device(chain, id),
        Action::Move(id, to) => session.move_device(chain, id, to),
        Action::Bypass(id, b) => session.set_bypass(chain, id, b),
        Action::Fold(id, expanded) => session.set_device_expanded(chain, id, expanded),
        Action::Param(id, p, v) => session.set_param(chain, id, p, v),
        Action::DeviceState(id, text) => session.set_device_state(chain, id, text),
        Action::Sampling(id, command) => session.sampling_command(chain, id, command),
        Action::ReEnable(id, p) => {
            if let Some(cell) = session.device_cell(chain, id, p) {
                session.re_enable_cell(chain, cell);
            }
        }
        Action::OpenEditor(id) => {
            if let Some(h) = session
                .runtimes
                .get_mut(&id)
                .and_then(|r| r.plugin.as_mut())
            {
                if let Err(e) = h.open_editor() {
                    *status = format!("Editor: {e}");
                }
            }
        }
        Action::CloseEditor(id) => {
            if let Some(h) = session
                .runtimes
                .get_mut(&id)
                .and_then(|r| r.plugin.as_mut())
            {
                h.close_editor();
            }
        }
        Action::LoadSample(id, slot) => {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Audio", rlive_audio_io::AUDIO_EXTENSIONS)
                .pick_file()
            {
                session.set_device_sample_at(chain, id, slot, path);
            }
        }
        Action::Reload(id) => session.reload_device(chain, id),
        Action::SetMpe(id, out) => session.set_device_mpe(chain, id, out),
        Action::SetSidechain(id, source) => {
            if !session.set_sidechain(chain, id, source) {
                *status = "That source would make a feedback loop".to_string();
            }
        }
        Action::EditInfo(id) => view.edit_info = Some(InfoTarget::Device(chain, id)),
        Action::LockSurface(row, device) => session.lock_surface(row, device),
        Action::EditInfoTarget(target) => view.edit_info = Some(target),
        Action::ShowAll(id) => {
            let all = &mut view.show_all;
            if !all.insert(id) {
                all.remove(&id);
            }
        }
        Action::Add(item) => add_item(session, chain, item),
        Action::SelectDevice(id, additive) => {
            select_device(&mut view.selected_devices, chain, id, additive);
        }
        Action::Rename(id, name) => session.rename_device(chain, id, &name),
        Action::BypassGroup(id, off) => {
            for (at, d) in group_members(chain, session.devices(chain), id) {
                session.set_bypass(at, d, off);
            }
        }
        Action::Group(id, to_drum) => {
            if !view.selected_devices.contains(&(chain, id)) {
                view.selected_devices = vec![(chain, id)];
            }
            group_selected(session, view, status, to_drum);
        }
        Action::Ungroup(id) => {
            session.ungroup_rack(chain, id);
            view.selected_devices.retain(|(_, d)| *d != id);
        }
        Action::Reveal(id, path) => {
            for (rack, c) in path {
                view.open_chains.insert(rack, c);
            }
            view.scroll_to = Some(id);
        }
        Action::Rack(rack_id, action) => rack::apply(session, view, status, chain, rack_id, action),
    }
}

/// Selects a device, the way a click on its title bar does.
fn select_device(
    selected: &mut Vec<(ChainRef, DeviceId)>,
    chain: ChainRef,
    id: DeviceId,
    additive: bool,
) {
    if additive {
        if let Some(i) = selected.iter().position(|s| *s == (chain, id)) {
            selected.remove(i);
        } else {
            selected.retain(|(c, _)| *c == chain);
            selected.push((chain, id));
        }
    } else {
        *selected = vec![(chain, id)];
    }
}

/// Who an Alt-click on an activator reaches: a device's whole chain inside a rack, or a rack and
/// all it holds at the top level.
fn group_members(chain: ChainRef, devices: &[Device], id: DeviceId) -> Vec<(ChainRef, DeviceId)> {
    let mut out = Vec::new();
    if matches!(chain, ChainRef::Nested(_)) {
        out.extend(devices.iter().map(|d| (chain, d.id)));
        return out;
    }
    let Some(device) = devices.iter().find(|d| d.id == id) else {
        return out;
    };
    collect_members(chain, device, &mut out);
    out
}

fn collect_members(chain: ChainRef, device: &Device, out: &mut Vec<(ChainRef, DeviceId)>) {
    out.push((chain, device.id));
    for c in device.chains() {
        if let Some(child) = chain.child(device.id, c.id) {
            for d in &c.devices {
                collect_members(child, d, out);
            }
        }
    }
}

/// The Group command: the chains selected in the Chain List if that has focus, or else the
/// devices selected in the Device View (manual p. 493).
pub(crate) fn group_selected(
    session: &mut Session,
    view: &mut DeviceViewState,
    status: &mut String,
    to_drum: bool,
) {
    if !to_drum {
        if let Some(KeyFocus::ChainList(rack_id)) = view.ui.key_focus {
            let selection = view.rack_views.get(&rack_id).map(|v| v.selection.clone());
            if let (Some(ids), Some(chain)) = (selection, session.chain_of_device(rack_id)) {
                if ids.len() > 1 {
                    if let Some((wrapper, _)) = session.group_chains(chain, rack_id, &ids) {
                        apply(
                            session,
                            view,
                            status,
                            chain,
                            Action::Rack(rack_id, RackAction::Select(vec![wrapper])),
                        );
                    }
                    return;
                }
            }
        }
    }
    let Some(&(chain, _)) = view.selected_devices.first() else {
        *status = "Select some devices to group first".into();
        return;
    };
    let ids: Vec<DeviceId> = view
        .selected_devices
        .iter()
        .filter(|(c, _)| *c == chain)
        .map(|(_, d)| *d)
        .collect();
    match session.group_devices(chain, &ids, to_drum) {
        Some(rack_id) => view.selected_devices = vec![(chain, rack_id)],
        None => *status = "Those devices can't be grouped".into(),
    }
}

/// The Ungroup command, for whichever racks are selected.
pub(crate) fn ungroup_selected(session: &mut Session, view: &mut DeviceViewState) {
    let racks = std::mem::take(&mut view.selected_devices);
    for (chain, id) in racks {
        let is_rack = session
            .devices(chain)
            .iter()
            .any(|d| d.id == id && matches!(d.kind, DeviceKind::Rack { .. }));
        if is_rack {
            session.ungroup_rack(chain, id);
        }
    }
}

/// Puts something from the browser in a chain. A sample comes in loaded in a new Sampler.
pub(super) fn add_item(session: &mut Session, chain: ChainRef, item: BrowserItem) {
    match item {
        BrowserItem::Builtin(t) => session.add_builtin(chain, t),
        BrowserItem::Plugin(p) => session.add_plugin(chain, &p),
        BrowserItem::Sample(path) => {
            session.add_builtin(chain, "rlive.sampler");
            let sampler = session.devices(chain).iter().rev().find(|d| {
                matches!(&d.kind, DeviceKind::Builtin { type_id, .. } if type_id == "rlive.sampler")
            });
            if let Some(id) = sampler.map(|d| d.id) {
                session.set_device_sample(chain, id, path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rlive_engine::create_engine;
    use rlive_plugin_host::scanner::PluginCache;
    use rlive_project::{Device, Project, Track, TrackKind};

    fn session() -> (rlive_engine::Engine, Session, ChainRef) {
        let (engine, handle) = create_engine(48_000.0, 512);
        let mut p = Project::empty();
        let t = Track::new("T", TrackKind::Midi, [1, 2, 3], p.scenes.len());
        let id = t.id;
        p.tracks = vec![t];
        let s = Session::new(handle, p, PluginCache::default());
        (engine, s, ChainRef::Track(id))
    }

    fn add(s: &mut Session, chain: ChainRef, name: &str) -> DeviceId {
        let d = Device::builtin("rlive.compressor", name);
        let id = d.id;
        s.add_device(chain, None, d);
        id
    }

    #[test]
    fn a_device_selection_grows_with_shift_and_stays_in_one_chain() {
        let t = rlive_core::TrackId(1);
        let (a, b) = (DeviceId(1), DeviceId(2));
        let there = ChainRef::Track(t);
        let elsewhere = ChainRef::Master;
        let mut list = Vec::new();
        select_device(&mut list, there, a, false);
        select_device(&mut list, there, b, true);
        assert_eq!(list, [(there, a), (there, b)]);
        select_device(&mut list, there, a, true);
        assert_eq!(list, [(there, b)]);
        select_device(&mut list, elsewhere, a, true);
        assert_eq!(list, [(elsewhere, a)]);
        select_device(&mut list, there, b, false);
        assert_eq!(list, [(there, b)]);
    }

    #[test]
    fn group_and_ungroup_act_on_the_selection() {
        let (_e, mut s, chain) = session();
        let (a, b) = (add(&mut s, chain, "A"), add(&mut s, chain, "B"));
        let mut view = DeviceViewState::default();
        let mut q = ActionQueue::default();
        q.push(chain, Action::SelectDevice(a, false));
        q.push(chain, Action::SelectDevice(b, true));
        q.push(chain, Action::Group(a, false));
        run(q, &mut s, &mut view);
        assert_eq!(s.devices(chain).len(), 1);
        let rack = s.devices(chain)[0].id;
        assert!(matches!(s.devices(chain)[0].kind, DeviceKind::Rack { .. }));
        assert_eq!(view.selected_devices, [(chain, rack)]);

        let mut q = ActionQueue::default();
        q.push(chain, Action::Ungroup(rack));
        run(q, &mut s, &mut view);
        assert_eq!(s.devices(chain).len(), 2);
        assert!(view.selected_devices.is_empty());
    }

    #[test]
    fn alt_click_reaches_a_rack_and_all_it_holds() {
        let (_e, mut s, chain) = session();
        let a = add(&mut s, chain, "A");
        let b = add(&mut s, chain, "B");
        let rack = s.group_devices(chain, &[a, b], false).unwrap();
        let mut q = ActionQueue::default();
        q.push(chain, Action::BypassGroup(rack, true));
        run(q, &mut s, &mut DeviceViewState::default());
        let mut bypassed = Vec::new();
        s.devices(chain)[0].for_each(&mut |d| bypassed.push(d.bypass));
        assert_eq!(bypassed, [true, true, true]);
    }

    fn run(q: ActionQueue, s: &mut Session, view: &mut DeviceViewState) {
        q.apply(s, view, &mut String::new());
    }

    #[test]
    fn actions_run_only_when_applied_and_in_order() {
        let (_e, mut s, chain) = session();
        let (a, b) = (add(&mut s, chain, "A"), add(&mut s, chain, "B"));
        let mut q = ActionQueue::default();
        q.push(chain, Action::Move(b, 0));
        q.push(chain, Action::Fold(a, false));
        q.push(chain, Action::Param(a, 0, -30.0));
        // Nothing has changed yet: the queue is what drawing leaves behind.
        assert_eq!(s.devices(chain)[0].id, a);
        assert!(s.devices(chain)[0].expanded);

        run(q, &mut s, &mut DeviceViewState::default());
        assert_eq!(s.devices(chain)[0].id, b);
        assert!(!s.devices(chain)[1].expanded);
        assert_eq!(s.devices(chain)[1].params.get(&0), Some(&-30.0));
    }

    #[test]
    fn an_action_on_a_device_deleted_this_frame_is_harmless() {
        let (_e, mut s, chain) = session();
        let a = add(&mut s, chain, "A");
        let mut q = ActionQueue::default();
        q.push(chain, Action::Remove(a));
        q.push(chain, Action::Param(a, 0, -1.0));
        q.push(chain, Action::Bypass(a, true));
        q.push(chain, Action::Fold(a, false));
        q.push(chain, Action::Move(a, 3));
        run(q, &mut s, &mut DeviceViewState::default());
        assert!(s.devices(chain).is_empty());
    }

    #[test]
    fn view_state_is_changed_by_actions_too() {
        let (_e, mut s, chain) = session();
        let mut view = DeviceViewState::default();
        let a = add(&mut s, chain, "A");
        let mut q = ActionQueue::default();
        q.push(chain, Action::ShowAll(a));
        q.push(chain, Action::Rack(a, RackAction::Select(vec![ChainId(7)])));
        run(q, &mut s, &mut view);
        assert!(view.show_all.contains(&a));
        assert_eq!(view.open_chains[&a], ChainId(7));
        let mut q = ActionQueue::default();
        q.push(chain, Action::ShowAll(a));
        run(q, &mut s, &mut view);
        assert!(!view.show_all.contains(&a));
    }

    #[test]
    fn a_browser_drop_adds_to_the_chain_it_landed_on() {
        let (_e, mut s, chain) = session();
        let mut q = ActionQueue::default();
        q.push(chain, Action::Add(BrowserItem::Builtin("rlive.compressor")));
        run(q, &mut s, &mut DeviceViewState::default());
        assert_eq!(s.devices(chain).len(), 1);
    }

    #[test]
    fn a_dropped_sample_comes_in_loaded_in_a_new_sampler() {
        let (_e, mut s, chain) = session();
        let path = std::env::temp_dir().join(format!("rlive-k7-{}.wav", std::process::id()));
        rlive_devices::write_wav(&path, &[0.0; 16], &[0.0; 16], 48_000).unwrap();
        let mut q = ActionQueue::default();
        q.push(chain, Action::Add(BrowserItem::Sample(path.clone())));
        run(q, &mut s, &mut DeviceViewState::default());
        s.samples.wait_idle();
        s.idle();
        std::fs::remove_file(&path).ok();
        let sampler = &s.devices(chain)[0];
        assert_eq!(sampler.sample(0).map(|f| f.path.clone()), Some(path));
    }
}
