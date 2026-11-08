//! Everything above the lanes: the overview, the beat-time ruler, the loop brace, the scrub area
//! with its locators, and the time signature markers. Below them is the minutes-seconds ruler.

use super::grid;
use super::{LoopDrag, EDGE_W, LOOP_H};
use crate::session::{InfoTarget, Session};
use crate::ui::info_view::edit_info_item;
use crate::ui::keymap::Platform;
use crate::ui::theme::{self, *};
use crate::ui::timeline::{self, TimeAxis};
use crate::ui::widgets::{info, info_note};
use crate::ui::UiState;
use egui::{Align2, CursorIcon, Pos2, Rect, Sense, Stroke, Vec2};
use rlive_core::PPQ;

/// Height of the row of bar numbers, which drags to zoom.
pub const BEAT_H: f32 = 14.0;
/// Height of the scrub area, where the locators are.
pub const SCRUB_H: f32 = 15.0;
/// Height of the signature marker strip, shown only when there are markers.
pub const SIGNATURE_H: f32 = 14.0;
/// How long the scrub area is held down before it repeats instead of launching.
const HOLD_SECONDS: f64 = 0.25;

/// Stops the fragmentary bar hatch being drawn thicker than a pixel.
const HATCH_STEP: f32 = 5.0;

/// Drawing and interaction for the rows above the lanes.
#[allow(clippy::too_many_arguments)]
pub(super) fn show(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    ruler: Rect,
    axis: &TimeAxis,
    grid: u64,
    bar: u64,
) {
    let p = ui.painter_at(ruler);
    p.rect_filled(ruler, 0.0, shade(0x333333));
    let loop_row = Rect::from_min_max(ruler.min, Pos2::new(ruler.right(), ruler.top() + LOOP_H));
    let beat_row = Rect::from_min_size(
        Pos2::new(ruler.left(), loop_row.bottom()),
        Vec2::new(ruler.width(), BEAT_H),
    );
    let scrub_row = Rect::from_min_size(
        Pos2::new(ruler.left(), beat_row.bottom()),
        Vec2::new(ruler.width(), SCRUB_H),
    );
    let marker_row = Rect::from_min_max(Pos2::new(ruler.left(), scrub_row.bottom()), ruler.max);
    bar_numbers(ui, session, beat_row, axis, bar);
    beat_row_input(ui, session, state, beat_row, axis);
    loop_brace(ui, session, state, loop_row, axis, grid);
    scrub_area(ui, session, state, scrub_row, axis);
    paint_fragments(ui, session, ruler, scrub_row, axis);
    locators(ui, session, state, scrub_row, axis, grid);
    if !session.project.arrangement.signatures.is_empty() {
        signature_markers(ui, session, state, marker_row, axis, grid);
    }
    let marker = axis.x(session.insert_marker());
    let tri = vec![
        Pos2::new(marker - 4.0, ruler.bottom() - 7.0),
        Pos2::new(marker + 4.0, ruler.bottom() - 7.0),
        Pos2::new(marker, ruler.bottom()),
    ];
    p.add(egui::Shape::convex_polygon(
        tri,
        shade(0xe0e0e0),
        Stroke::NONE,
    ));
    p.line_segment(
        [ruler.left_bottom(), ruler.right_bottom()],
        Stroke::new(1.0, pal().separator),
    );
}

fn bar_numbers(ui: &egui::Ui, session: &Session, beat_row: Rect, axis: &TimeAxis, bar: u64) {
    let p = ui.painter_at(beat_row);
    let bars = session.project.bar_map();
    let step = timeline::ruler_bar_step(axis.px_per_beat, bar).max(1);
    let mut t = axis.tick(beat_row.left()).max(0.0);
    let mut guard = 0;
    while axis.x(t) <= beat_row.right() && guard < 4096 {
        guard += 1;
        let b = bars.bar_at(t);
        let x = axis.x(b.start);
        if b.start >= axis.tick(beat_row.left()) - 1.0 && b.index.is_multiple_of(step) {
            p.line_segment(
                [
                    Pos2::new(x, beat_row.top() + 2.0),
                    Pos2::new(x, beat_row.bottom()),
                ],
                Stroke::new(1.0, shade(0x6a6a6a)),
            );
            p.text(
                Pos2::new(x + 3.0, beat_row.bottom()),
                Align2::LEFT_BOTTOM,
                (b.index + 1).to_string(),
                theme::font(10.5),
                pal().text_dim,
            );
        }
        t = if b.end > t { b.end } else { t + 1.0 };
    }
}

