//! The time ruler and the note ruler with its piano keys (manual pp. 253-255, 285-286).

use super::{is_black, Geo, Preview, Shown, MAX_KEY_H, MIN_KEY_H};
use crate::session::Session;
use crate::ui::theme::*;
use crate::ui::widgets::{info, state_button};
use crate::ui::UiState;
use egui::{Align2, Pos2, Rect, Sense, Stroke, Vec2};
use rlive_core::{TimeSignature, TrackId, PPQ};
use rlive_project::note_edit::span_of;
use rlive_project::note_view::{spell, Spelling};
use rlive_project::{Device, DeviceKind, RackKind, Tuning};

/// The chains of the track's Drum Rack, as (note, name), if it has one with something in it.
pub(super) fn drum_pads(session: &Session, track: TrackId) -> Vec<(u8, String)> {
    let Some(t) = session.project.track(track) else {
        return Vec::new();
    };
    t.devices.iter().find_map(pads_of).unwrap_or_default()
}

fn pads_of(d: &Device) -> Option<Vec<(u8, String)>> {
    match &d.kind {
        DeviceKind::Rack {
            kind: RackKind::Drum,
            chains,
            ..
        } => {
            let pads: Vec<(u8, String)> = chains
                .iter()
                .filter(|c| !c.devices.is_empty())
                .filter_map(|c| Some((c.zones.drum?.receive?, c.name.clone())))
                .collect();
            Some(pads)
        }
        _ => None,
    }
}

/// What the Info View says about a note under a tuning system.
pub(super) fn tuned_note_text(key: u8, t: &Tuning) -> String {
    format!("{} — {:.2} Hz", t.key_label(key), t.key_hz(key))
}

/// A key on a loaded tuning, as degree and pseudo-octave.
pub(super) fn tuning_label(key: u8, t: &Tuning) -> String {
    t.key_label(key)
}

/// What a double click on the time ruler zooms to: the selection, or from first note to last.
fn time_fit(state: &UiState, shown: &[Shown], fg: usize) -> Option<(u64, u64)> {
    let ps = &state.piano;
    if let Some(t) = ps.time.filter(|(a, b)| b > a) {
        return Some(t);
    }
    if let Some(sel) = span_of(shown[fg].clip.notes(), &ps.selected) {
        return Some(sel);
    }
    let notes = shown[fg].clip.notes();
    Some((
        notes.iter().map(|n| n.start).min()?,
        notes.iter().map(|n| n.start + n.length).max()?,
    ))
}

/// Shows `[from, to)` across the grid.
pub fn zoom_time(state: &mut UiState, from: u64, to: u64, width: f32) {
    let ps = &mut state.piano;
    let beats = ((to - from) as f32 / PPQ as f32).max(0.01);
    ps.px_per_beat = (width / beats * 0.96).clamp(10.0, 800.0);
    ps.scroll.x = from as f32 / PPQ as f32 * ps.px_per_beat;
}

/// Fits the key tracks with notes, or the selection's, to the view height.
pub fn fit_keys(state: &mut UiState, keys: &[u8], height: f32) {
    let ps = &mut state.piano;
    let (Some(hi), Some(lo)) = (keys.iter().copied().max(), keys.iter().copied().min()) else {
        return;
    };
    let top = ps.rows.iter().position(|k| *k <= hi).unwrap_or(0);
    let bottom = ps
        .rows
        .iter()
        .rposition(|k| *k >= lo)
        .unwrap_or(ps.rows.len().saturating_sub(1));
    let rows = (bottom + 1).saturating_sub(top).max(1) as f32;
    ps.key_h = (height / rows).clamp(MIN_KEY_H, MAX_KEY_H);
    ps.scroll.y = top as f32 * ps.key_h;
}

#[allow(clippy::too_many_arguments)]
pub(super) fn time_ruler(
    ui: &mut egui::Ui,
    ruler: Rect,
    geo: &Geo,
    state: &mut UiState,
    sig: TimeSignature,
    shown: &[Shown],
    fg: usize,
    _span: (u64, u64),
) {
    let p = ui.painter_at(ruler);
    p.rect_filled(ruler, 0.0, shade(0x333333));
    let bar = sig.ticks_per_bar();
    let first_bar = (geo.scroll.x / geo.ppb * PPQ as f32) as u64 / bar;
    let label_every = if bar as f32 / PPQ as f32 * geo.ppb < 40.0 {
        4
    } else {
        1
    };
    let mut b = first_bar;
    while geo.x((b * bar) as f64) <= ruler.right() {
        let x = geo.x((b * bar) as f64);
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
                font(10.0),
                pal().text_dim,
            );
        }
        b += 1;
    }
    let resp = ui.interact(
        ruler,
        ui.scope_id().with("note_time_ruler"),
        Sense::click_and_drag(),
    );
    if resp.dragged() {
        let d = resp.drag_delta();
        let ps = &mut state.piano;
        if d.y != 0.0 {
            let px = resp
                .interact_pointer_pos()
                .map_or(0.0, |pos| pos.x - geo.grid.left());
            let (ppb, scroll) =
                super::zoom_about(ps.scroll.x, px, ps.px_per_beat, 1.0 + d.y * 0.01);
            ps.px_per_beat = ppb;
            ps.scroll.x = scroll;
        }
        ps.scroll.x -= d.x;
    }
    if resp.double_clicked() {
        if let Some((from, to)) = time_fit(state, shown, fg) {
            zoom_time(state, from, to.max(from + 1), geo.grid.width());
        }
    }
    info(
        resp,
        "Time Ruler — drag up or down to zoom, left or right to scroll; double-click to fit",
    );
}

