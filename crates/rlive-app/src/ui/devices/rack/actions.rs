//! What a user did to a rack, applied once the frame has been drawn.

use super::super::queue::add_item;
use super::super::DeviceViewState;
use super::selection::click_selection;
use super::{ActivePreview, ChainDrag, KeyFocus, MapMode, RackSection, RackView};
use crate::session::{ChainRef, ExtractNotes, PadDrop, Session, ZoneKind};
use crate::ui::BrowserItem;
use rlive_core::param::ParamRef;
use rlive_core::{ChainId, DeviceId, ParamId, TrackId};
use rlive_project::{DeviceKind, DrumPad, RackKind, Zone};

const PREVIEW_VELOCITY: f32 = 0.8;

/// Something to do to a rack, once the frame has been drawn.
#[derive(Clone)]
pub(crate) enum RackAction {
    Toggle(RackSection),
    ZoneEditor(Option<ZoneKind>),
    AutoSelect(bool),
    Select(Vec<ChainId>),
    ChainClick {
        id: ChainId,
        order: Vec<ChainId>,
        shift: bool,
        cmd: bool,
    },
    ChainParam(ChainId, ParamId, f64),
    RenameChain(ChainId, String),
    ChainColor(ChainId, Option<[u8; 3]>),
    AddChain,
    AddChainWith(BrowserItem),
    AddToChain(ChainId, BrowserItem),
    DeleteChains(Vec<ChainId>),
    GroupChains(Vec<ChainId>),
    DuplicateChains(Vec<ChainId>),
    MoveChains {
        from: ChainRef,
        from_rack: DeviceId,
        ids: Vec<ChainId>,
        index: usize,
    },
    AddReturn,
    ReturnOutput(ChainId, Option<TrackId>),
    Zone(ChainId, ZoneKind, Zone),
    Selector(f64),
    FiltersCtrl(bool),
    DrumPad(ChainId, DrumPad),
    Preview(ActivePreview, bool),
    SelectPad(u8, Option<ChainId>),
    PadStart(u8),
    PadDrop(u8, BrowserItem),
    PadSwap(u8, u8),
    PadLayer(u8, u8),
    MacroValue(usize, f64),
    MacroCount(u8),
    MacroName(usize, String),
    MacroColor(usize, Option<[u8; 3]>),
    MacroExcludeRandom(usize, bool),
    MacroExcludeVariations(usize, bool),
    ToggleMap,
    MapTarget(ParamRef),
    Map(usize),
    Unmap(usize, usize),
    Range(usize, usize, f64, f64),
    Invert(usize, usize),
    Randomize,
    StoreVariation,
    SelectVariation(usize),
    LaunchVariation(usize),
    OverwriteVariation(usize),
    RenameVariation(usize, String),
    DuplicateVariation(usize),
    DeleteVariation(usize),
    /// A drag from the mixer out to the track or return area (manual p. 509).
    Extract(ChainDrag),
}

fn view_of<'a>(
    view: &'a mut DeviceViewState,
    session: &Session,
    chain: ChainRef,
    rack: DeviceId,
) -> &'a mut RackView {
    let kind = match session
        .devices(chain)
        .iter()
        .find(|d| d.id == rack)
        .map(|d| &d.kind)
    {
        Some(DeviceKind::Rack { kind, .. }) => *kind,
        _ => RackKind::Audio,
    };
    view.rack_views
        .entry(rack)
        .or_insert_with(|| RackView::for_kind(kind))
}

/// Selects these chains and opens the last one.
fn select_chains(
    view: &mut DeviceViewState,
    session: &Session,
    chain: ChainRef,
    rack: DeviceId,
    selection: Vec<ChainId>,
) {
    let last = selection.last().copied();
    let v = view_of(view, session, chain, rack);
    v.anchor = selection.first().copied().or(v.anchor);
    v.selection = selection;
    if let Some(id) = last {
        view.open_chains.insert(rack, id);
    }
}

