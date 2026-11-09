//! The Velocity, Release Velocity and Chance lanes, and the controls under them (manual
//! pp. 277-284).

use super::gestures::{lane_gesture, take_lane};
use super::{edit_foreground, Geo, Shown, UiState};
use crate::session::Session;
use crate::ui::keymap::{self, Platform};
use crate::ui::theme::*;
use crate::ui::widgets::{info, state_button};
use egui::{Color32, Pos2, Rect, RichText, Sense, Shape, Stroke, UiBuilder, Vec2};
use rlive_project::note_edit::{
    self as edit, deviation_bounds, drawn_notes, group_status, line_values, LaneValue,
};
use rlive_project::NoteData;

/// Which lanes there are, whether they show, and the settings of the controls under them.
pub struct LaneState {
    /// Velocity, Release Velocity, Chance.
    pub shown: [bool; 3],
    pub hide_all: bool,
    pub height: f32,
    /// The lane the keyboard is typing into.
    pub focus: Option<LaneValue>,
    pub typed: String,
    pub random: f32,
    pub chance_random: f32,
    pub ramp: (f32, f32),
    pub deviation: f32,
    /// The note under the pointer, whose marker is highlighted.
    pub hover_note: Option<usize>,
}

impl Default for LaneState {
    fn default() -> Self {
        Self {
            shown: [true, false, false],
            hide_all: false,
            height: 52.0,
            focus: None,
            typed: String::new(),
            random: 0.0,
            chance_random: 0.0,
            ramp: (0.5, 1.0),
            deviation: 0.0,
            hover_note: None,
        }
    }
}

const KINDS: [LaneValue; 3] = [LaneValue::Velocity, LaneValue::Release, LaneValue::Chance];
const LABELS: [&str; 3] = ["Velocity", "Release", "Chance"];

impl LaneState {
    pub fn total_height(&self) -> f32 {
        if self.hide_all {
            return 0.0;
        }
        self.shown.iter().filter(|s| **s).count() as f32 * self.height
    }

    /// Velocity and Chance swap places in the lane (manual p. 254).
    pub fn swap(&mut self) {
        self.shown.swap(0, 2);
    }
}

/// How a lane drag began, to know what it changes.
#[derive(Clone, Copy, PartialEq)]
pub enum LaneMode {
    Value,
    Deviation,
    Grid,
    Individual,
    Line { from: (f64, f32) },
}

pub struct LaneDrag {
    what: LaneValue,
    base: Vec<NoteData>,
    picks: Vec<usize>,
    origin: Pos2,
    mode: LaneMode,
    height: f32,
}

fn value_at(pos: Pos2, inner: Rect) -> f32 {
    ((inner.bottom() - pos.y) / inner.height()).clamp(0.0, 1.0)
}

fn marker_x(geo: &Geo, s: &Shown, n: &NoteData, at: &mut Vec<f64>) -> f32 {
    s.span.occurrences(n.start, at);
    geo.x(at.first().copied().unwrap_or(n.start as f64)) + 1.0
}

