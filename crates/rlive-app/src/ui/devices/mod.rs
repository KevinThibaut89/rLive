//! Device view, styled after Live: devices sit side by side (signal flows left → right), each
//! with a title bar (power, fold, name) and a body laid out from what the device declares. A rack
//! shows its chains and the devices of the one that's open, nested to any depth.
//!
//! Drawing only reads the session. What the user does queues up and applies afterwards.

mod layout;
mod queue;
mod tree;
mod views;

pub(crate) use queue::{Action, ActionQueue};

use super::icons;
use super::panels::{self, PanelCtx};
use super::theme::{self, *};
use super::widgets::{self, info, rgb, ControlState, Led};
use super::{BrowserItem, UiState};
use crate::session::{ChainRef, DeviceRuntime, InfoTarget, Session};
use egui::{Align2, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, UiBuilder, Vec2};
use layout::{Placed, TITLE_H};
use rlive_core::{ChainId, DeviceId, Item, ParamFlags, ParamId, ParamInfo};
use rlive_project::{Device, DeviceKind};
use std::collections::{HashMap, HashSet};
use tree::{ControlPlan, DeviceNode, NodeBody, OpenChain, PlanInput, CHAIN_LIST_W, DEVICE_GAP};
use views::ViewStore;

const FOLDED_NAME_Y: f32 = 46.0;
const DROP_ZONE_W: f32 = 170.0;
const CHAIN_ROW_H: f32 = 18.0;

