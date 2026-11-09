//! The Chain List (manual pp. 495-496, 501): one row per chain with its activator, solo, volume
//! and pan, the Drum Rack's note and send controls, return chains, and the drop area below.

use super::super::DrawCx;
use super::selection::{arrow_step, insertion_index};
use super::{
    tiny_button, ActivePreview, ChainActivityView, ChainDrag, KeyFocus, RackAction, RackRef,
    Rename, RenameTarget,
};
use crate::session::SwapTarget;
use crate::session::{ChainRef, InfoTarget, ZoneKind};
use crate::ui::devices::{Action, ActionQueue};
use crate::ui::session_view::color_picker;
use crate::ui::theme::{self, pal, shade};
use crate::ui::widgets::{info, rgb};
use crate::ui::BrowserItem;
use egui::{Align2, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use rlive_core::param::{chain as controls, ParamOwner, ParamRef};
use rlive_core::{ChainId, DeviceId, ParamId, ParamInfo};
use rlive_project::{Chain, RackKind};

pub const ROW_H: f32 = 22.0;
pub const HEADER_H: f32 = 18.0;
pub const DROP_H: f32 = 22.0;
const BASE_W: f32 = 232.0;
const IN_OUT_W: f32 = 176.0;
const SEND_W: f32 = 34.0;
const RETURN_OUT_W: f32 = 70.0;
/// A return chain row's share of the list, in rows.
const MAX_RETURN_ROWS: usize = 3;
/// The preview note for a chain that answers every note.
const ALL_NOTES_PREVIEW: u8 = 60;

/// How wide the list is with these columns showing.
pub fn list_width(kind: RackKind, view: &super::RackView, returns: usize) -> f32 {
    let mut w = BASE_W;
    if kind == RackKind::Drum {
        if view.in_out {
            w += IN_OUT_W;
        }
        if view.sends {
            w += returns as f32 * SEND_W;
        }
        if view.returns {
            w += RETURN_OUT_W;
        }
    }
    w
}

/// Which zone editors a rack has: Audio Effect Racks have no key or velocity zones (p. 497), and
/// Drum Racks no zones at all.
pub fn zone_editors(kind: RackKind) -> &'static [ZoneKind] {
    match kind {
        RackKind::Drum => &[],
        RackKind::Audio => &[ZoneKind::Select],
        RackKind::Instrument | RackKind::Midi => {
            &[ZoneKind::Key, ZoneKind::Velocity, ZoneKind::Select]
        }
    }
}

fn zone_label(kind: ZoneKind) -> &'static str {
    match kind {
        ZoneKind::Key => "Key",
        ZoneKind::Velocity => "Vel",
        ZoneKind::Select => "Chain",
    }
}

/// How far the list can scroll with `rows` rows in `height`.
pub fn max_scroll(rows: usize, height: f32) -> f32 {
    (rows as f32 * ROW_H - height).max(0.0)
}

/// Draws the list.
pub fn section(
    ui: &mut egui::Ui,
    cx: &mut DrawCx,
    r: &RackRef,
    rect: Rect,
    activity: &ChainActivityView,
    open: Option<ChainId>,
) {
    ui.painter().rect_filled(rect, 2.0, pal().field_bg);
    let header = Rect::from_min_size(rect.min, Vec2::new(rect.width(), HEADER_H));
    header_row(ui, cx, r, header);

    let return_rows = if r.kind == RackKind::Drum && r.view.returns {
        r.returns.len().min(MAX_RETURN_ROWS) + 1
    } else {
        0
    };
    let bottom = rect.bottom() - DROP_H - return_rows as f32 * ROW_H;
    let rows = Rect::from_min_max(
        Pos2::new(rect.left(), header.bottom()),
        Pos2::new(rect.right(), bottom),
    );

    let limit = max_scroll(r.chains.len(), rows.height());
    let mut scroll = cx.ui.list_scroll.get(&r.dev.id).copied().unwrap_or(0.0);
    if ui.rect_contains_pointer(rows) {
        scroll -= ui.input(|i| i.smooth_scroll_delta.y);
    }
    let scroll = scroll.clamp(0.0, limit);
    cx.ui.list_scroll.insert(r.dev.id, scroll);

    let mut lu = ui.new_child(egui::UiBuilder::new().max_rect(rows));
    lu.set_clip_rect(rows.intersect(ui.clip_rect()));
    let infos = controls::infos();
    for (i, c) in r.chains.iter().enumerate() {
        let row = Rect::from_min_size(
            Pos2::new(rect.left(), rows.top() + i as f32 * ROW_H - scroll),
            Vec2::new(rect.width(), ROW_H),
        );
        if row.bottom() < rows.top() || row.top() > rows.bottom() {
            continue;
        }
        chain_row(&mut lu, cx, r, c, i, row, &infos, false, activity, open);
    }
    keyboard(ui, cx, r);

    if return_rows > 0 {
        return_section(ui, cx, r, rect, bottom, &infos, activity, open);
    }
    drop_area(ui, cx, r, rect);
}