/// Dragging the beat-time ruler scrolls or zooms; double-clicking frames the selection.
fn beat_row_input(
    ui: &mut egui::Ui,
    session: &Session,
    state: &mut UiState,
    beat_row: Rect,
    axis: &TimeAxis,
) {
    let resp = ui.interact(
        beat_row,
        ui.scope_id().with("arr_beat_ruler"),
        Sense::click_and_drag(),
    );
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
    }
    if resp.double_clicked() {
        state.arr.zoom_to_selection(session, beat_row.width());
    } else if resp.clicked() {
        state.arr.follow.pause();
    }
    if resp.dragged() {
        let d = resp.drag_delta();
        let a = &mut state.arr;
        if let Some(pos) = resp.interact_pointer_pos() {
            let new = (a.px_per_beat * (1.0 - d.y * 0.01)).clamp(1.0, 240.0);
            a.scroll.x = axis.scroll_after_zoom(pos.x, new) - d.x;
            a.px_per_beat = new;
        }
        a.follow.pause();
    }
    info(
        resp,
        "Beat-time ruler — drag left or right to scroll, up or down to zoom, double-click to zoom to the selection",
    );
}

fn loop_brace(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    top: Rect,
    axis: &TimeAxis,
    grid: u64,
) {
    let p = ui.painter_at(top);
    let a = session.project.arrangement.clone();
    let (x0, x1) = (axis.x(a.loop_start as f64), axis.x(a.loop_end as f64));
    let brace = Rect::from_min_max(
        Pos2::new(x0, top.top() + 1.0),
        Pos2::new(x1, top.top() + LOOP_H),
    );
    let color = match (state.arr.loop_selected, a.loop_enabled) {
        (true, _) => pal().select,
        (false, true) => shade(0xb0b0b0),
        (false, false) => shade(0x5a5a5a),
    };
    p.rect_filled(brace, 0.0, color);
    let loop_resp = ui.interact(top, ui.scope_id().with("arr_loop"), Sense::click_and_drag());
    let hover_kind = |x: f32| {
        if (x - x0).abs() < EDGE_W {
            Some(LoopDrag::Start)
        } else if (x - x1).abs() < EDGE_W {
            Some(LoopDrag::End)
        } else if x > x0 && x < x1 {
            Some(LoopDrag::Whole)
        } else {
            None
        }
    };
    if let Some(pos) = loop_resp.hover_pos() {
        if matches!(hover_kind(pos.x), Some(LoopDrag::Start | LoopDrag::End)) {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        }
    }
    if loop_resp.clicked() {
        if let Some(pos) = loop_resp.interact_pointer_pos() {
            if hover_kind(pos.x).is_some() {
                state.arr.select_loop(session);
            }
        }
    }
    if loop_resp.drag_started() {
        if let Some(pos) = loop_resp.interact_pointer_pos() {
            state.arr.loop_drag = hover_kind(pos.x).map(|k| (k, pos.x, a.loop_start, a.loop_end));
            state.arr.loop_selected = state.arr.loop_drag.is_some();
        }
    }
    if let (Some((kind, origin, s0, e0)), Some(pos)) =
        (state.arr.loop_drag, loop_resp.interact_pointer_pos())
    {
        let m = ui.input(|i| i.modifiers);
        let snapping = state.arr.grid.snapping(m, Platform::current());
        let bars = session.project.bar_map();
        let dt = ((pos.x - origin) / axis.px_per_beat) as f64 * PPQ as f64;
        let snapped = |v: u64| grid::snap_tick(v as f64 + dt, grid, snapping, &bars, &[], 0.0);
        let (s, e) = match kind {
            LoopDrag::Whole => {
                let s = snapped(s0);
                (s, s + (e0 - s0))
            }
            LoopDrag::Start => (snapped(s0).min(e0.saturating_sub(grid.max(1))), e0),
            LoopDrag::End => (s0, snapped(e0).max(s0 + grid.max(1))),
        };
        session.set_loop(a.loop_enabled, s, e);
    }
    if loop_resp.drag_stopped() {
        state.arr.loop_drag = None;
    }
    let resp = info(
        loop_resp,
        "Loop brace — click selects what's in the loop, drag to move, drag an end to resize, right-click for options",
    );
    resp.context_menu(|ui| {
        let on = a.song_start == Some(a.loop_start);
        if ui
            .selectable_label(on, "Set Song Start Time Here")
            .clicked()
        {
            session.toggle_song_start(a.loop_start);
            ui.close();
        }
    });
}

