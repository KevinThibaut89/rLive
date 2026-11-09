//! Device view, styled after Live: devices sit side by side (signal flows left → right), each
//! with a title bar (power, fold, name) and a body laid out from what the device declares. A rack
//! shows its chains and the devices of the one that's open, nested to any depth.
//!
//! Drawing only reads the session. What the user does queues up and applies afterwards.

mod layout;
mod queue;
pub(crate) mod rack;
mod tree;
mod views;

pub(crate) use queue::{add_item, group_selected, ungroup_selected, Action, ActionQueue};
use rack::{RackUi, RackView};

use super::automation;
use super::icons;
use super::keymap::FocusArea;
use super::panels::{self, PanelCtx};
use super::theme::{self, *};
use super::widgets::{self, info, rgb, ControlState, Led};
use super::{BrowserItem, UiState};
use crate::session::{ChainRef, DeviceRuntime, InfoTarget, Session};
use egui::{Align2, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, UiBuilder, Vec2};
use layout::{Placed, TITLE_H};
use rlive_core::{ChainId, DeviceId, Item, ParamFlags, ParamId, ParamInfo, ParamOwner, ParamRef};
use rlive_project::{Device, DeviceKind};
use std::collections::{HashMap, HashSet};
use tree::{ControlPlan, DeviceNode, NodeBody, PlanInput, DEVICE_GAP};
use views::ViewStore;

/// A device being dragged by its title bar, to save it into a browser folder.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DeviceDrag {
    pub chain: ChainRef,
    pub device: DeviceId,
}

const FOLDED_NAME_Y: f32 = 46.0;
const DROP_ZONE_W: f32 = 170.0;
/// How rounded a rack's corners are, which sets it apart from a device (manual p. 494).
const RACK_ROUNDING: f32 = 6.0;

/// The device view's own state, none of it saved.
#[derive(Default)]
pub struct DeviceViewState {
    show_all: HashSet<DeviceId>,
    open_chains: HashMap<DeviceId, ChainId>,
    views: ViewStore,
    /// Something whose info text was just asked to be edited.
    edit_info: Option<InfoTarget>,
    /// What each rack shows.
    pub(crate) rack_views: HashMap<DeviceId, RackView>,
    /// What's going on in the racks on screen.
    pub(crate) ui: RackUi,
    /// The devices selected in the Device View, for grouping.
    pub(crate) selected_devices: Vec<(ChainRef, DeviceId)>,
    /// A device to bring into view.
    scroll_to: Option<DeviceId>,
    /// Which rack mixers are unfolded in the Session View.
    pub(crate) mixer_open: HashSet<crate::ui::rack_mixer::MixFold>,
    /// The chains selected in the Session View's mixer.
    pub(crate) mixer_selection: Vec<crate::ui::rack_mixer::MixChain>,
    pub(crate) chain_meters: HashMap<ChainId, super::widgets::MeterState>,
    /// Hot-Swap requests from what was drawn this frame, for the browser to act on.
    pub(crate) swap_requests: Vec<crate::ui::browser::SwapRequest>,
    /// A preset just saved, for the browser to show.
    pub(crate) reveal_saved: Option<std::path::PathBuf>,
    /// A device whose default exists: the overwrite question is showing.
    pub(crate) confirm_default: Option<(ChainRef, DeviceId)>,
}

impl DeviceViewState {
    /// Call at the start of a frame.
    pub fn begin_frame(&mut self) {
        self.views.begin_frame();
    }

    /// Call at the end of a frame, whether the device view was drawn or not, so a device that
    /// isn't on screen stops measuring.
    pub fn end_frame(&mut self) {
        self.views.end_frame();
    }
}

/// What every device draws with.
pub(crate) struct DrawCx<'a> {
    session: &'a Session,
    views: &'a mut ViewStore,
    queue: &'a mut ActionQueue,
    dt: f32,
    rack_views: &'a HashMap<DeviceId, RackView>,
    ui: &'a mut RackUi,
    selected_devices: &'a [(ChainRef, DeviceId)],
    /// Everything Map Mode's rack can map.
    mappable: HashSet<ParamOwner>,
    /// The arrow keys are the Device View's to use this frame.
    keyboard_free: bool,
    scroll_to: Option<DeviceId>,
    /// Device controls drawn this frame, for their automation commands afterwards.
    zones: Vec<(Rect, ChainRef, rlive_core::param::EnvTarget)>,
    /// What Hot-Swap is pointed at, to light its button.
    swap_target: Option<crate::session::SwapTarget>,
}

