//! What a user did while the device view was drawn. Actions queue up and apply once drawing is
//! done, so drawing only ever reads the session.

use super::super::BrowserItem;
use super::DeviceViewState;
use crate::session::{ChainRef, Session};
use rlive_core::{ChainId, DeviceId};
use rlive_project::DeviceKind;

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
    ShowAll(DeviceId),
    /// Show this chain of a rack.
    OpenChain(DeviceId, ChainId),
    /// Open the Info View's editor on this device.
    EditInfo(DeviceId),
    /// Something dropped from the browser into a chain.
    Add(BrowserItem),
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
                .add_filter("WAV", &["wav"])
                .pick_file()
            {
                session.set_device_sample_at(chain, id, slot, path);
            }
        }
        Action::Reload(id) => session.reload_device(chain, id),
        Action::EditInfo(id) => view.edit_info = Some((chain, id)),
        Action::ShowAll(id) => {
            let all = &mut view.show_all;
            if !all.insert(id) {
                all.remove(&id);
            }
        }
        Action::OpenChain(rack, chain) => {
            view.open_chains.insert(rack, chain);
        }
        Action::Add(item) => add_item(session, chain, item),
    }
}

/// Puts something from the browser in a chain. A sample comes in loaded in a new Sampler.
fn add_item(session: &mut Session, chain: ChainRef, item: BrowserItem) {
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
        q.push(chain, Action::OpenChain(a, ChainId(7)));
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
        std::fs::remove_file(&path).ok();
        let sampler = &s.devices(chain)[0];
        assert_eq!(sampler.sample(0).map(|f| f.path.clone()), Some(path));
    }
}
