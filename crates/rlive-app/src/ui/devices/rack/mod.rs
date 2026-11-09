//! A rack in the Device View (manual ch. 25): its view column, Macro Controls and their
//! variations, Chain List, zone editors, Drum Rack pads and the devices of the selected chain.
//!
//! Like the rest of the device view, drawing only reads the session. What the user does queues up
//! as [`RackAction`]s and applies after the frame, in [`actions`].

mod actions;
pub(crate) mod chain_list;
pub(crate) mod macros;
pub(crate) mod mapping_browser;
pub(crate) mod pads;
pub(crate) mod selection;
pub(crate) mod zones;

pub(crate) use actions::{apply, RackAction};

use super::tree::{DeviceNode, OpenChain, DEVICE_GAP, INNER_DROP_W};
use super::{draw_chain, Action, DrawCx};
use crate::session::{ChainRef, ZoneKind};
use crate::ui::theme::{self, pal, shade};
use crate::ui::widgets::info;
use egui::{Align2, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use rlive_core::param::ParamRef;
use rlive_core::{ChainId, DeviceId};
use rlive_project::{Chain, Device, DeviceKind, Macro, RackKind, Variation};
use selection::ChainActivity;
use std::collections::{HashMap, HashSet};

/// The view column on a rack's far left.
pub const VIEW_W: f32 = 20.0;
const GAP: f32 = 4.0;

/// The sections of a rack that the view column shows and hides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RackSection {
    Macros,
    Variations,
    Chains,
    Pads,
    Devices,
    InOut,
    Sends,
    Returns,
}

/// What one rack shows, and where it is in its lists.
#[derive(Debug, Clone, PartialEq)]
pub struct RackView {
    pub macros: bool,
    pub variations: bool,
    pub chains: bool,
    pub pads: bool,
    pub devices: bool,
    pub zones: Option<ZoneKind>,
    pub in_out: bool,
    pub sends: bool,
    pub returns: bool,
    pub auto_select: bool,
    /// The selected chains, in the order they were selected.
    pub selection: Vec<ChainId>,
    pub anchor: Option<ChainId>,
    /// The lowest note on the visible page of pads.
    pub pad_start: u8,
    pub last_pad: Option<u8>,
    pub selected_variation: Option<usize>,
}

impl RackView {
    /// How a rack of this kind starts out.
    pub fn for_kind(kind: RackKind) -> Self {
        let drum = kind == RackKind::Drum;
        Self {
            macros: !drum,
            variations: false,
            chains: !drum,
            pads: drum,
            devices: true,
            zones: None,
            in_out: drum,
            sends: drum,
            returns: drum,
            auto_select: false,
            selection: Vec::new(),
            anchor: None,
            pad_start: 36,
            last_pad: None,
            selected_variation: None,
        }
    }

    fn set(&mut self, section: RackSection, on: bool) {
        match section {
            RackSection::Macros => self.macros = on,
            RackSection::Variations => self.variations = on,
            RackSection::Chains => self.chains = on,
            RackSection::Pads => self.pads = on,
            RackSection::Devices => self.devices = on,
            RackSection::InOut => self.in_out = on,
            RackSection::Sends => self.sends = on,
            RackSection::Returns => self.returns = on,
        }
    }

    pub fn is_on(&self, section: RackSection) -> bool {
        match section {
            RackSection::Macros => self.macros,
            RackSection::Variations => self.variations,
            RackSection::Chains => self.chains,
            RackSection::Pads => self.pads,
            RackSection::Devices => self.devices,
            RackSection::InOut => self.in_out,
            RackSection::Sends => self.sends,
            RackSection::Returns => self.returns,
        }
    }

    /// Whether anything but the view column is on show: when nothing is, a rack folds down to it
    /// (manual p. 494).
    pub fn all_hidden(&self, kind: RackKind) -> bool {
        !(self.macros
            || self.variations
            || self.chains
            || self.devices
            || (kind == RackKind::Drum && self.pads))
    }
}

/// How wide each part of a rack is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RackWidths {
    pub macros: f32,
    pub variations: f32,
    pub pads: f32,
    pub list: f32,
    pub zones: f32,
}