/// The scrub area: click launches, hold repeats, and it holds the add-locator menu.
fn scrub_area(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    row: Rect,
    axis: &TimeAxis,
) {
    ui.painter_at(row).rect_filled(row, 0.0, shade(0x2c2c2c));
    let resp = ui.interact(
        row,
        ui.scope_id().with("arr_scrub"),
        Sense::click_and_drag(),
    );
    let now = ui.input(|i| i.time);
    let tick_at = |pos: Pos2| axis.tick(pos.x).max(0.0);
    if resp.is_pointer_button_down_on() {
        let pointer = resp.interact_pointer_pos();
        match state.arr.scrub_hold {
            None => {
                if let Some(pos) = pointer {
                    session.scrub(tick_at(pos), false);
                    state.arr.scrub_hold = Some((now, tick_at(pos), false));
                    state.arr.follow.resume();
                }
            }
            Some((since, tick, false)) if now - since >= HOLD_SECONDS => {
                session.scrub(tick, true);
                state.arr.scrub_hold = Some((since, tick, true));
            }
            _ => {}
        }
    } else if let Some((_, _, held)) = state.arr.scrub_hold.take() {
        if held {
            session.release_hold();
        }
    }
    info(
        resp.clone(),
        "Scrub area — click to play from here, hold to repeat a bar, right-click to add a locator or a time signature",
    );
    let at = session.insert_marker() as u64;
    resp.context_menu(|ui| {
        if ui.button("Add Locator").clicked() {
            let i = session.set_locator(at);
            state.arr.selected_locator = Some(i);
            ui.close();
        }
        if ui.button("Insert Time Signature Change").clicked() {
            let sig = session.project.time_signature;
            session.add_signature_marker(at, sig);
            ui.close();
        }
    });
}

/// Hatches the fragmentary bars across the top rows, where a click can correct them.
fn paint_fragments(
    ui: &mut egui::Ui,
    session: &mut Session,
    ruler: Rect,
    row: Rect,
    axis: &TimeAxis,
) {
    let fragments = session.fragments();
    if fragments.is_empty() {
        return;
    }
    let p = ui.painter_at(ruler);
    for f in fragments {
        let (x0, x1) = (axis.x(f.start as f64), axis.x(f.end as f64));
        if x1 < ruler.left() || x0 > ruler.right() {
            continue;
        }
        let r = Rect::from_min_max(Pos2::new(x0, row.top()), Pos2::new(x1, row.bottom()));
        let stroke = Stroke::new(1.0, pal().text_dim);
        let mut x = x0 - r.height();
        while x < x1 {
            p.with_clip_rect(r).line_segment(
                [Pos2::new(x, r.bottom()), Pos2::new(x + r.height(), r.top())],
                stroke,
            );
            x += HATCH_STEP;
        }
        let resp = ui.interact(
            r,
            ui.scope_id().with(("fragment", f.marker)),
            Sense::click(),
        );
        info(
            resp.clone(),
            "Fragmentary bar — a signature change cut this bar short. Right-click to correct it",
        );
        resp.context_menu(|ui| {
            if ui.button("Delete Fragmentary Bar Time").clicked() {
                session.delete_fragment(f);
                ui.close();
            }
            if ui.button("Complete Fragmentary Bar").clicked() {
                session.complete_fragment(f);
                ui.close();
            }
        });
    }
}