pub(crate) fn apply(
    session: &mut Session,
    view: &mut DeviceViewState,
    status: &mut String,
    chain: ChainRef,
    rack: DeviceId,
    action: RackAction,
) {
    match action {
        RackAction::Toggle(section) => {
            let v = view_of(view, session, chain, rack);
            let on = v.is_on(section);
            v.set(section, !on);
        }
        RackAction::ZoneEditor(kind) => view_of(view, session, chain, rack).zones = kind,
        RackAction::AutoSelect(on) => view_of(view, session, chain, rack).auto_select = on,
        RackAction::Select(selection) => {
            view.ui.key_focus = Some(KeyFocus::ChainList(rack));
            select_chains(view, session, chain, rack, selection);
        }
        RackAction::ChainClick {
            id,
            order,
            shift,
            cmd,
        } => {
            let v = view_of(view, session, chain, rack);
            let next = click_selection(&order, &v.selection, v.anchor, id, shift, cmd);
            select_chains(view, session, chain, rack, next);
        }
        RackAction::ChainParam(id, control, value) => {
            session.set_chain_param(chain, rack, id, control, value);
        }
        RackAction::RenameChain(id, name) => session.rename_chain(chain, rack, id, &name),
        RackAction::ChainColor(id, color) => session.set_chain_color(chain, rack, id, color),
        RackAction::AddChain => {
            if let Some(id) = session.add_chain(chain, rack, "Chain") {
                select_chains(view, session, chain, rack, vec![id]);
            }
        }
        RackAction::AddChainWith(item) => add_chain_with(session, view, chain, rack, item),
        RackAction::AddToChain(id, item) => {
            if let Some(child) = chain.child(rack, id) {
                add_item(session, child, item);
            }
        }
        RackAction::DeleteChains(ids) => {
            for id in &ids {
                session.delete_chain(chain, rack, *id);
            }
            let v = view_of(view, session, chain, rack);
            v.selection.retain(|c| !ids.contains(c));
        }
        RackAction::GroupChains(ids) => {
            if let Some((wrapper, _)) = session.group_chains(chain, rack, &ids) {
                select_chains(view, session, chain, rack, vec![wrapper]);
            }
        }
        RackAction::DuplicateChains(ids) => {
            let mut last = None;
            for id in ids {
                last = session.duplicate_chain(chain, rack, id).or(last);
            }
            if let Some(id) = last {
                select_chains(view, session, chain, rack, vec![id]);
            }
        }
        RackAction::MoveChains {
            from,
            from_rack,
            ids,
            index,
        } => {
            for (i, id) in ids.iter().enumerate() {
                session.move_chain(from, from_rack, *id, chain, rack, index + i);
            }
            let moved = ids.clone();
            select_chains(view, session, chain, rack, moved);
        }
        RackAction::AddReturn => {
            let n = match session
                .devices(chain)
                .iter()
                .find(|d| d.id == rack)
                .map(|d| &d.kind)
            {
                Some(DeviceKind::Rack { returns, .. }) => returns.len(),
                _ => 0,
            };
            let name = format!("Return {}", (b'A' + n as u8) as char);
            session.add_return_chain(chain, rack, &name);
        }
        RackAction::ReturnOutput(id, output) => {
            session.set_return_output(chain, rack, id, output);
        }
        RackAction::Zone(id, kind, zone) => session.set_zone(chain, rack, id, kind, zone),
        RackAction::Selector(value) => session.set_chain_selector(chain, rack, value),
        RackAction::FiltersCtrl(on) => session.set_select_filters_ctrl(chain, rack, on),
        RackAction::DrumPad(id, pad) => session.set_drum_pad(chain, rack, id, pad),
        RackAction::Preview(p, on) => {
            session.preview_note(p.chain, p.rack, p.target, p.key, PREVIEW_VELOCITY, on);
        }
        RackAction::SelectPad(note, id) => {
            let v = view_of(view, session, chain, rack);
            v.last_pad = Some(note);
            if let Some(id) = id {
                select_chains(view, session, chain, rack, vec![id]);
            }
        }
        RackAction::PadStart(start) => view_of(view, session, chain, rack).pad_start = start,
        RackAction::PadDrop(note, item) => {
            session.drop_on_pad(chain, rack, note, pad_drop(item));
            let v = view_of(view, session, chain, rack);
            v.last_pad = Some(note);
        }
        RackAction::PadSwap(a, b) => session.swap_pads(chain, rack, a, b),
        RackAction::PadLayer(from, to) => session.layer_pads(chain, rack, from, to),
        RackAction::MacroValue(n, value) => session.set_macro(chain, rack, n, value),
        RackAction::MacroCount(n) => session.set_visible_macros(chain, rack, n),
        RackAction::MacroName(n, name) => session.set_macro_name(chain, rack, n, &name),
        RackAction::MacroColor(n, color) => session.set_macro_color(chain, rack, n, color),
        RackAction::MacroExcludeRandom(n, on) => {
            session.set_macro_exclude_random(chain, rack, n, on);
        }
        RackAction::MacroExcludeVariations(n, on) => {
            session.set_macro_exclude_variations(chain, rack, n, on);
        }
        RackAction::ToggleMap => {
            view.ui.map = match view.ui.map {
                Some(m) if m.rack == rack => None,
                _ => Some(MapMode {
                    chain,
                    rack,
                    selected: None,
                }),
            };
        }
        RackAction::MapTarget(target) => {
            if let Some(m) = &mut view.ui.map {
                m.selected = Some(target);
            }
        }
        RackAction::Map(n) => match view.ui.map.and_then(|m| m.selected) {
            Some(target) => {
                if session.map_macro(chain, rack, n, target) {
                    if let Some(m) = &mut view.ui.map {
                        m.selected = None;
                    }
                } else {
                    *status = "That parameter is already mapped or isn't in this Rack".into();
                }
            }
            None => *status = "Select a parameter to map first".into(),
        },
        RackAction::Unmap(n, mapping) => session.unmap_macro(chain, rack, n, mapping),
        RackAction::Range(n, mapping, min, max) => {
            session.set_macro_range(chain, rack, n, mapping, min, max);
        }
        RackAction::Invert(n, mapping) => session.invert_macro_range(chain, rack, n, mapping),
        RackAction::Randomize => session.randomize_macros(chain, rack),
        RackAction::StoreVariation => {
            if let Some(i) = session.store_variation(chain, rack) {
                view_of(view, session, chain, rack).selected_variation = Some(i);
            }
        }
        RackAction::SelectVariation(i) => {
            view_of(view, session, chain, rack).selected_variation = Some(i);
        }
        RackAction::LaunchVariation(i) => session.launch_variation(chain, rack, i),
        RackAction::OverwriteVariation(i) => session.overwrite_variation(chain, rack, i),
        RackAction::RenameVariation(i, name) => session.rename_variation(chain, rack, i, &name),
        RackAction::DuplicateVariation(i) => session.duplicate_variation(chain, rack, i),
        RackAction::DeleteVariation(i) => session.delete_variation(chain, rack, i),
        RackAction::Extract(drag) => extract(session, drag),
    }
}