fn header_row(ui: &mut egui::Ui, cx: &mut DrawCx, r: &RackRef, header: Rect) {
    let mut x = header.left() + 2.0;
    for kind in zone_editors(r.kind).iter().copied() {
        let b = Rect::from_min_size(Pos2::new(x, header.top() + 2.0), Vec2::new(36.0, 14.0));
        let on = r.view.zones == Some(kind);
        let label = zone_label(kind);
        let resp = tiny_button(ui, b, ("zone_kind", label), label, on, pal().accent);
        let hover = match kind {
            ZoneKind::Key => "Key — show the Key Zone Editor",
            ZoneKind::Velocity => "Velocity — show the Velocity Zone Editor",
            ZoneKind::Select => "Chain — show the Chain Select Editor",
        };
        if info(resp, hover).clicked() {
            cx.queue.push(
                r.chain,
                Action::Rack(r.dev.id, RackAction::ZoneEditor((!on).then_some(kind))),
            );
        }
        x += 38.0;
    }
    if !zone_editors(r.kind).is_empty() {
        let b = Rect::from_min_size(Pos2::new(x, header.top() + 2.0), Vec2::new(36.0, 14.0));
        let resp = tiny_button(ui, b, "zone_hide", "Hide", false, pal().accent);
        if info(resp, "Hide — hide the zone editors").clicked() {
            cx.queue.push(
                r.chain,
                Action::Rack(r.dev.id, RackAction::ZoneEditor(None)),
            );
        }
    }
    let add = Rect::from_min_size(
        Pos2::new(header.right() - 18.0, header.top() + 2.0),
        Vec2::new(16.0, 14.0),
    );
    let resp = tiny_button(ui, add, "chain_add", "+", false, pal().accent);
    if info(resp, "Add an empty chain").clicked() {
        cx.queue
            .push(r.chain, Action::Rack(r.dev.id, RackAction::AddChain));
    }
}

