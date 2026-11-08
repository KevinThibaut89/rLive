//! The MIDI Note Editor of the Clip View: ruler with markers, piano keys, note grid and velocity
//! lane.

use super::clip_view::markers::{self, Axis, MARKER_H, SCRUB_H};
use super::keymap::{self, Platform};
use super::theme::{self, *};
use super::timeline::snap;
use super::widgets::{info, rgb};
use super::UiState;
use crate::session::{ClipRef, Session};
use egui::{Align2, Color32, CursorIcon, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use rlive_core::{MidiMessage, PPQ};
use rlive_project::NoteData;

const KEYBOARD_W: f32 = 48.0;
const RULER_H: f32 = 20.0;
const VELOCITY_H: f32 = 52.0;

#[derive(Clone, Copy, PartialEq)]
enum DragKind {
    Move,
    Resize,
    Velocity,
}

struct NoteDrag {
    kind: DragKind,
    origin: Pos2,
    originals: Vec<(usize, NoteData)>,
}

pub struct PianoState {
    pub px_per_beat: f32,
    pub key_h: f32,
    pub grid: u64,
    pub reset_view: bool,
    pub selected: Vec<usize>,
    /// Where Paste puts notes: the last clicked grid position.
    pub insert_tick: Option<u64>,
    pub new_velocity: f32,
    pub scroll: Vec2,
    /// Snap to Grid is off.
    pub snap_off: bool,
    /// The grid stays at its size however far the view zooms.
    pub fixed_grid: bool,
    /// Views to go back to, for each zoom in: how wide a beat is and the scroll.
    pub zoom_back: Vec<(f32, Vec2)>,
    /// How wide the grid was last drawn.
    pub view_width: f32,
    last_length: u64,
    drag: Option<NoteDrag>,
    preview: Option<u8>,
    /// Whether the sounding preview note comes from the on-screen keyboard.
    preview_from_keys: bool,
    /// When a note-triggered preview should stop (seconds, `ui.input(|i| i.time)`).
    preview_deadline: f64,
}

impl Default for PianoState {
    fn default() -> Self {
        Self {
            px_per_beat: 64.0,
            key_h: 12.0,
            grid: PPQ / 4,
            reset_view: true,
            selected: Vec::new(),
            insert_tick: None,
            new_velocity: 0.8,
            scroll: Vec2::ZERO,
            snap_off: false,
            fixed_grid: false,
            zoom_back: Vec::new(),
            view_width: 0.0,
            last_length: PPQ / 4,
            drag: None,
            preview: None,
            preview_from_keys: false,
            preview_deadline: 0.0,
        }
    }
}

impl PianoState {
    pub fn has_selection(&self) -> bool {
        !self.selected.is_empty()
    }
}

/// The MIDI notes of the selected clip, if any.
fn selected_notes(session: &Session, state: &UiState) -> Option<Vec<NoteData>> {
    let clip = session.clip_at(state.selected_clip?)?;
    clip.is_midi().then(|| clip.notes().to_vec())
}

fn store_notes(session: &mut Session, state: &UiState, notes: Vec<NoteData>) {
    let Some(cref) = state.selected_clip else {
        return;
    };
    if let Some(n) = session.clip_at_mut(cref).and_then(|c| c.notes_mut()) {
        *n = notes;
        session.sync_ref(cref);
    }
}

/// The selected notes, moved so the earliest starts at tick 0.
pub fn copy_selection(session: &Session, state: &mut UiState) -> Option<Vec<NoteData>> {
    let notes = selected_notes(session, state)?;
    let mut picked: Vec<NoteData> = state
        .piano
        .selected
        .iter()
        .filter_map(|&i| notes.get(i).cloned())
        .collect();
    let first = picked.iter().map(|n| n.start).min()?;
    let end = picked.iter().map(|n| n.start + n.length).max()?;
    for n in &mut picked {
        n.start -= first;
    }
    // A plain Paste lands right after what was copied.
    state.piano.insert_tick = Some(end);
    Some(picked)
}

pub fn delete_selected(session: &mut Session, state: &mut UiState) {
    let Some(notes) = selected_notes(session, state) else {
        return;
    };
    let sel = std::mem::take(&mut state.piano.selected);
    let kept = notes
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !sel.contains(i))
        .map(|(_, n)| n)
        .collect();
    store_notes(session, state, kept);
}