pub fn show(ui: &mut egui::Ui, session: &mut Session, state: &mut UiState) {
    let full = ui.max_rect();
    // Claim the whole panel so egui keeps its size (children don't advance the cursor).
    ui.expand_to_include_rect(full);
    ui.painter().rect_filled(full, 0.0, pal().panel_dark);
    let Some(chain) = state.selected else {
        ui.painter().text(
            full.center(),
            Align2::CENTER_CENTER,
            "Select a track to see its devices",
            theme::font(12.0),
            pal().text_dim,
        );
        return;
    };
    let (title, color) = chain_header(session, chain);
    // Left strip in the track colour with the track name (like Live's detail view title).
    let strip = Rect::from_min_size(full.min, Vec2::new(18.0, full.height()));
    ui.painter().rect_filled(strip, 0.0, color);
    let strip_resp = ui.interact(strip, ui.scope_id().with("device_selector"), Sense::click());
    let galley = ui
        .painter()
        .layout_no_wrap(title, theme::font_bold(11.0), theme::text_on(color));
    let pos = Pos2::new(
        strip.center().x - galley.size().y / 2.0,
        strip.top() + 8.0 + galley.size().x,
    );
    ui.painter().add(
        egui::epaint::TextShape::new(pos, galley, theme::text_on(color))
            .with_angle(-std::f32::consts::FRAC_PI_2),
    );

    let area = Rect::from_min_max(
        Pos2::new(strip.right() + 6.0, full.top() + 6.0),
        Pos2::new(full.right(), full.bottom() - 6.0),
    );
    let mut au = ui.new_child(UiBuilder::new().max_rect(area));
    let height = area.height();
    let dt = ui.input(|i| i.stable_dt).min(0.1);
    let mut queue = ActionQueue::default();
    let zones;
    {
        let session: &Session = session;
        let root = chain.owner();
        strip_resp.context_menu(|ui| {
            rack::device_navigator(ui, &mut queue, root, session.devices(root), &mut Vec::new());
        });
        let input = PlanInput {
            root: session.devices(root),
            runtimes: &session.runtimes,
            show_all: &state.devices.show_all,
            open_chains: &state.devices.open_chains,
            racks: &state.devices.rack_views,
        };
        let nodes = tree::plan_chain(chain, session.devices(chain), &input, height);
        let keyboard_free =
            state.focus == FocusArea::Device && !ui.ctx().egui_wants_keyboard_input();
        let mappable = state
            .devices
            .ui
            .map
            .and_then(|m| {
                session
                    .devices(m.chain)
                    .iter()
                    .find(|d| d.id == m.rack)
                    .map(rack::mappable_owners)
            })
            .unwrap_or_default();
        let released = !ui.input(|i| i.pointer.primary_down());
        if let Some(p) = state.devices.ui.preview.filter(|_| released) {
            state.devices.ui.preview = None;
            queue.push(
                p.chain,
                Action::Rack(p.rack, rack::RackAction::Preview(p, false)),
            );
        }
        let mut cx = DrawCx {
            session,
            views: &mut state.devices.views,
            queue: &mut queue,
            dt,
            rack_views: &state.devices.rack_views,
            ui: &mut state.devices.ui,
            selected_devices: &state.devices.selected_devices,
            mappable,
            keyboard_free,
            scroll_to: state.devices.scroll_to,
            zones: Vec::new(),
            swap_target: state.browser.hot_swap.map(|s| s.target),
        };
        egui::ScrollArea::horizontal()
            .id_salt("device_chain")
            .auto_shrink([false, false])
            .show(&mut au, |ui| {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = DEVICE_GAP;
                    draw_chain(ui, &mut cx, chain, &nodes, height, DROP_ZONE_W);
                });
            });
        zones = std::mem::take(&mut cx.zones);
    }
    for (rect, zone_chain, target) in zones {
        automation::control_zone(&au, session, state, rect, zone_chain, target);
    }
    state.devices.scroll_to = None;
    queue.apply(session, &mut state.devices, &mut state.status);
    if let Some(target) = state.devices.edit_info.take() {
        state.edit_info_text(session, target);
    }
}

