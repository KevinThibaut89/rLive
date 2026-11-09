//! The right-hand side of the automation lanes: choosers, and the buttons that add and remove
//! lanes.

use super::super::arrangement::Rows;
use super::super::keymap::Platform;
use super::super::session_view;
use super::super::theme::{self, *};
use super::super::widgets::{self, info, rgb};
use super::super::UiState;
use super::lanes::{self, AddLane, LaneKey};
use super::{lane_target, popups};
use crate::session::{ChainRef, EnvGroup, Session};
use egui::{Align2, ComboBox, Layout, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use rlive_core::param::{transport, EnvTarget, ParamOwner, ParamRef};
use rlive_core::DeviceId;
use rlive_project::TrackKind;

const CHOOSER_H: f32 = 18.0;

pub fn show_headers(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    headers: Rect,
    rows: &Rows,
) {
    let sy = state.arr.scroll.y;
    for (i, row) in rows.rows.iter().enumerate() {
        let main = rows.main(headers, i, sy);
        if main.bottom() >= headers.top() && main.top() <= headers.bottom() {
            if row.index.is_none() {
                bus_header(ui, session, state, main, row.chain);
            }
            main_lane_header(ui, session, state, headers, main, row.chain);
        }
        for n in 0..row.extra.len() {
            let rect = rows.extra(headers, i, n, sy);
            if rect.bottom() >= headers.top() && rect.top() <= headers.bottom() {
                extra_lane_header(ui, session, state, headers, rect, row.chain, n);
            }
        }
    }
}

/// The title of a row with no clip lane of its own header: a return track or the Master.
fn bus_header(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    rect: Rect,
    chain: ChainRef,
) {
    let (name, color) = match chain {
        ChainRef::Track(t) => session
            .project
            .track(t)
            .filter(|t| t.kind == TrackKind::Return)
            .map_or(("".to_string(), [80, 80, 80]), |t| {
                (t.name.clone(), t.color)
            }),
        _ => ("Main".to_string(), [110, 110, 110]),
    };
    let resp = ui.allocate_rect(rect, Sense::click());
    let selected = state.selected == Some(chain);
    let p = ui.painter().clone();
    p.rect_filled(
        rect,
        0.0,
        if selected {
            shade(0x3b3b3b)
        } else {
            shade(0x303030)
        },
    );
    p.line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        Stroke::new(1.0, pal().separator),
    );
    p.rect_filled(
        Rect::from_min_size(rect.min, Vec2::new(6.0, rect.height())),
        0.0,
        rgb(color),
    );
    p.text(
        rect.left_top() + Vec2::new(12.0, 5.0),
        Align2::LEFT_TOP,
        name,
        theme::font_bold(11.0),
        pal().text,
    );
    if resp.clicked() {
        state.selected = Some(chain);
    }
}

fn main_lane_header(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    headers: Rect,
    rect: Rect,
    chain: ChainRef,
) {
    let key = LaneKey { chain, slot: None };
    let target = lane_target(session, key);
    let inner = Rect::from_min_max(
        Pos2::new(rect.left() + 10.0, rect.bottom() - 42.0),
        Pos2::new(rect.right() - 10.0, rect.bottom() - 2.0),
    );
    header_zone(ui, session, state, inner, key);
    choosers(ui, session, state, headers, inner, key, target);
    if target == Some(transport::tempo_target()) {
        let row = Rect::from_min_size(
            Pos2::new(rect.left() + 10.0, rect.top() + 22.0),
            Vec2::new(rect.width() - 56.0, CHOOSER_H),
        );
        tempo_range(ui, session, headers, row);
    }
    lane_buttons(ui, session, state, headers, rect, chain, target);
}