/// The Preview switch above the piano.
pub(super) fn preview_switch(ui: &mut egui::Ui, rect: Rect, state: &mut UiState) {
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    let on = state.piano.preview_on;
    let r = state_button(&mut child, rect.size(), on, pal().accent, "Prev");
    if info(
        r,
        "Preview — hear notes as you add, select and move them, for every MIDI track",
    )
    .clicked()
    {
        state.piano.preview_on = !on;
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn keys(
    ui: &mut egui::Ui,
    keys: Rect,
    geo: &Geo,
    session: &Session,
    state: &mut UiState,
    shown: &[Shown],
    fg: usize,
    preview: &mut Preview,
) {
    let p = ui.painter_at(keys);
    p.rect_filled(keys, 0.0, shade(0x202020));
    let clip = &shown[fg].clip;
    let scale = Session::pitch_scale(clip);
    let pads = drum_pads(session, shown[fg].r.track());
    let tuning = session.track_tuning(shown[fg].r.track());
    let spelling = state.piano.spelling;
    let first = ((geo.scroll.y / geo.kh).floor() as usize).min(geo.rows.len());
    let last = (((geo.scroll.y + geo.grid.height()) / geo.kh).ceil() as usize).min(geo.rows.len());
    for row in first..last {
        let key = geo.rows[row];
        let r = Rect::from_min_size(
            Pos2::new(keys.left(), geo.y(row)),
            Vec2::new(keys.width(), geo.kh),
        );
        let held = state.piano.preview == Some(key);
        let in_scale = scale.is_some_and(|s| rlive_project::note_view::spell_in_scale(key, s));
        let is_root = scale.is_some_and(|s| key % 12 == s.0);
        let fill = if held {
            pal().accent
        } else if is_root {
            pal().accent.gamma_multiply(0.85)
        } else if in_scale {
            pal().accent.gamma_multiply(0.45)
        } else if is_black(key) {
            hex(0x1a1a1a)
        } else {
            hex(0xd8d8d8)
        };
        let kr = if is_black(key) && pads.is_empty() {
            Rect::from_min_size(r.min, Vec2::new(keys.width() * 0.62, geo.kh))
        } else {
            r
        };
        p.rect_filled(kr.shrink2(Vec2::new(0.0, 0.5)), 0.0, fill);
        if let Some(t) = tuning.filter(|_| ui.rect_contains_pointer(kr)) {
            let resp = ui.interact(
                kr,
                ui.scope_id().with(("ruler_pitch", key)),
                egui::Sense::hover(),
            );
            info(resp, tuned_note_text(key, t));
        }
        let name = pads
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, n)| n.clone())
            .or_else(|| tuning.map(|t| tuning_label(key, t)));
        let show_label = name.is_some() || key.is_multiple_of(12) || geo.kh >= 11.0;
        if show_label && geo.kh >= 7.0 {
            let text = name.unwrap_or_else(|| spell(key, spelling, scale));
            let dark = !is_black(key) || in_scale || is_root || held;
            p.text(
                Pos2::new(r.right() - 3.0, r.center().y),
                Align2::RIGHT_CENTER,
                text,
                font(9.0),
                if dark { hex(0x404040) } else { pal().text_dim },
            );
        }
    }
    let resp = ui.interact(
        keys,
        ui.scope_id().with("piano_keys"),
        Sense::click_and_drag(),
    );
    let shift = ui.input(|i| i.modifiers.shift);
    if resp.dragged() {
        // Horizontal: how tall the key tracks are.
        let dx = resp.drag_delta().x;
        if dx != 0.0 {
            let ps = &mut state.piano;
            let at = (ps.scroll.y + geo.grid.height() / 2.0) / ps.key_h;
            ps.key_h = (ps.key_h + dx * 0.05).clamp(MIN_KEY_H, MAX_KEY_H);
            ps.scroll.y = at * ps.key_h - geo.grid.height() / 2.0;
        }
    }
    if resp.clicked() && shift {
        if let Some(key) = resp
            .interact_pointer_pos()
            .and_then(|pos| geo.key_at(pos.y))
        {
            let selected = &mut state.piano.selected;
            rlive_project::note_select::toggle_key_track(clip.notes(), selected, key);
        }
    } else if resp.is_pointer_button_down_on() && !shift {
        if let Some(key) = resp
            .interact_pointer_pos()
            .and_then(|pos| geo.key_at(pos.y))
        {
            if state.piano.preview != Some(key) {
                preview.off = state.piano.preview.is_some();
                preview.on = Some(key);
                preview.from_keys = true;
            }
        }
    } else if state.piano.preview.is_some() && state.piano.preview_from_keys {
        preview.off = true;
    }
    if resp.double_clicked() {
        let chosen: Vec<u8> = if state.piano.selected.is_empty() {
            clip.notes().iter().map(|n| n.key).collect()
        } else {
            state
                .piano
                .selected
                .iter()
                .filter_map(|i| clip.notes().get(*i).map(|n| n.key))
                .collect()
        };
        let h = geo.grid.height();
        fit_keys(state, &chosen, h);
    }
    resp.context_menu(|ui| {
        ui.label("Spelling");
        for s in Spelling::ALL {
            if ui
                .selectable_label(state.piano.spelling == s, s.label())
                .clicked()
            {
                state.piano.spelling = s;
                ui.close();
            }
        }
        ui.separator();
        if ui
            .selectable_label(state.piano.highlight, "Highlight Scale")
            .clicked()
        {
            state.piano.highlight = !state.piano.highlight;
            ui.close();
        }
    });
    info(
        resp,
        "Note Ruler — click to hear a key, Shift-click to select its notes; drag left or right \
         to resize; double-click to fit",
    );
}
