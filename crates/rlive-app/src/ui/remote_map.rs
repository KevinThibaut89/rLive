//! MIDI and Key Map Mode: the overlays on every mappable control and the Mapping Browser.
//!
//! Widgets call [`overlay`] with the rect of something mappable. In a map mode that draws the
//! tint, takes the click from the widget underneath and shows what it's mapped to; otherwise it
//! does nothing at all, so it costs the rest of the time nothing but a map lookup.

use super::theme::{self, pal};
use super::{widgets, RliveApp, UiState};
use crate::session::{MapMode, MappingRow, Session};
use egui::{Align2, Color32, Id, Key, Modifiers, Rect, RichText, Sense, Stroke, Vec2};
use rlive_core::param::{ParamId, ParamOwner, ParamRef};
use rlive_core::TrackId;
use rlive_project::{CcEncoding, CrossfaderPoint, MapTarget, RemoteControl as C};
use std::collections::HashMap;

/// What the overlays need to know this frame.
#[derive(Clone, Default)]
pub struct MapFrame {
    mode: Option<MapMode>,
    selected: Option<MapTarget>,
    labels: HashMap<MapTarget, String>,
    clicked: Option<MapTarget>,
    delete: Option<usize>,
    /// A modulator's Map button is waiting for a parameter.
    mod_armed: bool,
    mod_clicked: Option<ParamRef>,
}

fn frame_id() -> Id {
    Id::unique("rlive_remote_map_frame")
}

fn with_frame<R>(ctx: &egui::Context, f: impl FnOnce(&mut MapFrame) -> R) -> R {
    ctx.data_mut(|d| f(d.get_temp_mut_or_default(frame_id())))
}

/// Call at the start of a frame.
pub fn begin_frame(ctx: &egui::Context, session: &Session) {
    let mode = session.map_mode();
    with_frame(ctx, |f| {
        f.mode = mode;
        f.selected = session.map_target();
        f.clicked = None;
        f.mod_armed = session.armed_mod_map().is_some();
        f.mod_clicked = None;
        f.labels.clear();
        if let Some(mode) = mode {
            for row in session.mapping_rows(mode) {
                let target = match mode {
                    MapMode::Midi => session.project.midi_mappings[row.index].target,
                    MapMode::Key => session.project.key_mappings[row.index].target,
                };
                f.labels.insert(target, row.control);
            }
        }
    });
}

/// Call at the end of a frame: applies a click on an overlay.
pub fn end_frame(ctx: &egui::Context, session: &mut Session) {
    if let Some(target) = with_frame(ctx, |f| f.mod_clicked.take()) {
        session.finish_mod_map(target);
    }
    if let Some(target) = with_frame(ctx, |f| f.clicked.take()) {
        session.select_map_target(Some(target));
    }
    if let Some(index) = with_frame(ctx, |f| f.delete.take()) {
        match session.map_mode() {
            Some(MapMode::Midi) => session.delete_midi_mapping(index),
            Some(MapMode::Key) => session.delete_key_mapping(index),
            None => {}
        }
    }
}

fn map_color(mode: MapMode) -> Color32 {
    match mode {
        MapMode::Midi => pal().solo_blue,
        MapMode::Key => pal().arm_red,
    }
}

/// Marks `rect` as mappable to `target`. Does nothing outside the map modes.
pub fn overlay(ui: &egui::Ui, rect: Rect, target: MapTarget) {
    if let MapTarget::Param(param) = target {
        if with_frame(ui.ctx(), |f| f.mod_armed) {
            let id = ui.scope_id().with(("mod_overlay", target));
            let resp = ui.interact(rect, id, Sense::click());
            ui.painter_at(rect.expand(1.0)).rect_filled(
                rect,
                2.0,
                pal().accent.gamma_multiply(0.3),
            );
            if resp.clicked() {
                with_frame(ui.ctx(), |f| f.mod_clicked = Some(param));
            }
            return;
        }
    }
    let info = with_frame(ui.ctx(), |f| {
        f.mode
            .map(|m| (m, f.selected, f.labels.get(&target).cloned()))
    });
    let Some((mode, selected, label)) = info else {
        return;
    };
    let id = ui.scope_id().with(("remote_overlay", target));
    let resp = ui.interact(rect, id, Sense::click_and_drag());
    let is_selected = selected == Some(target);
    let base = map_color(mode);
    let fill = if is_selected {
        base.gamma_multiply(0.75)
    } else if label.is_some() {
        base.gamma_multiply(0.55)
    } else {
        base.gamma_multiply(0.28)
    };
    let p = ui.painter_at(rect.expand(1.0));
    p.rect_filled(rect, 2.0, fill);
    if is_selected {
        p.rect_stroke(
            rect,
            2.0,
            Stroke::new(1.5, pal().text),
            egui::StrokeKind::Inside,
        );
    }
    if let Some(label) = label {
        let size = (rect.height() - 2.0).clamp(7.0, 10.0);
        p.text(
            rect.center(),
            Align2::CENTER_CENTER,
            label,
            theme::font(size),
            theme::text_on(base),
        );
    }
    if resp.clicked() || resp.drag_started() {
        with_frame(ui.ctx(), |f| f.clicked = Some(target));
    }
    resp.on_hover_text(match mode {
        MapMode::Midi => "Click, then send a MIDI message from your controller",
        MapMode::Key => "Click, then press a key",
    });
}