fn extra_lane_header(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    headers: Rect,
    rect: Rect,
    chain: ChainRef,
    slot: usize,
) {
    let key = LaneKey {
        chain,
        slot: Some(slot),
    };
    let p = ui.painter();
    let selected = state.arr.auto.headers.contains(&key) || state.arr.auto.focus == Some(key);
    p.rect_filled(
        rect,
        0.0,
        if selected {
            shade(0x3b3b3b)
        } else {
            shade(0x2c2c2c)
        },
    );
    p.line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        Stroke::new(1.0, pal().separator),
    );
    let inner = Rect::from_min_max(
        rect.min + Vec2::new(10.0, 3.0),
        Pos2::new(rect.right() - 24.0, rect.top() + 43.0),
    );
    let target = lane_target(session, key);
    header_zone(ui, session, state, rect, key);
    choosers(ui, session, state, headers, inner, key, target);
    if target == Some(transport::tempo_target()) {
        let row = Rect::from_min_size(
            Pos2::new(rect.left() + 10.0, rect.top() + 43.0),
            Vec2::new(rect.width() - 34.0, 17.0),
        );
        tempo_range(ui, session, headers, row);
    }
    let mut bu = session_view::child(
        ui,
        headers,
        Rect::from_min_size(
            Pos2::new(rect.right() - 22.0, rect.top() + 3.0),
            Vec2::new(18.0, 16.0),
        ),
        Layout::left_to_right(egui::Align::Center),
    );
    let r = widgets::state_button(&mut bu, Vec2::new(18.0, 16.0), false, pal().accent, "-");
    if info(
        r,
        "Remove Automation Lane — Cmd-click removes every lane below as well",
    )
    .clicked()
    {
        remove_lane(session, state, bu.input(|i| i.modifiers), key, slot);
    }
}

fn remove_lane(
    session: &mut Session,
    state: &mut UiState,
    m: egui::Modifiers,
    key: LaneKey,
    slot: usize,
) {
    let chain = key.chain;
    let count = session
        .track_ui(chain)
        .map_or(0, |ui| ui.automation_lanes.len());
    let selected: Vec<usize> = state
        .arr
        .auto
        .headers
        .iter()
        .filter(|k| k.chain == chain)
        .filter_map(|k| k.slot)
        .collect();
    let slots = lanes::slots_to_remove(slot, count, &selected, m, Platform::current());
    session.edit_track_ui(chain, |ui| lanes::remove_lanes(ui, &slots));
    state.arr.auto.headers.clear();
    state.arr.auto.focus = Some(LaneKey { chain, slot: None });
}

/// The strip of a lane header that isn't a control: selects it and has the lane's menu.
fn header_zone(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    rect: Rect,
    key: LaneKey,
) {
    let id = ui.scope_id().with(("auto_header", key));
    let resp = ui.interact(rect, id, Sense::click());
    if resp.clicked() {
        select_header(state, key, ui.input(|i| i.modifiers));
    }
    let resp = info(
        resp,
        "Automation Lane — right-click for lane and clear commands",
    );
    resp.context_menu(|ui| popups::header_menu(ui, session, state, key));
}

fn select_header(state: &mut UiState, key: LaneKey, m: egui::Modifiers) {
    let auto = &mut state.arr.auto;
    auto.focus = Some(key);
    if key.slot.is_none() {
        return;
    }
    let same_track = |k: &LaneKey| k.chain == key.chain && k.slot.is_some();
    if m.shift {
        if let (Some(a), Some(b)) = (
            auto.headers
                .iter()
                .rev()
                .find(|k| same_track(k))
                .and_then(|k| k.slot),
            key.slot,
        ) {
            for slot in a.min(b)..=a.max(b) {
                let k = LaneKey {
                    chain: key.chain,
                    slot: Some(slot),
                };
                if !auto.headers.contains(&k) {
                    auto.headers.push(k);
                }
            }
            return;
        }
    }
    if m.command {
        match auto.headers.iter().position(|k| *k == key) {
            Some(i) => {
                auto.headers.remove(i);
            }
            None => auto.headers.push(key),
        }
    } else {
        auto.headers = vec![key];
    }
}