/// Dragging a chain out to a new track carries its notes only when it came from the mixer.
fn extract(session: &mut Session, drag: ChainDrag) {
    let notes = if drag.from_mixer {
        ExtractNotes::With
    } else {
        ExtractNotes::Without
    };
    for id in drag.ids {
        if drag.is_return {
            session.extract_return_to_track(drag.at, drag.rack, id);
        } else {
            session.extract_chain_to_track(drag.at, drag.rack, id, notes);
        }
    }
}

/// A chain from something in the browser, called after it.
fn add_chain_with(
    session: &mut Session,
    view: &mut DeviceViewState,
    chain: ChainRef,
    rack: DeviceId,
    item: BrowserItem,
) {
    let name = match &item {
        BrowserItem::Builtin(t) => rlive_devices::find(t)
            .map_or("Chain", |d| d.name)
            .to_string(),
        BrowserItem::Plugin(p) => p.name.clone(),
        BrowserItem::Sample(path) => path
            .file_stem()
            .map_or_else(|| "Chain".to_string(), |s| s.to_string_lossy().into_owned()),
    };
    let Some(id) = session.add_chain(chain, rack, &name) else {
        return;
    };
    if let Some(child) = chain.child(rack, id) {
        add_item(session, child, item);
        // The rack's rules can refuse what was dropped; an empty chain would be left behind.
        if session.devices(child).is_empty() {
            session.delete_chain(chain, rack, id);
            return;
        }
    }
    select_chains(view, session, chain, rack, vec![id]);
}

