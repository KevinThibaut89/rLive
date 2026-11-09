//! Take lanes in the Arrangement: their rows, clips, headers, and the comp gestures on them.
//!
//! Editing goes through [`Session`]; this draws the lanes and turns gestures into those calls.

use super::{fades, grid, paint_clip, Hit, Rows, TimeAxis, CLIP_TITLE_H, FOLDED_H};
use crate::session::{ChainRef, Session};
use crate::ui::keymap::Platform;
use crate::ui::session_view;
use crate::ui::theme::{self, pal, shade};
use crate::ui::widgets::{self, info, rgb};
use crate::ui::UiState;
use egui::{Align2, Color32, CursorIcon, Layout, Pos2, Rect, Sense, Stroke, Vec2};
use rlive_core::TrackId;
use rlive_project::arrangement_edit::effective_fades;

/// A take lane's height until it is resized.
pub const TAKE_H: f32 = 34.0;
const MAX_TAKE_H: f32 = 240.0;
/// How close to a comp's split a pointer has to be to grab it, in points.
const SPLIT_GRAB: f32 = 4.0;

/// Dragging a take lane header, to reorder it.
#[derive(Clone, Copy)]
struct TakeDrag {
    track: TrackId,
    lane: usize,
}

/// The take lane heights of a row, or none when they aren't showing.
pub(super) fn row_lanes(session: &Session, chain: ChainRef) -> Vec<f32> {
    let ChainRef::Track(id) = chain else {
        return Vec::new();
    };
    let Some(t) = session.project.track(id) else {
        return Vec::new();
    };
    if !t.ui.show_take_lanes || t.ui.folded || !t.can_have_take_lanes() {
        return Vec::new();
    }
    t.take_lanes
        .iter()
        .enumerate()
        .map(|(i, _)| {
            t.ui.take_lane_heights
                .get(i)
                .copied()
                .filter(|h| *h >= FOLDED_H)
                .unwrap_or(TAKE_H)
        })
        .collect()
}

/// Sets a take lane's height by `dh`. Height is view state, so undo leaves it.
pub fn resize_lane(session: &mut Session, track: TrackId, n: usize, dh: f32) {
    session.edit_track_ui(ChainRef::Track(track), |ui| {
        if ui.take_lane_heights.len() <= n {
            ui.take_lane_heights.resize(n + 1, TAKE_H);
        }
        let h = &mut ui.take_lane_heights[n];
        *h = (*h + dh).clamp(FOLDED_H, MAX_TAKE_H);
    });
}

/// A washed-out version of a colour, for take material outside any comp.
fn desaturated(c: Color32) -> Color32 {
    let grey = (c.r() as u16 + c.g() as u16 + c.b() as u16) / 3;
    let mix = |v: u8| ((v as u16 + grey * 2) / 3) as u8;
    theme::darken(Color32::from_rgb(mix(c.r()), mix(c.g()), mix(c.b())), 0.2)
}