/// Paints the stems of one lane and returns what's under the pointer.
#[allow(clippy::too_many_arguments)]
fn paint_lane(
    p: &egui::Painter,
    rect: Rect,
    inner: Rect,
    geo: &Geo,
    s: &Shown,
    what: LaneValue,
    selected: &[usize],
    hover: Option<usize>,
) {
    p.rect_filled(rect, 0.0, shade(0x262626));
    p.line_segment(
        [rect.left_top(), rect.right_top()],
        Stroke::new(1.0, pal().separator),
    );
    let mut at = Vec::new();
    let mut groups_done: Vec<u32> = Vec::new();
    for (i, n) in s.clip.notes().iter().enumerate() {
        let x = marker_x(geo, s, n, &mut at);
        if x < rect.left() - 4.0 || x > rect.right() + 4.0 {
            continue;
        }
        if what == LaneValue::Chance {
            if let Some(g) = n.group {
                if groups_done.contains(&g.id) {
                    continue;
                }
                groups_done.push(g.id);
            }
        }
        let y = inner.bottom() - inner.height() * what.get(n);
        let sel = selected.contains(&i);
        let c = if sel || hover == Some(i) {
            shade(0xb5e0ff)
        } else if n.muted {
            shade(0x6a6a6a)
        } else {
            s.color
        };
        if what == LaneValue::Velocity && n.vel_range != 0.0 {
            let (lo, hi) = deviation_bounds(n);
            let band = Rect::from_x_y_ranges(
                x - 3.0..=x + 3.0,
                inner.bottom() - inner.height() * hi..=inner.bottom() - inner.height() * lo,
            );
            p.rect_filled(band, 0.0, c.gamma_multiply(0.35));
        }
        p.line_segment(
            [Pos2::new(x, inner.bottom()), Pos2::new(x, y)],
            Stroke::new(1.5, c),
        );
        let center = Pos2::new(x, y);
        match (what == LaneValue::Chance, n.group) {
            (true, Some(g)) if g.one => {
                p.add(Shape::convex_polygon(
                    vec![
                        center + Vec2::new(0.0, -4.0),
                        center + Vec2::new(4.0, 3.0),
                        center + Vec2::new(-4.0, 3.0),
                    ],
                    c,
                    Stroke::NONE,
                ));
            }
            (true, Some(_)) => {
                p.add(Shape::convex_polygon(
                    vec![
                        center + Vec2::new(0.0, -4.0),
                        center + Vec2::new(4.0, 0.0),
                        center + Vec2::new(0.0, 4.0),
                        center + Vec2::new(-4.0, 0.0),
                    ],
                    c,
                    Stroke::NONE,
                ));
            }
            _ => {
                p.circle_filled(center, 2.5, c);
                if what == LaneValue::Velocity && n.vel_range != 0.0 {
                    p.circle_filled(center, 1.0, shade(0x101010));
                }
            }
        }
    }
}

/// The markers near `x`, and the group members of any that's in a group.
fn markers_at(geo: &Geo, s: &Shown, what: LaneValue, x: f32, selected: &[usize]) -> Vec<usize> {
    let mut at = Vec::new();
    let notes = s.clip.notes();
    let near: Vec<usize> = notes
        .iter()
        .enumerate()
        .filter(|(_, n)| (marker_x(geo, s, n, &mut at) - x).abs() < 5.0)
        .map(|(i, _)| i)
        .collect();
    if near.is_empty() {
        return near;
    }
    if near.iter().any(|t| selected.contains(t)) {
        return selected.to_vec();
    }
    if what == LaneValue::Chance {
        let groups: Vec<u32> = near
            .iter()
            .filter_map(|i| notes[*i].group.map(|g| g.id))
            .collect();
        let mut all = near;
        for (i, n) in notes.iter().enumerate() {
            if n.group.is_some_and(|g| groups.contains(&g.id)) && !all.contains(&i) {
                all.push(i);
            }
        }
        return all;
    }
    near
}

