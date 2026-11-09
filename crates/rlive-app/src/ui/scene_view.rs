//! Scenes: their rows in the Main track, and the Scene View (manual pp. 190-193).

use super::icons;
use super::info_view::edit_info_item;
use super::launch_panel::{follow_controls, merge_follow};
use super::session_select::{click_scene, SceneEdit, SceneField};
use super::session_view::{child, color_picker};
use super::theme::{self, *};
use super::widgets::{self, info, info_note, rgb};
use super::{Detail, UiState};
use crate::session::{InfoTarget, Session};
use egui::{Align2, Key, Layout, Modifiers, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use rlive_core::TimeSignature;
use std::sync::atomic::Ordering;

/// How wide the scene number column is.
const NUMBER_W: f32 = 20.0;
/// How much wider the Main track has to be for the tempo and signature controls to show.
pub const FIELDS_W: f32 = 84.0;
const TEMPO_W: f32 = 44.0;
const SIG_W: f32 = 36.0;

/// A scene row being dragged.
#[derive(Clone, Copy)]
struct SceneDrag(usize);

/// What the Main track's scene rows are drawn in.
pub(super) struct SceneRows {
    pub grid: Rect,
    pub left: f32,
    pub right: f32,
    pub first: usize,
    pub last: usize,
    pub scroll_y: f32,
    pub slot_h: f32,
    pub blink: bool,
}

impl SceneRows {
    fn y(&self, i: usize) -> f32 {
        self.grid.top() + i as f32 * self.slot_h - self.scroll_y
    }

    fn rect(&self, i: usize) -> Rect {
        Rect::from_min_size(
            Pos2::new(self.left + 1.0, self.y(i)),
            Vec2::new(self.right - self.left - 1.0, self.slot_h - 1.0),
        )
    }
}

/// Selects a scene and shows it in the Scene View.
pub(super) fn select_scene(state: &mut UiState, scene: usize) {
    click_scene(state, scene, false, false);
    state.detail = Detail::Scene;
    state.show_detail = true;
}

/// Launches a scene as its button does, and looks after what's selected.
pub(super) fn launch_scene(session: &mut Session, state: &mut UiState, scene: usize) {
    session.launch_scene(scene);
    let prefs = state.sel.prefs;
    if prefs.select_on_launch {
        click_scene(state, scene, false, false);
        state.detail = Detail::Scene;
    }
    // Next up is the scene below, whether or not the launched one was selected.
    if prefs.select_next_scene && scene + 1 < session.project.scenes.len() {
        state.sel.scenes = vec![scene + 1];
        state.sel.scene_anchor = Some(scene + 1);
    }
}

/// Starts typing a new name for the first selected scene.
pub fn rename_selected(session: &Session, state: &mut UiState) {
    if let Some(scene) = state.sel.scenes.first().copied() {
        start_edit(session, state, scene, SceneField::Name);
    }
}

/// Moves the selected scenes one place, gaps and all.
pub fn shift_selected(session: &mut Session, state: &mut UiState, delta: isize) {
    let selection = state.sel.scenes.clone();
    state.sel.scenes = session.shift_scenes(&selection, delta);
}

/// Parses a scene's time signature, "3/4".
fn parse_signature(text: &str) -> Option<TimeSignature> {
    let (n, d) = text.split_once('/')?;
    Some(TimeSignature {
        numerator: n.trim().parse().ok()?,
        denominator: d.trim().parse().ok()?,
    })
}

fn field_text(scene: &rlive_project::Scene, field: SceneField) -> String {
    match field {
        SceneField::Name => scene.name.clone(),
        SceneField::Tempo => scene.tempo.map_or(String::new(), |t| format!("{t:.2}")),
        SceneField::Signature => scene.signature.map_or(String::new(), |s| {
            format!("{}/{}", s.numerator, s.denominator)
        }),
    }
}

fn start_edit(session: &Session, state: &mut UiState, scene: usize, field: SceneField) {
    let Some(s) = session.project.scenes.get(scene) else {
        return;
    };
    state.sel.scene_edit = Some(SceneEdit {
        scene,
        field,
        text: field_text(s, field),
        focus: true,
    });
}

/// Writes what was typed into the scene.
fn commit_edit(session: &mut Session, edit: &SceneEdit) {
    let scenes = [edit.scene];
    match edit.field {
        SceneField::Name => {
            let name = edit.text.clone();
            session.edit_scenes(&scenes, "Rename Scene", |s| s.name = name.clone());
        }
        SceneField::Tempo => {
            let text = edit.text.trim();
            if text.is_empty() {
                session.set_scene_tempo(&scenes, None);
            } else if let Ok(t) = text.parse::<f64>() {
                session.set_scene_tempo(&scenes, Some(t));
            }
        }
        SceneField::Signature => {
            let text = edit.text.trim();
            if text.is_empty() {
                session.set_scene_signature(&scenes, None);
            } else if let Some(sig) = parse_signature(text) {
                session.set_scene_signature(&scenes, Some(sig));
            }
        }
    }
}

/// Where Tab goes from a field: the next one along, on to the next scene after the last.
fn next_field(
    scene: usize,
    field: SceneField,
    forward: bool,
    show_fields: bool,
    n_scenes: usize,
) -> Option<(usize, SceneField)> {
    let order: &[SceneField] = if show_fields {
        &[SceneField::Name, SceneField::Tempo, SceneField::Signature]
    } else {
        &[SceneField::Name]
    };
    let i = order.iter().position(|f| *f == field).unwrap_or(0);
    if forward {
        if i + 1 < order.len() {
            Some((scene, order[i + 1]))
        } else {
            (scene + 1 < n_scenes).then(|| (scene + 1, order[0]))
        }
    } else if i > 0 {
        Some((scene, order[i - 1]))
    } else {
        scene.checked_sub(1).map(|s| (s, order[order.len() - 1]))
    }
}

/// The text field a scene's row turns into while it's being typed in.
fn edit_field(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    rect: Rect,
    show_fields: bool,
) {
    let Some(mut edit) = state.sel.scene_edit.take() else {
        return;
    };
    let tab = ui.input_mut(|i| {
        let shift = i.consume_key(Modifiers::SHIFT, Key::Tab);
        let plain = i.consume_key(Modifiers::NONE, Key::Tab);
        (plain, shift)
    });
    let resp = ui.put(
        rect,
        egui::TextEdit::singleline(&mut edit.text)
            .font(theme::font(11.0))
            .margin(Vec2::new(2.0, 0.0)),
    );
    if edit.focus {
        resp.request_focus();
        edit.focus = false;
    }
    if tab.0 || tab.1 {
        commit_edit(session, &edit);
        let n = session.project.scenes.len();
        if let Some((scene, field)) = next_field(edit.scene, edit.field, tab.0, show_fields, n) {
            start_edit(session, state, scene, field);
        }
        return;
    }
    if ui.input(|i| i.key_pressed(Key::Escape)) {
        return;
    }
    if resp.lost_focus() {
        commit_edit(session, &edit);
        if edit.field != SceneField::Name && ui.input(|i| i.key_pressed(Key::Enter)) {
            select_scene(state, edit.scene);
        }
        return;
    }
    state.sel.scene_edit = Some(edit);
}

/// A Scene Tempo or Time Signature control.
#[allow(clippy::too_many_arguments)]
fn value_control(
    cu: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    r: Rect,
    i: usize,
    field: SceneField,
    show_fields: bool,
) {
    let editing = state
        .sel
        .scene_edit
        .as_ref()
        .is_some_and(|e| e.scene == i && e.field == field);
    if editing {
        edit_field(cu, session, state, r, show_fields);
        return;
    }
    let id = cu.scope_id().with(("scene_value", i, field as u8));
    let resp = cu.interact(r, id, Sense::click_and_drag());
    let scene = &session.project.scenes[i];
    let selected = state.sel.scene_field == Some((i, field));
    let (text, on) = match field {
        SceneField::Tempo => (
            scene.tempo.map_or("-".to_string(), |t| format!("{t:.0}")),
            scene.tempo.is_some(),
        ),
        _ => (
            scene.signature.map_or("-".to_string(), |s| {
                format!("{}/{}", s.numerator, s.denominator)
            }),
            scene.signature.is_some(),
        ),
    };
    let p = cu.painter();
    p.rect_filled(r, 1.0, pal().field_bg);
    p.text(
        r.center(),
        Align2::CENTER_CENTER,
        text,
        theme::font(10.5),
        if on { pal().accent } else { pal().text_faint },
    );
    if selected {
        widgets::outline(p, r, pal().accent);
    }
    if resp.dragged() {
        let dy = -resp.drag_delta().y as f64;
        match field {
            SceneField::Tempo => {
                let from = scene.tempo.unwrap_or(session.project.tempo);
                session.set_scene_tempo(&[i], Some((from + dy * 0.5).round()));
            }
            _ => {
                let from = scene.signature.unwrap_or(session.project.time_signature);
                let n = (from.numerator as f64 + dy * 0.2).round().clamp(1.0, 99.0) as u8;
                session.set_scene_signature(
                    &[i],
                    Some(TimeSignature {
                        numerator: n,
                        ..from
                    }),
                );
            }
        }
    }
    if resp.double_clicked() {
        disable_value(session, i, field);
    } else if resp.clicked() {
        state.sel.scene_field = Some((i, field));
        state.detail = Detail::Scene;
        state.show_detail = true;
        resp.request_focus();
    }
    if selected && resp.has_focus() {
        let typed = cu.input_mut(|inp| {
            let mut typed = String::new();
            inp.events.retain(|e| match e {
                egui::Event::Text(t) => {
                    typed.push_str(t);
                    false
                }
                _ => true,
            });
            typed
        });
        if !typed.is_empty() {
            start_edit(session, state, i, field);
            if let Some(e) = state.sel.scene_edit.as_mut() {
                e.text = typed;
            }
        }
        let (delete, enter) = cu.input(|inp| {
            (
                inp.key_pressed(Key::Delete) || inp.key_pressed(Key::Backspace),
                inp.key_pressed(Key::Enter),
            )
        });
        if delete {
            disable_value(session, i, field);
        } else if enter {
            select_scene(state, i);
        }
    }
    resp.context_menu(|ui| {
        if ui.button("Return to Default").clicked() {
            disable_value(session, i, field);
            ui.close();
        }
    });
    let name = if field == SceneField::Tempo {
        "Scene Tempo — set when the scene launches; drag, or click and type"
    } else {
        "Scene Time Signature — set when the scene launches; drag, or click and type"
    };
    info(resp, name);
}

fn disable_value(session: &mut Session, i: usize, field: SceneField) {
    match field {
        SceneField::Tempo => session.set_scene_tempo(&[i], None),
        _ => session.set_scene_signature(&[i], None),
    }
}

/// The scene rows in the Main track.
pub(super) fn rows(ui: &mut egui::Ui, session: &mut Session, state: &mut UiState, g: &SceneRows) {
    let show_fields = state.sel.scene_fields_w >= FIELDS_W * 0.5;
    let mut delete = None;
    let any_queued = session
        .track_shared
        .values()
        .any(|s| s.queued_slot.load(Ordering::Relaxed) >= 0);
    for i in g.first..g.last {
        let rect = g.rect(i);
        let mut cu = child(ui, g.grid, rect, Layout::left_to_right(egui::Align::Center));
        let resp = cu.allocate_rect(rect, Sense::click_and_drag());
        let cp = cu.painter().clone();
        let scene = &session.project.scenes[i];
        let selected = state.sel.scenes.contains(&i);
        let fill = match scene.color {
            Some(c) => theme::darken(rgb(c), if selected { 0.1 } else { 0.35 }),
            None if selected => shade(0x585858),
            None if resp.hovered() => shade(0x484848),
            None => shade(0x3a3a3a),
        };
        cp.rect_filled(rect, 0.0, fill);
        let launch = Rect::from_min_size(rect.min, Vec2::new(g.slot_h.min(22.0), rect.height()));
        let over_launch = resp.hover_pos().is_some_and(|pos| launch.contains(pos));
        // A scene is "queued" when every track with a clip in that row is queued on it.
        let queued_here = any_queued
            && session.project.tracks.iter().any(|t| {
                session
                    .track_shared
                    .get(&t.id)
                    .is_some_and(|s| s.queued_slot.load(Ordering::Relaxed) == i as i32)
            });
        let has_timing = scene.tempo.is_some() || scene.signature.is_some();
        let col = if queued_here && g.blink {
            pal().play_green
        } else if over_launch {
            shade(0xffffff)
        } else if has_timing {
            pal().accent
        } else {
            shade(0xb0b0b0)
        };
        if scene.follow.assigned() {
            icons::play_striped(&cp, launch, col, fill);
        } else {
            icons::play(&cp, launch, col);
        }
        let number_x = rect.right() - NUMBER_W;
        cp.text(
            Pos2::new(rect.right() - 3.0, rect.center().y),
            Align2::RIGHT_CENTER,
            (i + 1).to_string(),
            theme::font(10.5),
            if scene.color.is_some() {
                theme::text_on(fill)
            } else {
                pal().text_dim
            },
        );
        let fields_w = if show_fields {
            TEMPO_W + SIG_W + 2.0
        } else {
            0.0
        };
        let name_r = Rect::from_min_max(
            Pos2::new(launch.right() + 3.0, rect.top()),
            Pos2::new(number_x - fields_w, rect.bottom()),
        );
        let editing_name = state
            .sel
            .scene_edit
            .as_ref()
            .is_some_and(|e| e.scene == i && e.field == SceneField::Name);
        if editing_name {
            edit_field(&mut cu, session, state, name_r, show_fields);
        } else {
            cp.with_clip_rect(name_r.intersect(cu.clip_rect())).text(
                Pos2::new(name_r.left(), rect.center().y),
                Align2::LEFT_CENTER,
                scene.name.clone(),
                theme::font(11.0),
                pal().text,
            );
        }
        if show_fields {
            let tempo_r = Rect::from_min_size(
                Pos2::new(number_x - fields_w, rect.top() + 1.0),
                Vec2::new(TEMPO_W, rect.height() - 2.0),
            );
            let sig_r = Rect::from_min_size(
                Pos2::new(tempo_r.right() + 2.0, rect.top() + 1.0),
                Vec2::new(SIG_W, rect.height() - 2.0),
            );
            value_control(&mut cu, session, state, tempo_r, i, SceneField::Tempo, true);
            value_control(
                &mut cu,
                session,
                state,
                sig_r,
                i,
                SceneField::Signature,
                true,
            );
        }
        super::remote_map::overlay(&cu, resp.rect, rlive_project::MapTarget::Scene(i));
        if resp.clicked() {
            if over_launch {
                launch_scene(session, state, i);
            } else {
                let (shift, cmd) = cu.input(|m| (m.modifiers.shift, m.modifiers.command));
                click_scene(state, i, shift, cmd);
                state.detail = Detail::Scene;
                state.show_detail = true;
            }
        }
        if resp.double_clicked() && !over_launch {
            start_edit(session, state, i, SceneField::Name);
        }
        resp.dnd_set_drag_payload(SceneDrag(i));
        if resp.dnd_hover_payload::<SceneDrag>().is_some() {
            cp.line_segment(
                [rect.left_top(), rect.right_top()],
                Stroke::new(2.0, pal().accent),
            );
        }
        if let Some(from) = resp.dnd_release_payload::<SceneDrag>() {
            let selection = if state.sel.scenes.contains(&from.0) {
                state.sel.scenes.clone()
            } else {
                vec![from.0]
            };
            // Dragged downwards, a scene lands below the row it's dropped on.
            let to = if from.0 < i { i + 1 } else { i };
            state.sel.scenes = session.move_scenes(&selection, to);
        }
        let resp = info(
            resp,
            "Scene — click ▶ to launch the row, click to select, drag to reorder",
        );
        info_note(
            &resp,
            session.info_text(InfoTarget::Scene(i)).unwrap_or_default(),
        );
        resp.context_menu(|ui| {
            scene_menu(ui, session, state, i, &mut delete);
        });
    }
    if let Some(i) = delete {
        session.delete_scene(i);
        state.sel.scenes.clear();
    }
}

fn scene_menu(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    i: usize,
    delete: &mut Option<usize>,
) {
    let targets = if state.sel.scenes.contains(&i) {
        state.sel.scenes.clone()
    } else {
        vec![i]
    };
    if ui.button("Launch").clicked() {
        launch_scene(session, state, i);
        ui.close();
    }
    if ui.button("Rename").clicked() {
        start_edit(session, state, i, SceneField::Name);
        ui.close();
    }
    edit_info_item(ui, session, state, InfoTarget::Scene(i));
    ui.menu_button("Color", |ui| {
        if let Some(c) = color_picker(ui) {
            session.edit_scenes(&targets, "Scene Color", |s| s.color = Some(c));
            ui.close();
        }
    });
    ui.separator();
    if ui.button("Insert Scene").clicked() {
        if let Some(new) = session.insert_scene(i) {
            click_scene(state, new, false, false);
        }
        ui.close();
    }
    if ui.button("Capture and Insert Scene").clicked() {
        if let Some(new) = session.capture_and_insert_scene(i) {
            click_scene(state, new, false, false);
        }
        ui.close();
    }
    ui.separator();
    if ui
        .button(RichText::new("Delete Scene").color(pal().arm_red))
        .clicked()
    {
        *delete = Some(i);
        ui.close();
    }
}

/// The Scene View: the selected scenes' tempo, signature and Follow Actions.
pub fn show(ui: &mut egui::Ui, session: &mut Session, state: &mut UiState) {
    let targets: Vec<usize> = state
        .sel
        .scenes
        .iter()
        .copied()
        .filter(|i| *i < session.project.scenes.len())
        .collect();
    let Some(first) = targets.first().copied() else {
        ui.centered_and_justified(|ui| {
            ui.label(RichText::new("Select a scene").color(pal().text_dim));
        });
        return;
    };
    let scene = session.project.scenes[first].clone();
    egui::ScrollArea::vertical()
        .id_salt("scene_view")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let title = if targets.len() > 1 {
                format!("{} Scenes Selected", targets.len())
            } else {
                let name = if scene.name.is_empty() {
                    "Scene".to_string()
                } else {
                    scene.name.clone()
                };
                format!("{}  {name}", first + 1)
            };
            let fill = scene.color.map(rgb);
            let (rect, resp) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 20.0), Sense::click());
            ui.painter()
                .rect_filled(rect, 0.0, fill.unwrap_or(shade(0x585858)));
            ui.painter().text(
                rect.left_center() + Vec2::new(6.0, 0.0),
                Align2::LEFT_CENTER,
                title,
                theme::font_bold(11.0),
                fill.map_or(pal().text, theme::text_on),
            );
            resp.context_menu(|ui| {
                if ui.button("Rename").clicked() {
                    start_edit(session, state, first, SceneField::Name);
                    ui.close();
                }
                ui.menu_button("Color", |ui| {
                    if let Some(c) = color_picker(ui) {
                        session.edit_scenes(&targets, "Scene Color", |s| s.color = Some(c));
                        ui.close();
                    }
                });
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(170.0);
                    timing_section(ui, session, &targets, &scene);
                });
                ui.vertical(|ui| {
                    ui.set_width(210.0);
                    let sig = session.project.time_signature;
                    if let Some(next) = follow_controls(ui, "scene", scene.follow, false, sig) {
                        let before = scene.follow;
                        session.edit_scenes(&targets, "Scene Follow Action", |s| {
                            merge_follow(&before, &next, &mut s.follow)
                        });
                    }
                });
            });
        });
}

