//! The automation lanes' menus, the box a value is typed into, and the context menu of mixer
//! controls.

use super::super::keymap::FocusArea;
use super::super::theme::{self, *};
use super::super::{MainView, UiState};
use super::geom::{parse_value, EnvHit};
use super::headers::{add_lane_with, device_of};
use super::lanes::{self, AddLane, LaneKey};
use super::{lane_target, LaneView};
use crate::session::{ChainRef, Session};
use egui::{Align2, Area, Frame, Id, Order, Pos2, Rect, RichText, Vec2};
use rlive_core::param::EnvTarget;
use rlive_project::envelope_edit::{self, Shape};

/// What the pointer was on when the lane's menu opened.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum MenuHit {
    Point(usize),
    Segment(usize),
    Line,
    Background,
}

impl From<EnvHit> for MenuHit {
    fn from(hit: EnvHit) -> Self {
        match hit {
            EnvHit::Point(i) => Self::Point(i),
            EnvHit::Line(Some(i)) | EnvHit::Near(i) => Self::Segment(i),
            EnvHit::Line(None) => Self::Line,
            EnvHit::Background => Self::Background,
        }
    }
}

pub(super) struct LaneMenu {
    pub key: LaneKey,
    pub pos: Pos2,
    pub hit: MenuHit,
    pub tick: f64,
}

/// A value being typed for a breakpoint, or a segment's nearer end.
pub(super) struct EditValue {
    key: LaneKey,
    target: EnvTarget,
    anchor: usize,
    group: Vec<usize>,
    text: String,
    pos: Pos2,
    focused: bool,
}

pub(super) struct ParamMenu {
    chain: ChainRef,
    target: EnvTarget,
    pos: Pos2,
    opened_at: f64,
}

fn selection_points(view: &LaneView, span: Option<(f64, f64)>) -> Vec<usize> {
    span.map_or_else(Vec::new, |(a, b)| {
        view.points
            .iter()
            .enumerate()
            .filter(|(_, p)| p.time >= a && p.time <= b)
            .map(|(i, _)| i)
            .collect()
    })
}

fn open_edit_value(
    state: &mut UiState,
    session: &Session,
    view: &LaneView,
    target: EnvTarget,
    anchor: usize,
    group: Vec<usize>,
    pos: Pos2,
) {
    let value = view.points.get(anchor).map_or(view.base, |p| p.value);
    let text = session.env_format(view.key.chain, target, value);
    state.arr.auto.edit_value = Some(EditValue {
        key: view.key,
        target,
        anchor,
        group,
        text,
        pos,
        focused: false,
    });
}

/// The context menu of a lane's background and envelope.
pub(super) fn lane_menu(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    view: &LaneView,
    grid: u64,
) {
    let Some(target) = view.target else {
        ui.close();
        return;
    };
    let Some(menu) = state.arr.auto.menu.as_ref().filter(|m| m.key == view.key) else {
        ui.close();
        return;
    };
    let (hit, tick, pos) = (menu.hit, menu.tick, menu.pos);
    let range = view.geom.range;
    let span = super::in_sel(&state.arr.time, view.key);
    if matches!(hit, MenuHit::Point(_) | MenuHit::Segment(_)) && ui.button("Edit Value").clicked() {
        let (anchor, group) = match hit {
            MenuHit::Point(i) => {
                let group = selection_points(view, span);
                let group = if group.contains(&i) { group } else { vec![i] };
                (i, group)
            }
            MenuHit::Segment(i) => {
                let nearest = if (tick - view.points[i].time).abs()
                    <= (view.points[i + 1].time - tick).abs()
                {
                    i
                } else {
                    i + 1
                };
                (nearest, vec![i, i + 1])
            }
            _ => unreachable!(),
        };
        open_edit_value(state, session, view, target, anchor, group, pos);
        ui.close();
    }
    if matches!(hit, MenuHit::Line | MenuHit::Segment(_)) && ui.button("Add Value").clicked() {
        let value = envelope_edit::value_at(&view.points, tick, range).unwrap_or(view.base);
        let mut points = view.points.clone();
        let at = envelope_edit::insert(&mut points, tick, value, range);
        session.set_envelope(view.key.chain, target, points);
        let refreshed = LaneView {
            points: session.env_points(view.key.chain, target).to_vec(),
            ..clone_view(view)
        };
        open_edit_value(state, session, &refreshed, target, at, vec![at], pos);
        ui.close();
    }
    if matches!(hit, MenuHit::Point(_)) && ui.button("Delete").clicked() {
        delete_at_hit(session, state, view, target, hit);
        ui.close();
    }
    if let Some((a, b)) = span {
        if ui.button("Delete Selection").clicked() {
            let points = envelope_edit::delete_range(&view.points, a, b, range);
            session.set_envelope(view.key.chain, target, points);
            ui.close();
        }
        if ui.button("Simplify Envelope").clicked() {
            let points = envelope_edit::simplify(&view.points, Some((a, b)), range);
            session.set_envelope(view.key.chain, target, points);
            ui.close();
        }
    }
    ui.separator();
    ui.menu_button("Insert Shape", |ui| {
        let (start, end) = span.unwrap_or_else(|| {
            let marker = session.insert_marker();
            (marker, marker + grid as f64)
        });
        for shapes in [&Shape::WAVEFORMS, &Shape::OTHERS] {
            for shape in shapes.iter().filter(|s| s.allowed(range.quantized)) {
                if ui.button(shape.name()).clicked() {
                    let points = envelope_edit::insert_shape(
                        &view.points,
                        *shape,
                        start,
                        end,
                        view.base,
                        range,
                    );
                    session.set_envelope(view.key.chain, target, points);
                    ui.close();
                }
            }
            ui.separator();
        }
    });
    ui.separator();
    let snap = state.arr.grid.snap;
    if ui.selectable_label(snap, "Snap to Grid").clicked() {
        state.arr.grid.snap = !snap;
        ui.close();
    }
    if !view.points.is_empty() && ui.button("Clear Envelope").clicked() {
        session.clear_envelope(view.key.chain, target);
        ui.close();
    }
}