/// The overlay for a parameter of a track.
pub fn overlay_track(ui: &egui::Ui, rect: Rect, track: TrackId, id: ParamId) {
    overlay(
        ui,
        rect,
        MapTarget::Param(ParamRef {
            owner: ParamOwner::Track(track),
            id,
        }),
    );
}

/// The overlay for a parameter of the Main track.
pub fn overlay_main(ui: &egui::Ui, rect: Rect, id: ParamId) {
    overlay(
        ui,
        rect,
        MapTarget::Param(ParamRef {
            owner: ParamOwner::Master,
            id,
        }),
    );
}

/// The crossfader's three fields, drawn over its bar.
pub fn overlay_crossfader(ui: &egui::Ui, rect: Rect) {
    let third = rect.width() / 3.0;
    for (i, point) in [
        CrossfaderPoint::Left,
        CrossfaderPoint::Center,
        CrossfaderPoint::Right,
    ]
    .into_iter()
    .enumerate()
    {
        let r = Rect::from_min_size(
            rect.min + Vec2::new(i as f32 * third, 0.0),
            Vec2::new(third, rect.height()),
        );
        overlay(ui, r, MapTarget::Crossfader(point));
    }
}

/// Draws the map mode's extra controls, which exist only as things to map. Below the Session
/// grid is the Relative Session strip (manual p. 872); the rest have no other home on screen.
pub fn strip(ui: &mut egui::Ui, session: &Session, state: &UiState) {
    let controls = [
        (C::SceneUp, "Scene Up"),
        (C::SceneDown, "Scene Down"),
        (C::SceneScroll, "Scene Select"),
        (C::LaunchHighlightedScene, "Launch"),
        (C::CancelSceneLaunch, "Cancel"),
        (C::StopAllClips, "Stop Clips"),
        (C::NewTake, "New"),
        (C::PreviousLocator, "Prev Loc"),
        (C::SetLocator, "Set Loc"),
        (C::NextLocator, "Next Loc"),
        (C::StepBack, "Step <"),
        (C::StepForward, "Step >"),
        (C::NudgeBack, "Nudge <"),
        (C::Scrub, "Scrub"),
        (C::NudgeForward, "Nudge >"),
        (C::ClipLoop, "Clip Loop"),
        (C::ClipLoopPosition, "Loop Pos"),
        (C::ClipLoopLength, "Loop Len"),
        (C::Continue, "Continue"),
    ];
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        ui.label(
            RichText::new("Relative Session")
                .size(10.0)
                .color(pal().text_dim),
        );
        for (control, label) in controls {
            let r = widgets::state_button(ui, Vec2::new(62.0, 18.0), false, pal().accent, label);
            overlay(ui, r.rect, MapTarget::Control(control));
        }
        ui.separator();
        ui.label(RichText::new("Tracks").size(10.0).color(pal().text_dim));
        for t in session.project.tracks.iter().take(16) {
            if t.kind == rlive_project::TrackKind::Group {
                continue;
            }
            let label: String = t.name.chars().take(6).collect();
            let r = widgets::state_button(ui, Vec2::new(44.0, 18.0), false, pal().accent, &label);
            overlay(ui, r.rect, MapTarget::HighlightedSlot(t.id));
        }
    });
    let _ = state;
}

fn value_text(session: &Session, row: &MappingRow, value: f64) -> String {
    let _ = (session, row);
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.2}")
    }
}

