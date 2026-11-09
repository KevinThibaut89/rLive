//! The rack mixer in the Session View (manual pp. 508-510): chains as strips beside their track.
//! Its controls and the Chain List's read and write the same values, so each follows the other.

use super::devices::rack::chain_list::{chain_menu, push_chain_move, ChainMenu};
use super::devices::rack::{ChainDrag, RackAction, Rename, RenameTarget};
use super::devices::{Action, ActionQueue};
use super::theme::{self, pal, shade};
use super::widgets::{self, info, rgb};
use super::{Detail, UiState};
use crate::session::{ChainRef, Session};
use egui::{Align2, Layout, Pos2, Rect, Sense, Stroke, UiBuilder, Vec2};
use rlive_core::param::chain as controls;
use rlive_core::{ChainId, DeviceId, ParamId, ParamInfo, ParamOwner, ParamRef, TrackId};
use rlive_project::{Chain, Device, DeviceKind, RackKind};
use std::collections::HashSet;

const FOLD_W: f32 = 14.0;

/// A level of the rack mixer that can be unfolded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MixFold {
    Track(TrackId),
    Chain(ChainId),
}

/// A chain selected in the mixer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MixChain {
    pub at: ChainRef,
    pub rack: DeviceId,
    pub id: ChainId,
}

/// A column of the rack mixer.
#[derive(Debug, Clone, PartialEq)]
pub struct ChainColumn {
    /// The device chain holding the rack.
    pub at: ChainRef,
    pub rack: DeviceId,
    pub chain: ChainId,
    pub name: String,
    pub color: Option<[u8; 3]>,
    pub is_return: bool,
    /// How many racks deep it is.
    pub depth: usize,
    /// It holds a rack with a mixer of its own.
    pub foldable: bool,
    /// The chains in this rack, as listed, for reordering.
    pub siblings: Vec<ChainId>,
}

/// A rack that shows up in the mixer has more than one chain (manual p. 508).
fn rack_has_mixer(d: &Device) -> bool {
    match &d.kind {
        DeviceKind::Rack {
            kind: RackKind::Instrument | RackKind::Drum,
            chains,
            ..
        } => chains.len() > 1,
        _ => false,
    }
}

fn chain_has_mixer(c: &Chain) -> bool {
    c.devices.iter().any(rack_has_mixer)
}

/// Whether a track gets the button that folds its racks' mixer in or out.
pub fn track_has_mixer(devices: &[Device]) -> bool {
    devices.iter().any(rack_has_mixer)
}

/// The columns to show after a track, depth first, as unfolded.
pub fn chain_columns(
    track: TrackId,
    devices: &[Device],
    open: &HashSet<MixFold>,
) -> Vec<ChainColumn> {
    let mut out = Vec::new();
    if open.contains(&MixFold::Track(track)) {
        walk(ChainRef::Track(track), devices, open, 1, &mut out);
    }
    out
}

fn walk(
    at: ChainRef,
    devices: &[Device],
    open: &HashSet<MixFold>,
    depth: usize,
    out: &mut Vec<ChainColumn>,
) {
    for d in devices.iter().filter(|d| rack_has_mixer(d)) {
        let DeviceKind::Rack {
            chains, returns, ..
        } = &d.kind
        else {
            continue;
        };
        let siblings: Vec<ChainId> = chains.iter().map(|c| c.id).collect();
        let row = |c: &Chain, is_return: bool| ChainColumn {
            at,
            rack: d.id,
            chain: c.id,
            name: c.name.clone(),
            color: c.color,
            is_return,
            depth,
            foldable: chain_has_mixer(c),
            siblings: siblings.clone(),
        };
        for c in chains {
            out.push(row(c, false));
            if open.contains(&MixFold::Chain(c.id)) {
                if let Some(child) = at.child(d.id, c.id) {
                    walk(child, &c.devices, open, depth + 1, out);
                }
            }
        }
        for c in returns {
            out.push(row(c, true));
        }
    }
}