fn lane_buttons(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    headers: Rect,
    rect: Rect,
    chain: ChainRef,
    target: Option<EnvTarget>,
) {
    let area = Rect::from_min_size(
        Pos2::new(rect.right() - 54.0, rect.top() + 22.0),
        Vec2::new(44.0, 16.0),
    );
    let mut bu = session_view::child(
        ui,
        headers,
        area,
        Layout::right_to_left(egui::Align::Center),
    );
    bu.spacing_mut().item_spacing.x = 4.0;
    if target.is_some() {
        let r = widgets::state_button(&mut bu, Vec2::new(18.0, 16.0), false, pal().accent, "+");
        let r = info(
            r,
            "Add Automation Lane — Option-click adds a duplicate, Cmd-click adds one for each automated envelope",
        );
        if r.clicked() {
            add_lane(session, state, chain, bu.input(|i| i.modifiers));
        }
    }
    let (count, hidden) = session.track_ui(chain).map_or((0, false), |ui| {
        (ui.automation_lanes.len(), ui.automation_lanes_hidden)
    });
    if count > 0 {
        let r = widgets::state_button(
            &mut bu,
            Vec2::new(18.0, 16.0),
            !hidden,
            pal().accent,
            if hidden { ">" } else { "v" },
        );
        if info(r, "Show or hide this track's additional automation lanes").clicked() {
            session.edit_track_ui(chain, lanes::toggle_hidden);
        }
    }
}

pub(super) fn add_lane(
    session: &mut Session,
    state: &mut UiState,
    chain: ChainRef,
    m: egui::Modifiers,
) {
    let mode = AddLane::of(m, Platform::current());
    add_lane_with(session, state, chain, mode);
}

pub(super) fn add_lane_with(
    session: &mut Session,
    state: &mut UiState,
    chain: ChainRef,
    mode: AddLane,
) {
    let automated = session.automated_targets(chain);
    let mut added = None;
    session.edit_track_ui(chain, |ui| {
        added = lanes::add_lane(ui, chain, mode, &automated);
    });
    if let Some(slot) = added {
        state.arr.auto.focus = Some(LaneKey {
            chain,
            slot: Some(slot),
        });
    }
}

fn tempo_range(ui: &mut egui::Ui, session: &mut Session, headers: Rect, row: Rect) {
    let mut cu = session_view::child(ui, headers, row, Layout::left_to_right(egui::Align::Center));
    cu.spacing_mut().item_spacing.x = 3.0;
    let (mut min, mut max) = (
        session.project.arrangement.tempo.min,
        session.project.arrangement.tempo.max,
    );
    cu.label(RichText::new("Min").size(10.0).color(pal().text_dim));
    let r = cu.add(
        egui::DragValue::new(&mut min)
            .range(20.0..=998.0)
            .speed(1.0),
    );
    let mut changed = r.changed();
    info(
        r,
        "Tempo Minimum — the bottom of the tempo envelope, in BPM",
    );
    cu.label(RichText::new("Max").size(10.0).color(pal().text_dim));
    let r = cu.add(
        egui::DragValue::new(&mut max)
            .range(21.0..=999.0)
            .speed(1.0),
    );
    changed |= r.changed();
    info(r, "Tempo Maximum — the top of the tempo envelope, in BPM");
    if changed {
        session.set_tempo_range(min, max);
    }
}

/// Which device of the catalog a target belongs to, by group.
fn group_of(groups: &[EnvGroup], target: Option<EnvTarget>) -> Option<usize> {
    let target = target?;
    groups
        .iter()
        .position(|g| g.params.iter().any(|p| p.target == target))
}

/// The device a target belongs to, as a device id, for Clear All Envelopes of it.
pub(super) fn device_of(target: EnvTarget) -> Option<DeviceId> {
    match target {
        EnvTarget::Param(ParamRef {
            owner: ParamOwner::Device(d),
            ..
        }) => Some(d),
        _ => None,
    }
}

fn automated_label(text: &str, automated: bool) -> RichText {
    if automated {
        RichText::new(format!("• {text}")).color(pal().arm_red)
    } else {
        RichText::new(text)
    }
}