impl RackWidths {
    pub fn of(kind: RackKind, view: &RackView, visible_macros: u8, returns: usize) -> Self {
        let drum = kind == RackKind::Drum;
        Self {
            macros: if view.macros {
                macros::section_width(visible_macros)
            } else {
                0.0
            },
            variations: if view.variations {
                macros::VARIATIONS_W
            } else {
                0.0
            },
            pads: if drum && view.pads { PADS_W } else { 0.0 },
            list: if view.chains {
                chain_list::list_width(kind, view, returns)
            } else {
                0.0
            },
            zones: if view.chains && !drum && view.zones.is_some() {
                ZONES_W
            } else {
                0.0
            },
        }
    }

    /// Everything but the devices, gaps included.
    pub fn chrome(&self) -> f32 {
        let sections = [
            self.macros,
            self.variations,
            self.pads,
            self.list,
            self.zones,
        ];
        VIEW_W
            + sections
                .iter()
                .filter(|w| **w > 0.0)
                .map(|w| w + GAP)
                .sum::<f32>()
    }
}

const PADS_W: f32 = 240.0;
const ZONES_W: f32 = 320.0;

/// A pending rename, and the text so far.
#[derive(Debug, Clone, PartialEq)]
pub struct Rename {
    pub target: RenameTarget,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameTarget {
    Chain(DeviceId, ChainId),
    Macro(DeviceId, usize),
    Variation(DeviceId, usize),
    Device(DeviceId),
}

/// Macro Map Mode (manual p. 505).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapMode {
    pub chain: ChainRef,
    pub rack: DeviceId,
    /// The parameter clicked, waiting for a macro's Map button.
    pub selected: Option<ParamRef>,
}

/// A Preview note being held down.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ActivePreview {
    pub chain: ChainRef,
    pub rack: DeviceId,
    pub target: Option<ChainId>,
    pub key: u8,
}

/// Where the arrow keys go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyFocus {
    ChainList(DeviceId),
    Pads(DeviceId),
}

/// A chain being dragged, from the chain list or from the mixer, where it brings its notes along
/// if it leaves for a new track (manual p. 509).
#[derive(Debug, Clone, PartialEq)]
pub struct ChainDrag {
    pub at: ChainRef,
    pub rack: DeviceId,
    pub ids: Vec<ChainId>,
    pub from_mixer: bool,
    pub is_return: bool,
}

/// What only matters while the rack is on screen.
#[derive(Default)]
pub struct RackUi {
    /// Where the last device title was drawn, for tests to click on.
    #[cfg(test)]
    pub last_title: Option<egui::Rect>,
    /// A plug-in menu asked for the MPE/Multi-channel Settings dialog.
    pub mpe_settings: Option<crate::ui::midi_settings::MpeTarget>,
    pub rename: Option<Rename>,
    pub zone_drag: Option<zones::ZoneDrag>,
    pub map: Option<MapMode>,
    pub preview: Option<ActivePreview>,
    pub key_focus: Option<KeyFocus>,
    pub list_scroll: HashMap<DeviceId, f32>,
    pub activity: HashMap<ChainId, ChainActivity>,
}

/// A rack and how it's shown.
pub struct RackRef<'a> {
    pub chain: ChainRef,
    pub dev: &'a Device,
    pub kind: RackKind,
    pub chains: &'a [Chain],
    pub returns: &'a [Chain],
    pub macros: &'a [Macro],
    pub visible_macros: u8,
    pub variations: &'a [Variation],
    pub select_filters_ctrl: bool,
    pub view: RackView,
}

impl<'a> RackRef<'a> {
    pub fn of(
        chain: ChainRef,
        dev: &'a Device,
        views: &HashMap<DeviceId, RackView>,
    ) -> Option<Self> {
        let DeviceKind::Rack {
            kind,
            chains,
            returns,
            macros,
            visible_macros,
            select_filters_ctrl,
            variations,
        } = &dev.kind
        else {
            return None;
        };
        Some(Self {
            chain,
            dev,
            kind: *kind,
            chains,
            returns,
            macros,
            visible_macros: *visible_macros,
            variations,
            select_filters_ctrl: *select_filters_ctrl,
            view: views
                .get(&dev.id)
                .cloned()
                .unwrap_or_else(|| RackView::for_kind(*kind)),
        })
    }