/// Locators, as flags in the scrub area.
fn locators(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    row: Rect,
    axis: &TimeAxis,
    grid: u64,
) {
    let count = session.project.arrangement.locators.len();
    for i in 0..count {
        let Some(locator) = session.project.arrangement.locators.get(i) else {
            break;
        };
        let x = axis.x(locator.tick as f64);
        let name = locator.name.clone();
        let tick = locator.tick;
        if x < row.left() - 80.0 || x > row.right() {
            continue;
        }
        let rect = Rect::from_min_size(Pos2::new(x, row.top()), Vec2::new(64.0, row.height()));
        let p = ui.painter_at(row);
        let selected = state.arr.selected_locator == Some(i);
        let color = if selected { pal().select } else { pal().accent };
        p.line_segment(
            [rect.left_top(), rect.left_bottom()],
            Stroke::new(1.0, color),
        );
        p.text(
            Pos2::new(x + 3.0, rect.top()),
            Align2::LEFT_TOP,
            &name,
            theme::font(10.0),
            color,
        );
        let resp = ui.interact(
            rect,
            ui.scope_id().with(("locator", i)),
            Sense::click_and_drag(),
        );
        let m = ui.input(|i| i.modifiers);
        if resp.double_clicked() && !session.is_playing() {
            state.arr.selected_locator = Some(i);
            session.seek(tick as f64);
            session.play();
        } else if resp.clicked() {
            state.arr.selected_locator = Some(i);
            state.arr.selected_signature = None;
            state.arr.selection.clear();
            state.arr.time = None;
            if session.is_playing() {
                session.trigger(tick as f64, false);
            }
        }
        if resp.dragged() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let snapping = state.arr.grid.snapping(m, Platform::current());
                let bars = session.project.bar_map();
                let to = grid::snap_tick(axis.tick(pos.x), grid, snapping, &bars, &[], 0.0);
                if to != tick {
                    state.arr.selected_locator = Some(session.move_locator(i, to));
                }
            }
        }
        let resp = info(
            resp,
            format!(
                "Locator '{name}' — click to jump, double-click to play from here, drag to move"
            ),
        );
        info_note(
            &resp,
            session
                .info_text(InfoTarget::Locator(i))
                .unwrap_or_default(),
        );
        let on = session.project.arrangement.song_start == Some(tick);
        resp.context_menu(|ui| {
            if ui.button("Rename").clicked() {
                state.arr.renaming_locator = Some((i, name.clone()));
                ui.close();
            }
            edit_info_item(ui, session, state, InfoTarget::Locator(i));
            if ui.button("Delete").clicked() {
                session.delete_locator(i);
                state.arr.selected_locator = None;
                ui.close();
            }
            ui.separator();
            if ui.button("Loop to Next Locator").clicked() {
                session.loop_to_next_locator(i);
                ui.close();
            }
            if ui
                .selectable_label(on, "Set Song Start Time Here")
                .clicked()
            {
                session.toggle_song_start(tick);
                ui.close();
            }
        });
    }
}

/// Time signature markers, dragged anywhere the grid allows. Shown only when there are some.
fn signature_markers(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    row: Rect,
    axis: &TimeAxis,
    grid: u64,
) {
    let p = ui.painter_at(row);
    p.rect_filled(row, 0.0, shade(0x2c2c2c));
    let count = session.project.arrangement.signatures.len();
    for i in 0..count {
        let Some(m) = session.project.arrangement.signatures.get(i).copied() else {
            break;
        };
        let x = axis.x(m.tick as f64);
        if x < row.left() - 30.0 || x > row.right() {
            continue;
        }
        let text = format!("{}/{}", m.signature.numerator, m.signature.denominator);
        let rect = Rect::from_min_size(Pos2::new(x, row.top()), Vec2::new(30.0, row.height()));
        let selected = state.arr.selected_signature == Some(i);
        let color = if selected { pal().select } else { pal().text };
        p.line_segment(
            [rect.left_top(), rect.left_bottom()],
            Stroke::new(1.0, color),
        );
        p.text(
            Pos2::new(x + 3.0, rect.top()),
            Align2::LEFT_TOP,
            &text,
            theme::font(10.0),
            color,
        );
        let resp = ui.interact(
            rect,
            ui.scope_id().with(("sigmark", i)),
            Sense::click_and_drag(),
        );
        if resp.clicked() {
            state.arr.selected_signature = Some(i);
            state.arr.selected_locator = None;
            state.arr.selection.clear();
            state.arr.time = None;
        }
        if resp.dragged() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let m_keys = ui.input(|i| i.modifiers);
                let snapping = state.arr.grid.snapping(m_keys, Platform::current());
                let bars = session.project.bar_map();
                let to = grid::snap_tick(axis.tick(pos.x), grid, snapping, &bars, &[], 0.0);
                if to != m.tick {
                    state.arr.selected_signature = Some(session.move_signature_marker(i, to));
                }
            }
        }
        info(
            resp.clone(),
            format!("Time signature {text} — drag to move, right-click to edit or delete"),
        );
        let sig_text = text.clone();
        resp.context_menu(|ui| {
            if ui.button("Edit Time Signature").clicked() {
                state.arr.editing_signature = Some((i, sig_text.clone()));
                ui.close();
            }
            if ui.button("Delete").clicked() {
                session.delete_signature_marker(i);
                state.arr.selected_signature = None;
                ui.close();
            }
        });
    }
}