/// Draws the take lanes and their clips, adding each clip to `hits` so it can be clicked.
#[allow(clippy::too_many_arguments)]
pub(super) fn paint_lanes(
    ui: &egui::Ui,
    session: &Session,
    state: &UiState,
    lanes: Rect,
    axis: &TimeAxis,
    rows: &Rows,
    hits: &mut Vec<Hit>,
) {
    let p = ui.painter_at(lanes);
    let sy = state.arr.scroll.y;
    let edge = session
        .edge_fades
        .then(|| crate::session::edge_fade_ticks(session.project.tempo));
    for (i, row) in rows.rows.iter().enumerate() {
        let Some(index) = row.index else {
            continue;
        };
        if row.takes.is_empty() {
            continue;
        }
        let t = &session.project.tracks[index];
        let sources = t.comp_sources();
        for n in 0..row.takes.len() {
            let lane = rows.take(lanes, i, n, sy);
            if lane.bottom() < lanes.top() || lane.top() > lanes.bottom() {
                continue;
            }
            let selected = state.arr.take_header == Some((t.id, n));
            let fill = if selected {
                shade(0x303030)
            } else {
                shade(0x212121)
            };
            p.rect_filled(lane, 0.0, fill);
            p.line_segment(
                [lane.left_bottom(), lane.right_bottom()],
                Stroke::new(1.0, pal().separator),
            );
            if t.auditioned_lane() == Some(n) {
                let strip = Rect::from_min_size(lane.min, Vec2::new(3.0, lane.height()));
                p.rect_filled(strip, 0.0, pal().accent);
            }
            let clips = &t.take_lanes[n].clips;
            let shown = effective_fades(clips, edge);
            for (c, f) in clips.iter().zip(&shown) {
                let (x0, x1) = (axis.x(c.start as f64), axis.x(c.end() as f64));
                if x1 < lanes.left() || x0 > lanes.right() {
                    continue;
                }
                let rect = Rect::from_min_max(
                    Pos2::new(x0, lane.top() + 1.0),
                    Pos2::new(x1.max(x0 + 2.0), lane.bottom() - 1.0),
                );
                let color = rgb(c.clip.color);
                paint_clip(
                    &p,
                    session,
                    c,
                    rect,
                    lanes,
                    axis,
                    state.arr.selection.contains(&(t.id, c.id)),
                    t.mute,
                    state.arr.wave_zoom,
                    Some(desaturated(color)),
                );
                // Where this material is in the comp, it has the track's colour back.
                for s in sources
                    .iter()
                    .filter(|s| s.lane == n && s.start < c.end() && s.end > c.start)
                {
                    let (a, b) = (
                        axis.x(s.start.max(c.start) as f64),
                        axis.x(s.end.min(c.end()) as f64),
                    );
                    let highlight = Rect::from_min_max(
                        Pos2::new(a, rect.top() + CLIP_TITLE_H),
                        Pos2::new(b, rect.bottom()),
                    );
                    p.rect_filled(highlight, 0.0, color.gamma_multiply(0.32));
                    p.line_segment(
                        [
                            Pos2::new(a, rect.top() + CLIP_TITLE_H),
                            Pos2::new(b, rect.top() + CLIP_TITLE_H),
                        ],
                        Stroke::new(1.5, color),
                    );
                }
                if c.is_audio() && rect.height() >= fades::MIN_HEIGHT {
                    fades::paint_ramps(&p, rect, c, f, axis, lanes);
                }
                hits.push(Hit {
                    track: t.id,
                    id: c.id,
                    rect,
                });
            }
        }
    }
}

/// The edge of each highlight that meets the next one can be dragged, moving the comp's split.
pub(super) fn split_handles(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    lanes: Rect,
    axis: &TimeAxis,
    grid: u64,
    rows: &Rows,
) {
    let sy = state.arr.scroll.y;
    let snapping = state
        .arr
        .grid
        .snapping(ui.input(|i| i.modifiers), Platform::current());
    let bars = session.project.bar_map();
    for (i, row) in rows.rows.iter().enumerate() {
        let (Some(index), false) = (row.index, row.takes.is_empty()) else {
            continue;
        };
        let t = &session.project.tracks[index];
        let track = t.id;
        let sources = t.comp_sources();
        // Each boundary between two parts, with the lane the left part came from.
        let splits: Vec<(usize, rlive_core::ArrClipId, rlive_core::ArrClipId, u64)> = t
            .arrangement
            .windows(2)
            .filter(|w| w[0].end() == w[1].start)
            .filter_map(|w| {
                let s = sources.iter().find(|s| s.main == w[0].id)?;
                sources.iter().find(|s| s.main == w[1].id)?;
                Some((s.lane, w[0].id, w[1].id, w[0].end()))
            })
            .collect();
        for (n, left, right, at) in splits {
            if n >= row.takes.len() {
                continue;
            }
            let lane = rows.take(lanes, i, n, sy);
            let x = axis.x(at as f64);
            let hot = Rect::from_min_max(
                Pos2::new(x - SPLIT_GRAB, lane.top()),
                Pos2::new(x + SPLIT_GRAB, lane.bottom()),
            );
            if !hot.intersects(lanes) {
                continue;
            }
            let resp = ui.interact(
                hot,
                ui.scope_id().with(("comp_split", track, left, right)),
                Sense::drag(),
            );
            if resp.hovered() || resp.dragged() {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
            }
            if resp.dragged() {
                if let Some(pos) = resp.interact_pointer_pos() {
                    let to = grid::snap_tick(axis.tick(pos.x), grid, snapping, &bars, &[], 0.0);
                    session.move_comp_split(track, left, right, to);
                }
            }
            info(
                resp,
                "Comp split — drag to move the boundary between two parts of the comp",
            );
        }
    }
}