#[allow(clippy::too_many_arguments)]
fn choosers(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    headers: Rect,
    area: Rect,
    key: LaneKey,
    target: Option<EnvTarget>,
) {
    let groups = session.env_catalog(key.chain);
    let automated: Vec<EnvTarget> = session.automated_targets(key.chain);
    let is_automated = |t: EnvTarget| automated.contains(&t);
    let group_automated = |g: &EnvGroup| g.params.iter().any(|p| is_automated(p.target));
    let mut chosen: Option<Option<EnvTarget>> = None;
    let mut toggle_only = false;
    let row = |n: f32| {
        Rect::from_min_size(
            area.min + Vec2::new(0.0, n * (CHOOSER_H + 2.0)),
            Vec2::new(area.width(), CHOOSER_H),
        )
    };
    let mut cu = session_view::child(ui, headers, row(0.0), Layout::top_down(egui::Align::Min));
    let only = state.arr.auto.automated_only;
    if only {
        let label = match target.and_then(|t| group_of(&groups, Some(t)).map(|g| (t, g))) {
            Some((t, g)) => format!(
                "{}: {}",
                groups[g].label,
                groups[g]
                    .params
                    .iter()
                    .find(|p| p.target == t)
                    .map_or("", |p| p.info.name.as_str())
            ),
            None => "None".to_string(),
        };
        ComboBox::from_id_salt(("auto_combined", key))
            .width(area.width() - 6.0)
            .selected_text(RichText::new(label).size(10.5))
            .show_ui(&mut cu, |ui| {
                if ui.selectable_label(target.is_none(), "None").clicked() {
                    chosen = Some(None);
                }
                for g in &groups {
                    for p in g.params.iter().filter(|p| is_automated(p.target)) {
                        let text = format!("{}: {}", g.label, p.info.name);
                        if ui
                            .selectable_label(
                                target == Some(p.target),
                                automated_label(&text, true),
                            )
                            .clicked()
                        {
                            chosen = Some(Some(p.target));
                        }
                    }
                }
                ui.separator();
                if ui.button("Show All Parameters").clicked() {
                    toggle_only = true;
                }
            });
    } else {
        let current = group_of(&groups, target);
        let label = current.map_or("None", |g| groups[g].label.as_str());
        ComboBox::from_id_salt(("auto_device", key))
            .width(area.width() - 6.0)
            .selected_text(RichText::new(label).size(10.5))
            .show_ui(&mut cu, |ui| {
                if ui.selectable_label(current.is_none(), "None").clicked() {
                    chosen = Some(None);
                }
                for (n, g) in groups.iter().enumerate() {
                    let text = automated_label(&g.label, group_automated(g));
                    if ui.selectable_label(current == Some(n), text).clicked() {
                        let param = g
                            .params
                            .iter()
                            .find(|p| is_automated(p.target))
                            .or(g.params.first());
                        chosen = Some(param.map(|p| p.target));
                    }
                }
                ui.separator();
                if ui.button("Show Automated Parameters Only").clicked() {
                    toggle_only = true;
                }
            });
        let mut cu = session_view::child(ui, headers, row(1.0), Layout::top_down(egui::Align::Min));
        let params = current.map(|g| &groups[g].params);
        let param_label = target
            .and_then(|t| params?.iter().find(|p| p.target == t))
            .map_or("", |p| p.info.name.as_str());
        ComboBox::from_id_salt(("auto_control", key))
            .width(area.width() - 6.0)
            .selected_text(RichText::new(param_label).size(10.5))
            .show_ui(&mut cu, |ui| {
                for p in params.into_iter().flatten() {
                    let text = automated_label(&p.info.name, is_automated(p.target));
                    if ui
                        .selectable_label(target == Some(p.target), text)
                        .clicked()
                    {
                        chosen = Some(Some(p.target));
                    }
                }
                ui.separator();
                if ui.button("Show Automated Parameters Only").clicked() {
                    toggle_only = true;
                }
            });
    }
    if toggle_only {
        state.arr.auto.automated_only = !only;
    }
    if let Some(choice) = chosen {
        session.envelope_chooser_picked(choice);
        apply_choice(session, state, key, choice);
    }
}

fn apply_choice(
    session: &mut Session,
    state: &mut UiState,
    key: LaneKey,
    choice: Option<EnvTarget>,
) {
    let chain = key.chain;
    match key.slot {
        None => {
            let mut other = None;
            session.edit_track_ui(chain, |ui| other = lanes::choose_main(ui, choice));
            state.arr.auto.focus = Some(LaneKey { chain, slot: other });
        }
        Some(slot) => {
            if let Some(target) = choice {
                session.edit_track_ui(chain, |ui| {
                    if let Some(t) = ui.automation_lanes.get_mut(slot) {
                        *t = target;
                    }
                });
            }
            state.arr.auto.focus = Some(key);
        }
    }
}