/// The overview: the whole Arrangement in one strip, with the visible part outlined.
pub(super) fn overview(
    ui: &mut egui::Ui,
    session: &Session,
    state: &mut UiState,
    strip: Rect,
    lanes_width: f32,
    content_beats: f32,
) {
    let p = ui.painter_at(strip);
    p.rect_filled(strip, 0.0, shade(0x262626));
    let total = content_beats.max(1.0);
    let x_of = |beat: f32| strip.left() + beat / total * strip.width();
    let rows = session.project.tracks.len().max(1) as f32;
    let row_h = (strip.height() - 2.0) / rows;
    for (n, t) in session.project.tracks.iter().enumerate() {
        let y = strip.top() + 1.0 + n as f32 * row_h;
        for c in &t.arrangement {
            let (a, b) = (
                x_of(c.start as f32 / PPQ as f32),
                x_of(c.end() as f32 / PPQ as f32),
            );
            p.rect_filled(
                Rect::from_min_max(
                    Pos2::new(a, y),
                    Pos2::new(b.max(a + 1.0), y + row_h.max(1.0)),
                ),
                0.0,
                theme::darken(crate::ui::widgets::rgb(c.clip.color), 0.2),
            );
        }
    }
    let a = &mut state.arr;
    let beat_left = a.scroll.x / a.px_per_beat;
    let beat_right = beat_left + lanes_width / a.px_per_beat;
    let outline = Rect::from_min_max(
        Pos2::new(x_of(beat_left), strip.top()),
        Pos2::new(x_of(beat_right).min(strip.right()), strip.bottom()),
    );
    p.rect_stroke(
        outline,
        0.0,
        Stroke::new(1.0, pal().text),
        egui::StrokeKind::Inside,
    );
    let resp = ui.interact(
        strip,
        ui.scope_id().with("arr_overview"),
        Sense::click_and_drag(),
    );
    if resp.double_clicked() {
        let (px, scroll) = (lanes_width / total, 0.0);
        a.px_per_beat = px.clamp(1.0, 240.0);
        a.scroll.x = scroll;
    } else if resp.is_pointer_button_down_on() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let centre = (pos.x - strip.left()) / strip.width() * total;
            a.scroll.x = (centre - lanes_width / a.px_per_beat / 2.0).max(0.0) * a.px_per_beat;
        }
    }
    if resp.dragged() {
        let d = resp.drag_delta();
        a.px_per_beat = (a.px_per_beat * (1.0 - d.y * 0.01)).clamp(1.0, 240.0);
    }
    info(
        resp,
        "Overview — drag left or right to scroll, up or down to zoom, double-click to see everything",
    );
}