/// A move of one chain's control, spread across the mixer's selection.
pub fn spread_value(
    value: f64,
    source_before: f64,
    source_after: f64,
    lowest: f64,
    highest: f64,
) -> f64 {
    (value + source_after - source_before).clamp(lowest, highest)
}

/// Where a chain column draws.
#[derive(Debug, Clone, Copy)]
pub struct ColumnRects {
    pub header: Rect,
    pub grid: Rect,
    pub strip: Rect,
    pub clip: Rect,
    pub compact: bool,
}

/// Draws one column, and applies what the user did to it.
pub fn chain_column(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    col: &ChainColumn,
    r: ColumnRects,
) {
    let mut queue = ActionQueue::default();
    {
        let session: &Session = session;
        header(ui, session, state, col, r, &mut queue);
        let p = ui.painter().with_clip_rect(r.clip);
        p.rect_filled(r.grid.intersect(r.clip), 0.0, shade(0x202020));
        strip(ui, session, state, col, r, &mut queue);
    }
    queue.apply(session, &mut state.devices, &mut state.status);
}

fn selected(state: &UiState, col: &ChainColumn) -> bool {
    state
        .devices
        .mixer_selection
        .iter()
        .any(|c| c.id == col.chain)
}

fn header(
    ui: &mut egui::Ui,
    session: &Session,
    state: &mut UiState,
    col: &ChainColumn,
    r: ColumnRects,
    queue: &mut ActionQueue,
) {
    let rect = r.header;
    let mut cu = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(egui::Align::Center)),
    );
    cu.set_clip_rect(r.clip.intersect(ui.clip_rect()));
    let resp = cu.allocate_rect(rect, Sense::click_and_drag());
    let is_selected = selected(state, col);
    let base = col.color.map_or(shade(0x5a5a5a), rgb);
    let fill = if is_selected {
        base
    } else {
        theme::darken(base, 0.45)
    };
    cu.painter().rect_filled(rect, 0.0, fill);
    if is_selected {
        widgets::outline(cu.painter(), rect, shade(0xffffff));
    }
    // The ruler above the name shows how deep the chain sits.
    for d in 0..col.depth {
        let x = rect.left() + 2.0 + d as f32 * 3.0;
        cu.painter().line_segment(
            [
                Pos2::new(x, rect.top() + 2.0),
                Pos2::new(x, rect.bottom() - 2.0),
            ],
            Stroke::new(1.0, theme::text_on(fill)),
        );
    }
    let text_left =
        rect.left() + 5.0 + col.depth as f32 * 3.0 + if col.foldable { FOLD_W } else { 0.0 };
    let name_rect = Rect::from_min_max(
        Pos2::new(text_left, rect.top()),
        rect.max - Vec2::new(2.0, 0.0),
    );
    if col.foldable {
        let open = state
            .devices
            .mixer_open
            .contains(&MixFold::Chain(col.chain));
        super::icons::fold(
            cu.painter(),
            Rect::from_center_size(
                Pos2::new(text_left - FOLD_W / 2.0, rect.center().y),
                Vec2::splat(FOLD_W),
            ),
            open,
            theme::text_on(fill),
        );
    }
    let renaming = matches!(&state.devices.ui.rename, Some(Rename { target: RenameTarget::Chain(_, id), .. }) if *id == col.chain);
    if renaming {
        let mut done = None;
        if let Some(rename) = &mut state.devices.ui.rename {
            let edit = cu.put(
                name_rect,
                egui::TextEdit::singleline(&mut rename.text).font(theme::font(10.5)),
            );
            edit.request_focus();
            if edit.lost_focus() {
                done = Some(rename.text.clone());
            }
        }
        if let Some(text) = done {
            state.devices.ui.rename = None;
            queue.push(
                col.at,
                Action::Rack(col.rack, RackAction::RenameChain(col.chain, text)),
            );
        }
    } else {
        cu.painter()
            .with_clip_rect(name_rect.intersect(cu.clip_rect()))
            .text(
                Pos2::new(name_rect.left(), name_rect.center().y),
                Align2::LEFT_CENTER,
                &col.name,
                theme::font_bold(10.5),
                theme::text_on(fill),
            );
    }

    let on_fold = col.foldable
        && resp
            .interact_pointer_pos()
            .is_some_and(|pos| pos.x < text_left);
    if resp.clicked() {
        if on_fold {
            let key = MixFold::Chain(col.chain);
            if !state.devices.mixer_open.remove(&key) {
                state.devices.mixer_open.insert(key);
            }
        } else {
            click_chain(state, ui, col, queue);
        }
    }
    if resp.drag_started() {
        let ids = if is_selected {
            state
                .devices
                .mixer_selection
                .iter()
                .filter(|c| c.rack == col.rack)
                .map(|c| c.id)
                .collect()
        } else {
            vec![col.chain]
        };
        resp.dnd_set_drag_payload(ChainDrag {
            at: col.at,
            rack: col.rack,
            ids,
            from_mixer: true,
            is_return: col.is_return,
        });
    }
    if let Some(drag) = resp.dnd_release_payload::<ChainDrag>() {
        if drag.rack == col.rack && !col.is_return && !drag.is_return {
            if let Some(row) = col.siblings.iter().position(|c| *c == col.chain) {
                push_chain_move(queue, col.at, col.rack, &col.siblings, &drag, row);
            }
        }
    }
    let resp = info(
        resp,
        format!(
            "{} — click to show its devices; right-click for options",
            col.name
        ),
    );
    let selection: Vec<ChainId> = state
        .devices
        .mixer_selection
        .iter()
        .filter(|c| c.rack == col.rack)
        .map(|c| c.id)
        .collect();
    let chain_data = chain_of(session, col).cloned();
    if let Some(chain) = chain_data {
        let at = col.at;
        let rack = col.rack;
        let id = col.chain;
        let is_return = col.is_return;
        resp.context_menu(|ui| {
            chain_menu(
                ui,
                queue,
                &mut state.devices.ui.rename,
                ChainMenu {
                    at,
                    rack,
                    id,
                    name: &chain.name,
                    color: chain.color,
                    selection: &selection,
                    is_return,
                },
            );
        });
    }
}