#[allow(clippy::too_many_arguments)]
fn chain_row(
    ui: &mut egui::Ui,
    cx: &mut DrawCx,
    r: &RackRef,
    c: &Chain,
    index: usize,
    row: Rect,
    infos: &[ParamInfo],
    is_return: bool,
    activity: &ChainActivityView,
    open: Option<ChainId>,
) {
    let rack = r.dev.id;
    let chain = r.chain;
    let selected =
        r.view.selection.contains(&c.id) || (r.view.selection.is_empty() && open == Some(c.id));
    let resp = ui.interact(
        row,
        ui.scope_id().with(("chain_row", c.id)),
        Sense::click_and_drag(),
    );
    let p = ui.painter().clone();
    p.rect_filled(
        row,
        0.0,
        if selected {
            pal().raised_hover
        } else if resp.hovered() {
            pal().raised
        } else {
            egui::Color32::TRANSPARENT
        },
    );
    p.line_segment(
        [row.left_bottom(), row.right_bottom()],
        Stroke::new(1.0, pal().separator),
    );
    let swatch = Rect::from_min_size(row.min, Vec2::new(3.0, row.height() - 1.0));
    if let Some(color) = c.color {
        p.rect_filled(swatch, 0.0, rgb(color));
    }

    if resp.clicked() {
        cx.ui.key_focus = Some(KeyFocus::ChainList(rack));
        let (shift, cmd) = ui.input(|i| (i.modifiers.shift, i.modifiers.command));
        cx.queue.push(
            chain,
            Action::Rack(
                rack,
                RackAction::ChainClick {
                    id: c.id,
                    order: r.order(),
                    shift,
                    cmd,
                },
            ),
        );
    }
    if resp.double_clicked() {
        cx.ui.rename = Some(Rename {
            target: RenameTarget::Chain(rack, c.id),
            text: c.name.clone(),
        });
    }

    // Activator, solo, Hot-Swap.
    let mut x = row.left() + 5.0;
    let y = row.top() + 3.0;
    let btn = Vec2::new(16.0, ROW_H - 7.0);
    let on_r = Rect::from_min_size(Pos2::new(x, y), btn);
    let active = c.mix.active;
    let resp_on = tiny_button(ui, on_r, ("chain_on", c.id), "", active, pal().activator);
    map_or_press(
        ui,
        cx,
        chain,
        rack,
        c.id,
        resp_on,
        controls::ACTIVATOR,
        active,
    );
    x += 18.0;
    let solo_r = Rect::from_min_size(Pos2::new(x, y), btn);
    let solo = c.mix.solo;
    let resp_solo = tiny_button(ui, solo_r, ("chain_solo", c.id), "S", solo, pal().solo_blue);
    map_or_press(ui, cx, chain, rack, c.id, resp_solo, controls::SOLO, solo);
    x += 18.0;
    let swap_r = Rect::from_min_size(Pos2::new(x, y), btn);
    let swapping = cx.swap_target
        == Some(SwapTarget::RackChain {
            chain,
            rack,
            id: c.id,
        });
    let swap = tiny_button(
        ui,
        swap_r,
        ("chain_swap", c.id),
        "H",
        swapping,
        pal().accent,
    );
    if info(
        swap,
        "Hot-Swap — replace this chain's devices from the browser",
    )
    .clicked()
    {
        cx.queue.push(
            chain,
            Action::HotSwap(SwapTarget::RackChain {
                chain,
                rack,
                id: c.id,
            }),
        );
    }
    x += 20.0;

    // Name.
    let name_end = if is_return {
        row.right() - 130.0 - RETURN_OUT_W
    } else {
        row.left() + BASE_W - 100.0
    };
    let name = Rect::from_min_max(Pos2::new(x, row.top()), Pos2::new(name_end, row.bottom()));
    let renaming = matches!(&cx.ui.rename, Some(Rename { target: RenameTarget::Chain(d, id), .. }) if *d == rack && *id == c.id);
    if renaming {
        rename_field(ui, cx, chain, rack, c.id, name);
    } else {
        let text = if c.mix.active {
            pal().text
        } else {
            pal().text_faint
        };
        p.with_clip_rect(name.intersect(ui.clip_rect())).text(
            Pos2::new(name.left(), name.center().y),
            Align2::LEFT_CENTER,
            &c.name,
            theme::font(10.5),
            text,
        );
    }
    if activity.is_active(c.id) {
        p.circle_filled(
            Pos2::new(name.right() - 3.0, row.center().y),
            2.5,
            pal().accent,
        );
    }

    // Volume and pan.
    let mut cx_x = row.left() + BASE_W - 96.0;
    let vol = Rect::from_min_size(Pos2::new(cx_x, y), Vec2::new(54.0, btn.y));
    mini_control(ui, cx, r, c, controls::VOLUME, infos, vol);
    cx_x += 56.0;
    let pan = Rect::from_min_size(Pos2::new(cx_x, y), Vec2::new(38.0, btn.y));
    mini_control(ui, cx, r, c, controls::PAN, infos, pan);
    cx_x += 40.0;

    if r.kind == RackKind::Drum {
        if !is_return && r.view.in_out {
            pad_controls(ui, cx, r, c, Pos2::new(cx_x + 2.0, y));
            cx_x += IN_OUT_W;
        } else if is_return && r.view.in_out {
            cx_x += IN_OUT_W;
        }
        if r.view.sends {
            for n in 0..r.returns.len() {
                let rect =
                    Rect::from_min_size(Pos2::new(cx_x + 2.0, y), Vec2::new(SEND_W - 4.0, btn.y));
                if !is_return {
                    let control = ParamId(controls::FIRST_SEND.0 + n as u32);
                    mini_control(ui, cx, r, c, control, infos, rect);
                }
                cx_x += SEND_W;
            }
        }
        if is_return {
            return_output(ui, cx, r, c, Pos2::new(cx_x + 2.0, y));
        }
    }

    // Dragging.
    let dragged_ids = if r.view.selection.contains(&c.id) {
        r.view.selection.clone()
    } else {
        vec![c.id]
    };
    if resp.drag_started() {
        resp.dnd_set_drag_payload(ChainDrag {
            at: chain,
            rack,
            ids: dragged_ids,
            from_mixer: false,
            is_return,
        });
    }
    let drop_row = resp
        .interact_pointer_pos()
        .map(|pos| index + usize::from(pos.y > row.center().y));
    if let Some(drag) = resp.dnd_hover_payload::<ChainDrag>() {
        if !drag.is_return && !is_return {
            let y = if resp.hover_pos().is_some_and(|p| p.y > row.center().y) {
                row.bottom()
            } else {
                row.top()
            };
            p.line_segment(
                [Pos2::new(row.left(), y), Pos2::new(row.right(), y)],
                Stroke::new(2.0, pal().accent),
            );
        }
    }
    if let Some(drag) = crate::ui::widgets::released::<ChainDrag>(&resp) {
        if !is_return && !drag.is_return {
            let row = drop_row.unwrap_or(index);
            push_chain_move(cx.queue, chain, rack, &r.order(), &drag, row);
        }
    }
    if let Some(item) = crate::ui::widgets::released::<BrowserItem>(&resp) {
        cx.queue.push(
            chain,
            Action::Rack(rack, RackAction::AddToChain(c.id, (*item).clone())),
        );
    }

    let resp = info(
        resp,
        format!("Chain “{}” — right-click for options", c.name),
    );
    crate::ui::widgets::info_note(&resp, &c.info);
    let selection = r.view.selection.clone();
    let name = c.name.clone();
    let color = c.color;
    resp.context_menu(|ui| {
        chain_menu(
            ui,
            cx.queue,
            &mut cx.ui.rename,
            ChainMenu {
                at: chain,
                rack,
                id: c.id,
                name: &name,
                color,
                selection: &selection,
                is_return,
            },
        );
    });
}