pub fn select_all(session: &Session, state: &mut UiState) {
    if let Some(notes) = selected_notes(session, state) {
        state.piano.selected = (0..notes.len()).collect();
    }
}

/// Adds `notes` (starting at tick 0) at `at`, dropping any past the clip end, and selects them.
fn insert_notes(session: &mut Session, state: &mut UiState, notes: &[NoteData], at: u64) {
    let Some(cref) = state.selected_clip else {
        return;
    };
    let Some(length) = session.clip_at(cref).map(content_limit) else {
        return;
    };
    let Some(mut all) = selected_notes(session, state) else {
        return;
    };
    let first_new = all.len();
    all.extend(
        notes
            .iter()
            .map(|n| NoteData {
                start: n.start + at,
                ..n.clone()
            })
            .filter(|n| n.start < length),
    );
    state.piano.selected = (first_new..all.len()).collect();
    state.piano.insert_tick = notes.iter().map(|n| at + n.start + n.length).max();
    store_notes(session, state, all);
}

pub fn paste_notes(session: &mut Session, state: &mut UiState, notes: &[NoteData]) {
    let at = state.piano.insert_tick.unwrap_or(0);
    insert_notes(session, state, notes, at);
}

pub fn duplicate_selection(session: &mut Session, state: &mut UiState) {
    if let Some(copied) = copy_selection(session, state) {
        let at = state.piano.insert_tick.unwrap_or(0);
        insert_notes(session, state, &copied, at);
    }
}

/// How far a clip's content can reach: past its loop if its end marker is.
fn content_limit(c: &rlive_project::Clip) -> u64 {
    c.loop_end().max(c.end_marker()).max(c.length)
}

fn is_black(key: u8) -> bool {
    matches!(key % 12, 1 | 3 | 6 | 8 | 10)
}

pub(super) fn grid_label(g: u64) -> String {
    match g {
        x if x == PPQ => "1/4".into(),
        x if x == PPQ / 2 => "1/8".into(),
        x if x == PPQ / 4 => "1/16".into(),
        x if x == PPQ / 8 => "1/32".into(),
        x if x == PPQ / 3 => "1/8T".into(),
        x if x == PPQ / 6 => "1/16T".into(),
        _ => format!("{g}t"),
    }
}

/// Step recording: with the track armed and the transport stopped, the arrow keys step the insert
/// marker by the grid and write or take back whichever keys are held.
fn step_record_keys(ui: &egui::Ui, session: &mut Session, state: &mut UiState, cref: ClipRef) {
    if session.is_playing() || ui.ctx().text_edit_focused() {
        return;
    }
    if !session.project.track(cref.track()).is_some_and(|t| t.arm) {
        return;
    }
    let (right, left) = ui.input(|i| {
        (
            i.key_pressed(egui::Key::ArrowRight),
            i.key_pressed(egui::Key::ArrowLeft),
        )
    });
    let ps = &mut state.piano;
    if !ps.selected.is_empty() {
        return;
    }
    let at = ps.insert_tick.unwrap_or(0);
    if right {
        ps.insert_tick = Some(session.step_forward(cref, at, ps.grid));
    } else if left {
        ps.insert_tick = Some(session.step_back(cref, at, ps.grid));
    }
}