/// The device view's own state, none of it saved.
#[derive(Default)]
pub struct DeviceViewState {
    show_all: HashSet<DeviceId>,
    open_chains: HashMap<DeviceId, ChainId>,
    views: ViewStore,
    /// A device whose info text was just asked to be edited.
    edit_info: Option<(ChainRef, DeviceId)>,
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
struct DrawCx<'a> {
    session: &'a Session,
    views: &'a mut ViewStore,
    queue: &'a mut ActionQueue,
    dt: f32,
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
    {
        let session: &Session = session;
        let input = PlanInput {
            root: session.devices(chain.owner()),
            runtimes: &session.runtimes,
            show_all: &state.devices.show_all,
            open_chains: &state.devices.open_chains,
        };
        let nodes = tree::plan_chain(chain, session.devices(chain), &input, height);
        let mut cx = DrawCx {
            session,
            views: &mut state.devices.views,
            queue: &mut queue,
            dt,
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
    }
    queue.apply(session, &mut state.devices, &mut state.status);
    if let Some((chain, id)) = state.devices.edit_info.take() {
        state.edit_info_text(session, InfoTarget::Device(chain, id));
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
        ChainRef::Nested(_) => ("Rack Chain".to_string(), shade(0x9a9a9a)),
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
    if let Some(item) = resp.dnd_release_payload::<BrowserItem>() {
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

    let (rect, bg) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
    let p = ui.painter().clone();
    p.rect_filled(
        rect,
        1.0,
        if on { shade(0x3a3a3a) } else { shade(0x2f2f2f) },
    );
    p.rect_stroke(
        rect,
        1.0,
        Stroke::new(1.0, pal().separator),
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
    if info(
        r,
        if on {
            "Device on — click to bypass"
        } else {
            "Device bypassed — click to activate"
        },
    )
    .clicked()
    {
        cx.queue.push(chain, Action::Bypass(dev.id, on));
    }
    if dev.expanded {
        expanded_title(&mut tu, cx, chain, node, title, is_plugin, on);
    } else {
        folded_title(ui, cx, chain, node, rect, on);
    }

    let resp = info(bg, format!("{} — right-click for options", dev.name));
    widgets::info_note(&resp, &dev.info);
    resp.context_menu(|ui| device_menu(ui, cx, chain, dev, index, count, is_plugin));

    match &node.body {
        NodeBody::Folded => {}
        NodeBody::Controls(plan) => controls(ui, cx, chain, node, rect, title, plan, on),
        NodeBody::Rack(open) => rack(ui, cx, node, rect, title, open.as_ref(), on),
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
    let name_w =
        (title.right() - tu.cursor().left() - if is_plugin { 46.0 } else { 6.0 }).max(10.0);
    let (nr, _) = tu.allocate_exact_size(Vec2::new(name_w, 18.0), Sense::hover());
    let galley = tu.painter().layout_no_wrap(
        dev.name.clone(),
        theme::font_bold(11.0),
        if on { pal().text } else { pal().text_dim },
    );
    tu.painter().with_clip_rect(nr).galley(
        Pos2::new(nr.left() + 2.0, nr.center().y - galley.size().y / 2.0),
        galley,
        pal().text,
    );
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
fn device_menu(
    ui: &mut egui::Ui,
    cx: &mut DrawCx,
    chain: ChainRef,
    dev: &Device,
    index: usize,
    count: usize,
    is_plugin: bool,
) {
    if ui.button("Edit Info Text").clicked() {
        cx.queue.push(chain, Action::EditInfo(dev.id));
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
    if is_plugin && ui.button("Reload Plug-In").clicked() {
        cx.queue.push(chain, Action::Reload(dev.id));
        ui.close();
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
        if widgets::state_button(ui, Vec2::new(44.0, 18.0), false, pal().accent, "Load").clicked() {
            cx.queue.push(chain, Action::LoadSample(dev.id, slot));
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
    // A Macro or automation moves the value, not the knob's own setting, so show what it's doing.
    let shown = if owned || automated {
        effective.map_or(base, |e| prm.from_normalized(e as f64))
    } else {
        base
    };
    let text = rt.format_value(prm, shown);
    let re_enable = std::cell::Cell::new(false);
    let state = ControlState {
        effective: effective.filter(|_| !owned && !automated),
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
}

/// The knob position of a parameter: its project value, the plugin's own, or its default.
fn param_value(dev: &Device, rt: &DeviceRuntime, param: &ParamInfo) -> f64 {
    dev.params
        .get(&param.id)
        .copied()
        .or_else(|| rt.plugin.as_ref().and_then(|h| h.param_value(param.id)))
        .unwrap_or(param.default)
}

fn rack(
    ui: &mut egui::Ui,
    cx: &mut DrawCx,
    node: &DeviceNode,
    rect: Rect,
    title: Rect,
    open: Option<&OpenChain>,
    on: bool,
) {
    let body = Rect::from_min_max(
        Pos2::new(rect.left() + 6.0, title.bottom() + 6.0),
        rect.max - Vec2::new(6.0, 4.0),
    );
    let mut bu = body_ui(ui, body);
    if !on {
        bu.multiply_opacity(0.55);
    }
    let dev = node.dev;
    if let Some(err) = node.rt.and_then(|r| r.error.as_ref()) {
        bu.label(RichText::new(err).color(pal().arm_red).size(10.5));
    }

    let list = Rect::from_min_size(body.min, Vec2::new(CHAIN_LIST_W, body.height()));
    bu.painter().rect_filled(list, 1.0, pal().field_bg);
    let mut lu = bu.new_child(UiBuilder::new().max_rect(list.shrink(2.0)));
    lu.spacing_mut().item_spacing = Vec2::new(0.0, 1.0);
    egui::ScrollArea::vertical()
        .id_salt(("rack_chains", dev.id))
        .auto_shrink([false, false])
        .show(&mut lu, |ui| {
            for chain in dev.chains() {
                let selected = open.is_some_and(|o| o.chain.id == chain.id);
                chain_row(ui, cx, dev, chain, selected);
            }
        });

    let inner = Rect::from_min_max(Pos2::new(list.right() + DEVICE_GAP, body.top()), body.max);
    let mut iu = bu.new_child(UiBuilder::new().max_rect(inner));
    match open {
        Some(open) => {
            iu.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = DEVICE_GAP;
                draw_chain(
                    ui,
                    cx,
                    open.at,
                    &open.devices,
                    inner.height(),
                    tree::INNER_DROP_W,
                );
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

fn chain_row(
    ui: &mut egui::Ui,
    cx: &mut DrawCx,
    rack: &Device,
    chain: &rlive_project::Chain,
    selected: bool,
) {
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), CHAIN_ROW_H), Sense::click());
    let fill = if selected {
        pal().raised_hover
    } else if resp.hovered() {
        pal().raised
    } else {
        egui::Color32::TRANSPARENT
    };
    let p = ui.painter();
    p.rect_filled(rect, 1.0, fill);
    if let Some(color) = chain.color {
        let swatch = Rect::from_min_size(rect.min, Vec2::new(3.0, rect.height()));
        p.rect_filled(swatch, 0.0, rgb(color));
    }
    let g = p.layout_no_wrap(
        chain.name.clone(),
        theme::font(10.5),
        if chain.mix.active {
            pal().text
        } else {
            pal().text_faint
        },
    );
    p.with_clip_rect(rect).galley(
        Pos2::new(rect.left() + 6.0, rect.center().y - g.size().y / 2.0),
        g,
        pal().text,
    );
    let resp = info(
        resp,
        format!("Chain “{}” — click to show its devices", chain.name),
    );
    if resp.clicked() {
        // Which chain it's found in doesn't matter: the choice is the view's, keyed by rack.
        cx.queue
            .push(ChainRef::Master, Action::OpenChain(rack.id, chain.id));
    }
}