    /// The chains as listed, return chains after the rest.
    pub fn order(&self) -> Vec<ChainId> {
        self.chains.iter().map(|c| c.id).collect()
    }
}

/// Which of a rack's chains are processing this frame.
#[derive(Default)]
pub struct ChainActivityView {
    active: HashSet<ChainId>,
}

impl ChainActivityView {
    pub fn is_active(&self, id: ChainId) -> bool {
        self.active.contains(&id)
    }
}

/// Reads what the chains publish and works out which are processing.
fn read_activity(cx: &mut DrawCx, r: &RackRef, now: f64) -> ChainActivityView {
    let mut active = HashSet::new();
    let audio = r.kind == RackKind::Audio;
    for c in r.chains.iter().chain(r.returns) {
        let Some(shared) = cx.session.chain_shared(c.id) else {
            continue;
        };
        let pulse = shared.pulse.load(std::sync::atomic::Ordering::Relaxed);
        let held = shared.held.load(std::sync::atomic::Ordering::Relaxed);
        let signal = audio && shared.meter.left.load().max(shared.meter.right.load()) > 1e-4;
        if cx
            .ui
            .activity
            .entry(c.id)
            .or_default()
            .update(pulse, held, signal, now)
        {
            active.insert(c.id);
        }
    }
    ChainActivityView { active }
}

/// Draws a rack's body, under its title bar.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    ui: &mut egui::Ui,
    cx: &mut DrawCx,
    chain: ChainRef,
    node: &DeviceNode,
    rect: Rect,
    title: Rect,
    open: Option<&OpenChain>,
    on: bool,
) {
    let dev = node.dev;
    let Some(r) = RackRef::of(chain, dev, cx.rack_views) else {
        return;
    };
    let body = Rect::from_min_max(
        Pos2::new(rect.left() + 3.0, title.bottom() + 4.0),
        rect.max - Vec2::new(4.0, 4.0),
    );
    let mut bu = super::body_ui(ui, body);
    if !on {
        bu.multiply_opacity(0.55);
    }
    if let Some(err) = node.rt.and_then(|r| r.error.as_ref()) {
        bu.label(RichText::new(err).color(pal().arm_red).size(10.5));
    }
    let now = ui.input(|i| i.time);
    let activity = read_activity(cx, &r, now);
    if r.view.auto_select {
        let active: Vec<ChainId> = r
            .order()
            .into_iter()
            .filter(|id| activity.is_active(*id))
            .collect();
        if let Some(selection) = selection::auto_select(&r.view.selection, &active) {
            cx.queue
                .push(chain, Action::Rack(dev.id, RackAction::Select(selection)));
        }
    }

    let widths = RackWidths::of(r.kind, &r.view, r.visible_macros, r.returns.len());
    let mut x = body.left();
    let section = |x: &mut f32, w: f32| {
        let s = Rect::from_min_size(Pos2::new(*x, body.top()), Vec2::new(w, body.height()));
        *x += w + GAP;
        s
    };
    view_column(&mut bu, cx, &r, section(&mut x, VIEW_W));
    if r.view.all_hidden(r.kind) {
        slim_title(
            &bu,
            dev,
            Rect::from_min_size(Pos2::new(x, body.top()), Vec2::new(14.0, body.height())),
        );
        bracket(&bu, rect, false);
        return;
    }
    if widths.macros > 0.0 {
        let s = section(&mut x, widths.macros);
        macros::section(&mut bu, cx, &r, s);
    }
    if widths.variations > 0.0 {
        let s = section(&mut x, widths.variations);
        macros::variations(&mut bu, cx, &r, s);
    }
    if widths.pads > 0.0 {
        let s = section(&mut x, widths.pads);
        pads::view(&mut bu, cx, &r, s);
    }
    if widths.list > 0.0 {
        let s = section(&mut x, widths.list);
        chain_list::section(&mut bu, cx, &r, s, &activity, open.map(|o| o.chain.id));
    }
    if widths.zones > 0.0 {
        let s = section(&mut x, widths.zones);
        if let Some(kind) = r.view.zones {
            let scroll = cx.ui.list_scroll.get(&dev.id).copied().unwrap_or(0.0);
            // The rows line up with the chain list, below its own header.
            let rows = Rect::from_min_max(
                Pos2::new(s.left(), s.top() + chain_list::HEADER_H - zones::RULER_H),
                Pos2::new(s.right(), s.bottom() - chain_list::DROP_H),
            );
            zones::editor(&mut bu, cx, &r, kind, rows, scroll, &activity);
        }
    }
    if r.view.devices {
        let inner = Rect::from_min_max(
            Pos2::new(x, body.top()),
            Pos2::new(body.right() - 6.0, body.bottom()),
        );
        devices_area(&mut bu, cx, &r, inner, open);
        bracket(&bu, rect, true);
    } else {
        bracket(&bu, rect, false);
    }
}