fn delete_at_hit(
    session: &mut Session,
    state: &UiState,
    view: &LaneView,
    target: EnvTarget,
    hit: MenuHit,
) {
    let MenuHit::Point(i) = hit else { return };
    let mut group = selection_points(view, super::in_sel(&state.arr.time, view.key));
    if !group.contains(&i) {
        group = vec![i];
    }
    let points: Vec<_> = view
        .points
        .iter()
        .enumerate()
        .filter(|(n, _)| !group.contains(n))
        .map(|(_, p)| *p)
        .collect();
    session.set_envelope(view.key.chain, target, points);
}

fn clone_view(view: &LaneView) -> LaneView {
    LaneView {
        key: view.key,
        target: view.target,
        param: view.param.clone(),
        points: view.points.clone(),
        rect: view.rect,
        hit_rect: view.hit_rect,
        geom: view.geom,
        base: view.base,
    }
}

/// The context menu of a lane's header.
pub(super) fn header_menu(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    key: LaneKey,
) {
    let chain = key.chain;
    let target = lane_target(session, key);
    if key.slot.is_none() {
        if target.is_some() && ui.button("Add Automation Lane").clicked() {
            add_lane_with(session, state, chain, AddLane::Move);
            ui.close();
        }
        if ui.button("Add Lane for Each Automated Envelope").clicked() {
            add_lane_with(session, state, chain, AddLane::EachAutomated);
            ui.close();
        }
    } else if ui.button("Remove Automation Lane").clicked() {
        let selected: Vec<usize> = state
            .arr
            .auto
            .headers
            .iter()
            .filter(|k| k.chain == chain)
            .filter_map(|k| k.slot)
            .collect();
        let slots = if selected.contains(&key.slot.unwrap_or(usize::MAX)) {
            selected
        } else {
            key.slot.into_iter().collect()
        };
        session.edit_track_ui(chain, |ui| lanes::remove_lanes(ui, &slots));
        state.arr.auto.headers.clear();
        ui.close();
    }
    ui.separator();
    if let Some(target) = target.filter(|t| session.is_automated(chain, *t)) {
        if ui.button("Clear Envelope").clicked() {
            session.clear_envelope(chain, target);
            ui.close();
        }
        if let Some(d) = device_of(target) {
            let name = session
                .env_catalog(chain)
                .into_iter()
                .find(|g| g.device == Some(d))
                .map_or_else(|| "Device".to_string(), |g| g.label);
            if ui
                .button(format!("Clear All Envelopes of {name}"))
                .clicked()
            {
                session.clear_device_envelopes(chain, d);
                ui.close();
            }
        }
    }
    if ui.button("Clear All Envelopes of Track").clicked() {
        session.clear_track_envelopes(chain);
        ui.close();
    }
    if ui.button("Clear All Envelopes").clicked() {
        session.clear_all_envelopes();
        ui.close();
    }
}

/// A small tag showing a parameter value at a point on the screen.
pub(super) fn value_tag(ui: &egui::Ui, pos: Pos2, text: &str) {
    let painter = ui.ctx().layer_painter(egui::LayerId::new(
        Order::Tooltip,
        Id::unique("automation_value_tag"),
    ));
    let galley = painter.layout_no_wrap(text.to_string(), theme::font(10.0), pal().text);
    let rect = Rect::from_min_size(pos, galley.size() + Vec2::new(8.0, 4.0));
    painter.rect_filled(rect, 2.0, pal().raised);
    painter.galley(rect.min + Vec2::new(4.0, 2.0), galley, pal().text);
}

/// Shows a parameter's envelope in the main lane, or in the lane already showing it.
pub(super) fn show_control(
    session: &mut Session,
    state: &mut UiState,
    chain: ChainRef,
    target: EnvTarget,
) {
    if state.arr.auto.automated_only && !session.is_automated(chain, target) {
        return;
    }
    let mut other = None;
    session.edit_track_ui(chain, |ui| other = lanes::choose_main(ui, Some(target)));
    state.arr.auto.focus = Some(LaneKey {
        chain,
        slot: other,
        take: None,
    });
}