/// A click that's a mapping target in Map Mode, or a press of the button otherwise.
#[allow(clippy::too_many_arguments)]
fn map_or_press(
    ui: &mut egui::Ui,
    cx: &mut DrawCx,
    chain: ChainRef,
    rack: DeviceId,
    id: ChainId,
    resp: egui::Response,
    control: ParamId,
    on: bool,
) {
    let target = ParamRef {
        owner: ParamOwner::Chain(id),
        id: control,
    };
    let locked = cx.session.is_macro_mapped(chain, target);
    let rect = resp.rect;
    let hover = if control == controls::ACTIVATOR {
        "Chain Activator — turn this chain on or off"
    } else {
        "Solo — hear only this chain"
    };
    let resp = info(resp, hover);
    if resp.clicked() && !locked {
        cx.queue.push(
            chain,
            Action::Rack(
                rack,
                RackAction::ChainParam(id, control, (!on) as u8 as f64),
            ),
        );
    }
    super::map_overlay(ui, cx, chain, rect, target);
}

fn rename_field(
    ui: &mut egui::Ui,
    cx: &mut DrawCx,
    chain: ChainRef,
    rack: DeviceId,
    id: ChainId,
    rect: Rect,
) {
    let mut done = None;
    if let Some(rename) = &mut cx.ui.rename {
        let edit = ui.put(
            rect,
            egui::TextEdit::singleline(&mut rename.text).font(theme::font(10.5)),
        );
        edit.request_focus();
        if edit.lost_focus() {
            done = Some(rename.text.clone());
        }
    }
    if let Some(text) = done {
        cx.ui.rename = None;
        cx.queue
            .push(chain, Action::Rack(rack, RackAction::RenameChain(id, text)));
    }
}

