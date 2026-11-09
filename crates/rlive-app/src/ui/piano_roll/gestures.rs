//! What the pointer does on the note grid: selecting, drawing, moving, resizing, splitting,
//! chopping and Note Stretch.

use super::lanes::LaneDrag;
use super::paint::{clip_under, hit};
use super::shown::set_foreground;
use super::{Geo, Preview, Shown, UiState, Zone};
use crate::session::{ClipRef, Session};
use crate::ui::keymap::{self, Platform};
use crate::ui::theme::*;
use crate::ui::timeline::snap;
use crate::ui::widgets::info;
use egui::{CursorIcon, Modifiers, Pos2, Rect, Sense, Shape, Stroke};
use rlive_core::PPQ;
use rlive_project::note_edit::{
    self as edit, apply_stretch, magnetic_delta, resolve_overlaps, span_of, TimeMap,
};
use rlive_project::note_select;
use rlive_project::note_view::ClipSpan;
use rlive_project::{ClipEnvelope, NoteData};

/// A clip's notes as they were when a gesture began, and which it's moving.
pub(super) struct Pick {
    pub(super) r: ClipRef,
    pub(super) base: Vec<NoteData>,
    pub(super) picks: Vec<usize>,
    pub(super) span: ClipSpan,
    pub(super) limit: u64,
}

pub(super) struct NoteDrag {
    picks: Vec<Pick>,
    origin: Pos2,
    anchor: u64,
}

pub(super) struct Stroke1 {
    r: ClipRef,
    base: Vec<NoteData>,
    path: Vec<(u64, u8)>,
}

pub(super) struct SplitDrag {
    r: ClipRef,
    left: usize,
    right: usize,
    lo: u64,
    hi: u64,
}

pub(super) struct StretchDrag {
    base: Vec<NoteData>,
    picks: Vec<(usize, NoteData)>,
    envelopes: Vec<ClipEnvelope>,
    from: f64,
    to: f64,
    which: StretchHandle,
    origin_x: f32,
    display_from: u64,
}

#[derive(Clone, Copy)]
enum StretchHandle {
    From,
    To,
    Mid(f64),
}

pub(super) enum Gesture {
    Move(NoteDrag),
    Resize(NoteDrag, bool),
    Velocity(NoteDrag),
    Box {
        start: Pos2,
    },
    Melodic(Vec<Stroke1>),
    Locked {
        base: Vec<NoteData>,
        key: u8,
        first: u64,
    },
    Split(SplitDrag),
    SplitLine {
        start: Pos2,
        end: Pos2,
    },
    Chop {
        origin_y: f32,
    },
    Stretch(StretchDrag),
    Lane(LaneDrag),
}

fn no_snap(state: &UiState, mods: Modifiers) -> bool {
    keymap::ignores_grid(mods, Platform::current()) != state.piano.snap_off
}

pub(super) fn moved_start(p: &Pick, start: u64, dt: i64) -> u64 {
    match p.span {
        ClipSpan::Placed { region, .. } if region.looped && region.loop_len > 0.0 => {
            let len = region.loop_len;
            let at = (start as f64 + dt as f64 - region.loop_start).rem_euclid(len);
            (region.loop_start + at) as u64
        }
        _ => (start as i64 + dt).clamp(0, p.limit.saturating_sub(1) as i64) as u64,
    }
}

fn store(session: &mut Session, r: ClipRef, notes: Vec<NoteData>) {
    if let Some(n) = session.clip_at_mut(r).and_then(|c| c.notes_mut()) {
        *n = notes;
    }
}

fn picks_of(state: &UiState, shown: &[Shown], fg: usize, focused: bool) -> Vec<Pick> {
    let mut picks: Vec<Pick> = Vec::new();
    for (i, s) in shown.iter().enumerate() {
        let selected: Vec<usize> = if i == fg {
            state.piano.selected.clone()
        } else if !focused {
            state.piano.others.get(&s.r).cloned().unwrap_or_default()
        } else {
            Vec::new()
        };
        if !selected.is_empty() {
            picks.push(Pick {
                r: s.r,
                base: s.clip.notes().to_vec(),
                picks: selected,
                span: s.span,
                limit: super::content_limit(&s.clip),
            });
        }
    }
    picks
}

fn set_picks(state: &mut UiState, fg: ClipRef, r: ClipRef, sel: Vec<usize>) {
    if r == fg {
        state.piano.selected = sel;
    } else {
        state.piano.others.insert(r, sel);
    }
}

