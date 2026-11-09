//! The mixer's In/Out section, the Crossfade Assign buttons, the crossfader and the Main track's
//! Cue controls (manual pp. 372-375, 403-407).

use super::theme::{self, *};
use super::widgets::{self, info};
use super::UiState;
use crate::session::{RouteChoice, Session};
use egui::{Align2, ComboBox, Id, Pos2, Rect, Sense, Stroke, UiBuilder, Vec2};
use rlive_core::{CrossfadeCurve, TrackId};
use rlive_project::{Crossfade, Monitor, Route, Tap, TrackKind};

/// How tall the section is in a mixer strip, and how wide beside an Arrangement lane.
pub(super) const IN_OUT_H: f32 = 132.0;
pub(super) const ARR_IN_OUT_W: f32 = 236.0;
const ROW_H: f32 = 16.0;
/// How tall the Crossfade Assign row is, under the mixer strip's buttons.
pub(super) const ASSIGN_H: f32 = 18.0;

/// What one edit applies to: the selection, if the track touched is in it.
fn targets(state: &UiState, id: TrackId) -> Vec<TrackId> {
    if state.selected_tracks.contains(&id) {
        state.selected_tracks.clone()
    } else {
        vec![id]
    }
}

/// A compact chooser. `options` is only built while the menu is open.
fn chooser(
    ui: &mut egui::Ui,
    rect: Rect,
    salt: impl std::hash::Hash + std::fmt::Debug,
    current: &str,
    hint: &str,
    options: impl FnOnce() -> Vec<RouteChoice>,
) -> Option<RouteChoice> {
    let mut picked = None;
    let mut child = ui.new_child(UiBuilder::new().max_rect(rect));
    child.spacing_mut().interact_size.y = rect.height();
    child.spacing_mut().button_padding = Vec2::new(3.0, 0.0);
    let mut combo = ComboBox::from_id_salt(salt)
        .width(rect.width())
        .height(240.0)
        .selected_text(egui::RichText::new(current).size(10.0).color(pal().text));
    combo = combo.truncate();
    let response = combo
        .show_ui(&mut child, |ui| {
            ui.set_min_width(rect.width().max(120.0));
            for option in options() {
                if ui.selectable_label(false, &option.label).clicked() {
                    picked = Some(option);
                }
            }
        })
        .response;
    info(response, hint);
    picked
}

fn input_type_label(session: &Session, id: TrackId) -> String {
    let Some(t) = session.project.track(id) else {
        return String::new();
    };
    match &t.routing.input {
        Route::Default if t.kind == TrackKind::Midi => "All Ins".into(),
        Route::Default | Route::None | Route::SendsOnly => "No Input".into(),
        Route::External(_) => "Ext. In".into(),
        Route::Resampling => "Resampling".into(),
        Route::Track { track, .. } | Route::TrackChannel { track, .. } => session
            .project
            .track(*track)
            .map_or_else(|| "(gone)".to_string(), |t| t.name.clone()),
    }
}

fn output_type_label(session: &Session, id: TrackId) -> String {
    let Some(t) = session.project.track(id) else {
        return String::new();
    };
    match &t.routing.output {
        Route::Default | Route::Resampling => "Main".into(),
        Route::None => "No Output".into(),
        Route::SendsOnly => "Sends Only".into(),
        Route::External(_) => "Ext. Out".into(),
        Route::Track { track, .. } | Route::TrackChannel { track, .. } => session
            .project
            .track(*track)
            .map_or_else(|| "(gone)".to_string(), |t| t.name.clone()),
    }
}

fn channel_label(list: &[RouteChoice], route: &Route, tap: Option<Tap>) -> String {
    list.iter()
        .find(|c| c.route == *route && tap.is_none_or(|t| t == c.tap))
        .map_or_else(String::new, |c| c.label.clone())
}

fn apply_input(session: &mut Session, state: &mut UiState, ids: &[TrackId], c: RouteChoice) {
    let shown = c.label.clone();
    if !session.set_input(ids, c.route, c.tap) {
        state.status = format!("{shown} would make a feedback loop");
    }
}

fn apply_output(session: &mut Session, state: &mut UiState, ids: &[TrackId], c: RouteChoice) {
    let shown = c.label.clone();
    if !session.set_output(ids, c.route) {
        state.status = format!("{shown} would make a feedback loop");
    }
}

fn audio_of(session: &Session, id: TrackId) -> bool {
    session
        .project
        .track(id)
        .is_some_and(|t| t.kind != TrackKind::Midi || !session.is_midi_only(id))
}