/// Queues the move of the dragged chains to before `drop_row` in `order`.
pub(crate) fn push_chain_move(
    queue: &mut ActionQueue,
    to: ChainRef,
    rack: DeviceId,
    order: &[ChainId],
    drag: &ChainDrag,
    drop_row: usize,
) {
    let index = insertion_index(order, &drag.ids, drop_row);
    queue.push(
        to,
        Action::Rack(
            rack,
            RackAction::MoveChains {
                from: drag.at,
                from_rack: drag.rack,
                ids: drag.ids.clone(),
                index,
            },
        ),
    );
}

/// A narrow slider for a control on a chain's strip.
fn mini_control(
    ui: &mut egui::Ui,
    cx: &mut DrawCx,
    r: &RackRef,
    c: &Chain,
    control: ParamId,
    infos: &[ParamInfo],
    rect: Rect,
) {
    let Some(param) = infos.iter().find(|p| p.param_id() == control) else {
        return;
    };
    let target = ParamRef {
        owner: ParamOwner::Chain(c.id),
        id: control,
    };
    let locked = cx.session.is_macro_mapped(r.chain, target);
    let value = cx
        .session
        .chain_param(r.chain, r.dev.id, c.id, control)
        .unwrap_or(param.default);
    let shown = if locked {
        cx.session
            .chain_effective_norm(r.chain, c.id, control)
            .map_or(param.to_normalized(value), |n| n as f64)
    } else {
        param.to_normalized(value)
    };
    let sense = if locked {
        Sense::hover()
    } else {
        Sense::click_and_drag()
    };
    let resp = ui.interact(rect, ui.scope_id().with(("mini", c.id, control.0)), sense);
    let p = ui.painter();
    p.rect_filled(rect, 1.0, shade(0x1f1f1f));
    let fill = Rect::from_min_size(
        rect.min,
        Vec2::new(rect.width() * shown as f32, rect.height()),
    );
    p.rect_filled(
        fill,
        1.0,
        if locked {
            pal().text_faint
        } else {
            pal().accent.gamma_multiply(0.75)
        },
    );
    let text = format!("{}", param.from_normalized(shown).round());
    p.text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        theme::font(9.0),
        pal().text,
    );
    if resp.dragged() {
        let fine = ui.input(|i| i.modifiers.shift);
        let delta = resp.drag_delta().x / if fine { 800.0 } else { rect.width() * 3.0 };
        let next = param.from_normalized((shown + delta as f64).clamp(0.0, 1.0));
        cx.queue.push(
            r.chain,
            Action::Rack(r.dev.id, RackAction::ChainParam(c.id, control, next)),
        );
    }
    if resp.double_clicked() {
        cx.queue.push(
            r.chain,
            Action::Rack(
                r.dev.id,
                RackAction::ChainParam(c.id, control, param.default),
            ),
        );
    }
    info(resp, format!("{} — drag to change it", param.name));
    super::map_overlay(ui, cx, r.chain, rect, target);
}

#[allow(clippy::too_many_arguments)]
fn return_section(
    ui: &mut egui::Ui,
    cx: &mut DrawCx,
    r: &RackRef,
    rect: Rect,
    top: f32,
    infos: &[ParamInfo],
    activity: &ChainActivityView,
    open: Option<ChainId>,
) {
    let head = Rect::from_min_size(Pos2::new(rect.left(), top), Vec2::new(rect.width(), ROW_H));
    ui.painter().rect_filled(head, 0.0, pal().raised);
    ui.painter().text(
        Pos2::new(head.left() + 6.0, head.center().y),
        Align2::LEFT_CENTER,
        format!("Return Chains ({})", r.returns.len()),
        theme::font_bold(10.0),
        pal().text_dim,
    );
    if r.returns.len() < controls::MAX_RETURNS {
        let add = Rect::from_min_size(
            Pos2::new(head.right() - 20.0, head.top() + 3.0),
            Vec2::new(16.0, ROW_H - 7.0),
        );
        let resp = tiny_button(ui, add, ("return_add", r.dev.id), "+", false, pal().accent);
        if info(resp, "Add a return chain").clicked() {
            cx.queue
                .push(r.chain, Action::Rack(r.dev.id, RackAction::AddReturn));
        }
    }
    for (i, c) in r.returns.iter().take(MAX_RETURN_ROWS).enumerate() {
        let row = Rect::from_min_size(
            Pos2::new(rect.left(), head.bottom() + i as f32 * ROW_H),
            Vec2::new(rect.width(), ROW_H),
        );
        chain_row(ui, cx, r, c, i, row, infos, true, activity, open);
    }
}