/// Moves what was picked, or leaves it and moves a copy.
fn apply_move(
    session: &mut Session,
    state: &mut UiState,
    fg: ClipRef,
    d: &NoteDrag,
    dt: i64,
    dk: i32,
    copy: bool,
) {
    for p in &d.picks {
        let mut notes = p.base.clone();
        let mut sel = Vec::with_capacity(p.picks.len());
        for &i in &p.picks {
            let Some(orig) = p.base.get(i) else {
                continue;
            };
            let moved = NoteData {
                start: moved_start(p, orig.start, dt),
                key: (orig.key as i32 + dk).clamp(0, 127) as u8,
                ..orig.clone()
            };
            if copy {
                notes.push(moved);
                sel.push(notes.len() - 1);
            } else {
                notes[i] = moved;
                sel.push(i);
            }
        }
        store(session, p.r, notes);
        set_picks(state, fg, p.r, sel);
    }
}

/// Settles a finished move or draw: overlaps resolved and the engine told.
fn settle(
    session: &mut Session,
    state: &mut UiState,
    fg: ClipRef,
    clips: &[ClipRef],
    label: &'static str,
) {
    for &r in clips {
        let Some(mut notes) = session.clip_at(r).map(|c| c.notes().to_vec()) else {
            continue;
        };
        let winners = if r == fg {
            state.piano.selected.clone()
        } else {
            state.piano.others.get(&r).cloned().unwrap_or_default()
        };
        let kept = resolve_overlaps(&mut notes, &winners);
        store(session, r, notes);
        set_picks(state, fg, r, kept);
        session.history.pending = Some(label);
        session.sync_ref(r);
    }
}

fn apply_resize(session: &mut Session, d: &NoteDrag, dt: i64, left: bool, min: u64) {
    for p in &d.picks {
        let mut notes = p.base.clone();
        for &i in &p.picks {
            let Some(n) = notes.get_mut(i) else {
                continue;
            };
            let end = n.start + n.length;
            if left {
                let start = (n.start as i64 + dt).clamp(0, end.saturating_sub(min) as i64) as u64;
                n.start = start;
                n.length = end - start;
            } else {
                n.length = (n.length as i64 + dt).max(min as i64) as u64;
            }
        }
        store(session, p.r, notes);
    }
}

fn apply_velocity(session: &mut Session, d: &NoteDrag, dy: f32) {
    let by = -dy / 200.0;
    for p in &d.picks {
        let mut notes = p.base.clone();
        edit::change_value(&mut notes, &p.picks, edit::LaneValue::Velocity, by);
        store(session, p.r, notes);
    }
}