fn click_chain(state: &mut UiState, ui: &egui::Ui, col: &ChainColumn, queue: &mut ActionQueue) {
    let m = ui.input(|i| i.modifiers);
    let me = MixChain {
        at: col.at,
        rack: col.rack,
        id: col.chain,
    };
    if m.shift || m.command {
        match state.devices.mixer_selection.iter().position(|c| *c == me) {
            Some(i) => {
                state.devices.mixer_selection.remove(i);
            }
            None => state.devices.mixer_selection.push(me),
        }
    } else {
        state.devices.mixer_selection = vec![me];
    }
    if let Some(child) = col.at.child(col.rack, col.chain) {
        state.selected = Some(child);
        state.detail = Detail::Devices;
    }
    queue.push(
        col.at,
        Action::Rack(col.rack, RackAction::Select(vec![col.chain])),
    );
}

fn chain_of<'a>(session: &'a Session, col: &ChainColumn) -> Option<&'a Chain> {
    session
        .devices(col.at)
        .iter()
        .find(|d| d.id == col.rack)?
        .chains()
        .find(|c| c.id == col.chain)
}

fn strip(
    ui: &mut egui::Ui,
    session: &Session,
    state: &mut UiState,
    col: &ChainColumn,
    r: ColumnRects,
    queue: &mut ActionQueue,
) {
    let Some(chain) = chain_of(session, col) else {
        return;
    };
    let rect = r.strip;
    let p = ui.painter().with_clip_rect(r.clip);
    let is_selected = selected(state, col);
    p.rect_filled(
        rect,
        0.0,
        if is_selected {
            shade(0x363636)
        } else {
            pal().panel
        },
    );
    p.line_segment(
        [rect.right_top(), rect.right_bottom()],
        Stroke::new(1.0, pal().separator),
    );
    p.rect_filled(
        Rect::from_min_size(rect.min, Vec2::new(rect.width(), 2.0)),
        0.0,
        chain.color.map_or(shade(0x5a5a5a), rgb),
    );
    let bg = ui.interact(
        rect,
        ui.scope_id().with(("strip_bg", col.chain)),
        Sense::click(),
    );
    if bg.clicked() {
        click_chain(state, ui, col, queue);
    }
    let inner = rect.shrink2(Vec2::new(5.0, 6.0));
    let infos = controls::infos();
    let mut y = inner.top() + 2.0;
    if !r.compact {
        if col.is_return {
            y = audio_to(ui, session, col, chain, inner, y, queue);
        }
        if let Some(param) = control(&infos, controls::PAN) {
            let knob_r = Rect::from_center_size(
                Pos2::new(inner.center().x, y + 13.0),
                Vec2::new(28.0, 26.0),
            );
            let value = session
                .chain_param(col.at, col.rack, col.chain, controls::PAN)
                .unwrap_or(param.default) as f32;
            let mut ku = ui.new_child(UiBuilder::new().max_rect(knob_r));
            if locked(session, col, controls::PAN) {
                ku.disable();
            }
            map_chain_control(&ku, knob_r, col.chain, controls::PAN);
            if let Some(v) = widgets::pan_knob(&mut ku, value) {
                push_param(
                    session,
                    state,
                    col,
                    controls::PAN,
                    value as f64,
                    v as f64,
                    param,
                    queue,
                );
            }
            y += 30.0;
        }
        y = send_sliders(ui, session, state, col, inner, y, queue);
    }

    // Fader and meter take the room left above the buttons and dB box.
    let buttons_h = 17.0;
    let fader_bottom = inner.bottom() - buttons_h - 18.0;
    let fader_w = (inner.width() * 0.5).clamp(26.0, 44.0);
    let fader_r = Rect::from_min_max(
        Pos2::new(inner.center().x - fader_w / 2.0, y),
        Pos2::new(inner.center().x + fader_w / 2.0, fader_bottom.max(y + 20.0)),
    );
    let vol = session
        .chain_param(col.at, col.rack, col.chain, controls::VOLUME)
        .unwrap_or(0.0) as f32;
    let dt = ui.input(|i| i.stable_dt).min(0.1);
    let meter = {
        let peaks = session
            .chain_shared(col.chain)
            .map_or((0.0, 0.0), |s| s.meter.take());
        let m = state.devices.chain_meters.entry(col.chain).or_default();
        m.update(peaks, dt);
        m.get()
    };
    let mut fu = ui.new_child(UiBuilder::new().max_rect(fader_r));
    if locked(session, col, controls::VOLUME) {
        fu.disable();
    }
    map_chain_control(&fu, fader_r, col.chain, controls::VOLUME);
    if let Some(db) = widgets::fader(&mut fu, fader_r.size(), vol, meter, pal().accent) {
        if let Some(param) = control(&infos, controls::VOLUME) {
            push_param(
                session,
                state,
                col,
                controls::VOLUME,
                vol as f64,
                db as f64,
                param,
                queue,
            );
        }
    }
    let db_r = Rect::from_center_size(
        Pos2::new(inner.center().x, fader_r.bottom() + 9.0),
        Vec2::new(inner.width().min(54.0), 14.0),
    );
    p.rect_filled(db_r, 1.0, pal().field_bg);
    p.text(
        db_r.center(),
        Align2::CENTER_CENTER,
        if vol <= -70.0 {
            "-inf".to_string()
        } else {
            format!("{vol:.1}")
        },
        theme::font(10.0),
        pal().text,
    );

    let bw = ((inner.width() - 2.0) / 2.0).floor();
    let row = Rect::from_min_size(
        Pos2::new(inner.left(), inner.bottom() - buttons_h),
        Vec2::new(inner.width(), buttons_h),
    );
    let mut bu = ui.new_child(
        UiBuilder::new()
            .max_rect(row)
            .layout(Layout::left_to_right(egui::Align::Center)),
    );
    bu.spacing_mut().item_spacing.x = 2.0;
    if locked(session, col, controls::ACTIVATOR) {
        bu.disable();
    }
    let active = chain.mix.active;
    let r_on = widgets::state_button(
        &mut bu,
        Vec2::new(bw, buttons_h),
        active,
        pal().activator,
        "",
    );
    map_chain_control(&bu, r_on.rect, col.chain, controls::ACTIVATOR);
    if info(r_on, "Chain Activator — click to mute or unmute").clicked() {
        push_toggle(session, state, col, controls::ACTIVATOR, !active, queue);
    }
    let solo = chain.mix.solo;
    let r_solo = widgets::state_button(
        &mut bu,
        Vec2::new(bw, buttons_h),
        solo,
        pal().solo_blue,
        "S",
    );
    map_chain_control(&bu, r_solo.rect, col.chain, controls::SOLO);
    if info(r_solo, "Chain Solo").clicked() {
        push_toggle(session, state, col, controls::SOLO, !solo, queue);
    }
}