fn pad_drop(item: BrowserItem) -> PadDrop {
    match item {
        BrowserItem::Builtin(t) => PadDrop::Builtin(t.to_string()),
        BrowserItem::Plugin(p) => PadDrop::Plugin(p),
        BrowserItem::Sample(path) => PadDrop::Sample(path),
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::{Action, ActionQueue};
    use super::*;
    use rlive_core::param::chain as controls;
    use rlive_core::param::rack::MACRO_COUNT;
    use rlive_core::ParamOwner;
    use rlive_engine::create_engine;
    use rlive_plugin_host::scanner::PluginCache;
    use rlive_project::{Chain, Device, Project, Track, TrackKind};

    struct Rig {
        _engine: rlive_engine::Engine,
        s: Session,
        view: DeviceViewState,
        root: ChainRef,
        rack: DeviceId,
        chains: Vec<ChainId>,
    }

    fn rig(kind: RackKind, names: &[&str]) -> Rig {
        let (engine, handle) = create_engine(48_000.0, 512);
        let mut p = Project::empty();
        let mut t = Track::new("T", TrackKind::Midi, [1, 2, 3], p.scenes.len());
        let mut rack = Device::rack(kind, "Rack");
        let rack_id = rack.id;
        let mut chains = Vec::new();
        if let DeviceKind::Rack { chains: list, .. } = &mut rack.kind {
            for (i, name) in names.iter().enumerate() {
                let mut c = Chain::new(name);
                let compressor = Device::builtin("rlive.compressor", "Compressor");
                c.devices.push(compressor);
                if kind == RackKind::Drum {
                    c.zones.drum = Some(DrumPad {
                        receive: Some(36 + i as u8),
                        play: 60,
                        choke: None,
                    });
                }
                chains.push(c.id);
                list.push(c);
            }
        }
        t.devices.push(rack);
        let root = ChainRef::Track(t.id);
        p.tracks = vec![t];
        Rig {
            _engine: engine,
            s: Session::new(handle, p, PluginCache::default()),
            view: DeviceViewState::default(),
            root,
            rack: rack_id,
            chains,
        }
    }

    impl Rig {
        fn run(&mut self, action: RackAction) {
            let mut q = ActionQueue::default();
            q.push(self.root, Action::Rack(self.rack, action));
            q.apply(&mut self.s, &mut self.view, &mut String::new());
        }

        fn rack_device(&self) -> &Device {
            self.s
                .devices(self.root)
                .iter()
                .find(|d| d.id == self.rack)
                .unwrap()
        }

        fn chain(&self, id: ChainId) -> &Chain {
            self.rack_device().chains().find(|c| c.id == id).unwrap()
        }

        fn chain_names(&self) -> Vec<String> {
            match &self.rack_device().kind {
                DeviceKind::Rack { chains, .. } => chains.iter().map(|c| c.name.clone()).collect(),
                _ => Vec::new(),
            }
        }

        fn compressor(&self, chain: ChainId) -> DeviceId {
            self.chain(chain).devices[0].id
        }

        fn target(&self, chain: ChainId) -> ParamRef {
            ParamRef {
                owner: ParamOwner::Device(self.compressor(chain)),
                id: ParamId(0),
            }
        }
    }

    #[test]
    fn view_toggles_are_per_rack_and_start_from_the_rack_kind() {
        let mut r = rig(RackKind::Audio, &["A"]);
        r.run(RackAction::Toggle(RackSection::Macros));
        assert!(!r.view.rack_views[&r.rack].macros);
        assert!(r.view.rack_views[&r.rack].chains, "left at its default");
        r.run(RackAction::ZoneEditor(Some(ZoneKind::Key)));
        assert_eq!(r.view.rack_views[&r.rack].zones, Some(ZoneKind::Key));
        r.run(RackAction::AutoSelect(true));
        assert!(r.view.rack_views[&r.rack].auto_select);
    }

    #[test]
    fn selecting_opens_the_last_chain() {
        let mut r = rig(RackKind::Audio, &["A", "B", "C"]);
        let ids = r.chains.clone();
        r.run(RackAction::Select(vec![ids[0], ids[2]]));
        assert_eq!(r.view.rack_views[&r.rack].selection, [ids[0], ids[2]]);
        assert_eq!(r.view.open_chains[&r.rack], ids[2]);
    }

    #[test]
    fn a_chain_click_uses_the_selection_rules() {
        let mut r = rig(RackKind::Audio, &["A", "B", "C"]);
        let ids = r.chains.clone();
        let click = |id, shift| RackAction::ChainClick {
            id,
            order: ids.clone(),
            shift,
            cmd: false,
        };
        r.run(click(ids[0], false));
        r.run(click(ids[2], true));
        assert_eq!(r.view.rack_views[&r.rack].selection, ids);
    }

    #[test]
    fn chain_controls_reach_the_session() {
        let mut r = rig(RackKind::Audio, &["A"]);
        let id = r.chains[0];
        r.run(RackAction::ChainParam(id, controls::VOLUME, -12.0));
        r.run(RackAction::ChainParam(id, controls::ACTIVATOR, 0.0));
        r.run(RackAction::ChainParam(id, controls::SOLO, 1.0));
        assert_eq!(r.chain(id).mix.volume_db, -12.0);
        assert!(!r.chain(id).mix.active);
        assert!(r.chain(id).mix.solo);
    }

    #[test]
    fn a_control_a_macro_owns_stays_put() {
        let mut r = rig(RackKind::Audio, &["A"]);
        let id = r.chains[0];
        let target = ParamRef {
            owner: ParamOwner::Chain(id),
            id: controls::VOLUME,
        };
        r.run(RackAction::ToggleMap);
        r.run(RackAction::MapTarget(target));
        r.run(RackAction::Map(0));
        r.run(RackAction::ChainParam(id, controls::VOLUME, -30.0));
        assert_ne!(r.chain(id).mix.volume_db, -30.0);
        assert!(r.s.is_macro_mapped(r.root, target));
    }

    #[test]
    fn rename_color_and_info_text_land_on_the_chain_and_undo() {
        let mut r = rig(RackKind::Audio, &["A"]);
        let id = r.chains[0];
        r.s.commit_edits(false);
        r.run(RackAction::RenameChain(id, "Crunch".into()));
        r.s.commit_edits(false);
        assert_eq!(r.chain(id).name, "Crunch");
        assert!(r.s.undo());
        assert_eq!(r.chain(id).name, "A");
        r.run(RackAction::ChainColor(id, Some([1, 2, 3])));
        assert_eq!(r.chain(id).color, Some([1, 2, 3]));
        r.run(RackAction::ChainColor(id, None));
        assert_eq!(r.chain(id).color, None);
    }

    #[test]
    fn chains_are_added_deleted_grouped_and_copied() {
        let mut r = rig(RackKind::Audio, &["A", "B"]);
        r.run(RackAction::AddChain);
        assert_eq!(r.chain_names().len(), 3);
        let (a, b) = (r.chains[0], r.chains[1]);
        r.run(RackAction::DuplicateChains(vec![a]));
        assert_eq!(r.chain_names()[..2], ["A", "A"]);
        let copy = match &r.rack_device().kind {
            DeviceKind::Rack { chains, .. } => chains[1].clone(),
            _ => unreachable!(),
        };
        assert_ne!(copy.id, a, "a copy is its own chain");
        assert_ne!(copy.devices[0].id, r.compressor(a));
        r.run(RackAction::GroupChains(vec![a, b]));
        assert!(r.rack_device().chains().any(|c| matches!(
            c.devices.first().map(|d| &d.kind),
            Some(DeviceKind::Rack { .. })
        )));
        r.run(RackAction::DeleteChains(vec![copy.id]));
        assert!(r.rack_device().chains().all(|c| c.id != copy.id));
    }

    #[test]
    fn a_drop_moves_chains_within_a_rack() {
        let mut r = rig(RackKind::Audio, &["A", "B", "C"]);
        let (root, rack) = (r.root, r.rack);
        r.run(RackAction::MoveChains {
            from: root,
            from_rack: rack,
            ids: vec![r.chains[0]],
            index: 2,
        });
        assert_eq!(r.chain_names(), ["B", "C", "A"]);
    }

    #[test]
    fn the_drop_area_adds_a_chain_with_the_dropped_device_named_for_it() {
        let mut r = rig(RackKind::Audio, &[]);
        r.run(RackAction::AddChainWith(BrowserItem::Builtin(
            "rlive.utility",
        )));
        assert_eq!(r.chain_names(), ["Utility"]);
    }

    #[test]
    fn what_a_rack_refuses_leaves_no_empty_chain() {
        let mut r = rig(RackKind::Audio, &[]);
        r.run(RackAction::AddChainWith(BrowserItem::Builtin(
            "rlive.sampler",
        )));
        assert!(r.chain_names().is_empty());
    }

    #[test]
    fn a_zone_drag_writes_the_zone() {
        let mut r = rig(RackKind::Instrument, &["A"]);
        let id = r.chains[0];
        let z = Zone::span(10, 20);
        r.run(RackAction::Zone(id, ZoneKind::Key, z));
        assert_eq!(r.chain(id).zones.key, z);
    }

    #[test]
    fn map_mode_maps_the_clicked_parameter_to_the_macro_pressed() {
        let mut r = rig(RackKind::Audio, &["A"]);
        let target = r.target(r.chains[0]);
        r.run(RackAction::ToggleMap);
        assert_eq!(r.view.ui.map.map(|m| m.rack), Some(r.rack));
        r.run(RackAction::MapTarget(target));
        assert_eq!(r.view.ui.map.and_then(|m| m.selected), Some(target));
        r.run(RackAction::Map(2));
        assert!(r.s.is_macro_mapped(r.root, target));
        assert_eq!(r.view.ui.map.and_then(|m| m.selected), None);
        let d = r.s.macro_display(r.root, r.rack, 2);
        assert_ne!(d.name, "Macro 3", "named after the parameter");

        r.run(RackAction::Invert(2, 0));
        let macros = match &r.rack_device().kind {
            DeviceKind::Rack { macros, .. } => macros.clone(),
            _ => unreachable!(),
        };
        assert!(macros[2].mappings[0].min > macros[2].mappings[0].max);

        r.run(RackAction::Range(2, 0, -5.0, -1.0));
        r.run(RackAction::Unmap(2, 0));
        assert!(!r.s.is_macro_mapped(r.root, target));
        r.run(RackAction::ToggleMap);
        assert!(r.view.ui.map.is_none());
    }

    #[test]
    fn map_without_a_selection_says_so() {
        let mut r = rig(RackKind::Audio, &["A"]);
        let mut q = ActionQueue::default();
        q.push(r.root, Action::Rack(r.rack, RackAction::ToggleMap));
        q.push(r.root, Action::Rack(r.rack, RackAction::Map(0)));
        let mut status = String::new();
        q.apply(&mut r.s, &mut r.view, &mut status);
        assert!(status.contains("Select a parameter"), "{status}");
    }

    #[test]
    fn macros_are_named_coloured_and_counted() {
        let mut r = rig(RackKind::Audio, &["A"]);
        r.run(RackAction::MacroCount(3));
        assert_eq!(r.s.visible_macros(r.root, r.rack), 3);
        r.run(RackAction::MacroCount(MACRO_COUNT as u8 + 3));
        assert_eq!(r.s.visible_macros(r.root, r.rack), MACRO_COUNT as u8);
        r.run(RackAction::MacroName(0, "Drive".into()));
        assert_eq!(r.s.macro_display(r.root, r.rack, 0).name, "Drive");
        r.run(RackAction::MacroColor(0, Some([9, 9, 9])));
        r.run(RackAction::MacroValue(0, 100.0));
        assert_eq!(r.s.macro_value(r.root, r.rack, 0), 100.0);
        let m = match &r.rack_device().kind {
            DeviceKind::Rack { macros, .. } => macros[0].clone(),
            _ => unreachable!(),
        };
        assert_eq!(m.color, Some([9, 9, 9]));
    }

    #[test]
    fn rand_touches_only_mapped_macros_that_are_not_excluded() {
        let mut r = rig(RackKind::Audio, &["A"]);
        let target = r.target(r.chains[0]);
        r.run(RackAction::ToggleMap);
        r.run(RackAction::MapTarget(target));
        r.run(RackAction::Map(0));
        r.run(RackAction::MacroValue(1, 33.0));
        r.run(RackAction::Randomize);
        assert_eq!(r.s.macro_value(r.root, r.rack, 1), 33.0, "unmapped");
        r.run(RackAction::MacroExcludeRandom(0, true));
        let before = r.s.macro_value(r.root, r.rack, 0);
        r.run(RackAction::Randomize);
        assert_eq!(r.s.macro_value(r.root, r.rack, 0), before, "excluded");
    }

    #[test]
    fn variations_store_launch_overwrite_rename_duplicate_and_delete() {
        let mut r = rig(RackKind::Audio, &["A"]);
        r.run(RackAction::MacroValue(0, 10.0));
        r.run(RackAction::StoreVariation);
        assert_eq!(r.view.rack_views[&r.rack].selected_variation, Some(0));
        r.run(RackAction::MacroValue(0, 90.0));
        r.run(RackAction::LaunchVariation(0));
        assert_eq!(r.s.macro_value(r.root, r.rack, 0), 10.0);
        r.run(RackAction::MacroValue(0, 50.0));
        r.run(RackAction::OverwriteVariation(0));
        r.run(RackAction::MacroValue(0, 90.0));
        r.run(RackAction::LaunchVariation(0));
        assert_eq!(r.s.macro_value(r.root, r.rack, 0), 50.0);
        r.run(RackAction::RenameVariation(0, "Build".into()));
        r.run(RackAction::DuplicateVariation(0));
        let names = |r: &Rig| match &r.rack_device().kind {
            DeviceKind::Rack { variations, .. } => variations
                .iter()
                .map(|v| v.name.clone())
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        assert_eq!(names(&r), ["Build", "Build"]);
        r.run(RackAction::DeleteVariation(0));
        assert_eq!(names(&r).len(), 1);
    }

    #[test]
    fn a_drum_chain_takes_receive_play_and_choke() {
        let mut r = rig(RackKind::Drum, &["Kick"]);
        let id = r.chains[0];
        let pad = DrumPad {
            receive: None,
            play: 72,
            choke: Some(5),
        };
        r.run(RackAction::DrumPad(id, pad));
        assert_eq!(r.chain(id).zones.drum, Some(pad));
    }

    #[test]
    fn an_instrument_dropped_on_an_empty_pad_makes_a_chain_there() {
        let mut r = rig(RackKind::Drum, &[]);
        r.run(RackAction::PadDrop(
            40,
            BrowserItem::Builtin("rlive.sampler"),
        ));
        assert_eq!(r.chain_names(), ["Sampler"]);
        let c = r.s.chains_on_pad(r.root, r.rack, 40);
        assert_eq!(c.len(), 1);
        assert_eq!(r.chain(c[0]).devices.len(), 1);
        assert_eq!(r.view.rack_views[&r.rack].last_pad, Some(40));
    }

    #[test]
    fn an_effect_dropped_on_a_pad_goes_after_its_instrument() {
        let mut r = rig(RackKind::Drum, &[]);
        r.run(RackAction::PadDrop(
            40,
            BrowserItem::Builtin("rlive.sampler"),
        ));
        r.run(RackAction::PadDrop(
            40,
            BrowserItem::Builtin("rlive.utility"),
        ));
        let c = r.s.chains_on_pad(r.root, r.rack, 40);
        let names: Vec<_> = r
            .chain(c[0])
            .devices
            .iter()
            .map(|d| d.name.clone())
            .collect();
        assert_eq!(names.len(), 2);
        assert_eq!(names[0], "Sampler");
    }

    #[test]
    fn another_instrument_on_a_pad_replaces_only_the_instrument() {
        let mut r = rig(RackKind::Drum, &[]);
        r.run(RackAction::PadDrop(
            40,
            BrowserItem::Builtin("rlive.sampler"),
        ));
        r.run(RackAction::PadDrop(
            40,
            BrowserItem::Builtin("rlive.utility"),
        ));
        r.run(RackAction::PadDrop(
            40,
            BrowserItem::Builtin("rlive.operator"),
        ));
        let c = r.s.chains_on_pad(r.root, r.rack, 40);
        let devices = &r.chain(c[0]).devices;
        assert_eq!(devices.len(), 2);
        assert_ne!(devices[0].name, "Sampler", "the source was replaced");
        assert_eq!(devices[1].name, "Utility", "the effect stayed");
    }

    fn wav(tag: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("rlive-c15-{tag}-{}.wav", std::process::id()));
        rlive_devices::write_wav(&path, &[0.2; 64], &[0.2; 64], 48_000).unwrap();
        path
    }

    #[test]
    fn a_sample_on_an_empty_pad_makes_a_chain_with_a_loaded_sampler() {
        let mut r = rig(RackKind::Drum, &[]);
        let path = wav("empty");
        r.run(RackAction::PadDrop(38, BrowserItem::Sample(path.clone())));
        r.s.samples.wait_idle();
        r.s.idle();
        let c = r.s.chains_on_pad(r.root, r.rack, 38);
        let sampler = &r.chain(c[0]).devices[0];
        assert_eq!(
            sampler.sample(0).map(|f| f.path.clone()),
            Some(path.clone())
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_sample_on_a_full_pad_changes_only_the_sound_source() {
        let mut r = rig(RackKind::Drum, &[]);
        let (one, two) = (wav("one"), wav("two"));
        r.run(RackAction::PadDrop(38, BrowserItem::Sample(one.clone())));
        r.run(RackAction::PadDrop(
            38,
            BrowserItem::Builtin("rlive.utility"),
        ));
        r.run(RackAction::PadDrop(38, BrowserItem::Sample(two.clone())));
        r.s.samples.wait_idle();
        r.s.idle();
        let c = r.s.chains_on_pad(r.root, r.rack, 38);
        assert_eq!(c.len(), 1, "no new chain");
        let devices = &r.chain(c[0]).devices;
        assert_eq!(devices.len(), 2, "the effect stayed");
        assert_eq!(
            devices[0].sample(0).map(|f| f.path.clone()),
            Some(two.clone())
        );
        std::fs::remove_file(&one).ok();
        std::fs::remove_file(&two).ok();
    }

    #[test]
    fn several_samples_map_chromatically_or_layer_on_one_pad() {
        let mut r = rig(RackKind::Drum, &[]);
        let (a, b) = (wav("a"), wav("b"));
        r.s.drop_samples_on_pad(r.root, r.rack, 40, &[a.clone(), b.clone()], false);
        assert_eq!(r.s.chains_on_pad(r.root, r.rack, 40).len(), 1);
        assert_eq!(r.s.chains_on_pad(r.root, r.rack, 41).len(), 1);

        r.s.drop_samples_on_pad(r.root, r.rack, 50, &[a.clone(), b.clone()], true);
        let c = r.s.chains_on_pad(r.root, r.rack, 50);
        assert_eq!(c.len(), 1, "one chain on the pad");
        let inner = r.chain(c[0]).devices.first().unwrap();
        assert!(matches!(
            inner.kind,
            DeviceKind::Rack {
                kind: RackKind::Instrument,
                ..
            }
        ));
        assert_eq!(inner.chains().count(), 2);
        r.s.samples.wait_idle();
        std::fs::remove_file(&a).ok();
        std::fs::remove_file(&b).ok();
    }

    #[test]
    fn swapping_pads_swaps_their_notes() {
        let mut r = rig(RackKind::Drum, &["Kick", "Snare"]);
        r.run(RackAction::PadSwap(36, 37));
        let (kick, snare) = (r.chains[0], r.chains[1]);
        assert_eq!(r.chain(kick).zones.drum.unwrap().receive, Some(37));
        assert_eq!(r.chain(snare).zones.drum.unwrap().receive, Some(36));
    }

    #[test]
    fn layering_pads_nests_both_in_an_instrument_rack_on_the_target() {
        let mut r = rig(RackKind::Drum, &["Kick", "Snare"]);
        r.run(RackAction::PadLayer(37, 36));
        let on_pad = r.s.chains_on_pad(r.root, r.rack, 36);
        assert_eq!(on_pad.len(), 1);
        assert!(r.s.chains_on_pad(r.root, r.rack, 37).is_empty());
        let wrapper = r.chain(on_pad[0]).clone();
        let inner = &wrapper.devices[0];
        assert!(matches!(
            inner.kind,
            DeviceKind::Rack {
                kind: RackKind::Instrument,
                ..
            }
        ));
        assert_eq!(inner.chains().count(), 2);
        assert!(inner.chains().all(|c| c.zones.drum.is_none()));
    }

    #[test]
    fn layering_onto_an_empty_pad_just_moves_the_pad() {
        let mut r = rig(RackKind::Drum, &["Kick"]);
        r.run(RackAction::PadLayer(36, 50));
        assert_eq!(r.s.chains_on_pad(r.root, r.rack, 50).len(), 1);
        assert!(r.s.chains_on_pad(r.root, r.rack, 36).is_empty());
    }

    #[test]
    fn pad_view_paging_and_return_chains() {
        let mut r = rig(RackKind::Drum, &["Kick"]);
        r.run(RackAction::PadStart(52));
        assert_eq!(r.view.rack_views[&r.rack].pad_start, 52);
        r.run(RackAction::AddReturn);
        match &r.rack_device().kind {
            DeviceKind::Rack { returns, .. } => {
                assert_eq!(returns.len(), 1);
                assert_eq!(returns[0].name, "Return A");
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn a_chain_extracted_from_the_mixer_makes_a_track() {
        let mut r = rig(RackKind::Drum, &["Kick", "Snare"]);
        let before = r.s.project.tracks.len();
        let (root, rack) = (r.root, r.rack);
        let id = r.chains[0];
        r.run(RackAction::Extract(ChainDrag {
            at: root,
            rack,
            ids: vec![id],
            from_mixer: true,
            is_return: false,
        }));
        assert_eq!(r.s.project.tracks.len(), before + 1);
        assert_eq!(r.chain_names(), ["Snare"]);
    }
}