/// The upper or lower chooser pair of the section.
fn input_pair(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    id: TrackId,
    rects: [Rect; 2],
) {
    let hw = state.hardware.clone();
    let current = input_type_label(session, id);
    let picked = chooser(
        ui,
        rects[0],
        ("in_type", id),
        &current,
        "Input Type — where this track's signal comes from",
        || session.input_types(id, &hw),
    );
    if let Some(c) = picked {
        apply_input(session, state, &targets(state, id), c);
    }
    let Some(t) = session.project.track(id) else {
        return;
    };
    let (route, tap) = (t.routing.input.clone(), t.routing.tap);
    let list = session.input_channels(id, &hw);
    if list.is_empty() {
        return;
    }
    let shown = channel_label(&list, &route, Some(tap));
    let picked = chooser(
        ui,
        rects[1],
        ("in_channel", id),
        &shown,
        "Input Channel — which channel, or which point in the other track",
        || list,
    );
    if let Some(c) = picked {
        apply_input(session, state, &targets(state, id), c);
    }
}

fn output_pair(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    id: TrackId,
    rects: [Rect; 2],
) {
    let hw = state.hardware.clone();
    let current = output_type_label(session, id);
    let picked = chooser(
        ui,
        rects[0],
        ("out_type", id),
        &current,
        "Output Type — where this track goes",
        || session.output_types(id, &hw),
    );
    if let Some(c) = picked {
        apply_output(session, state, &targets(state, id), c);
    }
    let Some(t) = session.project.track(id) else {
        return;
    };
    let route = t.routing.output.clone();
    let list = session.output_channels(id, &hw);
    if list.is_empty() {
        return;
    }
    let shown = channel_label(&list, &route, None);
    let picked = chooser(
        ui,
        rects[1],
        ("out_channel", id),
        &shown,
        "Output Channel — the input, device or channel addressed there",
        || list,
    );
    if let Some(c) = picked {
        apply_output(session, state, &targets(state, id), c);
    }
}

/// The Monitor radio buttons. Delete puts them back to the default for the track's kind.
fn monitor_row(ui: &mut egui::Ui, session: &mut Session, state: &UiState, id: TrackId, rect: Rect) {
    let Some(t) = session.project.track(id) else {
        return;
    };
    let monitor = t.routing.monitor;
    let w = (rect.width() / 3.0).floor();
    let mut hovered = false;
    let options = [
        (Monitor::In, "In"),
        (Monitor::Auto, "Auto"),
        (Monitor::Off, "Off"),
    ];
    for (i, (mode, label)) in options.into_iter().enumerate() {
        let r = Rect::from_min_size(
            rect.min + Vec2::new(i as f32 * w, 0.0),
            Vec2::new(w - 1.0, rect.height()),
        );
        let mut child = ui.new_child(UiBuilder::new().max_rect(r));
        let resp =
            widgets::state_button(&mut child, r.size(), monitor == mode, pal().accent, label);
        hovered |= resp.hovered();
        let resp = info(resp, "Monitor — when this track's input is heard");
        if resp.clicked() {
            session.set_monitor(&targets(state, id), mode);
        }
        if mode != Monitor::Off {
            resp.context_menu(|ui| {
                let keep = session
                    .project
                    .track(id)
                    .is_some_and(|t| t.keep_monitor_latency);
                if ui
                    .selectable_label(keep, "Keep Monitoring Latency in Recorded Audio")
                    .clicked()
                {
                    session.set_keep_monitor_latency(&targets(state, id), !keep);
                    ui.close();
                }
            });
        }
    }
    let delete =
        ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace));
    if hovered && delete {
        session.reset_monitor(&targets(state, id));
    }
}

/// The In/Out section of a mixer strip, from `y`. Returns the height to carry on from.
pub(super) fn strip_section(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    id: TrackId,
    inner: Rect,
    y: f32,
) -> f32 {
    let Some(t) = session.project.track(id) else {
        return y;
    };
    let kind = t.kind;
    let has_input = matches!(kind, TrackKind::Audio | TrackKind::Midi);
    let audio = audio_of(session, id);
    let row = |n: usize| {
        Rect::from_min_size(
            Pos2::new(inner.left(), y + n as f32 * (ROW_H + 2.0)),
            Vec2::new(inner.width(), ROW_H),
        )
    };
    let p = ui.painter().clone();
    let small = |p: &egui::Painter, r: Rect, t: &str| {
        p.text(
            r.left_center(),
            Align2::LEFT_CENTER,
            t,
            theme::font(9.0),
            pal().text_faint,
        );
    };
    let mut n = 0;
    if has_input {
        small(
            &p,
            row(0),
            if kind == TrackKind::Midi {
                "MIDI From"
            } else {
                "Audio From"
            },
        );
        input_pair(ui, session, state, id, [row(1), row(2)]);
        monitor_row(ui, session, state, id, row(3));
        n = 4;
    }
    small(&p, row(n), if audio { "Audio To" } else { "MIDI To" });
    output_pair(ui, session, state, id, [row(n + 1), row(n + 2)]);
    y + IN_OUT_H
}