#[allow(clippy::too_many_arguments)]
pub(super) fn lanes(
    ui: &mut egui::Ui,
    area: Rect,
    geo: &Geo,
    session: &mut Session,
    state: &mut UiState,
    s: &Shown,
    _now: f64,
) {
    let height = state.piano.lanes.height;
    let visible: Vec<usize> = (0..3).filter(|i| state.piano.lanes.shown[*i]).collect();
    let mut top = area.top();
    let live = session
        .clip_at(s.r)
        .map(|c| c.notes().to_vec())
        .unwrap_or_default();
    let live_shown = Shown {
        r: s.r,
        clip: {
            let mut c = s.clip.clone();
            if let Some(n) = c.notes_mut() {
                n.clone_from(&live);
            }
            c
        },
        span: s.span,
        color: s.color,
    };
    for i in visible {
        let what = KINDS[i];
        let rect =
            Rect::from_min_size(Pos2::new(area.left(), top), Vec2::new(area.width(), height));
        top += height;
        let plot = Rect::from_min_max(Pos2::new(geo.grid.left(), rect.top()), rect.max);
        let inner = plot.shrink2(Vec2::new(0.0, 4.0));
        let p = ui.painter_at(rect);
        paint_lane(
            &p,
            plot,
            inner,
            geo,
            &live_shown,
            what,
            &state.piano.selected,
            state.piano.lanes.hover_note,
        );
        // Header.
        p.rect_filled(
            Rect::from_min_max(rect.min, Pos2::new(plot.left(), rect.bottom())),
            0.0,
            shade(0x303030),
        );
        p.text(
            rect.min + Vec2::new(3.0, 2.0),
            egui::Align2::LEFT_TOP,
            LABELS[i],
            font(9.0),
            pal().text_dim,
        );
        if state.piano.lanes.focus == Some(what) && !state.piano.lanes.typed.is_empty() {
            p.text(
                rect.left_bottom() + Vec2::new(3.0, -2.0),
                egui::Align2::LEFT_BOTTOM,
                state.piano.lanes.typed.clone(),
                font(9.0),
                pal().text,
            );
        }
        let header = Rect::from_min_max(rect.min, Pos2::new(plot.left(), rect.bottom()));
        let hr = ui.interact(header, ui.scope_id().with(("lane_h", i)), Sense::click());
        hr.context_menu(|ui| {
            if ui.button("Swap Lane").clicked() {
                state.piano.lanes.swap();
                ui.close();
            }
        });

        lane_interaction(ui, plot, inner, geo, session, state, &live_shown, what, i);
    }
    // Typing a value.
    if state.piano.lanes.focus.is_some() && !ui.ctx().egui_wants_keyboard_input() {
        let digits = [
            egui::Key::Num1,
            egui::Key::Num2,
            egui::Key::Num3,
            egui::Key::Num4,
            egui::Key::Num5,
            egui::Key::Num6,
            egui::Key::Num7,
            egui::Key::Num8,
            egui::Key::Num9,
        ];
        let pressed: Vec<char> = ui.input(|i| {
            digits
                .iter()
                .enumerate()
                .filter(|(_, k)| i.key_pressed(**k))
                .map(|(n, _)| char::from_digit(n as u32 + 1, 10).unwrap_or('1'))
                .collect()
        });
        state.piano.lanes.typed.extend(pressed);
    }
}

/// Puts a typed value on the selected markers. Velocity is 0-127, Chance 0-100.
pub fn commit_typed(session: &mut Session, state: &mut UiState) -> bool {
    let Some(what) = state.piano.lanes.focus else {
        return false;
    };
    let text = std::mem::take(&mut state.piano.lanes.typed);
    let Ok(n) = text.parse::<f32>() else {
        return false;
    };
    let v = match what {
        LaneValue::Chance => (n / 100.0).clamp(0.0, 1.0),
        _ => (n / 127.0).clamp(0.0, 1.0),
    };
    let label = if what == LaneValue::Chance {
        "Set Chance"
    } else {
        "Set Velocity"
    };
    edit_foreground(session, state, label, |notes, sel| {
        edit::set_value(notes, sel, what, v);
    });
    true
}