/// Marks a chain control as mappable.
fn map_chain_control(ui: &egui::Ui, rect: Rect, chain: rlive_core::ChainId, id: ParamId) {
    super::remote_map::overlay(
        ui,
        rect,
        rlive_project::MapTarget::Param(ParamRef {
            owner: ParamOwner::Chain(chain),
            id,
        }),
    );
}

fn control(infos: &[ParamInfo], id: ParamId) -> Option<&ParamInfo> {
    infos.iter().find(|p| p.param_id() == id)
}

fn locked(session: &Session, col: &ChainColumn, control: ParamId) -> bool {
    session.is_macro_mapped(
        col.at,
        ParamRef {
            owner: ParamOwner::Chain(col.chain),
            id: control,
        },
    )
}

/// The chains a mixer move applies to: this one, and the rest of the selection if it's in it
/// (manual p. 509).
fn mix_targets(state: &UiState, col: &ChainColumn) -> Vec<MixChain> {
    let me = MixChain {
        at: col.at,
        rack: col.rack,
        id: col.chain,
    };
    if selected(state, col) {
        state.devices.mixer_selection.clone()
    } else {
        vec![me]
    }
}

#[allow(clippy::too_many_arguments)]
fn push_param(
    session: &Session,
    state: &UiState,
    col: &ChainColumn,
    id: ParamId,
    before: f64,
    after: f64,
    param: &ParamInfo,
    queue: &mut ActionQueue,
) {
    for t in mix_targets(state, col) {
        let value = if t.id == col.chain {
            after
        } else {
            let current = session
                .chain_param(t.at, t.rack, t.id, id)
                .unwrap_or(before);
            spread_value(current, before, after, param.min, param.max)
        };
        queue.push(
            t.at,
            Action::Rack(t.rack, RackAction::ChainParam(t.id, id, value)),
        );
    }
}