/// The In/Out controls beside an Arrangement lane.
pub(super) fn header_section(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    id: TrackId,
    area: Rect,
) {
    let Some(t) = session.project.track(id) else {
        return;
    };
    let kind = t.kind;
    let has_input = matches!(kind, TrackKind::Audio | TrackKind::Midi);
    let reserved = if has_input { 80.0 } else { 0.0 };
    let w = ((area.width() - reserved) / 2.0 - 2.0).floor();
    let cell = |column: usize, row: usize| {
        Rect::from_min_size(
            Pos2::new(
                area.left() + column as f32 * (w + 2.0),
                area.top() + 2.0 + row as f32 * (ROW_H + 2.0),
            ),
            Vec2::new(w, ROW_H),
        )
    };
    let right = Rect::from_min_max(
        Pos2::new(area.right() - 76.0, area.top() + 2.0),
        Pos2::new(area.right(), area.top() + 2.0 + ROW_H),
    );
    let two_rows = area.height() >= 2.0 * ROW_H + 4.0;
    if has_input {
        input_pair(ui, session, state, id, [cell(0, 0), cell(1, 0)]);
        monitor_row(ui, session, state, id, right);
        if two_rows {
            output_pair(ui, session, state, id, [cell(0, 1), cell(1, 1)]);
        }
    } else {
        output_pair(ui, session, state, id, [cell(0, 0), cell(1, 0)]);
    }
}

/// The two Crossfade Assign buttons.
pub(super) fn assign_row(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &UiState,
    id: TrackId,
    rect: Rect,
) {
    let Some(t) = session.project.track(id) else {
        return;
    };
    let side = t.crossfade;
    let w = (rect.width() / 2.0).floor();
    for (i, (button, label)) in [(Crossfade::A, "A"), (Crossfade::B, "B")]
        .into_iter()
        .enumerate()
    {
        let r = Rect::from_min_size(
            rect.min + Vec2::new(i as f32 * w, 0.0),
            Vec2::new(w - 1.0, rect.height()),
        );
        let mut child = ui.new_child(UiBuilder::new().max_rect(r));
        let resp = widgets::state_button(&mut child, r.size(), side == button, pal().accent, label);
        super::remote_map::overlay_track(&child, r, id, rlive_core::param::mixer::CROSSFADE_ASSIGN);
        if info(
            resp,
            "Crossfade Assign — which side of the crossfader this track is on",
        )
        .clicked()
        {
            let next = if side == button {
                Crossfade::None
            } else {
                button
            };
            session.set_crossfade_assign(&targets(state, id), next);
        }
    }
}

/// The crossfader itself. Right-click picks its curve.
pub(super) fn crossfader(ui: &mut egui::Ui, session: &mut Session, rect: Rect) {
    let id = Id::unique("rlive_crossfader");
    let resp = ui.interact(rect, id, Sense::click_and_drag());
    super::remote_map::overlay_crossfader(ui, rect);
    let p = ui.painter();
    p.rect_filled(rect, 1.0, pal().field_bg);
    p.line_segment(
        [rect.left_center(), rect.right_center()],
        Stroke::new(1.0, pal().separator),
    );
    let position = session.project.crossfader.position;
    let x = rect.left() + (position + 1.0) * 0.5 * rect.width();
    let handle =
        Rect::from_center_size(Pos2::new(x, rect.center().y), Vec2::new(6.0, rect.height()));
    p.rect_filled(handle, 1.0, pal().accent);
    p.line_segment(
        [
            Pos2::new(rect.center().x, rect.top()),
            Pos2::new(rect.center().x, rect.bottom()),
        ],
        Stroke::new(1.0, shade(0x8a8a8a)),
    );
    if resp.double_clicked() {
        session.set_crossfader(0.0);
    } else if resp.dragged() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let at = ((pos.x - rect.left()) / rect.width() * 2.0 - 1.0).clamp(-1.0, 1.0);
            session.set_crossfader(at);
        }
    }
    let current = CrossfadeCurve::from_index(session.project.crossfader.curve);
    let resp = info(
        resp,
        "Crossfader — fades A tracks into B tracks; right-click for the curve",
    );
    resp.context_menu(|ui| {
        for curve in CrossfadeCurve::MENU {
            if ui
                .selectable_label(curve == current, curve.name())
                .clicked()
            {
                session.set_crossfade_curve(curve);
                ui.close();
            }
        }
    });
}