/// In Draw Mode, a click on a take lane fills a time selection on the main lane with that take.
pub(super) fn fill_main_selection(
    session: &mut Session,
    state: &UiState,
    track: TrackId,
    n: usize,
) -> bool {
    let Some(t) = state.arr.time.as_ref().filter(|t| !t.is_point()) else {
        return false;
    };
    let on_main = t
        .lanes
        .iter()
        .any(|k| k.chain == ChainRef::Track(track) && k.take.is_none());
    if !on_main {
        return false;
    }
    let (a, b) = t.ticks();
    session.comp_selection(&[(track, n)], a, b)
}

/// Copies the take lanes a time selection covers to their main lanes.
pub(super) fn comp_time_selection(session: &mut Session, state: &UiState) {
    let Some(t) = state.arr.time.as_ref().filter(|t| !t.is_point()) else {
        return;
    };
    let lanes: Vec<(TrackId, usize)> = t
        .lanes
        .iter()
        .filter_map(|k| match (k.chain, k.take) {
            (ChainRef::Track(id), Some(n)) => Some((id, n)),
            _ => None,
        })
        .collect();
    if lanes.is_empty() {
        return;
    }
    let (a, b) = t.ticks();
    session.comp_selection(&lanes, a, b);
}

/// The speaker on the Audition button.
fn speaker(p: &egui::Painter, r: Rect, color: Color32) {
    let c = r.center();
    let h = r.height() * 0.22;
    let body = Rect::from_center_size(Pos2::new(c.x - 2.5, c.y), Vec2::new(4.0, h * 2.0));
    p.rect_filled(body, 0.0, color);
    p.add(egui::Shape::convex_polygon(
        vec![
            Pos2::new(c.x - 0.5, c.y - h),
            Pos2::new(c.x + 3.0, c.y - h * 2.0),
            Pos2::new(c.x + 3.0, c.y + h * 2.0),
            Pos2::new(c.x - 0.5, c.y + h),
        ],
        color,
        Stroke::NONE,
    ));
}