/// The Mapping Browser.
pub fn browser(ui: &mut egui::Ui, session: &mut Session, state: &mut UiState) {
    let Some(mode) = session.map_mode() else {
        return;
    };
    ui.label(
        RichText::new(match mode {
            MapMode::Midi => "MIDI Mappings",
            MapMode::Key => "Key Mappings",
        })
        .strong()
        .color(pal().text),
    );
    ui.add_space(4.0);
    if let Some(t) = session.map_target() {
        let (path, name) = target_name(session, t);
        ui.label(
            RichText::new(format!(
                "{}: {name} ({path})",
                match mode {
                    MapMode::Midi => "Move a control for",
                    MapMode::Key => "Press a key for",
                }
            ))
            .size(11.0)
            .color(pal().select),
        );
        ui.add_space(4.0);
    }
    let rows = session.mapping_rows(mode);
    if rows.is_empty() {
        ui.label(
            RichText::new("No mappings yet. Click a highlighted control, then move a control.")
                .size(11.0)
                .color(pal().text_dim),
        );
        return;
    }
    egui::ScrollArea::vertical()
        .id_salt("remote_mappings")
        .show(ui, |ui| {
            egui::Grid::new("remote_mapping_rows")
                .num_columns(5)
                .spacing([6.0, 3.0])
                .show(ui, |ui| {
                    for h in ["Control", "Path", "Name", "Min", "Max"] {
                        ui.label(RichText::new(h).size(10.0).color(pal().text_dim));
                    }
                    ui.end_row();
                    for row in &rows {
                        mapping_row(ui, session, state, mode, row);
                    }
                });
        });
    // Delete from the browser (manual p. 869), once a row is selected.
    let delete = ui.input(|i| {
        !i.modifiers.any() && (i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace))
    });
    if delete {
        if let Some(index) = state.mapping_row {
            with_frame(ui.ctx(), |f| f.delete = Some(index));
            state.mapping_row = None;
        }
    }
}

fn target_name(session: &Session, t: MapTarget) -> (String, String) {
    session.mapping_describe(t)
}

fn mapping_row(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    mode: MapMode,
    row: &MappingRow,
) {
    let selected = state.mapping_row == Some(row.index);
    let name = RichText::new(&row.control).size(11.0).color(if selected {
        pal().select
    } else {
        pal().text
    });
    let resp = ui.selectable_label(selected, name);
    if resp.clicked() {
        state.mapping_row = Some(row.index);
    }
    resp.context_menu(|ui| {
        if ui.button("Invert").clicked() {
            session.invert_mapping(mode, row.index);
            ui.close();
        }
        if ui.button("Delete").clicked() {
            match mode {
                MapMode::Midi => session.delete_midi_mapping(row.index),
                MapMode::Key => session.delete_key_mapping(row.index),
            }
            ui.close();
        }
    });
    ui.label(RichText::new(&row.path).size(10.0).color(pal().text_dim));
    ui.label(RichText::new(&row.name).size(11.0).color(pal().text));
    if row.fixed_range {
        ui.label("");
        ui.label("");
    } else {
        range_cell(ui, session, mode, row, true);
        range_cell(ui, session, mode, row, false);
    }
    ui.end_row();
    if let Some(encoding) = row.encoding {
        ui.label("");
        ui.label("");
        let mut picked = encoding;
        egui::ComboBox::from_id_salt(("remote_encoding", row.index))
            .width(150.0)
            .selected_text(RichText::new(encoding.label()).size(10.0))
            .show_ui(ui, |ui| {
                for e in CcEncoding::ALL {
                    ui.selectable_value(&mut picked, e, e.label());
                }
            });
        if picked != encoding {
            session.set_mapping_encoding(row.index, picked);
        }
        ui.end_row();
    }
}

fn range_cell(
    ui: &mut egui::Ui,
    session: &mut Session,
    mode: MapMode,
    row: &MappingRow,
    min: bool,
) {
    let mut value = if min { row.min } else { row.max };
    let text = value_text(session, row, value);
    let drag = egui::DragValue::new(&mut value)
        .speed(0.25)
        .custom_formatter(move |_, _| text.clone());
    let resp = ui.add(drag);
    if resp.changed() {
        let (lo, hi) = if min {
            (value, row.max)
        } else {
            (row.min, value)
        };
        session.set_mapping_range(mode, row.index, lo, hi);
    }
}

/// What a key means in the map modes, or as a mapping. Returns whether `raw` consumed it.
pub fn key_event(app: &mut RliveApp, key: Key, modifiers: Modifiers, pressed: bool) -> bool {
    if pressed && app.session.map_mode() == Some(MapMode::Key) && !modifiers.command {
        if let Some(target) = app.session.map_target() {
            app.session.set_key_mapping(target, key.name());
            return true;
        }
    }
    if modifiers.command || modifiers.alt || modifiers.ctrl {
        return false;
    }
    if app.session.map_mode().is_some() {
        return false;
    }
    // Computer MIDI Keyboard notes take precedence over mapped keys (manual p. 379).
    if app.ui.computer_keys && crate::ui::keymap::is_note_key(key) {
        return false;
    }
    let mapped: Vec<_> = app
        .session
        .project
        .key_mappings
        .iter()
        .filter(|m| m.key == key.name())
        .cloned()
        .collect();
    if mapped.is_empty() {
        return false;
    }
    for m in &mapped {
        app.session.key_mapping_event(m, pressed);
    }
    true
}

/// For code that wants to know what mapping colour a mode uses.
pub fn mode_color(mode: MapMode) -> Color32 {
    map_color(mode)
}