fn push_toggle(
    session: &Session,
    state: &UiState,
    col: &ChainColumn,
    id: ParamId,
    on: bool,
    queue: &mut ActionQueue,
) {
    for t in mix_targets(state, col) {
        let target = ParamRef {
            owner: ParamOwner::Chain(t.id),
            id,
        };
        if !session.is_macro_mapped(t.at, target) {
            queue.push(
                t.at,
                Action::Rack(t.rack, RackAction::ChainParam(t.id, id, on as u8 as f64)),
            );
        }
    }
}

/// A return chain's Audio To chooser. Returns the height to carry on from.
fn audio_to(
    ui: &mut egui::Ui,
    session: &Session,
    col: &ChainColumn,
    chain: &Chain,
    inner: Rect,
    y: f32,
    queue: &mut ActionQueue,
) -> f32 {
    let area = Rect::from_min_size(Pos2::new(inner.left(), y), Vec2::new(inner.width(), 16.0));
    let mut ou = ui.new_child(UiBuilder::new().max_rect(area));
    let label = chain
        .mix
        .output
        .and_then(|t| session.project.track(t))
        .map_or("Rack".to_string(), |t| t.name.clone());
    egui::ComboBox::from_id_salt(("mix_audio_to", col.chain))
        .selected_text(label)
        .width(area.width())
        .show_ui(&mut ou, |ui| {
            if ui
                .selectable_label(chain.mix.output.is_none(), "Rack")
                .clicked()
            {
                queue.push(
                    col.at,
                    Action::Rack(col.rack, RackAction::ReturnOutput(col.chain, None)),
                );
            }
            for t in &session.project.returns {
                if ui
                    .selectable_label(chain.mix.output == Some(t.id), &t.name)
                    .clicked()
                {
                    queue.push(
                        col.at,
                        Action::Rack(col.rack, RackAction::ReturnOutput(col.chain, Some(t.id))),
                    );
                }
            }
        });
    y + 19.0
}