/// The minutes-seconds ruler, and the spacing of the grid.
pub(super) fn time_ruler(
    ui: &mut egui::Ui,
    session: &Session,
    state: &mut UiState,
    strip: Rect,
    axis: &TimeAxis,
    grid_text: &str,
) {
    let p = ui.painter_at(strip);
    p.rect_filled(strip, 0.0, shade(0x2c2c2c));
    let maps = crate::session::time_maps(&session.project);
    let seconds_at = |x: f32| maps.tempo.tick_to_seconds(axis.tick(x).max(0.0));
    let (t0, t1) = (seconds_at(strip.left()), seconds_at(strip.right()));
    let per_px = ((t1 - t0) / strip.width() as f64).max(1e-6);
    // The finest round step whose labels don't touch.
    let step = [
        0.1, 0.5, 1.0, 5.0, 10.0, 30.0, 60.0, 300.0, 600.0, 1800.0, 3600.0,
    ]
    .into_iter()
    .find(|s| s / per_px >= 64.0)
    .unwrap_or(3600.0);
    let mut t = (t0 / step).floor() * step;
    while t <= t1 {
        let x = axis.x(maps.tempo.seconds_to_tick(t));
        p.line_segment(
            [Pos2::new(x, strip.top()), Pos2::new(x, strip.top() + 4.0)],
            Stroke::new(1.0, shade(0x6a6a6a)),
        );
        p.text(
            Pos2::new(x + 3.0, strip.center().y),
            Align2::LEFT_CENTER,
            format_seconds(t),
            theme::font(9.5),
            pal().text_faint,
        );
        t += step;
    }
    p.text(
        Pos2::new(strip.right() - 4.0, strip.center().y),
        Align2::RIGHT_CENTER,
        grid_text,
        theme::font(9.5),
        pal().text_dim,
    );
    let resp = ui.interact(strip, ui.scope_id().with("arr_time_ruler"), Sense::drag());
    if resp.dragged() {
        state.arr.scroll.x -= resp.drag_delta().x;
        state.arr.follow.pause();
    }
    info(
        resp,
        "Time ruler — minutes, seconds and milliseconds; drag to scroll",
    );
}

/// Seconds as minutes:seconds:milliseconds.
pub(super) fn format_seconds(seconds: f64) -> String {
    let ms = (seconds.max(0.0) * 1000.0).round() as u64;
    format!("{}:{:02}:{:03}", ms / 60_000, ms / 1000 % 60, ms % 1000)
}

/// Accepts a locator rename and a signature edit from their little windows.
pub(super) fn editors(ctx: &egui::Context, session: &mut Session, state: &mut UiState) {
    if let Some((i, mut text)) = state.arr.renaming_locator.take() {
        let (keep, commit) = edit_window(ctx, "Rename Locator", &mut text);
        if commit {
            session.rename_locator(i, text.clone());
        }
        if keep {
            state.arr.renaming_locator = Some((i, text));
        }
    }
    if let Some((i, mut text)) = state.arr.editing_signature.take() {
        let (keep, commit) = edit_window(ctx, "Edit Time Signature", &mut text);
        if commit {
            match rlive_project::arrangement_edit::parse_signature(&text) {
                Some(sig) => session.set_signature_marker(i, sig),
                None => state.status = "Enter a signature like 3/4".into(),
            }
        }
        if keep {
            state.arr.editing_signature = Some((i, text));
        }
    }
    if let Some((at, mut text)) = state.arr.inserting_silence.take() {
        let (keep, commit) = edit_window(ctx, "Insert Silence (bars)", &mut text);
        if commit {
            let bar = session.project.time_signature.ticks_per_bar() as f64;
            match text.trim().parse::<f64>() {
                Ok(bars) if bars > 0.0 => session.insert_silence(at, (bars * bar).round() as u64),
                _ => state.status = "Enter a number of bars".into(),
            }
        }
        if keep {
            state.arr.inserting_silence = Some((at, text));
        }
    }
}

fn edit_window(ctx: &egui::Context, title: &str, text: &mut String) -> (bool, bool) {
    let mut open = true;
    let mut commit = false;
    let mut keep = true;
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .show(ctx, |ui| {
            let r = ui.text_edit_singleline(text);
            r.request_focus();
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                commit = true;
                keep = false;
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                keep = false;
            }
        });
    (keep && open, commit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seconds_read_as_minutes_seconds_milliseconds() {
        assert_eq!(format_seconds(0.0), "0:00:000");
        assert_eq!(format_seconds(61.5), "1:01:500");
        assert_eq!(format_seconds(125.007), "2:05:007");
    }
}
