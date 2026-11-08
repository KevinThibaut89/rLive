//! Walks a device chain, racks and all, into a plan of what to draw. Reads the project and the
//! runtimes; changes nothing and clones no device.

use super::layout::{self, SectionPlan};
use crate::session::{ChainRef, DeviceRuntime};
use rlive_core::{ChainId, DeviceId, ParamId, ParamOwner, ParamRef, Section};
use rlive_project::{Chain, Device, DeviceKind};
use std::collections::{HashMap, HashSet};

/// The width of a rack's chain list and of the drop zone at the end of one of its chains.
pub const CHAIN_LIST_W: f32 = 112.0;
pub const INNER_DROP_W: f32 = 90.0;
/// The gap between devices in a chain.
pub const DEVICE_GAP: f32 = 4.0;
/// Width of a rack's body, beyond the chain list and the devices in it.
const RACK_MARGIN: f32 = 12.0;
/// How much shorter a rack's body is than its card.
const RACK_INSET_H: f32 = 30.0;

/// What a plan is made from.
pub struct PlanInput<'a> {
    /// The root chain of the owner of the chain being planned, for the racks above it.
    pub root: &'a [Device],
    pub runtimes: &'a HashMap<DeviceId, DeviceRuntime>,
    /// Devices showing all their parameters, past the limit.
    pub show_all: &'a HashSet<DeviceId>,
    /// The chain each rack has open.
    pub open_chains: &'a HashMap<DeviceId, ChainId>,
}

/// A device and how it's laid out.
pub struct DeviceNode<'a> {
    pub dev: &'a Device,
    pub rt: Option<&'a DeviceRuntime>,
    pub width: f32,
    /// How tall the device is drawn.
    pub height: f32,
    pub body: NodeBody<'a>,
}

pub enum NodeBody<'a> {
    Folded,
    Controls(ControlPlan<'a>),
    /// `None` when the rack has no chains.
    Rack(Option<OpenChain<'a>>),
}

/// The controls of an ordinary device.
pub struct ControlPlan<'a> {
    pub sections: Vec<SectionPlan<'a>>,
    /// How many of a device's parameters would not fit under [`layout::MAX_PARAMS`].
    pub truncated: bool,
    /// How many parameters the device has, visible ones only.
    pub visible: usize,
    /// Macro-owned parameters of this device.
    pub macro_owned: HashSet<ParamId>,
}

/// The chain a rack has open and the devices in it.
pub struct OpenChain<'a> {
    pub at: ChainRef,
    pub chain: &'a Chain,
    pub devices: Vec<DeviceNode<'a>>,
}

/// The chain to show for a rack: the one the user chose if it's still there, or the first.
pub fn open_chain<'a>(rack: &'a Device, open: &HashMap<DeviceId, ChainId>) -> Option<&'a Chain> {
    let chosen = open.get(&rack.id);
    rack.chains()
        .find(|c| Some(&c.id) == chosen)
        .or_else(|| rack.chains().next())
}

/// Plan every device in a chain. `height` is the height they're all drawn at.
pub fn plan_chain<'a>(
    at: ChainRef,
    devices: &'a [Device],
    input: &PlanInput<'a>,
    height: f32,
) -> Vec<DeviceNode<'a>> {
    let mut owned = Vec::new();
    collect_ancestor_macros(at, input.root, &mut owned);
    plan_devices(at, devices, input, height, &mut owned)
}

/// A rack's macros own parameters of the devices inside it, so a chain drawn on its own still
/// needs the macros of the racks above it. A chain reached by walking collects them on the way
/// down instead.
fn collect_ancestor_macros(at: ChainRef, root: &[Device], owned: &mut Vec<ParamRef>) {
    let ChainRef::Nested(addr) = at else {
        return;
    };
    let mut devices = root;
    for step in addr.path.steps() {
        let Some(rack) = devices.iter().find(|d| d.id == step.rack) else {
            return;
        };
        owned.extend(macro_targets(rack));
        let Some(chain) = rack.chains().find(|c| c.id == step.chain) else {
            return;
        };
        devices = &chain.devices;
    }
}

/// The parameters a rack's macros own.
fn macro_targets(dev: &Device) -> impl Iterator<Item = ParamRef> + '_ {
    let macros = match &dev.kind {
        DeviceKind::Rack { macros, .. } => macros.as_slice(),
        _ => &[],
    };
    macros
        .iter()
        .flat_map(|m| m.mappings.iter().map(|map| map.target))
}

fn plan_devices<'a>(
    at: ChainRef,
    devices: &'a [Device],
    input: &PlanInput<'a>,
    height: f32,
    owned: &mut Vec<ParamRef>,
) -> Vec<DeviceNode<'a>> {
    devices
        .iter()
        .map(|dev| plan_device(at, dev, input, height, owned))
        .collect()
}