/// One compact slider per return chain. Returns the height to carry on from.
#[allow(clippy::too_many_arguments)]
fn send_sliders(
    ui: &mut egui::Ui,
    session: &Session,
    state: &UiState,
    col: &ChainColumn,
    inner: Rect,
    y: f32,
    queue: &mut ActionQueue,
) -> f32 {
    if col.is_return {
        return y;
    }
    let returns = session
        .devices(col.at)
        .iter()
        .find(|d| d.id == col.rack)
        .map_or(0, |d| match &d.kind {
            DeviceKind::Rack { returns, .. } => returns.len(),
            _ => 0,
        });
    let infos = controls::infos();
    let mut y = y;
    for n in 0..returns {
        let id = ParamId(controls::FIRST_SEND.0 + n as u32);
        let Some(param) = control(&infos, id) else {
            continue;
        };
        let rect = Rect::from_min_size(Pos2::new(inner.left(), y), Vec2::new(inner.width(), 11.0));
        let value = session
            .chain_param(col.at, col.rack, col.chain, id)
            .unwrap_or(param.default);
        let is_locked = locked(session, col, id);
        let sense = if is_locked {
            Sense::hover()
        } else {
            Sense::click_and_drag()
        };
        let resp = ui.interact(rect, ui.scope_id().with(("mix_send", col.chain, n)), sense);
        let p = ui.painter();
        p.rect_filled(rect, 1.0, shade(0x1f1f1f));
        let norm = param.to_normalized(value);
        p.rect_filled(
            Rect::from_min_size(
                rect.min,
                Vec2::new(rect.width() * norm as f32, rect.height()),
            ),
            1.0,
            pal().accent.gamma_multiply(0.75),
        );
        p.text(
            rect.center(),
            Align2::CENTER_CENTER,
            ((b'A' + n as u8) as char).to_string(),
            theme::font(8.5),
            pal().text,
        );
        if resp.dragged() {
            let next = param.from_normalized(
                (norm + resp.drag_delta().x as f64 / rect.width() as f64).clamp(0.0, 1.0),
            );
            push_param(session, state, col, id, value, next, param, queue);
        }
        info(
            resp,
            format!("Send {} — drag to set its level", (b'A' + n as u8) as char),
        );
        y += 13.0;
    }
    y
}

/// Handles chains dropped on the mixer's new-track zone: extracted, notes along if they came from
/// the mixer. Returns whether a chain drag hovers it.
pub fn extract_zone(
    ui: &egui::Ui,
    resp: &egui::Response,
    session: &mut Session,
    state: &mut UiState,
    rect: Rect,
) -> bool {
    let hovering = resp.dnd_hover_payload::<ChainDrag>().is_some();
    if hovering {
        ui.painter().rect_stroke(
            rect.shrink(2.0),
            2.0,
            Stroke::new(1.0, pal().accent),
            egui::StrokeKind::Inside,
        );
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            "Drop to extract",
            theme::font(11.0),
            pal().accent,
        );
    }
    if let Some(drag) = resp.dnd_release_payload::<ChainDrag>() {
        let mut queue = ActionQueue::default();
        let (at, rack) = (drag.at, drag.rack);
        queue.push(at, Action::Rack(rack, RackAction::Extract((*drag).clone())));
        queue.apply(session, &mut state.devices, &mut state.status);
    }
    hovering
}