/// Preview, Receive, Play and Choke (manual p. 501).
fn pad_controls(ui: &mut egui::Ui, cx: &mut DrawCx, r: &RackRef, c: &Chain, at: Pos2) {
    let Some(pad) = c.zones.drum else {
        return;
    };
    let rack = r.dev.id;
    let h = ROW_H - 7.0;
    let pv = Rect::from_min_size(at, Vec2::new(14.0, h));
    let resp = ui.interact(pv, ui.scope_id().with(("preview", c.id)), Sense::click());
    ui.painter().rect_filled(pv, 1.0, pal().raised);
    crate::ui::icons::play(ui.painter(), pv.shrink(2.0), pal().text);
    let pressed = resp.is_pointer_button_down_on();
    info(resp, "Preview — fires a note into this chain");
    if pressed && cx.ui.preview.is_none() {
        let active = ActivePreview {
            chain: r.chain,
            rack,
            target: Some(c.id),
            key: pad.receive.unwrap_or(ALL_NOTES_PREVIEW),
        };
        cx.ui.preview = Some(active);
        cx.queue.push(
            r.chain,
            Action::Rack(rack, RackAction::Preview(active, true)),
        );
    }

    let mut next = pad;
    let receive_r = Rect::from_min_size(at + Vec2::new(16.0, 0.0), Vec2::new(56.0, h));
    let mut ru = ui.new_child(egui::UiBuilder::new().max_rect(receive_r));
    ru.spacing_mut().interact_size.y = h;
    egui::ComboBox::from_id_salt(("receive", c.id))
        .selected_text(
            pad.receive
                .map_or("All Notes".to_string(), super::pads::note_name),
        )
        .width(56.0)
        .wrap_mode(egui::TextWrapMode::Truncate)
        .height(280.0)
        .show_ui(&mut ru, |ui| {
            if ui
                .selectable_label(pad.receive.is_none(), "All Notes")
                .clicked()
            {
                next.receive = None;
            }
            for key in (0..=127u8).rev() {
                let label = receive_label(key);
                if ui
                    .selectable_label(pad.receive == Some(key), label)
                    .clicked()
                {
                    next.receive = Some(key);
                }
            }
        });
    let enabled = pad.receive.is_some();
    let play_r = Rect::from_min_size(at + Vec2::new(74.0, 0.0), Vec2::new(40.0, h));
    let mut pu = ui.new_child(egui::UiBuilder::new().max_rect(play_r));
    pu.add_enabled_ui(enabled, |ui| {
        let mut key = pad.play as f64;
        let changed = ui
            .add_sized(
                play_r.size(),
                egui::DragValue::new(&mut key)
                    .range(0.0..=127.0)
                    .speed(0.2)
                    .custom_formatter(|v, _| super::pads::note_name(v.round() as u8)),
            )
            .changed();
        if changed {
            next.play = key.round() as u8;
        }
    });
    let choke_r = Rect::from_min_size(at + Vec2::new(116.0, 0.0), Vec2::new(54.0, h));
    let mut cu = ui.new_child(egui::UiBuilder::new().max_rect(choke_r));
    cu.add_enabled_ui(enabled, |ui| {
        ui.spacing_mut().interact_size.y = h;
        egui::ComboBox::from_id_salt(("choke", c.id))
            .selected_text(pad.choke.map_or("None".to_string(), |g| g.to_string()))
            .width(54.0)
            .wrap_mode(egui::TextWrapMode::Extend)
            .show_ui(ui, |ui| {
                if ui.selectable_label(pad.choke.is_none(), "None").clicked() {
                    next.choke = None;
                }
                for g in 1..=16u8 {
                    if ui
                        .selectable_label(pad.choke == Some(g), g.to_string())
                        .clicked()
                    {
                        next.choke = Some(g);
                    }
                }
            });
    });
    if next != pad {
        cx.queue
            .push(r.chain, Action::Rack(rack, RackAction::DrumPad(c.id, next)));
    }
}