fn devices_area(
    ui: &mut egui::Ui,
    cx: &mut DrawCx,
    r: &RackRef,
    inner: Rect,
    open: Option<&OpenChain>,
) {
    let mut iu = ui.new_child(egui::UiBuilder::new().max_rect(inner));
    if r.view.selection.len() > 1 {
        iu.painter().text(
            inner.center(),
            Align2::CENTER_CENTER,
            format!("{} Chains Selected", r.view.selection.len()),
            theme::font(11.0),
            pal().text_faint,
        );
        return;
    }
    match open {
        Some(open) => {
            iu.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = DEVICE_GAP;
                draw_chain(ui, cx, open.at, &open.devices, inner.height(), INNER_DROP_W);
            });
        }
        None => {
            iu.painter().text(
                inner.center(),
                Align2::CENTER_CENTER,
                "No chains",
                theme::font(11.0),
                pal().text_faint,
            );
        }
    }
}

/// When every view is hidden the title bar folds into the view column.
fn slim_title(ui: &egui::Ui, dev: &Device, rect: Rect) {
    let p = ui.painter();
    let galley = p.layout_no_wrap(dev.name.clone(), theme::font_bold(11.0), pal().text);
    let pos = Pos2::new(
        rect.center().x - galley.size().y / 2.0,
        rect.top() + 8.0 + galley.size().x,
    );
    p.add(
        egui::epaint::TextShape::new(pos, galley, pal().text)
            .with_angle(-std::f32::consts::FRAC_PI_2),
    );
}

/// The rounded bracket closing a rack. It detaches from the body when the devices are showing, so
/// the hierarchy is easy to read (manual p. 494).
fn bracket(ui: &egui::Ui, rect: Rect, detached: bool) {
    let inset = if detached { 7.0 } else { 3.0 };
    let color = shade(0x8a8a8a);
    let x = rect.right() - inset;
    let (top, bottom) = (rect.top() + 3.0, rect.bottom() - 3.0);
    let stroke = Stroke::new(1.5, color);
    let path = vec![
        Pos2::new(x - 5.0, top),
        Pos2::new(x - 1.0, top + 1.0),
        Pos2::new(x, top + 5.0),
        Pos2::new(x, bottom - 5.0),
        Pos2::new(x - 1.0, bottom - 1.0),
        Pos2::new(x - 5.0, bottom),
    ];
    ui.painter().add(egui::Shape::line(path, stroke));
}