#[cfg(test)]
mod tests {
    use super::*;
    use rlive_project::DrumPad;

    fn drum(names: &[&str], returns: &[&str]) -> Device {
        let mut d = Device::rack(RackKind::Drum, "Drums");
        if let DeviceKind::Rack {
            chains,
            returns: rets,
            ..
        } = &mut d.kind
        {
            for (i, name) in names.iter().enumerate() {
                let mut c = Chain::new(name);
                c.zones.drum = Some(DrumPad {
                    receive: Some(36 + i as u8),
                    play: 60,
                    choke: None,
                });
                chains.push(c);
            }
            for name in returns {
                rets.push(Chain::new(name));
            }
        }
        d
    }

    #[test]
    fn only_instrument_and_drum_racks_with_several_chains_have_a_mixer() {
        assert!(track_has_mixer(&[drum(&["A", "B"], &[])]));
        assert!(!track_has_mixer(&[drum(&["A"], &[])]));
        let mut audio = Device::rack(RackKind::Audio, "Fx");
        if let DeviceKind::Rack { chains, .. } = &mut audio.kind {
            chains.push(Chain::new("A"));
            chains.push(Chain::new("B"));
        }
        assert!(!track_has_mixer(&[audio]));
    }

    #[test]
    fn a_folded_track_has_no_chain_columns() {
        let t = TrackId(1);
        let d = drum(&["A", "B"], &[]);
        assert!(chain_columns(t, std::slice::from_ref(&d), &HashSet::new()).is_empty());
    }

    #[test]
    fn an_unfolded_track_lists_chains_then_return_chains() {
        let t = TrackId(1);
        let d = drum(&["A", "B"], &["R"]);
        let open = HashSet::from([MixFold::Track(t)]);
        let names: Vec<String> = chain_columns(t, &[d], &open)
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, ["A", "B", "R"]);
    }

    #[test]
    fn a_return_chain_is_marked_as_one() {
        let t = TrackId(1);
        let d = drum(&["A", "B"], &["R"]);
        let open = HashSet::from([MixFold::Track(t)]);
        let cols = chain_columns(t, &[d], &open);
        assert!(!cols[0].is_return);
        assert!(cols[2].is_return);
    }

    #[test]
    fn nested_racks_get_their_own_fold_button_and_appear_when_open() {
        let t = TrackId(1);
        let inner = drum(&["In1", "In2"], &[]);
        let mut d = drum(&["A", "B"], &[]);
        if let DeviceKind::Rack { chains, .. } = &mut d.kind {
            chains[0].devices.push(inner);
        }
        let a = d.chains().next().unwrap().id;
        let mut open = HashSet::from([MixFold::Track(t)]);
        let cols = chain_columns(t, std::slice::from_ref(&d), &open);
        assert_eq!(cols.len(), 2);
        assert!(cols[0].foldable);
        assert!(!cols[1].foldable);
        assert_eq!(cols[0].depth, 1);

        open.insert(MixFold::Chain(a));
        let names: Vec<String> = chain_columns(t, &[d], &open)
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, ["A", "In1", "In2", "B"]);
    }

    #[test]
    fn a_selection_move_keeps_relative_levels_and_stays_in_range() {
        assert_eq!(spread_value(-6.0, 0.0, -3.0, -70.0, 6.0), -9.0);
        assert_eq!(spread_value(5.0, 0.0, 3.0, -70.0, 6.0), 6.0);
        assert_eq!(spread_value(-69.0, 0.0, -3.0, -70.0, 6.0), -70.0);
    }
}