#[allow(clippy::too_many_arguments)]
fn lane_interaction(
    ui: &mut egui::Ui,
    plot: Rect,
    inner: Rect,
    geo: &Geo,
    session: &mut Session,
    state: &mut UiState,
    s: &Shown,
    what: LaneValue,
    index: usize,
) {
    let resp = ui.interact(
        plot,
        ui.scope_id().with(("lane", index)),
        Sense::click_and_drag(),
    );
    let mods = ui.input(|i| i.modifiers);
    let draw_mode = state.arr.auto.draw;
    if let Some(pos) = resp.hover_pos() {
        let near = markers_at(geo, s, what, pos.x, &[]);
        state.piano.lanes.hover_note = near.first().copied();
    }
    if resp.clicked() {
        state.piano.lanes.focus = Some(what);
        state.piano.lanes.typed.clear();
        if let Some(pos) = resp.interact_pointer_pos() {
            let hits = markers_at(geo, s, what, pos.x, &[]);
            let ps = &mut state.piano;
            if mods.shift {
                for h in hits {
                    if let Some(k) = ps.selected.iter().position(|x| *x == h) {
                        ps.selected.remove(k);
                    } else {
                        ps.selected.push(h);
                    }
                }
            } else if !hits.is_empty() {
                ps.selected = hits;
            }
        }
    }
    if resp.double_clicked() && what == LaneValue::Velocity {
        if let Some(pos) = resp.interact_pointer_pos() {
            let hits = markers_at(geo, s, what, pos.x, &[]);
            if !hits.is_empty() {
                session.edit_notes(s.r, "Velocity Deviation", |n| {
                    edit::set_deviation(n, &hits, 0.0);
                });
            }
        }
    }
    if resp.secondary_clicked() {
        state.piano.lanes.focus = Some(what);
    }
    resp.context_menu(|ui| {
        if ui.button("Swap Lane").clicked() {
            state.piano.lanes.swap();
            ui.close();
        }
        if what == LaneValue::Chance {
            let selected = state.piano.selected.clone();
            if ui.button("Play All").clicked() {
                session.edit_notes(s.r, "Group Notes", |n| {
                    edit::set_group_kind(n, &selected, false)
                });
                ui.close();
            }
            if ui.button("Play One").clicked() {
                session.edit_notes(s.r, "Group Notes", |n| {
                    edit::set_group_kind(n, &selected, true)
                });
                ui.close();
            }
            if ui.button("Ungroup Notes").clicked() {
                session.edit_notes(s.r, "Ungroup Notes", |n| edit::ungroup(n, &selected));
                ui.close();
            }
        }
    });
    if resp.drag_started() {
        if let Some(pos) = resp.interact_pointer_pos() {
            state.piano.lanes.focus = Some(what);
            let notes = s.clip.notes().to_vec();
            let ignore_grid = keymap::ignores_grid(mods, Platform::current());
            let cmd = mods.command;
            let hits = markers_at(geo, s, what, pos.x, &state.piano.selected);
            let mode = if draw_mode {
                if mods.alt {
                    let t = s.span.content_at(geo.tick(pos.x));
                    LaneMode::Line {
                        from: (t, value_at(pos, inner)),
                    }
                } else if ignore_grid || state.piano.snap_off {
                    LaneMode::Individual
                } else {
                    LaneMode::Grid
                }
            } else if cmd && what == LaneValue::Velocity {
                LaneMode::Deviation
            } else {
                LaneMode::Value
            };
            if !hits.is_empty() || draw_mode {
                let picks = if draw_mode {
                    state.piano.selected.clone()
                } else {
                    hits
                };
                lane_gesture(
                    state,
                    LaneDrag {
                        what,
                        base: notes,
                        picks,
                        origin: pos,
                        mode,
                        height: inner.height(),
                    },
                );
            }
        }
    }
    if resp.dragged() {
        if let (Some(pos), Some(d)) = (resp.interact_pointer_pos(), take_lane(state)) {
            let mut notes = d.base.clone();
            match d.mode {
                LaneMode::Value => {
                    let dv = -(pos.y - d.origin.y) / d.height;
                    edit::change_value(&mut notes, &d.picks, d.what, dv);
                }
                LaneMode::Deviation => {
                    let dv = -(pos.y - d.origin.y) / d.height;
                    edit::change_deviation(&mut notes, &d.picks, dv);
                }
                LaneMode::Grid | LaneMode::Individual => {
                    let grid = (d.mode == LaneMode::Grid).then_some(state.piano.grid);
                    let tick = s.span.content_at(geo.tick(pos.x));
                    let chosen = drawn_notes(&notes, &d.picks, tick, grid);
                    let v = value_at(pos, inner);
                    if !chosen.is_empty() {
                        edit::set_value(&mut notes, &chosen, d.what, v);
                    }
                }
                LaneMode::Line { from } => {
                    let tick = s.span.content_at(geo.tick(pos.x));
                    let (t0, v0) = from;
                    let v1 = if mods.shift { v0 } else { value_at(pos, inner) };
                    line_values(&mut notes, &d.picks, d.what, (t0, v0), (tick, v1));
                }
            }
            if let Some(n) = session.clip_at_mut(s.r).and_then(|c| c.notes_mut()) {
                *n = notes;
            }
            lane_gesture(state, d);
        }
    }
    if resp.drag_stopped() {
        if let Some(d) = take_lane(state) {
            session.history.pending = Some(match d.what {
                LaneValue::Chance => "Change Chance",
                LaneValue::Release => "Change Release Velocity",
                LaneValue::Velocity => "Change Velocity",
            });
            session.sync_ref(s.r);
        }
    }
    info(
        resp,
        "Lane — drag a marker up or down; Cmd-drag a velocity marker for its deviation; \
         double-click to reset it; type a value and press Enter",
    );
}