/// How a note reads in the Receive list: name, number and GM drum (manual p. 501).
pub fn receive_label(key: u8) -> String {
    let name = super::pads::note_name(key);
    match super::pads::gm_drum_name(key) {
        Some(drum) => format!("{name} ({key}) {drum}"),
        None => format!("{name} ({key})"),
    }
}

/// A return chain's Audio To chooser: the rack, or one of the Set's return tracks (p. 502).
fn return_output(ui: &mut egui::Ui, cx: &mut DrawCx, r: &RackRef, c: &Chain, at: Pos2) {
    let area = Rect::from_min_size(at, Vec2::new(RETURN_OUT_W - 6.0, ROW_H - 7.0));
    let mut ou = ui.new_child(egui::UiBuilder::new().max_rect(area));
    ou.spacing_mut().interact_size.y = area.height();
    let session = cx.session;
    let label = c
        .mix
        .output
        .and_then(|t| session.project.track(t))
        .map_or("Rack".to_string(), |t| t.name.clone());
    egui::ComboBox::from_id_salt(("audio_to", c.id))
        .selected_text(label)
        .width(area.width())
        .show_ui(&mut ou, |ui| {
            if ui
                .selectable_label(c.mix.output.is_none(), "Rack")
                .clicked()
            {
                cx.queue.push(
                    r.chain,
                    Action::Rack(r.dev.id, RackAction::ReturnOutput(c.id, None)),
                );
            }
            for t in &session.project.returns {
                if ui
                    .selectable_label(c.mix.output == Some(t.id), &t.name)
                    .clicked()
                {
                    cx.queue.push(
                        r.chain,
                        Action::Rack(r.dev.id, RackAction::ReturnOutput(c.id, Some(t.id))),
                    );
                }
            }
        });
}

/// The drop area under the list: new chains from the browser or from other racks (p. 495).
fn drop_area(ui: &mut egui::Ui, cx: &mut DrawCx, r: &RackRef, rect: Rect) {
    let area = Rect::from_min_max(Pos2::new(rect.left(), rect.bottom() - DROP_H), rect.max);
    let resp = ui.interact(
        area,
        ui.scope_id().with(("chain_drop", r.dev.id)),
        Sense::hover(),
    );
    let hovering = resp.dnd_hover_payload::<BrowserItem>().is_some()
        || resp.dnd_hover_payload::<ChainDrag>().is_some();
    ui.painter().rect_stroke(
        area.shrink(1.0),
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
    ui.painter().text(
        area.center(),
        Align2::CENTER_CENTER,
        "Drop to add a chain",
        theme::font(9.5),
        if hovering {
            pal().accent
        } else {
            pal().text_faint
        },
    );
    if let Some(item) = crate::ui::widgets::released::<BrowserItem>(&resp) {
        cx.queue.push(
            r.chain,
            Action::Rack(r.dev.id, RackAction::AddChainWith((*item).clone())),
        );
    }
    if let Some(drag) = crate::ui::widgets::released::<ChainDrag>(&resp) {
        if !drag.is_return {
            push_chain_move(
                cx.queue,
                r.chain,
                r.dev.id,
                &r.order(),
                &drag,
                r.chains.len(),
            );
        }
    }
    info(
        resp,
        "Drop instruments, effects, samples or chains here to add a chain",
    );
}

/// The arrow keys flip through the list while it has focus (p. 496).
fn keyboard(ui: &egui::Ui, cx: &mut DrawCx, r: &RackRef) {
    if cx.ui.key_focus != Some(KeyFocus::ChainList(r.dev.id)) || !cx.keyboard_free {
        return;
    }
    let (up, down) = ui.input(|i| {
        (
            i.key_pressed(egui::Key::ArrowUp),
            i.key_pressed(egui::Key::ArrowDown),
        )
    });
    let delta = match (up, down) {
        (true, _) => -1,
        (_, true) => 1,
        _ => return,
    };
    if let Some(id) = arrow_step(&r.order(), &r.view.selection, delta) {
        cx.queue.push(
            r.chain,
            Action::Rack(r.dev.id, RackAction::Select(vec![id])),
        );
    }
}

/// What a chain's context menu is about.
pub(crate) struct ChainMenu<'a> {
    pub at: ChainRef,
    pub rack: DeviceId,
    pub id: ChainId,
    pub name: &'a str,
    pub color: Option<[u8; 3]>,
    pub selection: &'a [ChainId],
    pub is_return: bool,
}