/// The headers of the take lanes, to the right of the lanes.
pub(super) fn headers(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    headers: Rect,
    rows: &Rows,
) {
    for (i, row) in rows.rows.iter().enumerate() {
        let ChainRef::Track(id) = row.chain else {
            continue;
        };
        for n in 0..row.takes.len() {
            let rect = rows.take(headers, i, n, state.arr.scroll.y);
            if rect.bottom() < headers.top() || rect.top() > headers.bottom() {
                continue;
            }
            lane_header(ui, session, state, headers, rect, id, n);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn lane_header(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    headers: Rect,
    rect: Rect,
    id: TrackId,
    n: usize,
) {
    let Some(t) = session.project.track(id) else {
        return;
    };
    let Some(lane) = t.take_lanes.get(n) else {
        return;
    };
    let (name, color, on) = (lane.name.clone(), t.color, t.auditioned_lane() == Some(n));
    let mut cu = session_view::child(
        ui,
        headers,
        rect,
        Layout::left_to_right(egui::Align::Center),
    );
    let resp = cu.allocate_rect(rect, Sense::click_and_drag());
    let p = cu.painter().clone();
    let selected = state.arr.take_header == Some((id, n));
    p.rect_filled(
        rect,
        0.0,
        if selected {
            shade(0x3b3b3b)
        } else {
            shade(0x2a2a2a)
        },
    );
    p.line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        Stroke::new(1.0, pal().separator),
    );
    p.rect_filled(
        Rect::from_min_size(rect.min, Vec2::new(6.0, rect.height())),
        0.0,
        desaturated(rgb(color)),
    );
    let button = Vec2::new(22.0, 16.0);
    let button_rect = Rect::from_min_size(
        Pos2::new(
            rect.right() - button.x - 6.0,
            rect.center().y - button.y / 2.0,
        ),
        button,
    );
    let name_rect = Rect::from_min_max(
        rect.min + Vec2::new(12.0, 2.0),
        Pos2::new(button_rect.left() - 4.0, rect.bottom() - 2.0),
    );
    let renaming = matches!(&state.arr.renaming_take, Some((t, l, _)) if *t == id && *l == n);
    if renaming {
        rename_field(&mut cu, session, state, name_rect);
    } else {
        p.with_clip_rect(name_rect.intersect(headers)).text(
            name_rect.left_center(),
            Align2::LEFT_CENTER,
            &name,
            theme::font(10.5),
            pal().text,
        );
    }
    let mut bu = session_view::child(
        &mut cu,
        headers,
        button_rect,
        Layout::left_to_right(egui::Align::Center),
    );
    let r = widgets::icon_button(&mut bu, button, on, pal().accent, speaker);
    if info(
        r,
        "Audition Take Lane — hear this lane instead of the main lane (T)",
    )
    .clicked()
    {
        session.toggle_audition(id, n);
    }
    if resp.clicked() {
        select_lane(state, id, n);
    }
    if resp.double_clicked() {
        state.arr.renaming_take = Some((id, n, name.clone()));
    }
    resp.dnd_set_drag_payload(TakeDrag { track: id, lane: n });
    if let Some(from) = widgets::released::<TakeDrag>(&resp) {
        if from.track == id && from.lane != n {
            session.move_take_lane(id, from.lane, n);
            state.arr.take_header = Some((id, n));
        }
    }
    let resp = info(
        resp,
        format!("Take lane '{name}' — click to select, double-click to rename, drag to reorder"),
    );
    resp.context_menu(|ui| lane_menu(ui, session, state, id, n, &name));

    let split = Rect::from_min_max(
        Pos2::new(rect.left(), rect.bottom() - 3.0),
        Pos2::new(rect.right(), rect.bottom() + 1.0),
    );
    let drag = cu.interact(
        split,
        cu.scope_id().with(("take_split", id, n)),
        Sense::drag(),
    );
    if drag.hovered() || drag.dragged() {
        cu.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
    }
    if drag.dragged() {
        resize_lane(session, id, n, drag.drag_delta().y);
    }
}

fn select_lane(state: &mut UiState, id: TrackId, n: usize) {
    state.arr.take_header = Some((id, n));
    state.arr.header_selected = false;
    state.arr.selection.clear();
    state.arr.time = None;
    state.selected = Some(ChainRef::Track(id));
    state.selected_tracks = vec![id];
}

/// A take lane's name, being typed. Tab moves on to the next lane, Shift-Tab back.
fn rename_field(ui: &mut egui::Ui, session: &mut Session, state: &mut UiState, rect: Rect) {
    let Some((id, n, mut text)) = state.arr.renaming_take.take() else {
        return;
    };
    let tab = ui.input_mut(|i| {
        if i.consume_key(egui::Modifiers::NONE, egui::Key::Tab) {
            Some(1i32)
        } else if i.consume_key(egui::Modifiers::SHIFT, egui::Key::Tab) {
            Some(-1)
        } else {
            None
        }
    });
    let edit = egui::TextEdit::singleline(&mut text).font(theme::font(10.5));
    let r = ui.put(rect, edit);
    r.request_focus();
    let done = r.lost_focus();
    if let Some(step) = tab {
        session.rename_take_lane(id, n, text);
        let next = n as i32 + step;
        let len = session.project.track(id).map_or(0, |t| t.take_lanes.len()) as i32;
        if next >= 0 && next < len {
            let name = session
                .project
                .track(id)
                .and_then(|t| t.take_lanes.get(next as usize))
                .map_or_else(String::new, |l| l.name.clone());
            state.arr.renaming_take = Some((id, next as usize, name));
            state.arr.take_header = Some((id, next as usize));
        }
        return;
    }
    if done {
        session.rename_take_lane(id, n, text);
    } else {
        state.arr.renaming_take = Some((id, n, text));
    }
}

fn lane_menu(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    id: TrackId,
    n: usize,
    name: &str,
) {
    if ui.button("Copy Selection to Main Lane   Enter").clicked() {
        comp_time_selection(session, state);
        ui.close();
    }
    if ui.button("Rename   ⌘R").clicked() {
        state.arr.take_header = Some((id, n));
        state.arr.renaming_take = Some((id, n, name.to_string()));
        ui.close();
    }
    if ui.button("Duplicate   ⌘D").clicked() {
        if let Some(i) = session.duplicate_take_lane(id, n) {
            select_lane(state, id, i);
        }
        ui.close();
    }
    if ui.button("Delete").clicked() {
        session.delete_take_lane(id, n);
        state.arr.take_header = None;
        ui.close();
    }
    ui.separator();
    insert_and_clear_items(ui, session, state, id, Some(n));
}

fn insert_and_clear_items(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    id: TrackId,
    after: Option<usize>,
) {
    if ui.button("Insert Take Lane   ⇧⌥T").clicked() {
        state.arr.auto.mode = false;
        session.insert_take_lane(&[id], after);
        ui.close();
    }
    let has_lanes = session
        .project
        .track(id)
        .is_some_and(|t| !t.take_lanes.is_empty());
    if ui
        .add_enabled(has_lanes, egui::Button::new("Delete All Take Lanes"))
        .clicked()
    {
        session.delete_all_take_lanes(id);
        state.arr.take_header = None;
        ui.close();
    }
    if ui
        .add_enabled(has_lanes, egui::Button::new("Delete All Unused Take Lanes"))
        .clicked()
    {
        session.delete_unused_take_lanes(id);
        state.arr.take_header = None;
        ui.close();
    }
}

/// The take lane and link commands in a track's context menu.
pub fn track_menu_items(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    id: TrackId,
    selection: &[TrackId],
) {
    let Some(t) = session.project.track(id) else {
        return;
    };
    let (group, show) = (t.is_group(), t.ui.show_take_lanes);
    if t.can_have_take_lanes() {
        let label = if show {
            "Hide Take Lanes   ⌘⌥U"
        } else {
            "Show Take Lanes   ⌘⌥U"
        };
        if ui.button(label).clicked() {
            state.arr.auto.mode = false;
            session.set_show_take_lanes(id, !show);
            ui.close();
        }
        insert_and_clear_items(ui, session, state, id, None);
    }
    let linked = session.project.linked_group(id).is_some();
    if group {
        if ui.button("Link Tracks").clicked() {
            session.link_group_tracks(id);
            ui.close();
        }
    } else if selection.len() >= 2 || linked {
        let others: Vec<TrackId> = selection.iter().copied().filter(|t| *t != id).collect();
        if ui.button("Link Tracks").clicked() {
            // Held Cmd adds the selection to the instance of the track clicked.
            if ui.input(|i| i.modifiers.command) && linked {
                session.add_tracks_to_link(&others, id);
            } else {
                session.link_tracks(selection);
            }
            ui.close();
        }
    }
    let any_linked = selection
        .iter()
        .any(|t| session.project.linked_group(*t).is_some());
    if any_linked && ui.button("Unlink Track(s)").clicked() {
        session.unlink_tracks(selection);
        ui.close();
    }
}

/// The buttons a track header gets for take lanes and links.
pub(super) fn header_buttons(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    id: TrackId,
) {
    let Some(t) = session.project.track(id) else {
        return;
    };
    if !t.can_have_take_lanes() {
        return;
    }
    let show = t.ui.show_take_lanes;
    let size = Vec2::new(16.0, 16.0);
    let r = widgets::state_button(ui, size, show, pal().accent, "T");
    if info(r, "Show/Hide Take Lanes (Cmd+Alt+U)").clicked() {
        state.arr.auto.mode = false;
        session.set_show_take_lanes(id, !show);
        if !show {
            // Nothing to see yet: asking for lanes on a track with none shows the first.
            let empty = session
                .project
                .track(id)
                .is_some_and(|t| t.take_lanes.is_empty());
            if empty {
                session.insert_take_lane(&[id], None);
            }
        }
    }
    let Some(group) = session.project.linked_group(id).cloned() else {
        return;
    };
    let r = widgets::state_button(ui, size, true, pal().accent, "L");
    if r.hovered() {
        state.arr.linked_hover = group.clone();
    }
    let r = info(
        r,
        "Linked tracks — edits apply to these tracks together. Click to select them",
    );
    if r.clicked() {
        state.selected_tracks = group;
        state.selected = Some(ChainRef::Track(id));
    }
}