/// The row of lane toggles, and the controls for the lane in view.
pub(super) fn bar(
    ui: &mut egui::Ui,
    rect: Rect,
    session: &mut Session,
    state: &mut UiState,
    _fg: usize,
    shown: &[Shown],
) {
    ui.painter().rect_filled(rect, 0.0, shade(0x303030));
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(rect.shrink2(Vec2::new(3.0, 1.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    child.spacing_mut().item_spacing = Vec2::new(4.0, 2.0);
    let hide = state.piano.lanes.hide_all;
    let r = state_button(&mut child, Vec2::new(16.0, 15.0), !hide, pal().accent, "v");
    if info(r, "Show/Hide All Expression Editors").clicked() {
        state.piano.lanes.hide_all = !hide;
    }
    child.menu_button("Lanes", |ui| {
        for (i, label) in LABELS.iter().enumerate() {
            let on = state.piano.lanes.shown[i];
            if ui.selectable_label(on, *label).clicked() {
                state.piano.lanes.shown[i] = !on;
            }
        }
    });
    let selected = state.piano.selected.clone();
    let fg_clip = shown
        .iter()
        .find(|s| Some(s.r) == state.selected_clip)
        .map(|s| s.r);
    let Some(r) = fg_clip else {
        return;
    };
    child.separator();
    if state.piano.lanes.shown[0] {
        velocity_controls(&mut child, session, state, r, &selected);
    }
    if state.piano.lanes.shown[2] {
        child.separator();
        chance_controls(&mut child, session, state, r, &selected);
    }
    let status = shown
        .iter()
        .find(|s| s.r == r)
        .and_then(|s| group_status(s.clip.notes(), &selected));
    if let Some(text) = status {
        state.status = text;
    }
}

fn velocity_controls(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    r: crate::session::ClipRef,
    selected: &[usize],
) {
    let lanes = &mut state.piano.lanes;
    let mut amount = lanes.random * 127.0;
    let change = ui
        .add(
            egui::DragValue::new(&mut amount)
                .range(0.0..=127.0)
                .speed(1.0)
                .max_decimals(0),
        )
        .changed();
    if change {
        lanes.random = amount / 127.0;
    }
    if info(ui.small_button("Randomize"), "Randomize Velocity").clicked() {
        let amount = lanes.random;
        let mut rng = session.tool_random();
        session.edit_notes(r, "Randomize Velocity", |n| {
            edit::randomize_velocity(n, selected, amount, &mut rng)
        });
    }
    let lanes = &mut state.piano.lanes;
    let mut ramp = (lanes.ramp.0 * 127.0, lanes.ramp.1 * 127.0);
    ui.label(RichText::new("Ramp").color(pal().text_dim));
    let a = ui.add(
        egui::DragValue::new(&mut ramp.0)
            .range(1.0..=127.0)
            .speed(1.0)
            .max_decimals(0),
    );
    let b = ui.add(
        egui::DragValue::new(&mut ramp.1)
            .range(1.0..=127.0)
            .speed(1.0)
            .max_decimals(0),
    );
    if a.changed() || b.changed() {
        lanes.ramp = (ramp.0 / 127.0, ramp.1 / 127.0);
        let (s, e) = lanes.ramp;
        session.edit_notes(r, "Velocity Ramp", |n| {
            edit::ramp_velocity(n, selected, s, e)
        });
    }
    let lanes = &mut state.piano.lanes;
    ui.label(RichText::new("Deviation").color(pal().text_dim));
    let mut dev = lanes.deviation * 127.0;
    let resp = ui.add(
        egui::DragValue::new(&mut dev)
            .range(-127.0..=127.0)
            .speed(1.0)
            .max_decimals(0),
    );
    if resp.changed() {
        lanes.deviation = dev / 127.0;
        let range = lanes.deviation;
        session.edit_notes(r, "Velocity Deviation", |n| {
            edit::set_deviation(n, selected, range)
        });
    }
}

fn chance_controls(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    r: crate::session::ClipRef,
    selected: &[usize],
) {
    let lanes = &mut state.piano.lanes;
    let mut amount = lanes.chance_random * 100.0;
    let changed = ui
        .add(
            egui::DragValue::new(&mut amount)
                .range(0.0..=100.0)
                .speed(1.0)
                .max_decimals(0),
        )
        .changed();
    if changed {
        lanes.chance_random = amount / 100.0;
    }
    if info(ui.small_button("Randomize"), "Randomize Chance").clicked() {
        let amount = lanes.chance_random;
        let mut rng = session.tool_random();
        session.edit_notes(r, "Randomize Chance", |n| {
            edit::randomize_chance(n, selected, amount, &mut rng)
        });
    }
    let on = state.piano.last_group_one;
    let mut chosen = None;
    if state_button(ui, Vec2::new(46.0, 15.0), !on, pal().accent, "Play All").clicked() {
        chosen = Some(false);
    }
    if state_button(ui, Vec2::new(46.0, 15.0), on, pal().accent, "Play One").clicked() {
        chosen = Some(true);
    }
    if let Some(one) = chosen {
        state.piano.last_group_one = one;
        session.edit_notes(r, "Group Notes", |n| {
            let already = selected
                .iter()
                .any(|i| n.get(*i).is_some_and(|x| x.group.is_some()));
            if already {
                edit::set_group_kind(n, selected, one);
            } else {
                edit::group_notes(n, selected, one);
            }
        });
    }
    if ui.small_button("Ungroup").clicked() {
        session.edit_notes(r, "Ungroup Notes", |n| edit::ungroup(n, selected));
    }
}

/// The Quantize settings, as Cmd+Shift+U opens them.
pub(super) fn quantize_window(ui: &mut egui::Ui, session: &mut Session, state: &mut UiState) {
    let mut open = state.piano.show_quantize;
    egui::Window::new("Quantize")
        .open(&mut open)
        .resizable(false)
        .collapsible(false)
        .show(ui.ctx(), |ui| {
            let q = &mut state.piano.quantize;
            egui::ComboBox::from_label("Grid")
                .selected_text(super::grid_label(q.grid))
                .show_ui(ui, |ui| {
                    for g in [
                        rlive_core::PPQ,
                        rlive_core::PPQ / 2,
                        rlive_core::PPQ / 4,
                        rlive_core::PPQ / 8,
                        rlive_core::PPQ / 3,
                        rlive_core::PPQ / 6,
                    ] {
                        ui.selectable_value(&mut q.grid, g, super::grid_label(g));
                    }
                });
            ui.checkbox(&mut q.starts, "Quantize starts");
            ui.checkbox(&mut q.ends, "Quantize ends");
            ui.add(egui::Slider::new(&mut q.amount, 0.0..=1.0).text("Amount"));
            if ui.button("Quantize").clicked() {
                let q = state.piano.quantize;
                super::edit_selected(session, state, "Quantize", |notes, sel| {
                    edit::quantize(notes, sel, q);
                });
            }
        });
    state.piano.show_quantize = open;
    let _ = Color32::TRANSPARENT;
}
