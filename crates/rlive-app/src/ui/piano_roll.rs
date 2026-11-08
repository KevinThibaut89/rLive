//! MIDI clip editor (piano roll) and audio clip properties.

use super::widgets::{rgb, ACCENT, PLAY_GREEN, TEXT_DIM};
use super::UiState;
use crate::session::Session;
use egui::{Align2, Color32, FontId, Key, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use rlive_core::{MidiMessage, TrackId, PPQ};
use rlive_project::{ClipContent, NoteData};
use std::sync::atomic::Ordering;

const KEYBOARD_W: f32 = 46.0;

#[derive(Clone, Copy, PartialEq)]
enum DragKind {
    Move,
    Resize,
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
    pub new_velocity: f32,
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
            new_velocity: 0.8,
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

fn is_black(key: u8) -> bool {
    matches!(key % 12, 1 | 3 | 6 | 8 | 10)
}

fn snap(ticks: f64, grid: u64, enabled: bool) -> i64 {
    if !enabled || grid == 0 {
        return ticks.round() as i64;
    }
    ((ticks / grid as f64).round() as i64) * grid as i64
}

pub fn show(ui: &mut egui::Ui, session: &mut Session, state: &mut UiState) {
    let Some((track, slot)) = state.selected_clip else {
        return;
    };
    let Some(clip) = session.clip(track, slot).cloned() else {
        state.selected_clip = None;
        return;
    };
    let sig = session.project.time_signature;
    let bar_ticks = sig.ticks_per_bar();

    // ---------------- header
    let mut changed = false;
    ui.horizontal(|ui| {
        if ui.small_button("< Devices").clicked() {
            state.detail = super::Detail::Devices;
        }
        let swatch = rgb(clip.color);
        let (r, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
        ui.painter().rect_filled(r, 2.0, swatch);
        if let Some(c) = session.clip_mut(track, slot) {
            ui.add(egui::TextEdit::singleline(&mut c.name).desired_width(120.0));
            ui.separator();
            ui.label("Length");
            let mut bars = c.length as f64 / bar_ticks as f64;
            if ui
                .add(
                    egui::DragValue::new(&mut bars)
                        .range(0.25..=256.0)
                        .speed(0.05)
                        .max_decimals(2)
                        .suffix(" bars"),
                )
                .changed()
            {
                let beat = sig.ticks_per_beat() as f64;
                c.length = (((bars * bar_ticks as f64) / beat).round() * beat).max(beat) as u64;
                changed = true;
            }
            changed |= ui.checkbox(&mut c.looped, "Loop").changed();
        }
        if clip.is_midi() {
            ui.separator();
            ui.label("Grid");
            egui::ComboBox::from_id_salt("grid")
                .width(60.0)
                .selected_text(grid_label(state.piano.grid))
                .show_ui(ui, |ui| {
                    for g in [PPQ, PPQ / 2, PPQ / 4, PPQ / 8, PPQ / 3, PPQ / 6] {
                        ui.selectable_value(&mut state.piano.grid, g, grid_label(g));
                    }
                });
            ui.label("Vel");
            ui.add(
                egui::DragValue::new(&mut state.piano.new_velocity)
                    .range(0.05..=1.0)
                    .speed(0.01)
                    .fixed_decimals(2),
            );
            ui.label("Zoom");
            ui.add(egui::Slider::new(&mut state.piano.px_per_beat, 16.0..=240.0).show_value(false));
            if ui
                .button("Duplicate Loop")
                .on_hover_text("Double the clip length and copy its notes")
                .clicked()
            {
                if let Some(c) = session.clip_mut(track, slot) {
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
            if ui
                .button("Quantize")
                .on_hover_text("Snap selected (or all) notes to the grid")
                .clicked()
            {
                let grid = state.piano.grid;
                let sel = state.piano.selected.clone();
                if let Some(notes) = session.clip_mut(track, slot).and_then(|c| c.notes_mut()) {
                    for (i, n) in notes.iter_mut().enumerate() {
                        if sel.is_empty() || sel.contains(&i) {
                            n.start = snap(n.start as f64, grid, true).max(0) as u64;
                        }
                    }
                    changed = true;
                }
            }
        }
    });
    if changed {
        session.sync_clip(track, slot);
    }

    match &clip.content {
        ClipContent::Audio { path, gain_db } => {
            audio_clip_panel(ui, session, track, slot, path, *gain_db);
        }
        ClipContent::Midi { .. } => piano(ui, session, state, track, slot),
    }
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

fn audio_clip_panel(
    ui: &mut egui::Ui,
    session: &mut Session,
    track: TrackId,
    slot: usize,
    path: &std::path::Path,
    gain_db: f32,
) {
    ui.add_space(8.0);
    ui.label(RichText::new(path.display().to_string()).color(TEXT_DIM));
    let mut g = gain_db;
    ui.horizontal(|ui| {
        ui.label("Clip gain");
        if ui
            .add(egui::Slider::new(&mut g, -36.0..=12.0).suffix(" dB"))
            .changed()
        {
            if let Some(c) = session.clip_mut(track, slot) {
                if let ClipContent::Audio { gain_db, .. } = &mut c.content {
                    *gain_db = g;
                }
            }
            session.sync_clip(track, slot);
        }
    });
    // Waveform overview.
    if let Ok(sample) = session.samples.get(path) {
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 90.0), Sense::hover());
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 3.0, Color32::from_gray(28));
        let n = sample.len().max(1);
        let cols = rect.width() as usize;
        let color = session
            .clip(track, slot)
            .map(|c| rgb(c.color))
            .unwrap_or(ACCENT);
        for x in 0..cols {
            let a = x * n / cols;
            let b = ((x + 1) * n / cols).max(a + 1).min(n);
            let peak = sample.left[a..b].iter().fold(0f32, |m, s| m.max(s.abs()));
            let h = peak.min(1.0) * rect.height() * 0.5;
            let cx = rect.left() + x as f32;
            p.line_segment(
                [
                    Pos2::new(cx, rect.center().y - h),
                    Pos2::new(cx, rect.center().y + h),
                ],
                Stroke::new(1.0, color),
            );
        }
        ui.label(
            RichText::new(format!(
                "{:.2} s · {} Hz · warp off (plays at original speed)",
                sample.duration_secs(),
                sample.sample_rate
            ))
            .small()
            .color(TEXT_DIM),
        );
    }
}

fn piano(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    track: TrackId,
    slot: usize,
) {
    let Some(clip) = session.clip(track, slot).cloned() else {
        return;
    };
    let notes = clip.notes().to_vec();
    let ps = &mut state.piano;
    let ppb = ps.px_per_beat;
    let kh = ps.key_h;
    let beats = clip.length as f32 / PPQ as f32;
    let width = (beats * ppb).max(ui.available_width() - KEYBOARD_W - 20.0) + ppb;
    let height = 128.0 * kh;
    let color = rgb(clip.color);
    let sig = session.project.time_signature;
    let shared = session.track_shared.get(&track).cloned();
    let playing = shared
        .as_ref()
        .is_some_and(|s| s.playing_slot.load(Ordering::Relaxed) == slot as i32);
    let playhead = shared.as_ref().map_or(0.0, |s| s.clip_position.load());

    // Keyboard shortcuts.
    let (del, select_all) = ui.input(|i| {
        (
            i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace),
            i.modifiers.command && i.key_pressed(Key::A),
        )
    });
    let typing = ui.ctx().egui_wants_keyboard_input();
    let mut new_notes: Option<Vec<NoteData>> = None;
    if !typing && del && !ps.selected.is_empty() {
        let sel = std::mem::take(&mut ps.selected);
        new_notes = Some(
            notes
                .iter()
                .enumerate()
                .filter(|(i, _)| !sel.contains(i))
                .map(|(_, n)| n.clone())
                .collect(),
        );
    }
    if !typing && select_all {
        ps.selected = (0..notes.len()).collect();
    }

    let mut scroll = egui::ScrollArea::vertical()
        .id_salt("piano_v")
        .auto_shrink([false, false]);
    if ps.reset_view {
        // Center on the notes (or middle C).
        let center_key = if notes.is_empty() {
            60.0
        } else {
            notes.iter().map(|n| n.key as f32).sum::<f32>() / notes.len() as f32
        };
        scroll = scroll.vertical_scroll_offset(((127.0 - center_key) * kh - 80.0).max(0.0));
        ps.reset_view = false;
    }
    let mut preview_on: Option<u8> = None;
    let mut preview_off = false;
    let mut from_keys = false;
    let mut deadline = 0.0;
    let now = ui.input(|i| i.time);
    if ps.preview.is_some()
        && !ps.preview_from_keys
        && ps.drag.is_none()
        && now > ps.preview_deadline
    {
        preview_off = true;
    }
    scroll.show(ui, |ui| {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            // ---------- keyboard
            let (krect, kresp) =
                ui.allocate_exact_size(Vec2::new(KEYBOARD_W, height), Sense::click_and_drag());
            let p = ui.painter_at(krect);
            for key in 0..128u8 {
                let y = krect.top() + (127 - key) as f32 * kh;
                let r = Rect::from_min_size(Pos2::new(krect.left(), y), Vec2::new(KEYBOARD_W, kh));
                let held = ps.preview == Some(key);
                let fill = if held {
                    ACCENT
                } else if is_black(key) {
                    Color32::from_gray(30)
                } else {
                    Color32::from_gray(200)
                };
                p.rect_filled(r.shrink2(Vec2::new(0.0, 0.5)), 0.0, fill);
                if key % 12 == 0 {
                    p.text(
                        Pos2::new(r.right() - 3.0, r.center().y),
                        Align2::RIGHT_CENTER,
                        rlive_core::note_name(key),
                        FontId::proportional(9.0),
                        Color32::from_gray(40),
                    );
                }
            }
            if kresp.is_pointer_button_down_on() {
                if let Some(pos) = kresp.interact_pointer_pos() {
                    let key =
                        (127.0 - ((pos.y - krect.top()) / kh).floor()).clamp(0.0, 127.0) as u8;
                    if ps.preview != Some(key) {
                        preview_off = ps.preview.is_some();
                        preview_on = Some(key);
                        from_keys = true;
                    }
                }
            } else if ps.preview.is_some() && ps.preview_from_keys {
                preview_off = true;
            }

            // ---------- grid
            egui::ScrollArea::horizontal()
                .id_salt("piano_h")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let (rect, resp) =
                        ui.allocate_exact_size(Vec2::new(width, height), Sense::click_and_drag());
                    let p = ui.painter_at(rect);
                    let to_x = |t: f64| rect.left() + (t / PPQ as f64) as f32 * ppb;
                    let to_y = |k: u8| rect.top() + (127 - k) as f32 * kh;
                    // Rows
                    for key in 0..128u8 {
                        let r = Rect::from_min_size(
                            Pos2::new(rect.left(), to_y(key)),
                            Vec2::new(width, kh),
                        );
                        p.rect_filled(
                            r,
                            0.0,
                            if is_black(key) {
                                Color32::from_gray(36)
                            } else {
                                Color32::from_gray(44)
                            },
                        );
                        if key % 12 == 0 {
                            p.line_segment(
                                [r.left_bottom(), r.right_bottom()],
                                Stroke::new(1.0, Color32::from_gray(25)),
                            );
                        }
                    }
                    // Columns
                    let total = (width / ppb * PPQ as f32) as u64;
                    let mut t = 0;
                    while t <= total {
                        let x = to_x(t as f64);
                        let (w, c) = if t % sig.ticks_per_bar() == 0 {
                            (1.5, 18)
                        } else if t % PPQ == 0 {
                            (1.0, 28)
                        } else {
                            (1.0, 38)
                        };
                        p.line_segment(
                            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
                            Stroke::new(w, Color32::from_gray(c)),
                        );
                        t += ps.grid.max(1);
                    }
                    // Outside the loop
                    let end = to_x(clip.length as f64);
                    p.rect_filled(
                        Rect::from_min_max(Pos2::new(end, rect.top()), rect.max),
                        0.0,
                        Color32::from_black_alpha(110),
                    );
                    p.line_segment(
                        [Pos2::new(end, rect.top()), Pos2::new(end, rect.bottom())],
                        Stroke::new(2.0, ACCENT),
                    );

                    // Notes
                    let note_rect = |n: &NoteData| {
                        Rect::from_min_size(
                            Pos2::new(to_x(n.start as f64), to_y(n.key)),
                            Vec2::new(((n.length as f32 / PPQ as f32) * ppb).max(3.0), kh),
                        )
                    };
                    let live: &[NoteData] = new_notes.as_deref().unwrap_or(&notes);
                    for (i, n) in live.iter().enumerate() {
                        let r = note_rect(n).shrink(0.5);
                        let sel = ps.selected.contains(&i);
                        let fill = if sel {
                            Color32::from_rgb(255, 200, 120)
                        } else {
                            color.gamma_multiply(0.45 + 0.55 * n.velocity)
                        };
                        p.rect_filled(r, 2.0, fill);
                        p.rect_stroke(
                            r,
                            2.0,
                            Stroke::new(1.0, Color32::from_black_alpha(160)),
                            egui::StrokeKind::Inside,
                        );
                    }
                    if playing {
                        let x = to_x(playhead);
                        p.line_segment(
                            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
                            Stroke::new(1.5, PLAY_GREEN),
                        );
                    }

                    // ---------- interaction
                    let pos_to_tick_key = |pos: Pos2| {
                        (
                            ((pos.x - rect.left()) / ppb * PPQ as f32) as f64,
                            (127.0 - ((pos.y - rect.top()) / kh).floor()).clamp(0.0, 127.0) as u8,
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
                                egui::CursorIcon::ResizeHorizontal
                            } else {
                                egui::CursorIcon::Grab
                            });
                        }
                    }
                    let shift = ui.input(|i| i.modifiers.shift);
                    let no_snap = ui.input(|i| i.modifiers.alt);

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
                                    let start = (t / ps.grid as f64).floor() as u64 * ps.grid;
                                    if start < clip.length {
                                        let mut v = notes.clone();
                                        v.push(NoteData {
                                            start,
                                            length: ps.last_length.max(ps.grid / 2),
                                            key,
                                            velocity: ps.new_velocity,
                                        });
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
                                None => ps.selected.clear(),
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
                        if resp.dragged() {
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
                                        DragKind::Resize => {
                                            let min =
                                                if no_snap { 10 } else { ps.grid.max(1) as i64 };
                                            n.length = (orig.length as i64 + dt).max(min) as u64;
                                            ps.last_length = n.length;
                                        }
                                    }
                                }
                            }
                            // Apply to the model only (sync on release to avoid flooding the engine).
                            if let Some(notes_mut) =
                                session.clip_mut(track, slot).and_then(|c| c.notes_mut())
                            {
                                *notes_mut = v;
                            }
                        }
                    }
                    if resp.drag_stopped() && ps.drag.take().is_some() {
                        session.sync_clip(track, slot);
                        preview_off = true;
                    }
                });
        });
    });

    if let Some(v) = new_notes {
        if let Some(n) = session.clip_mut(track, slot).and_then(|c| c.notes_mut()) {
            *n = v;
        }
        session.sync_clip(track, slot);
    }
    if preview_off {
        if let Some(k) = state.piano.preview.take() {
            session.midi_in(
                track,
                MidiMessage::NoteOff {
                    channel: 0,
                    key: k,
                    velocity: 0.5,
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
                },
            );
        }
        session.midi_in(
            track,
            MidiMessage::NoteOn {
                channel: 0,
                key: k,
                velocity: state.piano.new_velocity,
            },
        );
    }
}