/// Notes a Draw Mode stroke is making from where the pointer is.
#[allow(clippy::too_many_arguments)]
fn draw_step(
    session: &mut Session,
    state: &UiState,
    shown: &[Shown],
    fg: ClipRef,
    strokes: &mut Vec<Stroke1>,
    t: f64,
    key: u8,
) {
    let grid = state.piano.grid.max(1);
    let velocity = state.piano.new_velocity;
    let focused = state.piano.focused();
    let target = if focused {
        shown.iter().find(|s| s.r == fg)
    } else {
        clip_under(shown, fg, t)
    };
    let Some(s) = target else {
        return;
    };
    let content = s.span.content_at(t);
    let cell = (content.max(0.0) / grid as f64) as u64;
    if strokes.iter().all(|x| x.r != s.r) {
        strokes.push(Stroke1 {
            r: s.r,
            base: s.clip.notes().to_vec(),
            path: Vec::new(),
        });
    }
    if let Some(st) = strokes.iter_mut().find(|x| x.r == s.r) {
        if !st.path.iter().any(|(c, _)| *c == cell) {
            st.path.push((cell, key));
            let notes = edit::draw_melodic(&st.base, &st.path, grid, velocity);
            store(session, st.r, notes);
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn grid(
    ui: &mut egui::Ui,
    geo: &Geo,
    session: &mut Session,
    state: &mut UiState,
    shown: &[Shown],
    fg: usize,
    limit: u64,
    now: f64,
    preview: &mut Preview,
) {
    let cref = shown[fg].r;
    let resp = ui.interact(
        geo.grid,
        ui.scope_id().with("note_grid"),
        Sense::click_and_drag(),
    );
    let (mods, press_origin) = ui.input(|i| (i.modifiers, i.pointer.press_origin()));
    let focused = state.piano.focused();
    let split_key = !state.computer_keys
        && !ui.ctx().egui_wants_keyboard_input()
        && ui.input(|i| i.key_down(egui::Key::E));
    let draw_mode = state.arr.auto.draw;
    let snapless = no_snap(state, mods);
    let ps_grid = state.piano.grid.max(1);
    let shift = mods.shift;

    if let Some(pos) = resp.hover_pos() {
        if split_key {
            ui.ctx().set_cursor_icon(CursorIcon::VerticalText);
        } else if draw_mode {
            ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
        } else if let Some((_, _, z)) = hit(geo, shown, fg, focused, pos) {
            ui.ctx().set_cursor_icon(if z == Zone::Body {
                CursorIcon::Grab
            } else {
                CursorIcon::ResizeHorizontal
            });
        }
        state.piano.lanes.hover_note = hit(geo, shown, fg, focused, pos)
            .filter(|(s, _, _)| *s == fg)
            .map(|(_, n, _)| n);
        if let Some(t) = session.track_tuning(shown[fg].r.track()) {
            let key = state
                .piano
                .lanes
                .hover_note
                .and_then(|n| shown[fg].clip.notes().get(n))
                .map(|n| n.key);
            if let Some(key) = key {
                info(resp.clone(), super::rulers::tuned_note_text(key, t));
            }
        }
    } else if !resp.dragged() {
        state.piano.lanes.hover_note = None;
    }

    // ---- clicks
    if resp.double_clicked() && !draw_mode && !split_key {
        if let Some(pos) = resp.interact_pointer_pos() {
            match hit(geo, shown, fg, focused, pos) {
                Some((s, n, _)) => {
                    let r = shown[s].r;
                    if let Some(notes) = session.clip_at_mut(r).and_then(|c| c.notes_mut()) {
                        if n < notes.len() {
                            notes.remove(n);
                        }
                    }
                    state.piano.selected.clear();
                    state.piano.others.clear();
                    session.history.pending = Some("Delete Note");
                    session.sync_ref(r);
                }
                None => {
                    add_note(session, state, geo, shown, fg, pos, limit, now, preview);
                }
            }
        }
    } else if resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            state.piano.lanes.focus = None;
            let hitting = hit(geo, shown, fg, focused, pos);
            match hitting {
                Some((s, n, _)) if draw_mode => {
                    let r = shown[s].r;
                    if let Some(notes) = session.clip_at_mut(r).and_then(|c| c.notes_mut()) {
                        if n < notes.len() {
                            notes.remove(n);
                        }
                    }
                    state.piano.deselect(false);
                    session.history.pending = Some("Delete Note");
                    session.sync_ref(r);
                }
                Some((s, n, _)) => {
                    let r = shown[s].r;
                    if r != cref {
                        set_foreground(state, r);
                    }
                    let ps = &mut state.piano;
                    if shift {
                        if let Some(k) = ps.selected.iter().position(|x| *x == n) {
                            ps.selected.remove(k);
                        } else {
                            ps.selected.push(n);
                        }
                    } else {
                        ps.selected = vec![n];
                        ps.others.clear();
                    }
                    if let Some(note) = shown[s].clip.notes().get(n) {
                        preview.on = Some(note.key);
                        preview.deadline = now + 0.3;
                    }
                }
                None if draw_mode => {
                    add_note(session, state, geo, shown, fg, pos, limit, now, preview);
                }
                None => {
                    let ps = &mut state.piano;
                    ps.deselect(false);
                    ps.filters.clear_applied();
                    let t = geo.tick(pos.x);
                    ps.insert_tick = Some(if snapless {
                        t.max(0.0) as u64
                    } else {
                        snap(t, ps_grid, true).max(0) as u64
                    });
                }
            }
        }
    }

    // ---- gesture start
    if resp.drag_started() {
        let pos = press_origin.or(resp.interact_pointer_pos());
        if let Some(pos) = pos {
            start_gesture(
                session,
                state,
                geo,
                shown,
                fg,
                pos,
                (mods, draw_mode, split_key),
                preview,
            );
        }
    }

    // ---- gesture in progress
    if resp.dragged() {
        if let Some(pos) = resp.interact_pointer_pos() {
            continue_gesture(
                session, state, geo, shown, fg, pos, mods, shift, ps_grid, snapless,
            );
        }
    }

    if resp.drag_stopped() {
        finish_gesture(session, state, geo, shown, fg);
        preview.off = true;
    }
    resp.context_menu(|ui| {
        note_menu(ui, state);
        ui.separator();
        crate::ui::clip_view::crop_menu(ui, session, state, cref);
    });
    info(
        resp,
        "Note Editor — drag to select; double-click to add or remove notes; drag a note to move, \
         its edge to resize; hold E to split, B to draw; Cmd+wheel zooms",
    );
}

/// The editor's context menu, which queues its commands for the end of the frame.
fn note_menu(ui: &mut egui::Ui, state: &mut UiState) {
    use crate::ui::commands::notes::Notes as N;
    use crate::ui::commands::MenuCommand as M;
    let items: [(&str, N); 9] = [
        ("Invert Selection", N::InvertSelection),
        ("Deactivate Note(s)", N::Deactivate),
        ("Split/Chop Note(s)", N::ChopOrSplit),
        ("Join Notes", N::Join),
        ("Fit to Time Range", N::FitToRange),
        ("Quantize", N::Quantize),
        ("Group Notes (Play All)", N::GroupPlayAll),
        ("Group Notes (Play One)", N::GroupPlayOne),
        ("Ungroup Notes", N::Ungroup),
    ];
    for (label, c) in items {
        if ui.button(label).clicked() {
            state.piano.queued.push(M::Notes(c));
            ui.close();
        }
    }
    let draw_label = if state.piano.pitch_lock {
        "Draw Mode (Pitch Lock On)"
    } else {
        "Draw Mode (Pitch Lock Off)"
    };
    if ui
        .selectable_label(state.arr.auto.draw, draw_label)
        .clicked()
    {
        state.arr.auto.draw = !state.arr.auto.draw;
        ui.close();
    }
}

#[allow(clippy::too_many_arguments)]
fn add_note(
    session: &mut Session,
    state: &mut UiState,
    geo: &Geo,
    shown: &[Shown],
    fg: usize,
    pos: Pos2,
    limit: u64,
    now: f64,
    preview: &mut Preview,
) {
    let ps = &mut state.piano;
    let t = geo.tick(pos.x);
    let key = geo.key_near(pos.y);
    let target = if ps.focused() {
        Some(&shown[fg])
    } else {
        clip_under(shown, shown[fg].r, t).or(Some(&shown[fg]))
    };
    let Some(s) = target else {
        return;
    };
    let content = s.span.content_at(t);
    let grid = ps.grid.max(1) as f64;
    let start = (content / grid).floor().max(0.0) as u64 * ps.grid.max(1);
    if start >= limit.max(super::content_limit(&s.clip)) {
        return;
    }
    let mut notes = s.clip.notes().to_vec();
    notes.push(NoteData::new(
        start,
        ps.last_length.max(ps.grid / 2),
        key,
        ps.new_velocity,
    ));
    let added = notes.len() - 1;
    let winners = resolve_overlaps(&mut notes, &[added]);
    let r = s.r;
    if r != shown[fg].r {
        ps.others.insert(r, winners);
    } else {
        ps.selected = winners;
        ps.others.clear();
    }
    store(session, r, notes);
    session.history.pending = Some("Add Note");
    session.sync_ref(r);
    preview.on = Some(key);
    preview.deadline = now + 0.3;
}

#[allow(clippy::too_many_arguments)]
fn start_gesture(
    session: &mut Session,
    state: &mut UiState,
    geo: &Geo,
    shown: &[Shown],
    fg: usize,
    pos: Pos2,
    (mods, draw_mode, split_key): (Modifiers, bool, bool),
    preview: &mut Preview,
) {
    let focused = state.piano.focused();
    let cref = shown[fg].r;
    if split_key {
        let mods_chop = mods.command || mods.alt;
        if let Some((s, n, _)) = hit(geo, shown, fg, focused, pos) {
            let selected = s == fg && state.piano.selected.contains(&n);
            if selected && mods_chop {
                start_chop(session, state, shown, fg);
                state.piano.gesture = Some(Gesture::Chop { origin_y: pos.y });
                return;
            }
            let r = shown[s].r;
            let Some(note) = shown[s].clip.notes().get(n).cloned() else {
                return;
            };
            let t = shown[s].span.content_at(geo.tick(pos.x));
            let at = if keymap::ignores_grid(mods, Platform::current()) {
                snap(t, state.piano.grid, true).max(0) as u64
            } else {
                t.max(0.0) as u64
            };
            let mut notes = shown[s].clip.notes().to_vec();
            let made = edit::split_at(&mut notes, &[n], at);
            store(session, r, notes);
            if made.len() == 2 {
                state.piano.gesture = Some(Gesture::Split(SplitDrag {
                    r,
                    left: made[0],
                    right: made[1],
                    lo: note.start + 1,
                    hi: note.start + note.length - 1,
                }));
            }
            return;
        }
        state.piano.gesture = Some(Gesture::SplitLine {
            start: pos,
            end: pos,
        });
        return;
    }
    if draw_mode {
        let key = geo.key_near(pos.y);
        let locked = state.piano.pitch_lock != mods.alt;
        if locked {
            let s = &shown[fg];
            let cell =
                (s.span.content_at(geo.tick(pos.x)).max(0.0) as u64) / state.piano.grid.max(1);
            state.piano.gesture = Some(Gesture::Locked {
                base: s.clip.notes().to_vec(),
                key,
                first: cell,
            });
        } else {
            let mut strokes = Vec::new();
            draw_step(
                session,
                state,
                shown,
                cref,
                &mut strokes,
                geo.tick(pos.x),
                key,
            );
            state.piano.gesture = Some(Gesture::Melodic(strokes));
        }
        preview.on = Some(key);
        preview.deadline = f64::MAX;
        return;
    }
    let filters_open = state.piano.filters.open;
    match hit(geo, shown, fg, focused, pos) {
        Some((s, n, zone)) if !(filters_open && mods.shift) => {
            let r = shown[s].r;
            if r != cref {
                set_foreground(state, r);
            }
            let ps = &mut state.piano;
            if !ps.selected.contains(&n) {
                ps.selected = vec![n];
                ps.others.clear();
            }
            let fg_now = shown.iter().position(|x| x.r == r).unwrap_or(fg);
            let velocity_mod = keymap::ignores_grid(mods, Platform::current());
            let picks = picks_of(state, shown, fg_now, focused);
            let anchor = shown[s].clip.notes().get(n).map_or(0, |x| match zone {
                Zone::End => x.start + x.length,
                _ => x.start,
            });
            let drag = NoteDrag {
                picks,
                origin: pos,
                anchor,
            };
            state.piano.gesture = Some(match zone {
                Zone::End => Gesture::Resize(drag, false),
                Zone::Start => Gesture::Resize(drag, true),
                Zone::Body if velocity_mod => Gesture::Velocity(drag),
                Zone::Body => Gesture::Move(drag),
            });
            if zone == Zone::Body {
                if let Some(note) = shown[s].clip.notes().get(n) {
                    preview.on = Some(note.key);
                    preview.deadline = f64::MAX;
                }
            }
        }
        _ => {
            if !mods.shift {
                state.piano.selected.clear();
                state.piano.others.clear();
                state.piano.filters.clear_applied();
            }
            state.piano.gesture = Some(Gesture::Box { start: pos });
        }
    }
}

/// The chop gesture's start: the notes as they are, and the count the grid would give.
pub(super) fn start_chop(_session: &mut Session, state: &mut UiState, shown: &[Shown], fg: usize) {
    let notes = shown[fg].clip.notes();
    let parts = state
        .piano
        .selected
        .first()
        .and_then(|i| notes.get(*i))
        .map_or(2, |n| edit::parts_on_grid(n, state.piano.grid).max(2));
    state.piano.chop = Some(super::Chop {
        base: notes.to_vec(),
        picks: state.piano.selected.clone(),
        parts,
    });
}

/// Rebuilds the chop with `parts` parts, from the notes as they were.
pub fn apply_chop(session: &mut Session, state: &mut UiState, r: ClipRef, parts: usize) {
    let Some(chop) = state.piano.chop.as_mut() else {
        return;
    };
    chop.parts = parts.max(1);
    let mut notes = chop.base.clone();
    let pieces = edit::chop_into(&mut notes, &chop.picks, chop.parts);
    state.piano.selected = pieces;
    store(session, r, notes);
    session.history.pending = Some("Chop Notes");
    session.sync_ref(r);
}

#[allow(clippy::too_many_arguments)]
fn continue_gesture(
    session: &mut Session,
    state: &mut UiState,
    geo: &Geo,
    shown: &[Shown],
    fg: usize,
    pos: Pos2,
    mods: Modifiers,
    shift: bool,
    grid: u64,
    snapless: bool,
) {
    let copy = keymap::copies_on_drag(mods, Platform::current());
    let cref = shown[fg].r;
    let Some(mut g) = state.piano.gesture.take() else {
        return;
    };
    match &mut g {
        Gesture::Move(d) => {
            let raw = ((pos.x - d.origin.x) / geo.ppb * PPQ as f32) as f64;
            let dt = if snapless {
                raw as i64
            } else {
                magnetic_delta(d.anchor, raw, grid)
            };
            let dk = rows_between(geo, d.origin.y, pos.y);
            apply_move(session, state, cref, d, dt, dk, copy);
        }
        Gesture::Resize(d, left) => {
            let raw = ((pos.x - d.origin.x) / geo.ppb * PPQ as f32) as f64;
            let dt = if snapless {
                raw as i64
            } else {
                magnetic_delta(d.anchor, raw, grid)
            };
            let min = if snapless { 10 } else { grid };
            apply_resize(session, d, dt, *left, min);
            if let Some(n) = d
                .picks
                .iter()
                .flat_map(|p| p.picks.first().and_then(|i| p.base.get(*i)))
                .next()
            {
                state.piano.last_length = (n.length as i64 + dt).max(min as i64) as u64;
            }
        }
        Gesture::Velocity(d) => apply_velocity(session, d, pos.y - d.origin.y),
        Gesture::Box { start } => box_select(state, geo, shown, fg, *start, pos, shift, snapless),
        Gesture::Melodic(strokes) => {
            draw_step(
                session,
                state,
                shown,
                cref,
                strokes,
                geo.tick(pos.x),
                geo.key_near(pos.y),
            );
        }
        Gesture::Locked { base, key, first } => {
            let s = &shown[fg];
            let cell = (s.span.content_at(geo.tick(pos.x)).max(0.0) as u64) / grid;
            let notes = edit::draw_locked(base, *key, *first, cell, grid, state.piano.new_velocity);
            store(session, cref, notes);
        }
        Gesture::Split(d) => {
            let s = &shown[fg];
            let content = s.span.content_at(geo.tick(pos.x)).max(0.0) as u64;
            let at = content.clamp(d.lo, d.hi);
            if let Some(notes) = session.clip_at_mut(d.r).and_then(|c| c.notes_mut()) {
                let right_end = notes[d.right].start + notes[d.right].length;
                let left_start = notes[d.left].start;
                notes[d.left].length = at - left_start;
                notes[d.right].start = at;
                notes[d.right].length = right_end - at;
            }
        }
        Gesture::SplitLine { end, .. } => *end = pos,
        Gesture::Chop { origin_y } => {
            let steps = ((*origin_y - pos.y) / 12.0) as i32;
            if steps != 0 {
                *origin_y = pos.y;
                let parts = state.piano.chop.as_ref().map_or(2, |c| c.parts);
                let mut p = parts;
                for _ in 0..steps.abs() {
                    p = edit::next_parts(p, steps > 0, shift);
                }
                apply_chop(session, state, cref, p);
            }
        }
        Gesture::Stretch(d) => {
            apply_stretch_drag(session, state, geo, cref, d, pos, snapless, grid)
        }
        Gesture::Lane(_) => {}
    }
    state.piano.gesture = Some(g);
}

fn rows_between(geo: &Geo, from: f32, to: f32) -> i32 {
    let a = geo.row_of(geo.key_near(from)).unwrap_or(0) as i32;
    let b = geo.row_of(geo.key_near(to)).unwrap_or(0) as i32;
    geo.rows
        .get(b as usize)
        .zip(geo.rows.get(a as usize))
        .map_or(0, |(k1, k0)| *k1 as i32 - *k0 as i32)
}

#[allow(clippy::too_many_arguments)]
fn box_select(
    state: &mut UiState,
    geo: &Geo,
    shown: &[Shown],
    fg: usize,
    start: Pos2,
    pos: Pos2,
    add: bool,
    snapless: bool,
) {
    let grid = state.piano.grid;
    let snap_to = |t: f64| {
        if snapless {
            t.max(0.0) as u64
        } else {
            snap(t, grid, true).max(0) as u64
        }
    };
    let (a, b) = (
        snap_to(geo.tick(start.x.min(pos.x))),
        snap_to(geo.tick(start.x.max(pos.x))),
    );
    let (k0, k1) = (
        geo.key_near(start.y.max(pos.y)),
        geo.key_near(start.y.min(pos.y)),
    );
    let ps = &mut state.piano;
    if ps.filters.open && add {
        let f = note_select::repeated_time(a, b.max(a + 1));
        ps.filters.set_time(f);
        return;
    }
    ps.time = Some((a, b.max(a)));
    ps.time_only = false;
    let focused = ps.focused();
    let mut fg_sel = if add { ps.selected.clone() } else { Vec::new() };
    for i in note_select::notes_in_box(shown[fg].clip.notes(), a, b.max(a + 1), k0, k1) {
        if !fg_sel.contains(&i) {
            fg_sel.push(i);
        }
    }
    ps.selected = fg_sel;
    if !focused {
        for (_, s) in shown.iter().enumerate().filter(|(i, _)| *i != fg) {
            let (from, to) = (
                s.span.content_at(a as f64) as u64,
                s.span.content_at(b as f64) as u64,
            );
            let found = note_select::notes_in_box(s.clip.notes(), from, to.max(from + 1), k0, k1);
            if found.is_empty() {
                ps.others.remove(&s.r);
            } else {
                ps.others.insert(s.r, found);
            }
        }
    }
    ps.insert_tick = Some(a);
}

fn finish_gesture(
    session: &mut Session,
    state: &mut UiState,
    geo: &Geo,
    shown: &[Shown],
    fg: usize,
) {
    let cref = shown[fg].r;
    let Some(g) = state.piano.gesture.take() else {
        return;
    };
    match g {
        Gesture::Move(d) => {
            let clips: Vec<ClipRef> = d.picks.iter().map(|p| p.r).collect();
            settle(session, state, cref, &clips, "Move Notes");
        }
        Gesture::Resize(d, _) => {
            for p in &d.picks {
                session.history.pending = Some("Resize Notes");
                session.sync_ref(p.r);
            }
        }
        Gesture::Velocity(d) => {
            for p in &d.picks {
                session.history.pending = Some("Change Velocity");
                session.sync_ref(p.r);
            }
        }
        Gesture::Melodic(strokes) => {
            for st in &strokes {
                session.history.pending = Some("Draw Notes");
                session.sync_ref(st.r);
            }
        }
        Gesture::Locked { .. } => {
            session.history.pending = Some("Draw Notes");
            session.sync_ref(cref);
        }
        Gesture::Split(d) => {
            session.history.pending = Some("Split Notes");
            session.sync_ref(d.r);
            state.piano.selected = vec![d.left, d.right];
        }
        Gesture::SplitLine { start, end } => {
            // Splits the notes the line crossed, at the line.
            let tick = geo.tick(start.x);
            let t = if no_snap(state, Modifiers::NONE) {
                tick.max(0.0) as u64
            } else {
                snap(tick, state.piano.grid, true).max(0) as u64
            };
            let (k0, k1) = (
                geo.key_near(start.y.max(end.y)),
                geo.key_near(start.y.min(end.y)),
            );
            let mut notes = shown[fg].clip.notes().to_vec();
            let crossing: Vec<usize> = edit::notes_crossing(&notes, t)
                .into_iter()
                .filter(|i| (k0..=k1).contains(&notes[*i].key))
                .collect();
            if !crossing.is_empty() {
                edit::split_at(&mut notes, &crossing, t);
                store(session, cref, notes);
                session.history.pending = Some("Split Notes");
                session.sync_ref(cref);
            }
        }
        Gesture::Chop { .. } => state.piano.chop = None,
        Gesture::Stretch(_) => {
            session.history.pending = Some("Note Stretch");
            session.sync_ref(cref);
        }
        Gesture::Box { .. } => {
            if state.piano.filters.open && state.piano.filters.any_applied() {
                super::filters::apply(session, state, shown, fg);
            }
        }
        Gesture::Lane(_) => {}
    }
}

// ------------------------------------------------------------- Note Stretch

/// The stretch of notes that the Note Stretch markers bracket: the time selection, or what spans
/// the selected notes when there's more than one.
fn stretch_range(state: &UiState, shown: &Shown) -> Option<(u64, u64)> {
    let ps = &state.piano;
    if let Some(t) = ps.time.filter(|(a, b)| b > a) {
        return Some(t);
    }
    (ps.selected.len() > 1)
        .then(|| span_of(shown.clip.notes(), &ps.selected))
        .flatten()
}

#[allow(clippy::too_many_arguments)]
fn apply_stretch_drag(
    session: &mut Session,
    state: &mut UiState,
    geo: &Geo,
    r: ClipRef,
    d: &StretchDrag,
    pos: Pos2,
    snapless: bool,
    grid: u64,
) {
    let raw = ((pos.x - d.origin_x) / geo.ppb * PPQ as f32) as f64;
    let moved = |anchor: f64| {
        if snapless {
            anchor + raw
        } else {
            anchor + magnetic_delta(anchor as u64, raw, grid) as f64
        }
    };
    let map = match d.which {
        StretchHandle::From => TimeMap {
            from: d.from,
            to: d.to,
            new_from: moved(d.from).max(0.0),
            new_to: d.to,
            mid: None,
        },
        StretchHandle::To => TimeMap {
            from: d.from,
            to: d.to,
            new_from: d.from,
            new_to: moved(d.to).max(0.0),
            mid: None,
        },
        StretchHandle::Mid(was) => TimeMap {
            from: d.from,
            to: d.to,
            new_from: d.from,
            new_to: d.to,
            mid: Some((was, moved(was).clamp(d.from + 1.0, d.to - 1.0))),
        },
    };
    let mut notes = d.base.clone();
    apply_stretch(&mut notes, &d.picks, map);
    if let Some(clip) = session.clip_at_mut(r) {
        edit::stretch_envelopes(&mut clip.envelopes, &d.envelopes, map);
        if let Some(n) = clip.notes_mut() {
            *n = notes;
        }
    }
    let shift = d.display_from as f64 - d.from;
    let (a, b) = (
        map.new_from.min(map.new_to) + shift,
        map.new_from.max(map.new_to) + shift,
    );
    state.piano.time = Some((a.max(0.0) as u64, b.max(0.0) as u64));
}

/// The Note Stretch strip, under the scrub area.
#[allow(clippy::too_many_arguments)]
pub(super) fn stretch_markers(
    ui: &mut egui::Ui,
    strip: Rect,
    geo: &Geo,
    session: &mut Session,
    state: &mut UiState,
    cref: ClipRef,
    s: &Shown,
) {
    let p = ui.painter_at(strip);
    p.rect_filled(strip, 0.0, shade(0x262626));
    let resp = ui.interact(
        strip,
        ui.scope_id().with("note_stretch"),
        Sense::click_and_drag(),
    );
    let Some((from, to)) = stretch_range(state, s) else {
        return;
    };
    let to_content = |t: u64| s.span.content_at(t as f64);
    let (cfrom, cto) = (to_content(from), to_content(from) + (to - from) as f64);
    let (x_a, x_b) = (geo.x(from as f64), geo.x(to as f64));
    let mark = |x: f32, color| {
        p.add(Shape::convex_polygon(
            vec![
                Pos2::new(x - 5.0, strip.top() + 1.0),
                Pos2::new(x + 5.0, strip.top() + 1.0),
                Pos2::new(x, strip.bottom() - 1.0),
            ],
            color,
            Stroke::NONE,
        ));
    };
    mark(x_a, pal().accent);
    mark(x_b, pal().accent);
    let hover = resp
        .hover_pos()
        .map(|p| p.x)
        .filter(|x| *x > x_a + 7.0 && *x < x_b - 7.0);
    if let Some(x) = hover {
        mark(x, pal().text_dim);
    }
    if resp.drag_started() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let which = if (pos.x - x_a).abs() < 8.0 {
                Some(StretchHandle::From)
            } else if (pos.x - x_b).abs() < 8.0 {
                Some(StretchHandle::To)
            } else if pos.x > x_a && pos.x < x_b {
                Some(StretchHandle::Mid(s.span.content_at(geo.tick(pos.x))))
            } else {
                None
            };
            if let Some(which) = which {
                let notes = s.clip.notes();
                let picks: Vec<(usize, NoteData)> = if state.piano.selected.is_empty() {
                    notes
                        .iter()
                        .enumerate()
                        .filter(|(_, n)| (n.start as f64) >= cfrom && (n.start as f64) < cto)
                        .map(|(i, n)| (i, n.clone()))
                        .collect()
                } else {
                    state
                        .piano
                        .selected
                        .iter()
                        .filter_map(|i| notes.get(*i).map(|n| (*i, n.clone())))
                        .collect()
                };
                state.piano.gesture = Some(Gesture::Stretch(StretchDrag {
                    base: notes.to_vec(),
                    picks,
                    envelopes: s.clip.envelopes.clone(),
                    from: cfrom,
                    to: cto,
                    which,
                    origin_x: pos.x,
                    display_from: from,
                }));
            }
        }
    }
    if resp.dragged() {
        if let (Some(pos), Some(Gesture::Stretch(_))) =
            (resp.interact_pointer_pos(), state.piano.gesture.as_ref())
        {
            let mods = ui.input(|i| i.modifiers);
            let snapless = no_snap(state, mods);
            let grid = state.piano.grid.max(1);
            if let Some(Gesture::Stretch(d)) = state.piano.gesture.take() {
                apply_stretch_drag(session, state, geo, cref, &d, pos, snapless, grid);
                state.piano.gesture = Some(Gesture::Stretch(d));
            }
        }
    }
    if resp.drag_stopped() && matches!(state.piano.gesture, Some(Gesture::Stretch(_))) {
        state.piano.gesture = None;
        session.history.pending = Some("Note Stretch");
        session.sync_ref(cref);
    }
    info(
        resp,
        "Note Stretch — drag a marker to stretch the selection, between them to move only what's \
         inside; past the other mirrors it",
    );
}

/// Starts a drag on a lane.
pub(super) fn lane_gesture(state: &mut UiState, d: LaneDrag) {
    state.piano.gesture = Some(Gesture::Lane(d));
}

/// The lane drag in progress, if any.
pub(super) fn take_lane(state: &mut UiState) -> Option<LaneDrag> {
    match state.piano.gesture.take() {
        Some(Gesture::Lane(d)) => Some(d),
        other => {
            state.piano.gesture = other;
            None
        }
    }
}