/// The buttons for a rack's sections, top to bottom.
fn view_column(ui: &mut egui::Ui, cx: &mut DrawCx, r: &RackRef, rect: Rect) {
    ui.painter().rect_filled(rect, 3.0, pal().raised);
    let drum = r.kind == RackKind::Drum;
    let mut buttons: Vec<(RackSection, &str, &str)> = vec![
        (
            RackSection::Macros,
            "M",
            "Macro Controls — show or hide the knobs",
        ),
        (
            RackSection::Variations,
            "V",
            "Macro Variations — show or hide the stored states of the macros",
        ),
    ];
    if drum {
        buttons.push((RackSection::Pads, "P", "Pad View — show or hide the pads"));
    }
    buttons.extend([
        (
            RackSection::Chains,
            "C",
            "Chain List — show or hide the chains",
        ),
        (
            RackSection::Devices,
            "D",
            "Devices — right-click to list every device",
        ),
    ]);
    if drum {
        buttons.extend([
            (
                RackSection::InOut,
                "I",
                "Input/Output — Receive, Play and Choke",
            ),
            (
                RackSection::Sends,
                "S",
                "Sends — levels into the return chains",
            ),
            (
                RackSection::Returns,
                "R",
                "Return Chains — show or hide them",
            ),
        ]);
    }
    let mut y = rect.top() + 2.0;
    let size = Vec2::new(rect.width() - 4.0, 16.0);
    for (section, label, hover) in buttons {
        let b = Rect::from_min_size(Pos2::new(rect.left() + 2.0, y), size);
        let on = r.view.is_on(section);
        let resp = tiny_button(
            ui,
            b,
            ("view", section as u8, r.dev.id),
            label,
            on,
            pal().accent,
        );
        let resp = info(resp, hover);
        if resp.clicked() {
            cx.queue
                .push(r.chain, Action::Rack(r.dev.id, RackAction::Toggle(section)));
        }
        if section == RackSection::Devices {
            let root = r.chain.owner();
            let nodes = cx.session.devices(root);
            resp.context_menu(|ui| {
                device_navigator(ui, cx.queue, root, nodes, &mut Vec::new());
            });
        }
        y += 18.0;
    }
    y += 4.0;
    let b = Rect::from_min_size(Pos2::new(rect.left() + 2.0, y), size);
    let on = r.view.auto_select;
    let resp = tiny_button(ui, b, ("auto", r.dev.id), "A", on, pal().accent);
    if info(resp, "Auto Select — select the chains that are processing").clicked() {
        cx.queue
            .push(r.chain, Action::Rack(r.dev.id, RackAction::AutoSelect(!on)));
    }
}

/// A list of every device under a chain, chains and nested racks as submenus (manual p. 494).
pub(crate) fn device_navigator(
    ui: &mut egui::Ui,
    queue: &mut super::ActionQueue,
    at: ChainRef,
    devices: &[Device],
    path: &mut Vec<(DeviceId, ChainId)>,
) {
    for d in devices {
        if !matches!(d.kind, DeviceKind::Rack { .. }) {
            if ui.button(&d.name).clicked() {
                queue.push(at, Action::Reveal(d.id, path.clone()));
                ui.close();
            }
            continue;
        }
        ui.menu_button(&d.name, |ui| {
            if ui.button("Show this Rack").clicked() {
                queue.push(at, Action::Reveal(d.id, path.clone()));
                ui.close();
            }
            ui.separator();
            for c in d.chains() {
                let Some(child) = at.child(d.id, c.id) else {
                    continue;
                };
                path.push((d.id, c.id));
                if c.devices.is_empty() {
                    ui.add_enabled(false, egui::Button::new(&c.name));
                } else {
                    ui.menu_button(&c.name, |ui| {
                        device_navigator(ui, queue, child, &c.devices, path);
                    });
                }
                path.pop();
            }
        });
    }
}

/// A small flat button at a fixed place.
pub(crate) fn tiny_button(
    ui: &mut egui::Ui,
    rect: Rect,
    id: impl egui::AsIdSalt,
    label: &str,
    on: bool,
    on_color: egui::Color32,
) -> egui::Response {
    let resp = ui.interact(rect, ui.scope_id().with(id), Sense::click());
    let p = ui.painter();
    let fill = if on {
        on_color
    } else if resp.hovered() {
        pal().raised_hover
    } else {
        pal().field_bg
    };
    p.rect_filled(rect, 2.0, fill);
    p.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        theme::font(9.5),
        if on {
            theme::text_on(on_color)
        } else {
            pal().text
        },
    );
    resp
}