fn timing_section(
    ui: &mut egui::Ui,
    session: &mut Session,
    targets: &[usize],
    scene: &rlive_project::Scene,
) {
    ui.horizontal(|ui| {
        let on = scene.tempo.is_some();
        let r = widgets::state_button(ui, Vec2::new(52.0, 16.0), on, pal().accent, "Tempo");
        if info(r, "Scene Tempo — set the tempo when these scenes launch").clicked() {
            let next = (!on).then_some(session.project.tempo);
            session.set_scene_tempo(targets, next);
        }
        ui.add_enabled_ui(on, |ui| {
            let mut bpm = scene.tempo.unwrap_or(session.project.tempo);
            let r = ui.add(
                egui::DragValue::new(&mut bpm)
                    .range(20.0..=999.0)
                    .speed(0.5)
                    .fixed_decimals(2),
            );
            if r.changed() {
                session.set_scene_tempo(targets, Some(bpm));
            }
        });
    });
    ui.horizontal(|ui| {
        let on = scene.signature.is_some();
        let r = widgets::state_button(ui, Vec2::new(52.0, 16.0), on, pal().accent, "Signature");
        if info(
            r,
            "Scene Time Signature — set the meter when these scenes launch",
        )
        .clicked()
        {
            let next = (!on).then_some(session.project.time_signature);
            session.set_scene_signature(targets, next);
        }
        ui.add_enabled_ui(on, |ui| {
            let sig = scene.signature.unwrap_or(session.project.time_signature);
            let (mut n, mut d) = (sig.numerator, sig.denominator);
            ui.spacing_mut().item_spacing.x = 2.0;
            let nr = ui.add(egui::DragValue::new(&mut n).range(1..=99));
            ui.label("/");
            let dr = egui::ComboBox::from_id_salt("scene_denominator")
                .width(40.0)
                .selected_text(d.to_string())
                .show_ui(ui, |ui| {
                    let mut changed = false;
                    for v in [1u8, 2, 4, 8, 16] {
                        changed |= ui.selectable_value(&mut d, v, v.to_string()).changed();
                    }
                    changed
                });
            if nr.changed() || dr.inner == Some(true) {
                session.set_scene_signature(
                    targets,
                    Some(TimeSignature {
                        numerator: n,
                        denominator: d,
                    }),
                );
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_parse_and_must_have_both_halves() {
        let sig = parse_signature("7/8").unwrap();
        assert_eq!((sig.numerator, sig.denominator), (7, 8));
        assert!(parse_signature("7").is_none());
    }

    #[test]
    fn tab_walks_the_fields_and_on_to_the_next_scene() {
        use SceneField::*;
        assert_eq!(next_field(0, Name, true, true, 3), Some((0, Tempo)));
        assert_eq!(next_field(0, Signature, true, true, 3), Some((1, Name)));
        assert_eq!(next_field(1, Name, false, true, 3), Some((0, Signature)));
        assert_eq!(next_field(2, Signature, true, true, 3), None);
        assert_eq!(next_field(0, Name, true, false, 3), Some((1, Name)));
    }
}