pub(super) fn note_editor(
    ui: &mut egui::Ui,
    area: Rect,
    session: &mut Session,
    state: &mut UiState,
    cref: ClipRef,
) {
    let Some(clip) = session.clip_at(cref).cloned() else {
        return;
    };
    let notes = clip.notes().to_vec();
    // A clip's own signature only changes how its bars are drawn.
    let sig = clip.signature.unwrap_or(session.project.time_signature);
    let color = rgb(clip.color);
    let track = cref.track();
    let playhead = session.clip_playhead(cref);
    let playing = playhead.is_some();
    let playhead = playhead.unwrap_or(0.0);
    let limit = content_limit(&clip);
    let now = ui.input(|i| i.time);

    // ---- regions
    let compact = area.height() < 210.0;
    let vel_h = if compact { 0.0 } else { VELOCITY_H };
    let ruler = Rect::from_min_max(
        Pos2::new(area.left() + KEYBOARD_W, area.top()),
        Pos2::new(area.right(), area.top() + RULER_H),
    );
    let marker_strip = Rect::from_min_max(
        Pos2::new(ruler.left(), ruler.bottom()),
        Pos2::new(ruler.right(), ruler.bottom() + MARKER_H),
    );
    let scrub_strip = Rect::from_min_max(
        Pos2::new(ruler.left(), marker_strip.bottom()),
        Pos2::new(ruler.right(), marker_strip.bottom() + SCRUB_H),
    );
    let vel = Rect::from_min_max(
        Pos2::new(area.left() + KEYBOARD_W, area.bottom() - vel_h),
        area.max,
    );
    let grid = Rect::from_min_max(
        Pos2::new(area.left() + KEYBOARD_W, scrub_strip.bottom()),
        Pos2::new(area.right(), vel.top()),
    );
    let keys = Rect::from_min_max(
        Pos2::new(area.left(), grid.top()),
        Pos2::new(grid.left(), grid.bottom()),
    );

    step_record_keys(ui, session, state, cref);
    let ps = &mut state.piano;
    ps.view_width = grid.width();
    // Fit the clip to the width the first time it is shown.
    let beats = limit as f32 / PPQ as f32;
    if ps.reset_view {
        ps.px_per_beat = ((grid.width() - 20.0) / beats.max(1.0)).clamp(14.0, 160.0);
        let center = if notes.is_empty() {
            60.0
        } else {
            notes.iter().map(|n| n.key as f32).sum::<f32>() / notes.len() as f32
        };
        ps.scroll = Vec2::new(0.0, (127.0 - center) * ps.key_h - grid.height() / 2.0);
        ps.reset_view = false;
    }

    // ---- wheel: scroll, Shift = horizontal, Cmd/Ctrl = horizontal zoom around the pointer
    if ui.rect_contains_pointer(Rect::from_min_max(keys.min, area.max)) {
        let (delta, shift, zoom_mod, pointer) = ui.input(|i| {
            (
                i.smooth_scroll_delta,
                i.modifiers.shift,
                i.modifiers.command,
                i.pointer.hover_pos(),
            )
        });
        if zoom_mod && delta.y != 0.0 {
            let px = pointer.map_or(grid.left(), |p| p.x);
            let t = (px - grid.left() + ps.scroll.x) / ps.px_per_beat;
            ps.px_per_beat = (ps.px_per_beat * (1.0 + delta.y * 0.004)).clamp(10.0, 400.0);
            ps.scroll.x = t * ps.px_per_beat - (px - grid.left());
        } else if shift {
            ps.scroll.x -= delta.y + delta.x;
        } else {
            ps.scroll.x -= delta.x;
            ps.scroll.y -= delta.y;
        }
    }
    if let Some(at) = session
        .clip_playhead(cref)
        .filter(|_| state.clip_view.follow)
    {
        let x = (at / PPQ as f64) as f32 * ps.px_per_beat;
        ps.scroll.x = super::arrangement::view::follow_scroll(
            ps.scroll.x,
            x,
            grid.width(),
            state.follow_scrolls,
        );
    }
    let content_w = (beats + 4.0) * ps.px_per_beat;
    ps.scroll.x = ps.scroll.x.clamp(0.0, (content_w - grid.width()).max(0.0));
    ps.scroll.y = ps
        .scroll
        .y
        .clamp(0.0, (128.0 * ps.key_h - grid.height()).max(0.0));
    let (ppb, kh, scroll) = (ps.px_per_beat, ps.key_h, ps.scroll);
    let to_x = |t: f64| grid.left() + (t / PPQ as f64) as f32 * ppb - scroll.x;
    let to_y = |k: u8| grid.top() + (127 - k) as f32 * kh - scroll.y;
    let note_rect = |n: &NoteData| {
        Rect::from_min_size(
            Pos2::new(to_x(n.start as f64), to_y(n.key)),
            Vec2::new(((n.length as f32 / PPQ as f32) * ppb).max(3.0), kh),
        )
    };

    let mut new_notes: Option<Vec<NoteData>> = None;
    let mut preview_on: Option<u8> = None;
    let mut preview_off = false;
    let mut from_keys = false;
    let mut deadline = 0.0;
    if ps.preview.is_some()
        && !ps.preview_from_keys
        && ps.drag.is_none()
        && now > ps.preview_deadline
    {
        preview_off = true;
    }

    // ================= painting
    // Grid rows
    {
        let p = ui.painter_at(grid);
        p.rect_filled(grid, 0.0, shade(0x2a2a2a));
        let first = ((scroll.y / kh).floor() as i32).max(0);
        let last = (((scroll.y + grid.height()) / kh).ceil() as i32).min(128);
        for row in first..last {
            let key = (127 - row) as u8;
            let y = to_y(key);
            let r = Rect::from_min_size(Pos2::new(grid.left(), y), Vec2::new(grid.width(), kh));
            if is_black(key) {
                p.rect_filled(r, 0.0, shade(0x242424));
            }
            if key.is_multiple_of(12) {
                p.line_segment(
                    [r.left_bottom(), r.right_bottom()],
                    Stroke::new(1.0, shade(0x171717)),
                );
            }
        }
        // Columns
        let bar = sig.ticks_per_bar();
        let step = if state.piano.fixed_grid {
            state.piano.grid.max(1)
        } else {
            ps_grid_step(state.piano.grid, ppb)
        };
        let first_t = ((scroll.x / ppb * PPQ as f32) as u64 / step) * step;
        let mut t = first_t;
        while to_x(t as f64) <= grid.right() {
            let x = to_x(t as f64);
            let (w, c) = if t.is_multiple_of(bar) {
                (1.0, pal().grid_line(shade(0x0f0f0f)))
            } else if t.is_multiple_of(PPQ) {
                (1.0, pal().grid_line(shade(0x1c1c1c)))
            } else {
                (1.0, pal().grid_line(shade(0x242424)))
            };
            p.line_segment(
                [Pos2::new(x, grid.top()), Pos2::new(x, grid.bottom())],
                Stroke::new(w, c),
            );
            t += step;
        }
        // What doesn't play is dimmed.
        let (from, to) = if clip.looped {
            (clip.start.min(clip.loop_start), clip.loop_end())
        } else {
            (clip.start, clip.end_marker())
        };
        for (a, b) in [
            (grid.left(), to_x(from as f64)),
            (to_x(to as f64), grid.right()),
        ] {
            if b > a {
                p.rect_filled(
                    Rect::from_x_y_ranges(a.max(grid.left())..=b.min(grid.right()), grid.y_range()),
                    0.0,
                    Color32::from_black_alpha(100),
                );
            }
        }
        // Notes
        let live: &[NoteData] = new_notes.as_deref().unwrap_or(&notes);
        for (i, n) in live.iter().enumerate() {
            let r = note_rect(n).shrink2(Vec2::new(0.5, 0.5));
            if !r.intersects(grid) {
                continue;
            }
            let sel = state.piano.selected.contains(&i);
            let fill = if sel {
                shade(0xb5e0ff)
            } else {
                color.gamma_multiply(0.45 + 0.55 * n.velocity)
            };
            p.rect_filled(r, 1.0, fill);
            p.rect_stroke(
                r,
                1.0,
                Stroke::new(1.0, Color32::from_black_alpha(170)),
                StrokeKind::Inside,
            );
        }
        if playing {
            let x = to_x(playhead);
            p.line_segment(
                [Pos2::new(x, grid.top()), Pos2::new(x, grid.bottom())],
                Stroke::new(1.0, pal().play_green),
            );
        }
    }
    let ps = &mut state.piano;

    // Ruler with bar numbers and loop brace
    {
        let p = ui.painter_at(ruler);
        p.rect_filled(ruler, 0.0, shade(0x333333));
        let bar = sig.ticks_per_bar();
        let mut b = (scroll.x / ppb * PPQ as f32) as u64 / bar;
        let label_every = if bar as f32 / PPQ as f32 * ppb < 40.0 {
            4
        } else {
            1
        };
        while to_x((b * bar) as f64) <= ruler.right() {
            let x = to_x((b * bar) as f64);
            p.line_segment(
                [
                    Pos2::new(x, ruler.bottom() - 6.0),
                    Pos2::new(x, ruler.bottom()),
                ],
                Stroke::new(1.0, pal().text_dim),
            );
            if b.is_multiple_of(label_every) {
                p.text(
                    Pos2::new(x + 3.0, ruler.top() + 2.0),
                    Align2::LEFT_TOP,
                    format!("{}", b + 1),
                    theme::font(10.0),
                    pal().text_dim,
                );
            }
            b += 1;
        }
    }
    let view = Axis {
        left: grid.left(),
        width: grid.width(),
        start: (scroll.x / ppb * PPQ as f32) as f64,
        ticks: (grid.width() / ppb * PPQ as f32) as f64,
    };
    let snap_grid = ps.grid;
    markers::marker_strip(ui, marker_strip, session, state, cref, &view, snap_grid);
    markers::scrub_strip(ui, scrub_strip, session, state, cref, &view);
    let ps = &mut state.piano;

    // Keyboard
    {
        let p = ui.painter_at(keys);
        p.rect_filled(keys, 0.0, shade(0x202020));
        let first = ((scroll.y / kh).floor() as i32).max(0);
        let last = (((scroll.y + grid.height()) / kh).ceil() as i32).min(128);
        for row in first..last {
            let key = (127 - row) as u8;
            let y = to_y(key);
            let r = Rect::from_min_size(Pos2::new(keys.left(), y), Vec2::new(KEYBOARD_W, kh));
            let held = ps.preview == Some(key);
            let fill = if held {
                pal().accent
            } else if is_black(key) {
                hex(0x1a1a1a)
            } else {
                hex(0xd8d8d8)
            };
            let kr = if is_black(key) {
                Rect::from_min_size(r.min, Vec2::new(KEYBOARD_W * 0.62, kh))
            } else {
                r
            };
            p.rect_filled(kr.shrink2(Vec2::new(0.0, 0.5)), 0.0, fill);
            if key.is_multiple_of(12) {
                p.text(
                    Pos2::new(r.right() - 3.0, r.center().y),
                    Align2::RIGHT_CENTER,
                    rlive_core::note_name(key),
                    theme::font(9.0),
                    hex(0x404040),
                );
            }
        }
        let kresp = ui.interact(
            keys,
            ui.scope_id().with("piano_keys"),
            Sense::click_and_drag(),
        );
        if kresp.is_pointer_button_down_on() {
            if let Some(pos) = kresp.interact_pointer_pos() {
                let key = (127.0 - ((pos.y - grid.top() + scroll.y) / kh).floor()).clamp(0.0, 127.0)
                    as u8;
                if ps.preview != Some(key) {
                    preview_off = ps.preview.is_some();
                    preview_on = Some(key);
                    from_keys = true;
                }
            }
        } else if ps.preview.is_some() && ps.preview_from_keys {
            preview_off = true;
        }
        info(kresp, "Piano Keys — click keys to preview notes");
    }
    // Corner above the keyboard
    ui.painter().rect_filled(
        Rect::from_min_max(area.min, Pos2::new(grid.left(), scrub_strip.bottom())),
        0.0,
        shade(0x333333),
    );

    // ================= grid interaction
    let resp = ui.interact(
        grid,
        ui.scope_id().with("note_grid"),
        Sense::click_and_drag(),
    );
    let pos_to_tick_key = |pos: Pos2| {
        (
            ((pos.x - grid.left() + scroll.x) / ppb * PPQ as f32) as f64,
            (127.0 - ((pos.y - grid.top() + scroll.y) / kh).floor()).clamp(0.0, 127.0) as u8,
        )
    };
    let hit = |pos: Pos2| -> Option<(usize, bool)> {
        notes.iter().enumerate().rev().find_map(|(i, n)| {
            let r = note_rect(n);
            r.contains(pos)
                .then(|| (i, pos.x > r.right() - 6.0_f32.min(r.width() * 0.4)))
        })
    };
    if let Some(pos) = resp.hover_pos() {
        if let Some((_, edge)) = hit(pos) {
            ui.ctx().set_cursor_icon(if edge {
                CursorIcon::ResizeHorizontal
            } else {
                CursorIcon::Grab
            });
        }
    }
    let (shift, no_snap) = ui.input(|i| {
        (
            i.modifiers.shift,
            keymap::ignores_grid(i.modifiers, Platform::current()),
        )
    });
    let no_snap = no_snap || ps.snap_off;
    if resp.double_clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            match hit(pos) {
                Some((i, _)) => {
                    let mut v = notes.clone();
                    v.remove(i);
                    ps.selected.clear();
                    new_notes = Some(v);
                }
                None => {
                    let (t, key) = pos_to_tick_key(pos);
                    let start = (t / ps.grid as f64).floor().max(0.0) as u64 * ps.grid;
                    if start < limit {
                        let mut v = notes.clone();
                        v.push(NoteData::new(
                            start,
                            ps.last_length.max(ps.grid / 2),
                            key,
                            ps.new_velocity,
                        ));
                        ps.selected = vec![v.len() - 1];
                        new_notes = Some(v);
                        preview_on = Some(key);
                        deadline = now + 0.3;
                    }
                }
            }
        }
    } else if resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            match hit(pos) {
                Some((i, _)) if shift => {
                    if let Some(k) = ps.selected.iter().position(|s| *s == i) {
                        ps.selected.remove(k);
                    } else {
                        ps.selected.push(i);
                    }
                }
                Some((i, _)) => ps.selected = vec![i],
                None => {
                    ps.selected.clear();
                    let t = pos_to_tick_key(pos).0;
                    ps.insert_tick = Some(snap(t, ps.grid, !no_snap).max(0) as u64);
                }
            }
        }
    }
    if resp.drag_started() {
        if let Some(pos) = resp.interact_pointer_pos() {
            if let Some((i, edge)) = hit(pos) {
                if !ps.selected.contains(&i) {
                    ps.selected = vec![i];
                }
                ps.drag = Some(NoteDrag {
                    kind: if edge {
                        DragKind::Resize
                    } else {
                        DragKind::Move
                    },
                    origin: pos,
                    originals: ps
                        .selected
                        .iter()
                        .filter_map(|&s| notes.get(s).map(|n| (s, n.clone())))
                        .collect(),
                });
                if !edge {
                    preview_on = Some(notes[i].key);
                    deadline = f64::MAX;
                }
            }
        }
    }
    if let (Some(drag), Some(pos)) = (&ps.drag, resp.interact_pointer_pos()) {
        if resp.dragged() && matches!(drag.kind, DragKind::Move | DragKind::Resize) {
            let d = pos - drag.origin;
            let dt = snap((d.x / ppb * PPQ as f32) as f64, ps.grid, !no_snap);
            let dk = -(d.y / kh).round() as i32;
            let mut v = notes.clone();
            for (i, orig) in &drag.originals {
                if let Some(n) = v.get_mut(*i) {
                    match drag.kind {
                        DragKind::Move => {
                            n.start = (orig.start as i64 + dt)
                                .clamp(0, limit.saturating_sub(1) as i64)
                                as u64;
                            n.key = (orig.key as i32 + dk).clamp(0, 127) as u8;
                        }
                        _ => {
                            let min = if no_snap { 10 } else { ps.grid.max(1) as i64 };
                            n.length = (orig.length as i64 + dt).max(min) as u64;
                            ps.last_length = n.length;
                        }
                    }
                }
            }
            if let Some(nm) = session.clip_at_mut(cref).and_then(|c| c.notes_mut()) {
                *nm = v;
            }
        }
    }
    if resp.drag_stopped() && ps.drag.take().is_some() {
        session.sync_ref(cref);
        preview_off = true;
    }
    resp.context_menu(|ui| {
        crate::ui::clip_view::crop_menu(ui, session, state, cref);
    });
    info(resp, "Note Editor — double-click to add or remove notes · drag to move (Cmd = no snap) · drag the right edge to resize · Cmd+wheel zooms");

    // ================= velocity lane
    if vel_h > 0.0 {
        let p = ui.painter_at(vel);
        p.rect_filled(vel, 0.0, shade(0x262626));
        p.line_segment(
            [vel.left_top(), vel.right_top()],
            Stroke::new(1.0, pal().separator),
        );
        let inner = vel.shrink2(Vec2::new(0.0, 4.0));
        let live: Vec<NoteData> = session
            .clip_at(cref)
            .map(|c| c.notes().to_vec())
            .unwrap_or_default();
        for (i, n) in live.iter().enumerate() {
            let x = to_x(n.start as f64) + 1.0;
            if x < vel.left() - 4.0 || x > vel.right() + 4.0 {
                continue;
            }
            let y = inner.bottom() - inner.height() * n.velocity;
            let sel = state.piano.selected.contains(&i);
            let c = if sel { shade(0xb5e0ff) } else { color };
            p.line_segment(
                [Pos2::new(x, inner.bottom()), Pos2::new(x, y)],
                Stroke::new(1.5, c),
            );
            p.circle_filled(Pos2::new(x, y), 2.5, c);
        }
        let ps = &mut state.piano;
        let vr = ui.interact(vel, ui.scope_id().with("velocity"), Sense::click_and_drag());
        if vr.drag_started() {
            if let Some(pos) = vr.interact_pointer_pos() {
                let target = live
                    .iter()
                    .enumerate()
                    .filter(|(_, n)| (to_x(n.start as f64) - pos.x).abs() < 5.0)
                    .map(|(i, _)| i)
                    .collect::<Vec<_>>();
                if !target.is_empty() {
                    let set: Vec<usize> = if target.iter().any(|t| ps.selected.contains(t)) {
                        ps.selected.clone()
                    } else {
                        target
                    };
                    ps.drag = Some(NoteDrag {
                        kind: DragKind::Velocity,
                        origin: pos,
                        originals: set
                            .iter()
                            .filter_map(|&s| live.get(s).map(|n| (s, n.clone())))
                            .collect(),
                    });
                }
            }
        }
        if let (Some(d), Some(pos)) = (&ps.drag, vr.interact_pointer_pos()) {
            if d.kind == DragKind::Velocity && vr.dragged() {
                let dv = -(pos.y - d.origin.y) / inner.height();
                let mut v = live.clone();
                for (i, orig) in &d.originals {
                    if let Some(n) = v.get_mut(*i) {
                        n.velocity = (orig.velocity + dv).clamp(0.02, 1.0);
                    }
                }
                if let Some(nm) = session.clip_at_mut(cref).and_then(|c| c.notes_mut()) {
                    *nm = v;
                }
            }
        }
        if vr.drag_stopped() && ps.drag.take().is_some() {
            session.sync_ref(cref);
        }
        info(
            vr,
            "Velocity — drag a stem up/down (selected notes move together)",
        );
    }

    // ================= apply edits
    if let Some(v) = new_notes {
        if let Some(n) = session.clip_at_mut(cref).and_then(|c| c.notes_mut()) {
            *n = v;
        }
        session.sync_ref(cref);
    }
    if preview_off {
        if let Some(k) = state.piano.preview.take() {
            session.midi_in(
                track,
                MidiMessage::NoteOff {
                    channel: 0,
                    key: k,
                    velocity: 0.5,
                    note_id: 0,
                },
            );
        }
    }
    if let Some(k) = preview_on {
        state.piano.preview_from_keys = from_keys;
        state.piano.preview_deadline = deadline;
        if let Some(old) = state.piano.preview.replace(k) {
            session.midi_in(
                track,
                MidiMessage::NoteOff {
                    channel: 0,
                    key: old,
                    velocity: 0.5,
                    note_id: 0,
                },
            );
        }
        session.midi_in(
            track,
            MidiMessage::NoteOn {
                channel: 0,
                key: k,
                velocity: state.piano.new_velocity,
                note_id: 0,
                tuning: 0.0,
            },
        );
    }
}

/// Grid line step that keeps lines at least ~8 px apart.
fn ps_grid_step(grid: u64, ppb: f32) -> u64 {
    let mut step = grid.max(1);
    while (step as f32 / PPQ as f32) * ppb < 8.0 {
        step *= 2;
    }
    step
}