/// The name and colour on the left of the device view.
fn chain_header(session: &Session, chain: ChainRef) -> (String, egui::Color32) {
    match chain {
        ChainRef::Master => ("Master".to_string(), shade(0x9a9a9a)),
        ChainRef::Track(t) => session
            .project
            .track(t)
            .map(|t| (t.name.clone(), rgb(t.color)))
            .unwrap_or_default(),
        ChainRef::Nested(addr) => {
            let step = addr.path.steps().last();
            let chain = step.and_then(|step| {
                let parent = addr.path.steps()[..addr.path.len() - 1].iter().try_fold(
                    session.devices(chain.owner()),
                    |devices, s| {
                        devices
                            .iter()
                            .find(|d| d.id == s.rack)?
                            .chains()
                            .find(|c| c.id == s.chain)
                            .map(|c| c.devices.as_slice())
                    },
                )?;
                parent
                    .iter()
                    .find(|d| d.id == step.rack)?
                    .chains()
                    .find(|c| c.id == step.chain)
            });
            match chain {
                Some(c) => (c.name.clone(), c.color.map_or(shade(0x9a9a9a), rgb)),
                None => ("Rack Chain".to_string(), shade(0x9a9a9a)),
            }
        }
    }
}

fn draw_chain(
    ui: &mut egui::Ui,
    cx: &mut DrawCx,
    at: ChainRef,
    nodes: &[DeviceNode],
    height: f32,
    drop_w: f32,
) {
    for (index, node) in nodes.iter().enumerate() {
        device(ui, cx, at, node, index, nodes.len());
    }
    drop_zone(ui, cx, at, height, drop_w);
}

fn drop_zone(ui: &mut egui::Ui, cx: &mut DrawCx, chain: ChainRef, height: f32, width: f32) {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let hovering = resp.dnd_hover_payload::<BrowserItem>().is_some();
    ui.painter().rect_stroke(
        rect.shrink(1.0),
        2.0,
        Stroke::new(
            1.0,
            if hovering {
                pal().accent
            } else {
                shade(0x3a3a3a)
            },
        ),
        StrokeKind::Inside,
    );
    let text = if width >= DROP_ZONE_W {
        "Drop Instruments or\nAudio Effects Here"
    } else {
        "Drop\nHere"
    };
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        theme::font(11.0),
        if hovering {
            pal().accent
        } else {
            pal().text_faint
        },
    );
    if let Some(item) = crate::ui::widgets::released::<BrowserItem>(&resp) {
        cx.queue.push(chain, Action::Add((*item).clone()));
    }
    info(
        resp,
        "Device Chain — drag devices, plug-ins or samples from the browser here",
    );
}

