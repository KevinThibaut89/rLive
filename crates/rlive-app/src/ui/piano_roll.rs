//! Clip view, styled after Live: a "Clip" properties box on the left and the note editor
//! (ruler with loop brace, piano keys, note grid, velocity lane) on the right.

use super::clip_envelopes;
use super::keymap::{self, Platform};
use super::sample_editor;
use super::theme::{self, *};
use super::timeline::snap;
use super::widgets::{self, info, rgb};
use super::{BrowserItem, UiState};
use crate::session::{ClipRef, Session};
use egui::{
    Align2, Color32, CursorIcon, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, UiBuilder, Vec2,
};
use rlive_core::{MidiMessage, PPQ};
use rlive_project::{ClipContent, NoteData};
use std::sync::atomic::Ordering;

const KEYBOARD_W: f32 = 48.0;
const RULER_H: f32 = 20.0;
const VELOCITY_H: f32 = 52.0;
const CLIP_BOX_W: f32 = 176.0;

#[derive(Clone, Copy, PartialEq)]
enum DragKind {
    Move,
    Resize,
    Velocity,
    LoopEnd,
}

struct NoteDrag {
    kind: DragKind,
    origin: Pos2,
    originals: Vec<(usize, NoteData)>,
    length: u64,
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
    let Some(length) = session.clip_at(cref).map(|c| c.length) else {
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

/// Where the song position is inside a timeline clip, while it's playing through it.
fn arrangement_position(session: &Session, cref: ClipRef) -> Option<f64> {
    let ClipRef::Arr { track, id } = cref else {
        return None;
    };
    if !session.is_playing() {
        return None;
    }
    let c = session.project.track(track)?.arr_clip(id)?;
    let tick = session.engine.shared.tick.load();
    if tick < c.start as f64 || tick >= c.end() as f64 {
        return None;
    }
    let local = c.offset as f64 + tick - c.start as f64;
    Some(if c.clip.looped && c.clip.length > 0 {
        local % c.clip.length as f64
    } else {
        local
    })
}

fn is_black(key: u8) -> bool {
    matches!(key % 12, 1 | 3 | 6 | 8 | 10)
}

fn grid_label(g: u64) -> String {
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

pub fn show(ui: &mut egui::Ui, session: &mut Session, state: &mut UiState) {
    let Some(cref) = state.selected_clip else {
        return;
    };
    let Some(clip) = session.clip_at(cref).cloned() else {
        state.selected_clip = None;
        return;
    };
    let full = ui.max_rect();
    // Claim the whole panel so egui keeps its size (children don't advance the cursor).
    ui.expand_to_include_rect(full);
    ui.painter().rect_filled(full, 0.0, pal().panel_dark);
    let narrow = full.width() < 760.0;
    let box_w = if narrow { 140.0 } else { CLIP_BOX_W };
    let boxr = Rect::from_min_size(full.min, Vec2::new(box_w, full.height()));
    let mut bu = ui.new_child(UiBuilder::new().max_rect(boxr.shrink(6.0)));
    bu.set_clip_rect(boxr.intersect(ui.clip_rect()));
    ui.painter().rect_filled(boxr, 0.0, pal().panel);
    ui.painter().line_segment(
        [boxr.right_top(), boxr.right_bottom()],
        Stroke::new(1.0, pal().separator),
    );
    clip_box(&mut bu, session, state, cref);

    let editor = Rect::from_min_max(Pos2::new(boxr.right() + 1.0, full.top()), full.max);
    let tabs = Rect::from_min_size(
        editor.min + Vec2::new(8.0, 3.0),
        Vec2::new(editor.width() - 16.0, 17.0),
    );
    clip_envelopes::tab_row(ui, tabs, clip.is_midi(), &mut state.env);
    let body = Rect::from_min_max(Pos2::new(editor.left(), tabs.bottom() + 2.0), editor.max);
    if state.env.open {
        clip_envelopes::show(ui, session, state, cref, body);
    } else {
        match &clip.content {
            ClipContent::Audio { file, .. } => {
                let mut eu = ui.new_child(UiBuilder::new().max_rect(body.shrink(8.0)));
                sample_editor::show(&mut eu, session, state, cref, &file.path);
            }
            ClipContent::Midi { .. } => note_editor(ui, body, session, state, cref),
        }
    }
    drop_sample_on_clip(ui, session, cref, full);
}

/// Dropping a sample onto the Clip View puts it in the clip, keeping every clip setting and the
/// envelopes (manual p. 537).
fn drop_sample_on_clip(ui: &mut egui::Ui, session: &mut Session, cref: ClipRef, area: Rect) {
    let id = ui.scope_id().with("clip_view_drop");
    let resp = ui.interact(area, id, Sense::hover());
    if let Some(item) = resp.dnd_release_payload::<BrowserItem>() {
        if let BrowserItem::Sample(path) = &*item {
            session.replace_clip_sample(cref, path);
        }
    }
}

fn clip_box(ui: &mut egui::Ui, session: &mut Session, state: &mut UiState, cref: ClipRef) {
    let sig = session.project.time_signature;
    let bar_ticks = sig.ticks_per_bar();
    let mut changed = false;
    widgets::section_label(ui, "CLIP");
    let is_midi = session.clip_at(cref).is_some_and(|c| c.is_midi());
    if let Some(c) = session.clip_at_mut(cref) {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(Vec2::new(12.0, 18.0), Sense::hover());
            ui.painter()
                .rect_filled(r.shrink2(Vec2::new(0.0, 3.0)), 1.0, rgb(c.color));
            let resp = ui.add(egui::TextEdit::singleline(&mut c.name).desired_width(f32::INFINITY));
            info(resp, "Clip Name — the name shown on the clip");
        });
        egui::Grid::new("clip_props")
            .num_columns(2)
            .spacing([6.0, 3.0])
            .show(ui, |ui| {
                ui.label(RichText::new("Length").color(pal().text_dim));
                let mut bars = c.length as f64 / bar_ticks as f64;
                let r = ui.add(
                    egui::DragValue::new(&mut bars)
                        .range(0.25..=256.0)
                        .speed(0.05)
                        .max_decimals(2)
                        .suffix(" bars"),
                );
                if r.changed() {
                    let beat = sig.ticks_per_beat() as f64;
                    c.length = (((bars * bar_ticks as f64) / beat).round() * beat).max(beat) as u64;
                    changed = true;
                }
                info(
                    r,
                    "Loop Length — in bars; you can also drag the loop end in the ruler",
                );
                ui.end_row();
                ui.label(RichText::new("Loop").color(pal().text_dim));
                let r = widgets::state_button(
                    ui,
                    Vec2::new(44.0, 16.0),
                    c.looped,
                    pal().accent,
                    if c.looped { "On" } else { "Off" },
                );
                if info(r, "Loop — repeat the clip, or play it once").clicked() {
                    c.looped = !c.looped;
                    changed = true;
                }
                ui.end_row();
            });
    }
    if is_midi {
        ui.add_space(6.0);
        widgets::section_label(ui, "NOTES");
        egui::Grid::new("note_props")
            .num_columns(2)
            .spacing([6.0, 3.0])
            .show(ui, |ui| {
                ui.label(RichText::new("Grid").color(pal().text_dim));
                let r = egui::ComboBox::from_id_salt("grid")
                    .width(62.0)
                    .selected_text(grid_label(state.piano.grid))
                    .show_ui(ui, |ui| {
                        for g in [PPQ, PPQ / 2, PPQ / 4, PPQ / 8, PPQ / 3, PPQ / 6] {
                            ui.selectable_value(&mut state.piano.grid, g, grid_label(g));
                        }
                    });
                info(
                    r.response,
                    "Grid — for drawing and snapping notes (hold the grid-bypass key to ignore)",
                );
                ui.end_row();
                ui.label(RichText::new("Velocity").color(pal().text_dim));
                let r = ui.add(
                    egui::DragValue::new(&mut state.piano.new_velocity)
                        .range(0.05..=1.0)
                        .speed(0.01)
                        .fixed_decimals(2),
                );
                info(r, "New Note Velocity — how hard new notes are played");
                ui.end_row();
            });
        ui.horizontal_wrapped(|ui| {
            let r = ui.button("Duplicate Loop");
            if info(
                r,
                "Duplicate Loop — double the clip length and copy its notes",
            )
            .clicked()
            {
                if let Some(c) = session.clip_at_mut(cref) {
                    let len = c.length;
                    if let Some(notes) = c.notes_mut() {
                        let copies: Vec<NoteData> = notes
                            .iter()
                            .map(|n| NoteData {
                                start: n.start + len,
                                ..n.clone()
                            })
                            .collect();
                        notes.extend(copies);
                    }
                    c.length = len * 2;
                    changed = true;
                }
            }
            let r = ui.button("Quantize");
            if info(r, "Quantize — snap selected, or all, notes to the grid").clicked() {
                let grid = state.piano.grid;
                let sel = state.piano.selected.clone();
                if let Some(notes) = session.clip_at_mut(cref).and_then(|c| c.notes_mut()) {
                    for (i, n) in notes.iter_mut().enumerate() {
                        if sel.is_empty() || sel.contains(&i) {
                            n.start = snap(n.start as f64, grid, true).max(0) as u64;
                        }
                    }
                    changed = true;
                }
            }
        });
    }
    if changed {
        session.sync_ref(cref);
    }
}

fn note_editor(
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
    let sig = session.project.time_signature;
    let color = rgb(clip.color);
    let track = cref.track();
    let shared = session.track_shared.get(&track).cloned();
    let arr_position = arrangement_position(session, cref);
    let playing = arr_position.is_some()
        || shared.as_ref().is_some_and(|s| {
            cref.as_slot()
                .is_some_and(|(_, slot)| s.playing_slot.load(Ordering::Relaxed) == slot as i32)
        });
    let playhead =
        arr_position.unwrap_or_else(|| shared.as_ref().map_or(0.0, |s| s.clip_position.load()));
    let now = ui.input(|i| i.time);

    // ---- regions
    let compact = area.height() < 210.0;
    let vel_h = if compact { 0.0 } else { VELOCITY_H };
    let ruler = Rect::from_min_max(
        Pos2::new(area.left() + KEYBOARD_W, area.top()),
        Pos2::new(area.right(), area.top() + RULER_H),
    );
    let vel = Rect::from_min_max(
        Pos2::new(area.left() + KEYBOARD_W, area.bottom() - vel_h),
        area.max,
    );
    let grid = Rect::from_min_max(
        Pos2::new(area.left() + KEYBOARD_W, ruler.bottom()),
        Pos2::new(area.right(), vel.top()),
    );
    let keys = Rect::from_min_max(
        Pos2::new(area.left(), grid.top()),
        Pos2::new(grid.left(), grid.bottom()),
    );

    step_record_keys(ui, session, state, cref);
    let ps = &mut state.piano;
    // Fit the clip to the width the first time it is shown.
    let beats = clip.length as f32 / PPQ as f32;
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
    let mut new_length: Option<u64> = None;
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
        let step = ps_grid_step(state.piano.grid, ppb);
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
        // Outside the loop
        let end = to_x(clip.length as f64);
        if end < grid.right() {
            p.rect_filled(
                Rect::from_min_max(Pos2::new(end, grid.top()), grid.max),
                0.0,
                Color32::from_black_alpha(100),
            );
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
        let (x0, x1) = (to_x(0.0), to_x(clip.length as f64));
        let brace = Rect::from_min_max(
            Pos2::new(x0, ruler.bottom() - 5.0),
            Pos2::new(x1, ruler.bottom() - 1.0),
        );
        p.rect_filled(
            brace,
            0.0,
            if clip.looped {
                shade(0x9a9a9a)
            } else {
                shade(0x5a5a5a)
            },
        );
        let handle =
            Rect::from_center_size(Pos2::new(x1, ruler.center().y), Vec2::new(8.0, RULER_H));
        let hr = ui.interact(handle, ui.scope_id().with("loop_end"), Sense::drag());
        p.rect_filled(
            Rect::from_center_size(Pos2::new(x1, ruler.bottom() - 7.0), Vec2::new(4.0, 12.0)),
            1.0,
            if hr.hovered() || hr.dragged() {
                pal().accent
            } else {
                shade(0xcfcfcf)
            },
        );
        if hr.hovered() || hr.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        }
        if hr.drag_started() {
            ps.drag = Some(NoteDrag {
                kind: DragKind::LoopEnd,
                origin: hr.interact_pointer_pos().unwrap_or_default(),
                originals: Vec::new(),
                length: clip.length,
            });
        }
        if let (Some(d), Some(pos)) = (&ps.drag, hr.interact_pointer_pos()) {
            if d.kind == DragKind::LoopEnd && hr.dragged() {
                let beat = sig.ticks_per_beat() as i64;
                let dt = ((pos.x - d.origin.x) / ppb * PPQ as f32) as i64;
                let len = ((d.length as i64 + dt) as f64 / beat as f64).round() as i64 * beat;
                new_length = Some(len.max(beat) as u64);
            }
        }
        if hr.drag_stopped() {
            ps.drag = None;
            session.sync_ref(cref);
        }
        info(hr, "Loop end — drag to change the clip length");
    }

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
        Rect::from_min_max(area.min, Pos2::new(grid.left(), ruler.bottom())),
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
                    if start < clip.length {
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
                    length: clip.length,
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
                                .clamp(0, clip.length.saturating_sub(1) as i64)
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
                        length: clip.length,
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
    if let Some(len) = new_length {
        if let Some(c) = session.clip_at_mut(cref) {
            c.length = len;
            session.dirty = true;
        }
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