fn plan_device<'a>(
    at: ChainRef,
    dev: &'a Device,
    input: &PlanInput<'a>,
    height: f32,
    owned: &mut Vec<ParamRef>,
) -> DeviceNode<'a> {
    let rt = input.runtimes.get(&dev.id);
    if !dev.expanded {
        return DeviceNode {
            dev,
            rt,
            width: layout::FOLDED_W,
            height,
            body: NodeBody::Folded,
        };
    }
    if matches!(dev.kind, DeviceKind::Rack { .. }) {
        return plan_rack(at, dev, rt, input, height, owned);
    }
    let params = rt.map_or(&[][..], |r| r.params.as_slice());
    let index = layout::param_index(params);
    let is_plugin = matches!(dev.kind, DeviceKind::Plugin { .. });
    let declared: &[Section] = rt.map_or(&[][..], |r| r.layout.as_slice());
    let (auto, truncated) = layout::auto_section(params, input.show_all.contains(&dev.id));
    let auto_truncated = declared.is_empty() && truncated;
    let extra_h = extra_rows_h(rt, auto_truncated);
    let body_h = layout::body_height(height, extra_h);
    let sections = if declared.is_empty() {
        vec![layout::plan_section(&auto, body_h, &index)]
    } else {
        declared
            .iter()
            .map(|s| layout::plan_section(s, body_h, &index))
            .collect()
    };
    let width = layout::device_width(&sections, is_plugin);
    let device = ParamOwner::Device(dev.id);
    let macro_owned = owned
        .iter()
        .filter(|r| r.owner == device)
        .map(|r| r.id)
        .collect();
    DeviceNode {
        dev,
        rt,
        width,
        height,
        body: NodeBody::Controls(ControlPlan {
            sections,
            truncated: auto_truncated,
            visible: index.len(),
            macro_owned,
        }),
    }
}

fn plan_rack<'a>(
    at: ChainRef,
    dev: &'a Device,
    rt: Option<&'a DeviceRuntime>,
    input: &PlanInput<'a>,
    height: f32,
    owned: &mut Vec<ParamRef>,
) -> DeviceNode<'a> {
    let inner_h = height - RACK_INSET_H;
    let mark = owned.len();
    owned.extend(macro_targets(dev));
    let open = open_chain(dev, input.open_chains).and_then(|chain| {
        let child = at.child(dev.id, chain.id)?;
        let devices = plan_devices(child, &chain.devices, input, inner_h, owned);
        Some(OpenChain {
            at: child,
            chain,
            devices,
        })
    });
    owned.truncate(mark);
    let inner_w: f32 = open.as_ref().map_or(0.0, |c| {
        c.devices.iter().map(|d| d.width + DEVICE_GAP).sum()
    });
    let width = CHAIN_LIST_W + DEVICE_GAP + inner_w + INNER_DROP_W + RACK_MARGIN;
    DeviceNode {
        dev,
        rt,
        width,
        height,
        body: NodeBody::Rack(open),
    }
}