/// Draws Map Mode's overlay on something mappable, and handles a click on it. Returns whether the
/// overlay covered the control.
pub(crate) fn map_overlay(
    ui: &mut egui::Ui,
    cx: &mut DrawCx,
    chain: ChainRef,
    rect: Rect,
    target: ParamRef,
) {
    crate::ui::remote_map::overlay(ui, rect, rlive_project::MapTarget::Param(target));
    let Some(map) = cx.ui.map else {
        return;
    };
    if !cx.mappable.contains(&target.owner) {
        return;
    }
    let selected = map.selected == Some(target);
    let mapped = cx.session.is_macro_mapped(chain, target);
    let color = if selected {
        pal().accent
    } else if mapped {
        pal().solo_blue
    } else {
        pal().accent.gamma_multiply(0.35)
    };
    let id = ui.scope_id().with(("map", target.owner, target.id));
    let resp = ui.interact(rect, id, Sense::click());
    ui.painter()
        .rect_filled(rect, 1.0, color.gamma_multiply(0.45));
    ui.painter().rect_stroke(
        rect,
        1.0,
        Stroke::new(if selected { 2.0 } else { 1.0 }, color),
        egui::StrokeKind::Inside,
    );
    if info(
        resp,
        "Macro Map Mode — click to select this parameter for mapping",
    )
    .clicked()
    {
        cx.queue.push(
            map.chain,
            Action::Rack(map.rack, RackAction::MapTarget(target)),
        );
    }
}

/// What can be mapped in Map Mode: everything nested in the mapping rack.
pub(crate) fn mappable_owners(rack: &Device) -> HashSet<rlive_core::ParamOwner> {
    let mut owners = HashSet::new();
    rack.for_each(&mut |d| {
        if d.id != rack.id {
            owners.insert(rlive_core::ParamOwner::Device(d.id));
        }
        for c in d.chains() {
            owners.insert(rlive_core::ParamOwner::Chain(c.id));
        }
    });
    owners
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_rack_shows_what_its_kind_is_for() {
        let audio = RackView::for_kind(RackKind::Audio);
        assert!(audio.macros && audio.chains && audio.devices && !audio.pads);
        let drum = RackView::for_kind(RackKind::Drum);
        assert!(drum.pads && drum.devices && !drum.macros && !drum.chains);
        assert_eq!(drum.pad_start, 36);
    }

    #[test]
    fn a_rack_folds_when_all_its_views_are_hidden() {
        let mut v = RackView::for_kind(RackKind::Audio);
        assert!(!v.all_hidden(RackKind::Audio));
        v.macros = false;
        v.chains = false;
        v.devices = false;
        assert!(v.all_hidden(RackKind::Audio));
        // Pads count on a Drum Rack only.
        v.pads = true;
        assert!(v.all_hidden(RackKind::Audio));
        assert!(!v.all_hidden(RackKind::Drum));
    }

    #[test]
    fn a_rack_takes_up_only_the_room_of_its_views() {
        let mut v = RackView::for_kind(RackKind::Audio);
        v.macros = false;
        v.chains = false;
        assert_eq!(RackWidths::of(RackKind::Audio, &v, 8, 0).chrome(), VIEW_W);
        v.macros = true;
        let widths = RackWidths::of(RackKind::Audio, &v, 8, 0);
        assert_eq!(widths.chrome(), VIEW_W + widths.macros + GAP);
        assert_eq!(
            RackWidths::of(RackKind::Audio, &v, 16, 0).macros,
            macros::section_width(16)
        );
    }

    #[test]
    fn only_a_chain_list_can_have_zones_and_drum_racks_have_none() {
        let mut v = RackView::for_kind(RackKind::Instrument);
        v.zones = Some(ZoneKind::Key);
        assert_eq!(
            RackWidths::of(RackKind::Instrument, &v, 8, 0).zones,
            ZONES_W
        );
        v.chains = false;
        assert_eq!(RackWidths::of(RackKind::Instrument, &v, 8, 0).zones, 0.0);
        let mut d = RackView::for_kind(RackKind::Drum);
        d.chains = true;
        d.zones = Some(ZoneKind::Key);
        assert_eq!(RackWidths::of(RackKind::Drum, &d, 8, 0).zones, 0.0);
    }
}