fn device(
    ui: &mut egui::Ui,
    cx: &mut DrawCx,
    chain: ChainRef,
    node: &DeviceNode,
    index: usize,
    count: usize,
) {
    let dev = node.dev;
    let width = node.width;
    let height = node.height;
    let on = !dev.bypass;
    let is_plugin = matches!(dev.kind, DeviceKind::Plugin { .. });
    let is_rack = matches!(dev.kind, DeviceKind::Rack { .. });
    let rounding = if is_rack { RACK_ROUNDING } else { 1.0 };
    let selected = cx.selected_devices.contains(&(chain, dev.id));

    let (rect, bg) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
    if cx.scroll_to == Some(dev.id) {
        ui.scroll_to_rect(rect, Some(egui::Align::Center));
    }
    let p = ui.painter().clone();
    p.rect_filled(
        rect,
        rounding,
        if on { shade(0x3a3a3a) } else { shade(0x2f2f2f) },
    );
    p.rect_stroke(
        rect,
        rounding,
        Stroke::new(
            1.0,
            if selected {
                shade(0xdddddd)
            } else {
                pal().separator
            },
        ),
        StrokeKind::Inside,
    );

    // ---- title bar
    let title = Rect::from_min_size(rect.min, Vec2::new(width, TITLE_H));
    let mut tu = ui.new_child(
        UiBuilder::new()
            .max_rect(title.shrink2(Vec2::new(2.0, 1.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    tu.spacing_mut().item_spacing.x = 2.0;
    if dev.expanded {
        p.rect_filled(
            title,
            1.0,
            if on { shade(0x505050) } else { shade(0x3a3a3a) },
        );
    }
    let r = widgets::bare_icon(&mut tu, Vec2::new(18.0, 18.0), |p, r, c| {
        icons::power(p, r, if on { pal().accent } else { c })
    });
    let power_rect = r.rect;
    if info(
        r,
        if on {
            "Device on — click to bypass; Alt-click for its whole group"
        } else {
            "Device bypassed — click to activate; Alt-click for its whole group"
        },
    )
    .clicked()
    {
        if ui.input(|i| i.modifiers.alt) {
            cx.queue.push(chain, Action::BypassGroup(dev.id, on));
        } else {
            cx.queue.push(chain, Action::Bypass(dev.id, on));
        }
    }
    rack::map_overlay(
        ui,
        cx,
        chain,
        power_rect,
        ParamRef {
            owner: ParamOwner::Device(dev.id),
            id: rlive_core::ACTIVATOR,
        },
    );
    if dev.expanded {
        expanded_title(&mut tu, cx, chain, node, title, is_plugin, on);
    } else {
        folded_title(ui, cx, chain, node, rect, on);
    }

    let resp = info(bg, format!("{} — right-click for options", dev.name));
    widgets::info_note(&resp, &dev.info);
    // The title bar drags a device to a browser folder, to save it there.
    let drag = ui.interact(
        title,
        ui.scope_id().with(("device_drag", dev.id)),
        Sense::drag(),
    );
    drag.dnd_set_drag_payload(DeviceDrag {
        chain,
        device: dev.id,
    });
    if drag.dragged() {
        crate::ui::browser::drag_device_label(ui, &dev.name);
    }
    let on_title = |resp: &egui::Response| {
        resp.interact_pointer_pos()
            .is_some_and(|pos| title.contains(pos) || !dev.expanded)
    };
    if resp.double_clicked() && on_title(&resp) {
        cx.queue.push(chain, Action::Fold(dev.id, !dev.expanded));
    } else if resp.clicked() && on_title(&resp) {
        let additive = ui.input(|i| i.modifiers.shift);
        cx.queue.push(chain, Action::SelectDevice(dev.id, additive));
    }
    resp.context_menu(|ui| device_menu(ui, cx, chain, dev, index, count, is_plugin));

    match &node.body {
        NodeBody::Folded => {}
        NodeBody::Controls(plan) => controls(ui, cx, chain, node, rect, title, plan, on),
        NodeBody::Rack(open) => rack::draw(ui, cx, chain, node, rect, title, open.as_ref(), on),
    }
}

#[allow(clippy::too_many_arguments)]
fn expanded_title(
    tu: &mut egui::Ui,
    cx: &mut DrawCx,
    chain: ChainRef,
    node: &DeviceNode,
    title: Rect,
    is_plugin: bool,
    on: bool,
) {
    let dev = node.dev;
    let r = widgets::bare_icon(tu, Vec2::new(14.0, 18.0), |p, r, c| {
        icons::fold(p, r, true, c)
    });
    if info(r, "Fold Device — collapse this device").clicked() {
        cx.queue.push(chain, Action::Fold(dev.id, false));
    }
    let is_rack = matches!(dev.kind, DeviceKind::Rack { .. });
    // A hand in the title says a control surface is locked to this device (manual p. 867).
    if cx.session.remote.surfaces.is_locked(dev.id) {
        let r = widgets::bare_icon(tu, Vec2::new(14.0, 18.0), icons::hand);
        info(r, "A control surface is locked to this device");
    }
    // With every view hidden the name moves into the view column, and nothing else fits.
    if rack::RackRef::of(chain, dev, cx.rack_views).is_some_and(|r| r.view.all_hidden(r.kind)) {
        return;
    }
    let reserved = match (is_plugin, is_rack) {
        (true, _) => 46.0,
        (_, true) => 44.0,
        _ => 6.0,
    };
    let reserved = reserved + 40.0;
    let name_w = (title.right() - tu.cursor().left() - reserved).max(10.0);
    // A rack is wider than the window, so what's in its title stays where it can be seen.
    let name_w = if is_rack { name_w.min(150.0) } else { name_w };
    let (nr, _) = tu.allocate_exact_size(Vec2::new(name_w, 18.0), Sense::hover());
    let renaming = matches!(
        &cx.ui.rename,
        Some(rack::Rename { target: rack::RenameTarget::Device(d), .. }) if *d == dev.id
    );
    if renaming {
        let mut done = None;
        if let Some(rename) = &mut cx.ui.rename {
            let edit = tu.put(
                nr,
                egui::TextEdit::singleline(&mut rename.text).font(theme::font(11.0)),
            );
            edit.request_focus();
            if edit.lost_focus() {
                done = Some(rename.text.clone());
            }
        }
        if let Some(name) = done {
            cx.ui.rename = None;
            cx.queue.push(chain, Action::Rename(dev.id, name));
        }
    }
    let shown_name = match cx.session.compare_slot(chain, dev.id) {
        Some(rlive_project::CompareSlot::B) => format!("{} (B)", dev.name),
        _ => dev.name.clone(),
    };
    let galley = tu.painter().layout_no_wrap(
        shown_name,
        theme::font_bold(11.0),
        if on { pal().text } else { pal().text_dim },
    );
    tu.painter().with_clip_rect(nr).galley(
        Pos2::new(nr.left() + 2.0, nr.center().y - galley.size().y / 2.0),
        galley,
        pal().text,
    );
    let swap_target = crate::session::SwapTarget::Device { chain, id: dev.id };
    let swapping = cx.swap_target == Some(swap_target);
    let r = widgets::state_button(tu, Vec2::new(16.0, 15.0), swapping, pal().accent, "Q");
    if info(
        r,
        "Hot-Swap Presets — replace this device from the browser (Q)",
    )
    .clicked()
    {
        cx.queue.push(chain, Action::HotSwap(swap_target));
    }
    let r = widgets::state_button(tu, Vec2::new(16.0, 15.0), false, pal().accent, "S");
    if info(r, "Save Preset — save this device to the User Library").clicked() {
        cx.queue.push(chain, Action::SavePreset(dev.id));
    }
    if is_rack {
        let r = widgets::state_button(tu, Vec2::new(36.0, 15.0), false, pal().accent, "Rand");
        if info(
            r,
            "Rand — randomize this Rack's mapped Macro Controls, except those excluded",
        )
        .clicked()
        {
            cx.queue
                .push(chain, Action::Rack(dev.id, rack::RackAction::Randomize));
        }
    }
    if let Some(h) = node.rt.and_then(|r| r.plugin.as_ref()) {
        if h.has_editor() {
            let open = h.is_editor_open();
            let r = widgets::state_button(tu, Vec2::new(40.0, 15.0), open, pal().accent, "Edit");
            if info(r, "Plug-In Editor — show or hide the plug-in's own window").clicked() {
                cx.queue.push(
                    chain,
                    if open {
                        Action::CloseEditor(dev.id)
                    } else {
                        Action::OpenEditor(dev.id)
                    },
                );
            }
        }
    }
}

/// Folded: just the fold arrow and the name written vertically.
fn folded_title(
    ui: &mut egui::Ui,
    cx: &mut DrawCx,
    chain: ChainRef,
    node: &DeviceNode,
    rect: Rect,
    on: bool,
) {
    let dev = node.dev;
    let p = ui.painter().clone();
    let arrow = Rect::from_min_size(
        Pos2::new(rect.left() + 3.0, rect.top() + 22.0),
        Vec2::splat(18.0),
    );
    let ar = ui.interact(
        arrow,
        ui.scope_id().with(("unfold", dev.id)),
        Sense::click(),
    );
    icons::fold(
        &p,
        arrow,
        false,
        if ar.hovered() {
            shade(0xffffff)
        } else {
            pal().text_dim
        },
    );
    if info(ar, "Unfold Device — expand this device").clicked() {
        cx.queue.push(chain, Action::Fold(dev.id, true));
    }
    let galley = p.layout_no_wrap(
        dev.name.clone(),
        theme::font_bold(11.0),
        if on { pal().text } else { pal().text_dim },
    );
    let pos = Pos2::new(
        rect.center().x - galley.size().y / 2.0,
        rect.top() + FOLDED_NAME_Y + galley.size().x,
    );
    p.add(
        egui::epaint::TextShape::new(pos, galley, pal().text)
            .with_angle(-std::f32::consts::FRAC_PI_2),
    );
}

#[allow(clippy::too_many_arguments)]
/// The "Lock to..." entry of a device's context menu, one row per control surface (manual p. 867).
fn lock_to_menu(ui: &mut egui::Ui, cx: &mut DrawCx, chain: ChainRef, dev: &Device) {
    let rows: Vec<(usize, String, Option<rlive_core::DeviceId>, bool)> = cx
        .session
        .remote
        .surfaces
        .in_use()
        .map(|(i, s)| {
            (
                i,
                s.surface.name().to_string(),
                s.locked,
                s.surface.supports_locking(),
            )
        })
        .collect();
    if rows.is_empty() {
        return;
    }
    ui.menu_button("Lock to...", |ui| {
        for (i, name, locked, can_lock) in rows {
            let on = locked == Some(dev.id);
            if ui
                .add_enabled(can_lock, egui::Button::new(name).selected(on))
                .clicked()
            {
                let device = if on { None } else { Some(dev.id) };
                cx.queue.push(chain, Action::LockSurface(i, device));
                ui.close();
            }
        }
    });
    ui.separator();
}

fn device_menu(
    ui: &mut egui::Ui,
    cx: &mut DrawCx,
    chain: ChainRef,
    dev: &Device,
    index: usize,
    count: usize,
    is_plugin: bool,
) {
    lock_to_menu(ui, cx, chain, dev);
    if ui.button("Rename").clicked() {
        cx.ui.rename = Some(rack::Rename {
            target: rack::RenameTarget::Device(dev.id),
            text: dev.name.clone(),
        });
        ui.close();
    }
    if ui.button("Edit Info Text").clicked() {
        cx.queue.push(chain, Action::EditInfo(dev.id));
        ui.close();
    }
    if ui.button("Group").clicked() {
        cx.queue.push(chain, Action::Group(dev.id, false));
        ui.close();
    }
    if ui.button("Group to Drum Rack").clicked() {
        cx.queue.push(chain, Action::Group(dev.id, true));
        ui.close();
    }
    if matches!(dev.kind, DeviceKind::Rack { .. }) && ui.button("Ungroup").clicked() {
        cx.queue.push(chain, Action::Ungroup(dev.id));
        ui.close();
    }
    if index > 0 && ui.button("Move Left").clicked() {
        cx.queue.push(chain, Action::Move(dev.id, index - 1));
        ui.close();
    }
    if index + 1 < count && ui.button("Move Right").clicked() {
        cx.queue.push(chain, Action::Move(dev.id, index + 1));
        ui.close();
    }
    if ui
        .button(if dev.expanded { "Fold" } else { "Unfold" })
        .clicked()
    {
        cx.queue.push(chain, Action::Fold(dev.id, !dev.expanded));
        ui.close();
    }
    if is_plugin {
        let on = dev.mpe.is_some();
        if ui.selectable_label(on, "Enable MPE Mode").clicked() {
            let out = (!on).then(rlive_core::MpeOutput::default);
            cx.queue.push(chain, Action::SetMpe(dev.id, out));
            ui.close();
        }
        if ui.button("MPE Settings…").clicked() {
            if !on {
                cx.queue.push(
                    chain,
                    Action::SetMpe(dev.id, Some(rlive_core::MpeOutput::default())),
                );
            }
            cx.ui.mpe_settings = Some(crate::ui::midi_settings::MpeTarget::Device(chain, dev.id));
            ui.close();
        }
    }
    if is_plugin && ui.button("Reload Plug-In").clicked() {
        cx.queue.push(chain, Action::Reload(dev.id));
        ui.close();
    }
    ui.separator();
    if ui.button("Save Preset").clicked() {
        cx.queue.push(chain, Action::SavePreset(dev.id));
        ui.close();
    }
    if ui.button("Hot-Swap Preset (Q)").clicked() {
        cx.queue.push(
            chain,
            Action::HotSwap(crate::session::SwapTarget::Device { chain, id: dev.id }),
        );
        ui.close();
    }
    if is_plugin {
        if ui.button("Save as Default Configuration").clicked() {
            cx.queue
                .push(chain, Action::SaveDefaultPluginConfig(dev.id));
            ui.close();
        }
    } else if ui.button("Save as Default Preset").clicked() {
        cx.queue.push(chain, Action::SaveDefaultPreset(dev.id));
        ui.close();
    }
    if let (Some(slot), DeviceKind::Builtin { .. }) =
        (cx.session.compare_slot(chain, dev.id), &dev.kind)
    {
        ui.separator();
        let (copy, switch) = match slot {
            rlive_project::CompareSlot::A => ("Compare: Copy A to B", "Compare: Switch to B"),
            rlive_project::CompareSlot::B => ("Compare: Copy B to A", "Compare: Switch to A"),
        };
        if ui.button(copy).clicked() {
            cx.queue.push(chain, Action::CompareCopy(dev.id));
            ui.close();
        }
        if ui.button(switch).clicked() {
            cx.queue.push(chain, Action::CompareSwitch(dev.id));
            ui.close();
        }
    }
    ui.separator();
    if ui
        .button(RichText::new("Delete").color(pal().arm_red))
        .clicked()
    {
        cx.queue.push(chain, Action::Remove(dev.id));
        ui.close();
    }
}

/// A child that lays out top to bottom whatever its parent does, clipped to `body`.
fn body_ui(ui: &mut egui::Ui, body: Rect) -> egui::Ui {
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(body)
            .layout(egui::Layout::top_down(egui::Align::LEFT)),
    );
    child.set_clip_rect(body.intersect(ui.clip_rect()));
    child
}

#[allow(clippy::too_many_arguments)]
fn controls(
    ui: &mut egui::Ui,
    cx: &mut DrawCx,
    chain: ChainRef,
    node: &DeviceNode,
    rect: Rect,
    title: Rect,
    plan: &ControlPlan,
    on: bool,
) {
    let dev = node.dev;
    let body = Rect::from_min_max(
        Pos2::new(rect.left() + 6.0, title.bottom() + 6.0),
        rect.max - Vec2::new(6.0, 4.0),
    );
    let mut bu = body_ui(ui, body);
    bu.spacing_mut().item_spacing = Vec2::new(0.0, 2.0);
    if !on {
        bu.multiply_opacity(0.55);
    }
    let Some(rt) = node.rt else {
        bu.label(RichText::new("Not loaded").color(pal().arm_red));
        return;
    };
    let is_plugin = matches!(dev.kind, DeviceKind::Plugin { .. });
    if let Some(err) = &rt.error {
        bu.label(RichText::new(err).color(pal().arm_red).size(10.5));
        if is_plugin
            && widgets::state_button(&mut bu, Vec2::new(44.0, 16.0), false, pal().accent, "Retry")
                .clicked()
        {
            cx.queue.push(chain, Action::Reload(dev.id));
        }
    }
    for slot in 0..rt.sample_slots {
        sample_row(&mut bu, cx, chain, dev, slot, body.width());
    }
    if rt.sidechain_input && !is_plugin {
        let mut picked = None;
        let current = super::in_out::sidechain_label(cx.session, dev.id);
        let response = egui::ComboBox::from_id_salt(("sidechain", dev.id))
            .width((body.width() - 8.0).min(150.0))
            .selected_text(RichText::new(current).size(10.0))
            .show_ui(&mut bu, |ui| {
                ui.set_min_width(170.0);
                if ui.selectable_label(false, "No Input").clicked() {
                    picked = Some(None);
                }
                for (label, source) in cx.session.sidechain_sources(dev.id) {
                    if ui.selectable_label(false, label).clicked() {
                        picked = Some(Some(source));
                    }
                }
            })
            .response;
        info(
            response,
            "Sidechain Input — the track that keys this device",
        );
        if let Some(source) = picked {
            cx.queue.push(chain, Action::SetSidechain(dev.id, source));
        }
    }

    let grid_top = bu.cursor().top();
    let mut x = body.left();
    let declared = !rt.layout.is_empty();
    for section in &plan.sections {
        if !section.title.is_empty() {
            bu.painter().text(
                Pos2::new(x, grid_top),
                Align2::LEFT_TOP,
                section.title,
                theme::font_bold(10.0),
                pal().text_dim,
            );
        }
        let origin = Pos2::new(x, grid_top).to_vec2();
        for placed in &section.items {
            let r = placed.rect.translate(origin);
            let mut iu = bu.new_child(UiBuilder::new().max_rect(r));
            item(&mut iu, cx, chain, dev, rt, placed, plan, on);
        }
        x += section.size.x + layout::SECTION_GAP;
    }
    let show_all_label = if plan.truncated {
        Some(format!("Show all {} parameters", plan.visible))
    } else if !declared && plan.visible > layout::MAX_PARAMS {
        Some("Show fewer parameters".to_string())
    } else {
        None
    };
    if let Some(label) = show_all_label {
        let r = Rect::from_min_size(
            Pos2::new(body.left(), body.bottom() - 18.0),
            Vec2::new(body.width(), 18.0),
        );
        let mut au = bu.new_child(UiBuilder::new().max_rect(r));
        if widgets::state_button(&mut au, r.size(), false, pal().accent, &label).clicked() {
            cx.queue.push(chain, Action::ShowAll(dev.id));
        }
    }
    if plan.visible == 0 && rt.error.is_none() && !declared {
        bu.label(RichText::new("No parameters").color(pal().text_dim));
    }
}

fn sample_row(
    ui: &mut egui::Ui,
    cx: &mut DrawCx,
    chain: ChainRef,
    dev: &Device,
    slot: usize,
    width: f32,
) {
    ui.horizontal(|ui| {
        let name = match dev.sample(slot) {
            Some(file) => file
                .path
                .file_stem()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            None => "No sample — drop one here".into(),
        };
        let (r, resp) = ui.allocate_exact_size(Vec2::new(width - 50.0, 18.0), Sense::hover());
        ui.painter().rect_filled(r, 1.0, pal().field_bg);
        let g = ui
            .painter()
            .layout_no_wrap(name, theme::font(10.5), pal().accent);
        ui.painter().with_clip_rect(r).galley(
            Pos2::new(r.left() + 4.0, r.center().y - g.size().y / 2.0),
            g,
            pal().accent,
        );
        info(resp, "Sample — the audio this Sampler plays");
        if widgets::state_button(ui, Vec2::new(22.0, 18.0), false, pal().accent, "Load").clicked() {
            cx.queue.push(chain, Action::LoadSample(dev.id, slot));
        }
        let target = crate::session::SwapTarget::Sample {
            chain,
            id: dev.id,
            slot,
        };
        let swapping = cx.swap_target == Some(target);
        let r = widgets::state_button(ui, Vec2::new(16.0, 18.0), swapping, pal().accent, "Q");
        if info(r, "Hot-Swap Sample — replace this sample from the browser").clicked() {
            cx.queue.push(chain, Action::HotSwap(target));
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn item(
    ui: &mut egui::Ui,
    cx: &mut DrawCx,
    chain: ChainRef,
    dev: &Device,
    rt: &DeviceRuntime,
    placed: &Placed,
    plan: &ControlPlan,
    on: bool,
) {
    match &placed.item {
        Item::Knob(_) | Item::Slider(_) | Item::Toggle(_) | Item::Menu(_) => {
            let Some(prm) = placed.param else {
                return;
            };
            param_control(ui, cx, chain, dev, rt, prm, &placed.item, plan);
        }
        Item::Meter(view) => {
            let rect = ui.max_rect().shrink(1.0);
            let peaks = cx.views.meter(dev.id, rt, *view, on, cx.dt);
            widgets::paint_meter(ui.painter(), rect, peaks);
        }
        Item::Scope(view, kind) => {
            let rect = ui.max_rect();
            let data = cx.views.scope(dev.id, rt, *view, on);
            widgets::paint_scope(ui.painter(), rect, *kind, data);
        }
        Item::Panel(name) => {
            let mut panel = PanelCtx::new(
                chain,
                dev,
                rt,
                on.then(|| cx.views.reader(dev.id, rt)).flatten(),
                cx.queue,
                cx.dt,
            );
            panels::draw(name, ui, &mut panel);
        }
        Item::Row(_) => {} // Flattened into its children when planned.
    }
}

#[allow(clippy::too_many_arguments)]
fn param_control(
    ui: &mut egui::Ui,
    cx: &mut DrawCx,
    chain: ChainRef,
    dev: &Device,
    rt: &DeviceRuntime,
    prm: &ParamInfo,
    item: &Item,
    plan: &ControlPlan,
) {
    let base = param_value(dev, rt, prm);
    let owned = plan.macro_owned.contains(&ParamId(prm.id));
    let disabled = owned || prm.flags.contains(ParamFlags::READ_ONLY);
    let effective = cx.session.effective_norm(chain, dev.id, prm.id);
    let cell = cx.session.device_cell(chain, dev.id, prm.id);
    let cell_state = cell.map_or(Default::default(), |c| cx.session.cell_state(chain, c));
    let automated = cell_state.automated() && !cell_state.overridden();
    let source = cell.and_then(|c| cx.session.source_norm(chain, c));
    // A Macro or automation moves the value, not the knob's own setting, so show what it's doing.
    // Modulation doesn't move the needle: that goes on the ring around it (manual p. 537).
    let shown = if owned || automated {
        source
            .or(effective)
            .map_or(base, |e| prm.from_normalized(e as f64))
    } else {
        base
    };
    let text = rt.format_value(prm, shown);
    let re_enable = std::cell::Cell::new(false);
    // A tuning system has no scale for a device to follow.
    let disabled = disabled || (prm.name == "Use Current Scale" && cx.session.tuning().is_some());
    let state = ControlState {
        effective: effective.filter(|_| !owned),
        disabled,
        led: Led::of(cell_state),
        re_enable: cell_state.overridden().then_some(&re_enable),
    };
    let changed = match item {
        Item::Knob(_) => widgets::knob(ui, prm, shown, &text, state),
        Item::Slider(_) => widgets::slider(ui, prm, shown, &text, state),
        Item::Toggle(_) => widgets::toggle(ui, prm, shown, state),
        Item::Menu(_) => widgets::menu(ui, prm, shown, state),
        _ => None,
    };
    if let Some(v) = changed {
        cx.queue.push(chain, Action::Param(dev.id, prm.id, v));
    }
    if re_enable.get() {
        cx.queue.push(chain, Action::ReEnable(dev.id, prm.id));
    }
    let rect = ui.max_rect();
    let target = ParamRef {
        owner: ParamOwner::Device(dev.id),
        id: ParamId(prm.id),
    };
    // Its own context menu is the Re-Enable one, while it applies.
    if !cell_state.overridden() {
        cx.zones
            .push((rect, chain, rlive_core::param::EnvTarget::Param(target)));
    }
    rack::map_overlay(ui, cx, chain, rect, target);
}

/// The knob position of a parameter: its project value, the plugin's own, or its default.
fn param_value(dev: &Device, rt: &DeviceRuntime, param: &ParamInfo) -> f64 {
    dev.params
        .get(&param.id)
        .copied()
        .or_else(|| rt.plugin.as_ref().and_then(|h| h.param_value(param.id)))
        .unwrap_or(param.default)
}