/// Rows drawn over a device's sections: an error, its sample slots, the "show all" switch.
fn extra_rows_h(rt: Option<&DeviceRuntime>, truncated: bool) -> f32 {
    let mut h = 0.0;
    if rt.is_some_and(|r| r.error.is_some()) {
        h += 34.0;
    }
    h += rt.map_or(0, |r| r.sample_slots) as f32 * 24.0;
    if truncated {
        h += 20.0;
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Session;
    use rlive_core::{ParamFlags, ParamInfo};
    use rlive_engine::create_engine;
    use rlive_plugin_host::scanner::PluginCache;
    use rlive_project::{Macro, MacroMapping, Project, RackKind, Track, TrackKind};

    fn session(devices: Vec<Device>) -> (rlive_engine::Engine, Session) {
        let (engine, handle) = create_engine(48_000.0, 512);
        let mut p = Project::empty();
        let mut t = Track::new("T", TrackKind::Midi, [1, 2, 3], p.scenes.len());
        t.devices = devices;
        p.tracks = vec![t];
        (engine, Session::new(handle, p, PluginCache::default()))
    }

    fn rack(name: &str, chains: Vec<Chain>) -> Device {
        let mut d = Device::rack(RackKind::Audio, name);
        if let DeviceKind::Rack { chains: c, .. } = &mut d.kind {
            *c = chains;
        }
        d
    }

    fn chain(name: &str, devices: Vec<Device>) -> Chain {
        let mut c = Chain::new(name);
        c.devices = devices;
        c
    }

    fn compressor() -> Device {
        Device::builtin("rlive.compressor", "Compressor")
    }

    /// `Outer -> [A -> [Inner -> [B -> [Compressor]]], X -> []]`.
    fn nested() -> Device {
        rack(
            "Outer",
            vec![
                chain(
                    "A",
                    vec![rack("Inner", vec![chain("B", vec![compressor()])])],
                ),
                chain("X", Vec::new()),
            ],
        )
    }

    fn root(s: &Session) -> ChainRef {
        ChainRef::Track(s.project.tracks[0].id)
    }

    /// Runs `f` on the plan of the first track's chain.
    fn with_plan<R>(s: &Session, f: impl FnOnce(&[DeviceNode]) -> R) -> R {
        let at = root(s);
        let (show_all, open_chains) = (HashSet::new(), HashMap::new());
        let input = PlanInput {
            root: s.devices(at),
            runtimes: &s.runtimes,
            show_all: &show_all,
            open_chains: &open_chains,
        };
        f(&plan_chain(at, s.devices(at), &input, 200.0))
    }

    fn opened<'p, 'a>(node: &'p DeviceNode<'a>) -> &'p OpenChain<'a> {
        match &node.body {
            NodeBody::Rack(Some(open)) => open,
            _ => panic!("not an open rack"),
        }
    }

    fn controls<'p, 'a>(node: &'p DeviceNode<'a>) -> &'p ControlPlan<'a> {
        match &node.body {
            NodeBody::Controls(c) => c,
            _ => panic!("no controls"),
        }
    }

    #[test]
    fn the_first_chain_opens_unless_another_was_chosen_and_still_exists() {
        let d = nested();
        let ids: Vec<ChainId> = d.chains().map(|c| c.id).collect();
        let mut open = HashMap::new();
        assert_eq!(open_chain(&d, &open).unwrap().id, ids[0]);
        open.insert(d.id, ids[1]);
        assert_eq!(open_chain(&d, &open).unwrap().id, ids[1]);
        open.insert(d.id, ChainId::new());
        assert_eq!(open_chain(&d, &open).unwrap().id, ids[0], "a stale choice");
        assert!(open_chain(&rack("Empty", vec![]), &open).is_none());
    }

    #[test]
    fn walking_descends_into_open_chains_and_addresses_each_level() {
        let d = nested();
        let (outer, a) = (d.id, d.chains().next().unwrap());
        let (inner, b) = (a.devices[0].id, a.devices[0].chains().next().unwrap().id);
        let a = a.id;
        let (_e, s) = session(vec![d]);
        let at = root(&s);
        with_plan(&s, |nodes| {
            let first = opened(&nodes[0]);
            assert_eq!(first.at, at.child(outer, a).unwrap());
            assert_eq!(first.chain.name, "A");
            let second = opened(&first.devices[0]);
            assert_eq!(second.at.depth(), 2);
            assert_eq!(
                second.at,
                at.child(outer, a).unwrap().child(inner, b).unwrap()
            );
            assert_eq!(second.devices[0].dev.name, "Compressor");
            assert!(matches!(second.devices[0].body, NodeBody::Controls(_)));
        });
    }

    #[test]
    fn each_level_of_rack_gets_shorter_devices() {
        let (_e, s) = session(vec![nested()]);
        with_plan(&s, |nodes| {
            let first = &opened(&nodes[0]).devices[0];
            assert_eq!(first.height, 200.0 - RACK_INSET_H);
            assert_eq!(opened(first).devices[0].height, 200.0 - 2.0 * RACK_INSET_H);
        });
    }

    #[test]
    fn a_rack_is_as_wide_as_what_it_holds() {
        let (_e, s) = session(vec![nested()]);
        with_plan(&s, |nodes| {
            let inner = opened(&nodes[0]).devices[0].width;
            assert_eq!(
                nodes[0].width,
                CHAIN_LIST_W + DEVICE_GAP + inner + DEVICE_GAP + INNER_DROP_W + RACK_MARGIN
            );
        });
    }

    #[test]
    fn a_folded_rack_is_a_folded_card_with_nothing_walked() {
        let mut d = nested();
        d.expanded = false;
        let (_e, s) = session(vec![d]);
        with_plan(&s, |nodes| {
            assert_eq!(nodes[0].width, layout::FOLDED_W);
            assert!(matches!(nodes[0].body, NodeBody::Folded));
        });
    }

    #[test]
    fn a_rack_with_no_chains_still_plans() {
        let (_e, s) = session(vec![rack("Empty", vec![])]);
        with_plan(&s, |nodes| {
            assert!(matches!(nodes[0].body, NodeBody::Rack(None)));
        });
    }

    #[test]
    fn a_macro_owns_parameters_of_devices_inside_its_rack_at_any_depth() {
        let mut d = nested();
        let comp = d.chains().next().unwrap().devices[0]
            .chains()
            .next()
            .unwrap()
            .devices[0]
            .id;
        if let DeviceKind::Rack { macros, .. } = &mut d.kind {
            macros.push(Macro {
                name: "M".into(),
                value: 0.0,
                mappings: vec![MacroMapping {
                    target: ParamRef {
                        owner: ParamOwner::Device(comp),
                        id: ParamId(0),
                    },
                    min: 0.0,
                    max: 1.0,
                }],
                exclude_random: false,
                exclude_variations: false,
            });
        }
        let (_e, s) = session(vec![d]);
        with_plan(&s, |nodes| {
            let c = opened(&opened(&nodes[0]).devices[0])
                .devices
                .first()
                .unwrap();
            let owned = &controls(c).macro_owned;
            assert!(owned.contains(&ParamId(0)));
            assert!(!owned.contains(&ParamId(1)));
        });
    }

    #[test]
    fn a_chain_drawn_on_its_own_still_knows_the_macros_above_it() {
        let mut d = nested();
        let comp = d.chains().next().unwrap().devices[0]
            .chains()
            .next()
            .unwrap()
            .devices[0]
            .id;
        let rack_id = d.id;
        let (a, b, inner) = {
            let a = d.chains().next().unwrap();
            let inner = &a.devices[0];
            (a.id, inner.chains().next().unwrap().id, inner.id)
        };
        if let DeviceKind::Rack { macros, .. } = &mut d.kind {
            macros.push(Macro {
                name: "M".into(),
                value: 0.0,
                mappings: vec![MacroMapping {
                    target: ParamRef {
                        owner: ParamOwner::Device(comp),
                        id: ParamId(3),
                    },
                    min: 0.0,
                    max: 1.0,
                }],
                exclude_random: false,
                exclude_variations: false,
            });
        }
        let (_e, s) = session(vec![d]);
        let at = root(&s).child(rack_id, a).unwrap().child(inner, b).unwrap();
        let (show_all, open_chains) = (HashSet::new(), HashMap::new());
        let input = PlanInput {
            root: s.devices(at.owner()),
            runtimes: &s.runtimes,
            show_all: &show_all,
            open_chains: &open_chains,
        };
        let nodes = plan_chain(at, s.devices(at), &input, 200.0);
        assert!(controls(&nodes[0]).macro_owned.contains(&ParamId(3)));
    }

    #[test]
    fn a_stub_gets_an_empty_auto_panel() {
        let (_e, s) = session(vec![Device::builtin("rlive.ds_kick", "DS Kick")]);
        with_plan(&s, |nodes| {
            assert_eq!(controls(&nodes[0]).visible, 0);
            assert_eq!(nodes[0].width, 150.0);
        });
    }

    #[test]
    fn hidden_parameters_are_left_out_of_an_auto_panel() {
        let (_e, mut s) = session(vec![compressor()]);
        let id = s.devices(root(&s))[0].id;
        let rt = s.runtimes.get_mut(&id).unwrap();
        rt.layout.clear();
        rt.params = vec![
            ParamInfo::continuous(0, "A", 0.0, 1.0, 0.0, ""),
            ParamInfo::continuous(1, "B", 0.0, 1.0, 0.0, "").with_flags(ParamFlags::HIDDEN),
        ];
        with_plan(&s, |nodes| {
            assert_eq!(controls(&nodes[0]).sections[0].items.len(), 1);
            assert_eq!(controls(&nodes[0]).visible, 1);
        });
    }

    #[test]
    fn a_declared_layout_is_used_as_declared() {
        let (_e, s) = session(vec![compressor()]);
        with_plan(&s, |nodes| {
            let titles: Vec<&str> = controls(&nodes[0])
                .sections
                .iter()
                .map(|p| p.title)
                .collect();
            assert_eq!(titles, ["Dynamics", "Output"]);
        });
    }

    #[test]
    fn the_extra_rows_make_room_for_errors_samples_and_the_show_all_switch() {
        let (_e, mut s) = session(vec![Device::builtin("rlive.sampler", "Sampler")]);
        let id = s.devices(root(&s))[0].id;
        assert_eq!(extra_rows_h(s.runtimes.get(&id), false), 24.0);
        s.runtimes.get_mut(&id).unwrap().error = Some("oops".into());
        assert_eq!(extra_rows_h(s.runtimes.get(&id), false), 58.0);
        assert_eq!(extra_rows_h(s.runtimes.get(&id), true), 78.0);
    }
}