/// The Main track's Main Out and Cue Out choosers, Solo/Cue switch and Cue Volume.
pub(super) fn main_section(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    inner: Rect,
    y: f32,
) -> f32 {
    let row = |n: usize| {
        Rect::from_min_size(
            Pos2::new(inner.left(), y + n as f32 * (ROW_H + 2.0)),
            Vec2::new(inner.width(), ROW_H),
        )
    };
    let hw = state.hardware.clone();
    let main_label = match &session.project.master.routing.output {
        Route::External(ext) => hw
            .outputs
            .iter()
            .find(|(_, r)| r == ext)
            .map_or_else(String::new, |(n, _)| n.clone()),
        _ => hw
            .outputs
            .first()
            .map_or_else(String::new, |(n, _)| n.clone()),
    };
    let main_out = match &session.project.master.routing.output {
        Route::External(ext) => Some(ext.clone()),
        _ => hw.outputs.first().map(|(_, r)| r.clone()),
    };
    let mut p_main = None;
    let picked = chooser(
        ui,
        row(0),
        "main_out",
        &format!("Main  {main_label}"),
        "Main Out — the output used for the mix",
        || {
            hw.outputs
                .iter()
                .map(|(name, r)| RouteChoice {
                    label: name.clone(),
                    route: Route::External(r.clone()),
                    tap: Tap::default(),
                })
                .collect()
        },
    );
    if let Some(c) = picked {
        p_main = Some(c);
    }
    if let Some(RouteChoice {
        route: Route::External(out),
        ..
    }) = p_main
    {
        session.set_main_out(out);
    }
    let cue_label = session.project.cue.out.as_ref().map_or_else(
        || "None".to_string(),
        |ext| {
            hw.outputs
                .iter()
                .find(|(_, r)| r == ext)
                .map_or_else(String::new, |(n, _)| n.clone())
        },
    );
    let picked = chooser(
        ui,
        row(1),
        "cue_out",
        &format!("Cue  {cue_label}"),
        "Cue Out — the output used for cueing, needing two outputs of its own",
        || {
            hw.outputs
                .iter()
                .filter(|(_, r)| Some(r) != main_out.as_ref())
                .map(|(name, r)| RouteChoice {
                    label: name.clone(),
                    route: Route::External(r.clone()),
                    tap: Tap::default(),
                })
                .collect()
        },
    );
    if let Some(RouteChoice {
        route: Route::External(out),
        ..
    }) = picked
    {
        session.set_cue_out(out);
    }
    // Cueing needs an output apart from Main.
    let can_cue = hw.outputs.len() >= 2;
    let cue_mode = session.project.cue.cue_mode;
    let toggle = Rect::from_min_size(row(2).min, Vec2::new(inner.width() * 0.5 - 1.0, ROW_H));
    let mut child = ui.new_child(UiBuilder::new().max_rect(toggle));
    if !can_cue {
        child.disable();
    }
    let resp = widgets::state_button(
        &mut child,
        toggle.size(),
        cue_mode,
        pal().solo_blue,
        if cue_mode { "Cue" } else { "Solo" },
    );
    if info(
        resp,
        "Solo/Cue — Solo switches become Cue switches feeding the Cue output",
    )
    .clicked()
    {
        session.set_cue_mode(!cue_mode);
    }
    let knob = Rect::from_min_size(
        Pos2::new(inner.left() + inner.width() * 0.5 + 1.0, row(2).top()),
        Vec2::new(inner.width() * 0.5 - 1.0, ROW_H),
    );
    let db = session.project.cue.volume_db;
    let mut value = db;
    let mut k = ui.new_child(UiBuilder::new().max_rect(knob));
    let resp = k.add(
        egui::DragValue::new(&mut value)
            .speed(0.25)
            .range(-96.0..=6.0)
            .max_decimals(1)
            .suffix(" dB"),
    );
    if value != db {
        session.set_cue_volume(value);
    }
    super::remote_map::overlay_main(&k, resp.rect, rlive_core::param::mixer::CUE_VOLUME);
    info(resp, "Cue Volume — the level of the Cue output");
    y + 3.0 * (ROW_H + 2.0)
}

/// A chooser in a device's own panel for where its sidechain comes from.
pub(super) fn sidechain_label(session: &Session, device: rlive_core::DeviceId) -> String {
    let chain = session.chain_of(device);
    let Some(source) = chain
        .and_then(|c| {
            session
                .devices_of(c)
                .iter()
                .find(|d| d.id == device)
                .map(|d| d.sidechain)
        })
        .flatten()
    else {
        return "No Input".to_string();
    };
    let track = session
        .project
        .track(source.track)
        .map_or("(gone)", |t| t.name.as_str());
    let tap = match source.tap {
        Tap::PreFx => "Pre FX",
        Tap::PostFx => "Post FX",
        _ => "Post Mixer",
    };
    format!("{track} | {tap}")
}