/// Records a right-click on a control that has no context menu of its own, so its automation
/// commands can be offered, and shows a control's envelope when one is clicked. Call right after
/// drawing the control; a control that has a menu of its own must not pass through here.
pub fn control_zone(
    ui: &egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    rect: Rect,
    chain: ChainRef,
    target: EnvTarget,
) {
    let (pos, secondary, primary, now) = ui.input(|i| {
        (
            i.pointer.interact_pos().or(i.pointer.hover_pos()),
            i.pointer.secondary_clicked(),
            i.pointer.primary_clicked(),
            i.time,
        )
    });
    let Some(pos) = pos.filter(|p| rect.contains(*p)) else {
        return;
    };
    if ui.ctx().layer_id_at(pos) != Some(ui.layer_id()) {
        return;
    }
    if secondary {
        state.arr.auto.param_menu = Some(ParamMenu {
            chain,
            target,
            pos,
            opened_at: now,
        });
    }
    if primary && state.arr.auto.mode {
        show_control(session, state, chain, target);
    }
}

/// Windows that float over everything: typing a value and a mixer control's menu.
pub fn show_popups(ctx: &egui::Context, session: &mut Session, state: &mut UiState) {
    edit_value_box(ctx, session, state);
    param_menu(ctx, session, state);
}

fn edit_value_box(ctx: &egui::Context, session: &mut Session, state: &mut UiState) {
    let Some(mut edit) = state.arr.auto.edit_value.take() else {
        return;
    };
    let mut keep = true;
    let mut commit = false;
    Area::new(Id::unique("automation_edit_value"))
        .order(Order::Foreground)
        .fixed_pos(edit.pos)
        .show(ctx, |ui| {
            Frame::popup(ui.style()).show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let r = ui.add(
                    egui::TextEdit::singleline(&mut edit.text)
                        .desired_width(90.0)
                        .font(theme::font(11.0)),
                );
                if !edit.focused {
                    r.request_focus();
                    edit.focused = true;
                }
                if r.lost_focus() {
                    commit = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    keep = false;
                }
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    keep = false;
                    commit = false;
                }
            });
        });
    if commit {
        let chain = edit.key.chain;
        if let (Some(param), Some(value)) = (
            session.env_param(chain, edit.target),
            parse_value(&edit.text),
        ) {
            let base = session.env_points(chain, edit.target).to_vec();
            let plain = if value.is_finite() {
                value
            } else {
                param.range.min
            };
            let points = envelope_edit::set_value_relative(
                &base,
                edit.anchor,
                &edit.group,
                plain,
                param.range,
            );
            session.set_envelope(chain, edit.target, points);
        } else if parse_value(&edit.text).is_none() {
            state.status = "That isn't a value".into();
        }
    }
    if keep {
        state.arr.auto.edit_value = Some(edit);
    }
}

fn param_menu(ctx: &egui::Context, session: &mut Session, state: &mut UiState) {
    let Some(menu) = state.arr.auto.param_menu.take() else {
        return;
    };
    let (chain, target) = (menu.chain, menu.target);
    let automated = session.is_automated(chain, target);
    let only = state.arr.auto.automated_only;
    let can_show = !only || automated;
    let mut close = false;
    let area = Area::new(Id::unique("automation_param_menu"))
        .order(Order::Foreground)
        .fixed_pos(menu.pos)
        .pivot(Align2::LEFT_TOP)
        .show(ctx, |ui| {
            Frame::menu(ui.style()).show(ui, |ui| {
                if ui
                    .add_enabled(can_show, egui::Button::new("Show Automation"))
                    .clicked()
                {
                    reveal(session, state, chain, target, false);
                    close = true;
                }
                if can_show && ui.button("Show Automation in New Lane").clicked() {
                    reveal(session, state, chain, target, true);
                    close = true;
                }
                if automated && ui.button("Delete Automation").clicked() {
                    session.delete_automation(chain, target);
                    close = true;
                }
                if !can_show {
                    ui.label(RichText::new("Only automated parameters are shown").small());
                }
            });
        });
    let elsewhere = ctx.input(|i| {
        i.time > menu.opened_at
            && i.pointer.any_click()
            && i.pointer
                .interact_pos()
                .is_some_and(|p| !area.response.rect.contains(p))
    });
    if !close && !elsewhere {
        state.arr.auto.param_menu = Some(menu);
    }
}

/// Automation Mode on, and the lane showing this parameter focused.
fn reveal(
    session: &mut Session,
    state: &mut UiState,
    chain: ChainRef,
    target: EnvTarget,
    new_lane: bool,
) {
    state.main_view = MainView::Arrangement;
    state.focus = FocusArea::Arrangement;
    state.arr.auto.mode = true;
    if new_lane {
        let mut slot = None;
        session.edit_track_ui(chain, |ui| {
            slot = lanes::show_in_new_lane(ui, chain, target)
        });
        state.arr.auto.focus = Some(LaneKey {
            chain,
            slot,
            take: None,
        });
    } else {
        show_control(session, state, chain, target);
    }
}