/// The chain's context menu, in the list and the mixer alike.
pub(crate) fn chain_menu(
    ui: &mut egui::Ui,
    queue: &mut ActionQueue,
    rename: &mut Option<Rename>,
    menu: ChainMenu,
) {
    let ChainMenu {
        at,
        rack,
        id,
        name,
        color,
        selection,
        is_return,
    } = menu;
    let ids: Vec<ChainId> = if selection.contains(&id) {
        selection.to_vec()
    } else {
        vec![id]
    };
    if ui.button("Rename").clicked() {
        *rename = Some(Rename {
            target: RenameTarget::Chain(rack, id),
            text: name.to_string(),
        });
        ui.close();
    }
    if ui.button("Edit Info Text").clicked() {
        queue.push(at, Action::EditInfoTarget(InfoTarget::Chain(at, rack, id)));
        ui.close();
    }
    ui.separator();
    if let Some(picked) = color_picker(ui) {
        queue.push(
            at,
            Action::Rack(rack, RackAction::ChainColor(id, Some(picked))),
        );
        ui.close();
    }
    if color.is_some() && ui.button("Clear Color").clicked() {
        queue.push(at, Action::Rack(rack, RackAction::ChainColor(id, None)));
        ui.close();
    }
    ui.separator();
    if !is_return {
        if ui
            .add_enabled(ids.len() > 1, egui::Button::new("Group"))
            .clicked()
        {
            queue.push(at, Action::Rack(rack, RackAction::GroupChains(ids.clone())));
            ui.close();
        }
        if ui.button("Duplicate").clicked() {
            queue.push(
                at,
                Action::Rack(rack, RackAction::DuplicateChains(ids.clone())),
            );
            ui.close();
        }
    }
    if ui
        .add_enabled(ids.len() == 1, egui::Button::new("Save Preset"))
        .clicked()
    {
        queue.push(at, Action::SaveChainPreset(rack, ids[0]));
        ui.close();
    }
    ui.separator();
    if ui
        .button(egui::RichText::new("Delete").color(pal().arm_red))
        .clicked()
    {
        queue.push(at, Action::Rack(rack, RackAction::DeleteChains(ids)));
        ui.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_has_the_zone_editors_the_manual_gives_it() {
        assert_eq!(zone_editors(RackKind::Audio), [ZoneKind::Select]);
        assert_eq!(zone_editors(RackKind::Drum), []);
        assert_eq!(
            zone_editors(RackKind::Midi),
            [ZoneKind::Key, ZoneKind::Velocity, ZoneKind::Select]
        );
        assert_eq!(zone_editors(RackKind::Instrument).len(), 3);
    }

    #[test]
    fn a_receive_entry_names_the_note_number_and_drum() {
        assert_eq!(receive_label(36), "C1 (36) Bass Drum 1");
        assert_eq!(receive_label(60), "C3 (60) Hi Bongo");
        assert_eq!(receive_label(100), "E6 (100)");
    }

    #[test]
    fn the_list_scrolls_only_as_far_as_its_rows() {
        assert_eq!(max_scroll(2, 100.0), 0.0);
        assert_eq!(max_scroll(10, 100.0), 120.0);
    }

    #[test]
    fn drum_columns_widen_the_list_and_others_do_not() {
        let v = super::super::RackView::for_kind(RackKind::Drum);
        let plain = list_width(RackKind::Audio, &v, 2);
        assert_eq!(plain, BASE_W);
        let drum = list_width(RackKind::Drum, &v, 2);
        assert_eq!(drum, BASE_W + IN_OUT_W + 2.0 * SEND_W + RETURN_OUT_W);
    }
}
