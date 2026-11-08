//! Session view, laid out like Ableton Live:
//!
//! ```text
//! ┌ track headers (sticky) ─────────────┬ Master ┐
//! │ clip grid (scrolls ↕ and ↔)          │ scenes │
//! ├ clip stop buttons (sticky) ─────────┼ StopAll┤
//! └ mixer strips (sticky, resizable) ───┴ master ┘
//! ```
//!
//! Track widths and slot heights adapt to the window; headers, stop row, mixer and the
//! Master/scene column stay put while the grid scrolls.

use super::icons;
use super::theme::{self, *};
use super::widgets::{self, info, rgb};
use super::{BrowserItem, Detail, UiState};
use crate::session::{ChainRef, Session};
use egui::{
    Align2, Color32, Layout, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, UiBuilder, Vec2,
};
use rlive_core::TrackId;
use rlive_engine::shared::{NO_SLOT, STOP_QUEUED};
use rlive_project::{Clip, TrackKind, PALETTE};
use std::sync::atomic::Ordering;

const HEADER_H: f32 = 22.0;
const STOP_ROW_H: f32 = 20.0;
const MIN_TRACK_W: f32 = 84.0;
const MAX_TRACK_W: f32 = 150.0;
const MIN_SLOT_H: f32 = 20.0;
const MAX_SLOT_H: f32 = 26.0;
const ADD_COL_W: f32 = 36.0;
const SCROLLBAR: f32 = 6.0;

/// Layout state that persists across frames.
pub struct SessionLayout {
    pub scroll: Vec2,
    pub mixer_h: f32,
    pub show_mixer: bool,
}

impl Default for SessionLayout {
    fn default() -> Self {
        Self {
            scroll: Vec2::ZERO,
            mixer_h: 190.0,
            show_mixer: true,
        }
    }
}

pub fn duplicate_clip(session: &mut Session, track: TrackId, slot: usize) {
    let Some(clip) = session.clip(track, slot).cloned() else {
        return;
    };
    let scenes = session.project.scenes.len();
    let target = (slot + 1..scenes).find(|s| session.clip(track, *s).is_none());
    let target = match target {
        Some(t) => t,
        None => {
            session.add_scene();
            session.project.scenes.len() - 1
        }
    };
    session.set_clip(track, target, Some(clip));
}

/// Geometry of the view for this frame.
struct Geo {
    track_w: f32,
    slot_h: f32,
    tracks_left: f32,
    tracks_right: f32,
    master: Rect,
    header: Rect,
    grid: Rect,
    stop_row: Rect,
    mixer: Rect,
    compact_mixer: bool,
}

impl Geo {
    fn track_x(&self, i: usize, scroll_x: f32) -> f32 {
        self.tracks_left + i as f32 * self.track_w - scroll_x
    }

    fn slot_y(&self, j: usize, scroll_y: f32) -> f32 {
        self.grid.top() + j as f32 * self.slot_h - scroll_y
    }
}

fn child(ui: &mut egui::Ui, clip: Rect, rect: Rect, layout: Layout) -> egui::Ui {
    let mut c = ui.new_child(UiBuilder::new().max_rect(rect).layout(layout));
    c.set_clip_rect(clip.intersect(ui.clip_rect()));
    c
}

pub fn show(ui: &mut egui::Ui, session: &mut Session, state: &mut UiState) {
    let full = ui.max_rect();
    let bg = ui.allocate_rect(full, Sense::click());
    ui.painter().rect_filled(full, 0.0, GRID_BG);

    let n_tracks = session.project.tracks.len();
    let n_scenes = session.project.scenes.len();

    // ---- geometry
    let master_w = (full.width() * 0.13).clamp(112.0, 150.0);
    let master_x = full.right() - master_w;
    let mixer_h = if state.layout.show_mixer {
        // On short windows the mixer gives room to the clip grid.
        state
            .layout
            .mixer_h
            .clamp(90.0, (full.height() * 0.42).max(90.0))
    } else {
        0.0
    };
    let mixer = Rect::from_min_max(
        Pos2::new(full.left(), full.bottom() - mixer_h),
        Pos2::new(master_x, full.bottom()),
    );
    let stop_row = Rect::from_min_max(
        Pos2::new(full.left(), mixer.top() - STOP_ROW_H),
        Pos2::new(master_x, mixer.top()),
    );
    let header = Rect::from_min_max(full.min, Pos2::new(master_x, full.top() + HEADER_H));
    let grid = Rect::from_min_max(
        Pos2::new(full.left(), header.bottom()),
        Pos2::new(master_x, stop_row.top()),
    );
    let avail_w = (master_x - full.left() - ADD_COL_W).max(10.0);
    let track_w = if n_tracks == 0 {
        MAX_TRACK_W
    } else {
        (avail_w / n_tracks as f32)
            .clamp(MIN_TRACK_W, MAX_TRACK_W)
            .floor()
    };
    let slot_h = if n_scenes == 0 {
        MAX_SLOT_H
    } else {
        (grid.height() / n_scenes as f32)
            .clamp(MIN_SLOT_H, MAX_SLOT_H)
            .floor()
    };
    let g = Geo {
        track_w,
        slot_h,
        tracks_left: full.left(),
        tracks_right: master_x,
        master: Rect::from_min_max(Pos2::new(master_x, full.top()), full.max),
        header,
        grid,
        stop_row,
        mixer,
        compact_mixer: mixer_h < 140.0,
    };

    // ---- scrolling
    let content_w = n_tracks as f32 * track_w + ADD_COL_W;
    let content_h = n_scenes as f32 * slot_h;
    let max_x = (content_w - (master_x - full.left())).max(0.0);
    let max_y = (content_h - grid.height()).max(0.0);
    if bg.contains_pointer() || ui.rect_contains_pointer(full) {
        let (delta, shift) = ui.input(|i| (i.smooth_scroll_delta, i.modifiers.shift));
        // Some platforms already turn Shift+wheel into a horizontal delta; handle both.
        let (dx, dy) = if shift {
            (delta.x + delta.y, 0.0)
        } else {
            (delta.x, delta.y)
        };
        state.layout.scroll.x -= dx;
        state.layout.scroll.y -= dy;
    }
    state.layout.scroll.x = state.layout.scroll.x.clamp(0.0, max_x);
    state.layout.scroll.y = state.layout.scroll.y.clamp(0.0, max_y);
    let scroll = state.layout.scroll;

    let blink = (ui.input(|i| i.time) * 3.0).fract() < 0.5;

    // ---- tracks
    let ids: Vec<TrackId> = session.project.tracks.iter().map(|t| t.id).collect();
    for (i, id) in ids.iter().enumerate() {
        let x = g.track_x(i, scroll.x);
        if x + track_w < full.left() || x > g.tracks_right {
            continue; // off screen
        }
        track_column(ui, session, state, &g, *id, i, x, scroll.y, blink);
    }

    // Empty area right of the last track: click = new MIDI track, drop = new track.
    let add_x = g.track_x(n_tracks, scroll.x);
    if add_x < g.tracks_right {
        let add = Rect::from_min_max(
            Pos2::new(add_x, full.top()),
            Pos2::new(g.tracks_right, g.stop_row.top()),
        );
        new_track_zone(ui, session, state, add);
    }

    // Separators between regions.
    let p = ui.painter();
    p.line_segment(
        [g.header.left_bottom(), g.header.right_bottom()],
        Stroke::new(1.0, SEPARATOR),
    );
    p.line_segment(
        [g.stop_row.left_top(), g.stop_row.right_top()],
        Stroke::new(1.0, SEPARATOR),
    );
    p.line_segment(
        [g.master.left_top(), g.master.left_bottom()],
        Stroke::new(2.0, SEPARATOR),
    );

    master_column(ui, session, state, &g, scroll.y, blink);
    mixer_splitter(ui, state, &g);
    scrollbars(ui, state, &g, full, content_w, content_h, max_x, max_y);

    if bg.clicked() {
        state.selected_clip = None;
    }
}

#[allow(clippy::too_many_arguments)]
fn track_column(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    g: &Geo,
    id: TrackId,
    index: usize,
    x: f32,
    scroll_y: f32,
    blink: bool,
) {
    let Some(t) = session.project.track(id) else {
        return;
    };
    let (name, color, kind, arm) = (t.name.clone(), t.color, t.kind, t.arm);
    let shared = session.track_shared.get(&id).cloned();
    let playing_slot = shared
        .as_ref()
        .map_or(NO_SLOT, |s| s.playing_slot.load(Ordering::Relaxed));
    let queued_slot = shared
        .as_ref()
        .map_or(NO_SLOT, |s| s.queued_slot.load(Ordering::Relaxed));
    let position = shared.as_ref().map_or(0.0, |s| s.clip_position.load());
    let selected = state.selected == Some(ChainRef::Track(id));
    let track_color = rgb(color);
    let w = g.track_w;
    let col_clip = Rect::from_min_max(
        Pos2::new(g.tracks_left, g.header.top()),
        Pos2::new(g.tracks_right, g.mixer.bottom()),
    );

    // Selected track column gets a slightly lighter background.
    if selected {
        let r = Rect::from_min_max(
            Pos2::new(x, g.grid.top()),
            Pos2::new(x + w, g.grid.bottom()),
        )
        .intersect(col_clip);
        ui.painter().rect_filled(r, 0.0, hex(0x262626));
    }

    // ---- header
    {
        let rect = Rect::from_min_size(
            Pos2::new(x, g.header.top()),
            Vec2::new(w - 1.0, HEADER_H - 1.0),
        );
        let mut cu = child(
            ui,
            g.header,
            rect,
            Layout::left_to_right(egui::Align::Center),
        );
        let resp = cu.allocate_rect(rect, Sense::click_and_drag());
        let fill = if selected {
            track_color
        } else {
            theme::darken(track_color, 0.35)
        };
        cu.painter().rect_filled(rect, 0.0, fill);
        if selected {
            widgets::outline(cu.painter(), rect, hex(0xffffff));
        }
        let renaming = matches!(&state.renaming, Some((tid, _)) if *tid == id);
        if renaming {
            let mut done = false;
            if let Some((_, text)) = &mut state.renaming {
                let r = cu.put(
                    rect.shrink(1.0),
                    egui::TextEdit::singleline(text).font(theme::font(11.5)),
                );
                r.request_focus();
                done = r.lost_focus();
            }
            if done {
                if let Some((_, text)) = state.renaming.take() {
                    if let Some(t) = session.project.track_mut(id) {
                        t.name = text;
                    }
                    session.dirty = true;
                }
            }
        } else {
            let galley = cu.painter().layout_no_wrap(
                name.clone(),
                theme::font_bold(11.0),
                theme::text_on(fill),
            );
            let clip_r = rect.shrink2(Vec2::new(5.0, 0.0));
            cu.painter()
                .with_clip_rect(clip_r.intersect(cu.clip_rect()))
                .galley(
                    Pos2::new(clip_r.left(), rect.center().y - galley.size().y / 2.0),
                    galley,
                    Color32::WHITE,
                );
        }
        if resp.clicked() {
            state.selected = Some(ChainRef::Track(id));
            state.detail = Detail::Devices;
        }
        if resp.double_clicked() {
            state.renaming = Some((id, name.clone()));
        }
        if resp.dnd_hover_payload::<BrowserItem>().is_some() {
            widgets::outline(cu.painter(), rect, ACCENT);
        }
        if let Some(item) = resp.dnd_release_payload::<BrowserItem>() {
            drop_on_track(session, state, id, &item, None);
        }
        let resp = info(
            resp,
            format!(
                "Track '{name}' — click to select, double-click to rename, right-click for options"
            ),
        );
        resp.context_menu(|ui| track_menu(ui, session, state, id, &name));
    }

    // ---- clip slots
    let n_scenes = session.project.scenes.len();
    let first = ((scroll_y / g.slot_h).floor().max(0.0)) as usize;
    let last = (((scroll_y + g.grid.height()) / g.slot_h).ceil() as usize).min(n_scenes);
    for slot in first..last {
        let y = g.slot_y(slot, scroll_y);
        let rect = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w - 1.0, g.slot_h - 1.0));
        let clip_info = session
            .clip(id, slot)
            .map(|c| (c.name.clone(), c.color, c.length));
        let mut cu = child(
            ui,
            g.grid.intersect(col_clip),
            rect,
            Layout::left_to_right(egui::Align::Center),
        );
        let resp = cu.allocate_rect(rect, Sense::click_and_drag());
        let p = cu.painter().clone();
        let is_sel = state.selected_clip == Some((id, slot));
        let launch = Rect::from_min_size(rect.min, Vec2::new(g.slot_h.min(22.0), rect.height()));
        let over_launch = resp.hover_pos().is_some_and(|pos| launch.contains(pos));
        let is_playing = playing_slot == slot as i32;
        let is_queued = queued_slot == slot as i32;

        match &clip_info {
            Some((cname, ccol, clen)) => {
                let fill = rgb(*ccol);
                let fill = if resp.hovered() {
                    fill.gamma_multiply(1.12)
                } else {
                    fill
                };
                p.rect_filled(
                    rect,
                    1.0,
                    if is_sel {
                        fill
                    } else {
                        theme::darken(fill, 0.12)
                    },
                );
                p.rect_filled(launch, 1.0, theme::darken(fill, 0.25));
                let tri = if is_playing || (is_queued && blink) {
                    PLAY_GREEN
                } else if over_launch {
                    hex(0xffffff)
                } else {
                    theme::text_on(fill)
                };
                icons::play(&p, launch, tri);
                let text_r = Rect::from_min_max(
                    Pos2::new(launch.right() + 4.0, rect.top()),
                    Pos2::new(rect.right() - 3.0, rect.bottom()),
                );
                let galley =
                    p.layout_no_wrap(cname.clone(), theme::font(11.0), theme::text_on(fill));
                p.with_clip_rect(text_r.intersect(cu.clip_rect())).galley(
                    Pos2::new(text_r.left(), rect.center().y - galley.size().y / 2.0),
                    galley,
                    Color32::WHITE,
                );
                if is_playing && *clen > 0 {
                    let frac = (position / *clen as f64).clamp(0.0, 1.0) as f32;
                    let bar = Rect::from_min_size(
                        Pos2::new(rect.left(), rect.bottom() - 2.0),
                        Vec2::new(rect.width() * frac, 2.0),
                    );
                    p.rect_filled(bar, 0.0, Color32::from_white_alpha(200));
                }
                if is_sel {
                    widgets::outline(&p, rect, hex(0xffffff));
                }
            }
            None => {
                p.rect_filled(
                    rect,
                    0.0,
                    if resp.hovered() {
                        SLOT_EMPTY_HOVER
                    } else {
                        SLOT_EMPTY
                    },
                );
                let col = if queued_slot == STOP_QUEUED && blink {
                    ACCENT
                } else if over_launch {
                    hex(0xe0e0e0)
                } else {
                    hex(0x6a6a6a)
                };
                if arm {
                    icons::record_ring(
                        &p,
                        launch,
                        if over_launch { ARM_RED } else { hex(0x8a4a4a) },
                    );
                } else {
                    icons::stop(&p, launch.shrink(3.0), col);
                }
                if is_sel {
                    widgets::outline(&p, rect, hex(0x9a9a9a));
                }
            }
        }
        if resp.dnd_hover_payload::<BrowserItem>().is_some()
            || resp.dnd_hover_payload::<ClipDrag>().is_some()
        {
            widgets::outline(&p, rect, ACCENT);
        }

        if resp.clicked() {
            state.selected = Some(ChainRef::Track(id));
            if over_launch {
                if clip_info.is_some() {
                    session.launch_clip(id, slot);
                } else {
                    session.stop_track(id);
                }
            } else {
                state.selected_clip = Some((id, slot));
                state.detail = if clip_info.is_some() {
                    Detail::Clip
                } else {
                    Detail::Devices
                };
                state.piano.reset_view = true;
            }
        }
        if resp.double_clicked() && !over_launch {
            if clip_info.is_none() && kind == TrackKind::Midi {
                let c = Clip::new_midi("MIDI Clip", color, 1, session.project.time_signature);
                session.set_clip(id, slot, Some(c));
                state.selected_clip = Some((id, slot));
                state.detail = Detail::Clip;
                state.show_detail = true;
                state.piano.reset_view = true;
            } else if clip_info.is_some() {
                state.detail = Detail::Clip;
                state.show_detail = true;
            }
        }
        if let Some(item) = resp.dnd_release_payload::<BrowserItem>() {
            drop_on_track(session, state, id, &item, Some(slot));
        }
        if clip_info.is_some() {
            resp.dnd_set_drag_payload(ClipDrag { track: id, slot });
        }
        if let Some(src) = resp.dnd_release_payload::<ClipDrag>() {
            if (src.track, src.slot) != (id, slot) {
                if let Some(c) = session.clip(src.track, src.slot).cloned() {
                    let copy = cu.input(|i| i.modifiers.alt);
                    session.set_clip(id, slot, Some(c));
                    if !copy {
                        session.set_clip(src.track, src.slot, None);
                    }
                    state.selected_clip = Some((id, slot));
                }
            }
        }
        let text = match &clip_info {
            Some((n, _, _)) => {
                format!("Clip '{n}' — click ▶ to launch, click to edit, drag to move (Alt = copy)")
            }
            None if kind == TrackKind::Midi => {
                "Empty slot — double-click to create a MIDI clip; ■ stops the track".to_string()
            }
            None => "Empty slot — drop a sample here; ■ stops the track".to_string(),
        };
        let resp = info(resp, text);
        resp.context_menu(|ui| clip_menu(ui, session, state, id, slot, kind));
    }

    // ---- clip stop button
    {
        let rect = Rect::from_min_size(
            Pos2::new(x, g.stop_row.top() + 1.0),
            Vec2::new(w - 1.0, STOP_ROW_H - 2.0),
        );
        let mut cu = child(
            ui,
            g.stop_row.intersect(col_clip),
            rect,
            Layout::left_to_right(egui::Align::Center),
        );
        let resp = cu.allocate_rect(rect, Sense::click());
        cu.painter().rect_filled(
            rect,
            0.0,
            if resp.hovered() {
                SLOT_EMPTY_HOVER
            } else {
                PANEL_DARK
            },
        );
        let active = playing_slot != NO_SLOT;
        let col = if queued_slot == STOP_QUEUED && blink {
            ACCENT
        } else if active {
            hex(0xd0d0d0)
        } else {
            hex(0x5a5a5a)
        };
        icons::stop(
            cu.painter(),
            Rect::from_center_size(rect.center(), Vec2::splat(rect.height())),
            col,
        );
        if info(resp, "Stop the clip playing on this track").clicked() {
            session.stop_track(id);
        }
    }

    // ---- mixer strip
    if g.mixer.height() > 0.0 {
        let rect = Rect::from_min_size(
            Pos2::new(x, g.mixer.top()),
            Vec2::new(w - 1.0, g.mixer.height()),
        );
        let mut cu = child(
            ui,
            g.mixer.intersect(col_clip),
            rect,
            Layout::top_down(egui::Align::Center),
        );
        mixer_strip(
            &mut cu,
            session,
            state,
            id,
            index,
            rect,
            g.compact_mixer,
            selected,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn mixer_strip(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    id: TrackId,
    index: usize,
    rect: Rect,
    compact: bool,
    selected: bool,
) {
    let Some(t) = session.project.track(id) else {
        return;
    };
    let (vol, pan, mute, solo, arm, color) =
        (t.volume_db, t.pan, t.mute, t.solo, t.arm, rgb(t.color));
    let meter = state.meters.get(&id).copied().unwrap_or_default();
    let p = ui.painter().clone();
    p.rect_filled(rect, 0.0, if selected { hex(0x363636) } else { PANEL });
    p.line_segment(
        [rect.right_top(), rect.right_bottom()],
        Stroke::new(1.0, SEPARATOR),
    );
    // Thin track colour line at the top of the strip.
    p.rect_filled(
        Rect::from_min_size(rect.min, Vec2::new(rect.width(), 2.0)),
        0.0,
        color,
    );
    let bg = ui.allocate_rect(rect, Sense::click());
    if bg.clicked() {
        state.selected = Some(ChainRef::Track(id));
        state.detail = Detail::Devices;
    }

    let mut new_mix = (vol, pan, mute, solo);
    let inner = rect.shrink2(Vec2::new(5.0, 6.0));
    let buttons_h = 17.0;
    let mut y = inner.top() + 2.0;

    if !compact {
        let knob_r =
            Rect::from_center_size(Pos2::new(inner.center().x, y + 13.0), Vec2::new(28.0, 26.0));
        let mut ku = ui.new_child(UiBuilder::new().max_rect(knob_r));
        if let Some(v) = widgets::pan_knob(&mut ku, pan) {
            new_mix.1 = v;
        }
        y += 30.0;
    }

    // Fader + meter take the remaining height above the buttons and dB box.
    let fader_bottom = inner.bottom() - buttons_h - 18.0;
    let fader_w = (inner.width() * 0.5).clamp(26.0, 44.0);
    let fader_r = Rect::from_min_max(
        Pos2::new(inner.center().x - fader_w / 2.0, y),
        Pos2::new(inner.center().x + fader_w / 2.0, fader_bottom.max(y + 20.0)),
    );
    let mut fu = ui.new_child(UiBuilder::new().max_rect(fader_r));
    if let Some(db) = widgets::fader(&mut fu, fader_r.size(), vol, meter.get(), ACCENT) {
        new_mix.0 = db;
    }
    let db_r = Rect::from_center_size(
        Pos2::new(inner.center().x, fader_r.bottom() + 9.0),
        Vec2::new(inner.width().min(54.0), 14.0),
    );
    p.rect_filled(db_r, 1.0, FIELD_BG);
    let db_text = if vol <= -70.0 {
        "-inf".to_string()
    } else {
        format!("{vol:.1}")
    };
    p.text(
        db_r.center(),
        Align2::CENTER_CENTER,
        db_text,
        theme::font(10.0),
        TEXT,
    );

    // Activator / Solo / Arm
    let bw = ((inner.width() - 4.0) / 3.0).floor();
    let row = Rect::from_min_size(
        Pos2::new(inner.left(), inner.bottom() - buttons_h),
        Vec2::new(inner.width(), buttons_h),
    );
    let mut bu = ui.new_child(
        UiBuilder::new()
            .max_rect(row)
            .layout(Layout::left_to_right(egui::Align::Center)),
    );
    bu.spacing_mut().item_spacing.x = 2.0;
    let r = widgets::state_button(
        &mut bu,
        Vec2::new(bw, buttons_h),
        !mute,
        ACTIVATOR,
        &(index + 1).to_string(),
    );
    if info(r, "Track Activator — click to mute/unmute").clicked() {
        new_mix.2 = !mute;
    }
    let r = widgets::state_button(&mut bu, Vec2::new(bw, buttons_h), solo, SOLO_BLUE, "S");
    if info(r, "Solo — Cmd-click to solo several tracks").clicked() {
        new_mix.3 = !solo;
        // Like Live: solo is exclusive unless Cmd is held.
        if !solo && !bu.input(|i| i.modifiers.command) {
            let others: Vec<TrackId> = session
                .project
                .tracks
                .iter()
                .filter(|t| t.id != id && t.solo)
                .map(|t| t.id)
                .collect();
            for o in others {
                if let Some(t) = session.project.track_mut(o) {
                    t.solo = false;
                }
                session.sync_mix(o);
            }
        }
    }
    let r = widgets::icon_button(
        &mut bu,
        Vec2::new(bw, buttons_h),
        arm,
        ARM_RED,
        icons::record,
    );
    if info(
        r,
        "Arm — route MIDI input to this track (Cmd-click to arm several)",
    )
    .clicked()
    {
        let exclusive = !bu.input(|i| i.modifiers.command);
        session.set_arm(id, !arm, exclusive);
    }

    if new_mix != (vol, pan, mute, solo) {
        if let Some(t) = session.project.track_mut(id) {
            t.volume_db = new_mix.0;
            t.pan = new_mix.1;
            t.mute = new_mix.2;
            t.solo = new_mix.3;
        }
        session.sync_mix(id);
    }
}

fn master_column(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    g: &Geo,
    scroll_y: f32,
    blink: bool,
) {
    let m = g.master;
    let p = ui.painter().clone();
    p.rect_filled(
        Rect::from_min_max(m.min, Pos2::new(m.right(), g.stop_row.bottom())),
        0.0,
        PANEL_DARK,
    );
    let selected = state.selected == Some(ChainRef::Master);

    // Header
    let hr = Rect::from_min_size(
        Pos2::new(m.left() + 1.0, m.top()),
        Vec2::new(m.width() - 1.0, HEADER_H - 1.0),
    );
    let resp = ui.allocate_rect(hr, Sense::click());
    p.rect_filled(
        hr,
        0.0,
        if selected {
            hex(0xbdbdbd)
        } else {
            hex(0x7a7a7a)
        },
    );
    p.text(
        hr.left_center() + Vec2::new(6.0, 0.0),
        Align2::LEFT_CENTER,
        "Master",
        theme::font_bold(11.0),
        TEXT_DARK,
    );
    if resp.clicked() {
        state.selected = Some(ChainRef::Master);
        state.detail = Detail::Devices;
    }
    if let Some(item) = resp.dnd_release_payload::<BrowserItem>() {
        match &*item {
            BrowserItem::Builtin(t) => session.add_builtin(ChainRef::Master, t),
            BrowserItem::Plugin(p) => session.add_plugin(ChainRef::Master, p),
            BrowserItem::Sample(_) => {}
        }
    }
    info(
        resp,
        "Master track — scene launch buttons; drop effects here",
    );

    // Scene launch buttons, aligned with the clip grid rows.
    let grid = Rect::from_min_max(
        Pos2::new(m.left(), g.grid.top()),
        Pos2::new(m.right(), g.grid.bottom()),
    );
    let n = session.project.scenes.len();
    let first = ((scroll_y / g.slot_h).floor().max(0.0)) as usize;
    let last = (((scroll_y + grid.height()) / g.slot_h).ceil() as usize).min(n);
    let mut delete = None;
    let any_queued = session
        .track_shared
        .values()
        .any(|s| s.queued_slot.load(Ordering::Relaxed) >= 0);
    for i in first..last {
        let y = g.slot_y(i, scroll_y);
        let rect = Rect::from_min_size(
            Pos2::new(m.left() + 1.0, y),
            Vec2::new(m.width() - 1.0, g.slot_h - 1.0),
        );
        let mut cu = child(ui, grid, rect, Layout::left_to_right(egui::Align::Center));
        let resp = cu.allocate_rect(rect, Sense::click());
        let cp = cu.painter().clone();
        cp.rect_filled(
            rect,
            0.0,
            if resp.hovered() {
                hex(0x484848)
            } else {
                hex(0x3a3a3a)
            },
        );
        let launch = Rect::from_min_size(rect.min, Vec2::new(g.slot_h.min(22.0), rect.height()));
        // A scene is "queued" when every track with a clip in that row is queued on it.
        let queued_here = any_queued
            && session.project.tracks.iter().any(|t| {
                session
                    .track_shared
                    .get(&t.id)
                    .is_some_and(|s| s.queued_slot.load(Ordering::Relaxed) == i as i32)
            });
        let col = if queued_here && blink {
            PLAY_GREEN
        } else if resp.hovered() {
            hex(0xffffff)
        } else {
            hex(0xb0b0b0)
        };
        icons::play(&cp, launch, col);
        let name = session.project.scenes[i].name.clone();
        cp.text(
            Pos2::new(launch.right() + 3.0, rect.center().y),
            Align2::LEFT_CENTER,
            name,
            theme::font(11.0),
            TEXT,
        );
        if resp.clicked() {
            session.launch_scene(i);
        }
        let resp = info(resp, "Scene — click to launch every clip in this row");
        resp.context_menu(|ui| {
            if let Some(scene) = session.project.scenes.get_mut(i) {
                ui.horizontal(|ui| {
                    ui.label("Name");
                    ui.text_edit_singleline(&mut scene.name);
                });
            }
            if ui.button("Insert Scene").clicked() {
                session.add_scene();
                ui.close();
            }
            if ui
                .button(RichText::new("Delete Scene").color(ARM_RED))
                .clicked()
            {
                delete = Some(i);
                ui.close();
            }
        });
    }
    if let Some(i) = delete {
        session.delete_scene(i);
    }
    // "+ scene" row after the last scene, if visible.
    let y = g.slot_y(n, scroll_y);
    if y < grid.bottom() {
        let rect = Rect::from_min_size(
            Pos2::new(m.left() + 1.0, y),
            Vec2::new(m.width() - 1.0, g.slot_h - 1.0),
        );
        let mut cu = child(ui, grid, rect, Layout::left_to_right(egui::Align::Center));
        let resp = cu.allocate_rect(rect, Sense::click());
        if resp.hovered() {
            cu.painter().rect_filled(rect, 0.0, hex(0x333333));
        }
        icons::plus(
            cu.painter(),
            Rect::from_center_size(rect.center(), Vec2::splat(rect.height())),
            hex(0x7a7a7a),
        );
        if info(resp, "Add a scene").clicked() {
            session.add_scene();
        }
    }

    // Stop all clips
    let sr = Rect::from_min_size(
        Pos2::new(m.left() + 1.0, g.stop_row.top() + 1.0),
        Vec2::new(m.width() - 1.0, STOP_ROW_H - 2.0),
    );
    let resp = ui.allocate_rect(sr, Sense::click());
    p.rect_filled(
        sr,
        0.0,
        if resp.hovered() {
            SLOT_EMPTY_HOVER
        } else {
            hex(0x353535)
        },
    );
    icons::stop(
        &p,
        Rect::from_min_size(sr.min, Vec2::splat(sr.height())),
        hex(0xd0d0d0),
    );
    p.text(
        Pos2::new(sr.left() + sr.height() + 2.0, sr.center().y),
        Align2::LEFT_CENTER,
        "Stop Clips",
        theme::font(10.5),
        TEXT_DIM,
    );
    if info(resp, "Stop all clips").clicked() {
        session.stop_all_clips();
    }

    // Master mixer strip
    if g.mixer.height() > 0.0 {
        let rect = Rect::from_min_max(Pos2::new(m.left() + 1.0, g.mixer.top()), m.max);
        p.rect_filled(rect, 0.0, if selected { hex(0x363636) } else { PANEL });
        let inner = rect.shrink2(Vec2::new(8.0, 8.0));
        let fader_r = Rect::from_min_max(
            Pos2::new(inner.center().x - 22.0, inner.top() + 2.0),
            Pos2::new(inner.center().x + 22.0, inner.bottom() - 20.0),
        );
        let mut fu = ui.new_child(UiBuilder::new().max_rect(fader_r));
        let vol = session.project.master.volume_db;
        if let Some(db) = widgets::fader(
            &mut fu,
            fader_r.size(),
            vol,
            state.master_meter.get(),
            ACCENT,
        ) {
            session.set_master_volume(db);
        }
        let db_r = Rect::from_center_size(
            Pos2::new(inner.center().x, inner.bottom() - 8.0),
            Vec2::new(54.0, 14.0),
        );
        p.rect_filled(db_r, 1.0, FIELD_BG);
        let t = if vol <= -70.0 {
            "-inf".to_string()
        } else {
            format!("{vol:.1}")
        };
        p.text(
            db_r.center(),
            Align2::CENTER_CENTER,
            t,
            theme::font(10.0),
            TEXT,
        );
        p.text(
            Pos2::new(inner.left(), inner.top()),
            Align2::LEFT_TOP,
            "Master",
            theme::font(10.0),
            TEXT_DIM,
        );
    }
}

/// Drag handle between the grid and the mixer to resize the mixer.
fn mixer_splitter(ui: &mut egui::Ui, state: &mut UiState, g: &Geo) {
    if !state.layout.show_mixer {
        return;
    }
    let r = Rect::from_min_max(
        Pos2::new(g.mixer.left(), g.mixer.top() - 3.0),
        Pos2::new(g.master.right(), g.mixer.top() + 3.0),
    );
    let resp = ui.allocate_rect(r, Sense::drag());
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        ui.painter().line_segment(
            [r.left_center(), r.right_center()],
            Stroke::new(2.0, ACCENT.gamma_multiply(0.6)),
        );
    }
    if resp.dragged() {
        state.layout.mixer_h = (g.mixer.height() - resp.drag_delta().y).clamp(90.0, 420.0);
    }
}

#[allow(clippy::too_many_arguments)]
fn scrollbars(
    ui: &mut egui::Ui,
    state: &mut UiState,
    g: &Geo,
    full: Rect,
    content_w: f32,
    content_h: f32,
    max_x: f32,
    max_y: f32,
) {
    let _ = full;
    if max_y > 0.0 {
        let track = Rect::from_min_max(
            Pos2::new(g.tracks_right - SCROLLBAR - 1.0, g.grid.top() + 1.0),
            Pos2::new(g.tracks_right - 1.0, g.grid.bottom() - 1.0),
        );
        let frac = g.grid.height() / content_h;
        let h = (track.height() * frac).max(20.0);
        let y = track.top() + (track.height() - h) * (state.layout.scroll.y / max_y);
        let thumb = Rect::from_min_size(Pos2::new(track.left(), y), Vec2::new(SCROLLBAR, h));
        let resp = ui.allocate_rect(track, Sense::drag());
        ui.painter().rect_filled(
            thumb,
            3.0,
            Color32::from_white_alpha(if resp.hovered() || resp.dragged() {
                90
            } else {
                45
            }),
        );
        if resp.dragged() {
            state.layout.scroll.y += resp.drag_delta().y / (track.height() - h).max(1.0) * max_y;
        }
    }
    if max_x > 0.0 {
        let track = Rect::from_min_max(
            Pos2::new(g.tracks_left + 1.0, g.stop_row.top() - SCROLLBAR - 1.0),
            Pos2::new(g.tracks_right - 1.0, g.stop_row.top() - 1.0),
        );
        let frac = (g.tracks_right - g.tracks_left) / content_w;
        let w = (track.width() * frac).max(20.0);
        let x = track.left() + (track.width() - w) * (state.layout.scroll.x / max_x);
        let thumb = Rect::from_min_size(Pos2::new(x, track.top()), Vec2::new(w, SCROLLBAR));
        let resp = ui.allocate_rect(track, Sense::drag());
        ui.painter().rect_filled(
            thumb,
            3.0,
            Color32::from_white_alpha(if resp.hovered() || resp.dragged() {
                90
            } else {
                45
            }),
        );
        if resp.dragged() {
            state.layout.scroll.x += resp.drag_delta().x / (track.width() - w).max(1.0) * max_x;
        }
    }
}

#[derive(Clone, Copy)]
struct ClipDrag {
    track: TrackId,
    slot: usize,
}

fn color_picker(ui: &mut egui::Ui) -> Option<[u8; 3]> {
    let mut picked = None;
    egui::Grid::new("palette")
        .spacing([2.0, 2.0])
        .show(ui, |ui| {
            for (i, c) in PALETTE.iter().enumerate() {
                let (r, resp) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::click());
                ui.painter().rect_filled(r, 1.0, rgb(*c));
                if resp.hovered() {
                    ui.painter().rect_stroke(
                        r,
                        1.0,
                        Stroke::new(1.0, Color32::WHITE),
                        StrokeKind::Inside,
                    );
                }
                if resp.clicked() {
                    picked = Some(*c);
                }
                if i % 6 == 5 {
                    ui.end_row();
                }
            }
        });
    picked
}

fn track_menu(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    id: TrackId,
    name: &str,
) {
    if ui.button("Rename").clicked() {
        state.renaming = Some((id, name.to_string()));
        ui.close();
    }
    ui.menu_button("Color", |ui| {
        if let Some(c) = color_picker(ui) {
            if let Some(t) = session.project.track_mut(id) {
                t.color = c;
            }
            session.dirty = true;
            ui.close();
        }
    });
    let idx = session.project.track_index(id).unwrap_or(0);
    if ui.button("Move Left").clicked() && idx > 0 {
        session.move_track(id, idx - 1);
        ui.close();
    }
    if ui.button("Move Right").clicked() {
        session.move_track(id, idx + 1);
        ui.close();
    }
    ui.separator();
    if ui.button("Insert MIDI Track").clicked() {
        let new = session.add_track(TrackKind::Midi, Some(idx + 1));
        state.selected = Some(ChainRef::Track(new));
        ui.close();
    }
    if ui.button("Insert Audio Track").clicked() {
        let new = session.add_track(TrackKind::Audio, Some(idx + 1));
        state.selected = Some(ChainRef::Track(new));
        ui.close();
    }
    ui.separator();
    if ui
        .button(RichText::new("Delete Track").color(ARM_RED))
        .clicked()
    {
        session.delete_track(id);
        if state.selected == Some(ChainRef::Track(id)) {
            state.selected = None;
        }
        if matches!(state.selected_clip, Some((t, _)) if t == id) {
            state.selected_clip = None;
        }
        ui.close();
    }
}

fn clip_menu(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    id: TrackId,
    slot: usize,
    kind: TrackKind,
) {
    let has = session.clip(id, slot).is_some();
    if !has {
        if kind == TrackKind::Midi && ui.button("Insert MIDI Clip").clicked() {
            let color = session
                .project
                .track(id)
                .map(|t| t.color)
                .unwrap_or(PALETTE[0]);
            let c = Clip::new_midi("MIDI Clip", color, 1, session.project.time_signature);
            session.set_clip(id, slot, Some(c));
            state.selected_clip = Some((id, slot));
            state.detail = Detail::Clip;
            ui.close();
        }
        if kind == TrackKind::Audio && ui.button("Load Audio File…").clicked() {
            ui.close();
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("WAV", &["wav"])
                .pick_file()
            {
                match session.audio_clip_from_file(&path) {
                    Ok(c) => session.set_clip(id, slot, Some(c)),
                    Err(e) => state.status = e,
                }
            }
        }
        return;
    }
    if ui.button("Launch").clicked() {
        session.launch_clip(id, slot);
        ui.close();
    }
    if ui.button("Edit").clicked() {
        state.selected_clip = Some((id, slot));
        state.detail = Detail::Clip;
        state.show_detail = true;
        ui.close();
    }
    if ui.button("Duplicate   ⌘D").clicked() {
        duplicate_clip(session, id, slot);
        ui.close();
    }
    let mut looped = session.clip(id, slot).is_some_and(|c| c.looped);
    if ui.checkbox(&mut looped, "Loop").changed() {
        if let Some(c) = session.clip_mut(id, slot) {
            c.looped = looped;
        }
        session.sync_clip(id, slot);
    }
    ui.menu_button("Color", |ui| {
        if let Some(c) = color_picker(ui) {
            if let Some(clip) = session.clip_mut(id, slot) {
                clip.color = c;
            }
            session.dirty = true;
            ui.close();
        }
    });
    if let Some(c) = session.clip_mut(id, slot) {
        ui.horizontal(|ui| {
            ui.label("Name");
            ui.text_edit_singleline(&mut c.name);
        });
    }
    ui.separator();
    if ui.button(RichText::new("Delete").color(ARM_RED)).clicked() {
        session.set_clip(id, slot, None);
        if state.selected_clip == Some((id, slot)) {
            state.selected_clip = None;
        }
        ui.close();
    }
}

fn drop_on_track(
    session: &mut Session,
    state: &mut UiState,
    id: TrackId,
    item: &BrowserItem,
    slot: Option<usize>,
) {
    match item {
        BrowserItem::Builtin(t) => session.add_builtin(ChainRef::Track(id), t),
        BrowserItem::Plugin(p) => session.add_plugin(ChainRef::Track(id), p),
        BrowserItem::Sample(path) => {
            let is_audio = session
                .project
                .track(id)
                .is_some_and(|t| t.kind == TrackKind::Audio);
            if !is_audio {
                state.status =
                    "Drop samples onto an audio track (or into the empty area to create one)"
                        .into();
                return;
            }
            match session.audio_clip_from_file(path) {
                Ok(c) => {
                    let slot = slot
                        .or_else(|| {
                            session
                                .project
                                .track(id)
                                .and_then(|t| t.clips.iter().position(|c| c.is_none()))
                        })
                        .unwrap_or(0);
                    session.set_clip(id, slot, Some(c));
                    state.selected_clip = Some((id, slot));
                }
                Err(e) => state.status = e,
            }
        }
    }
    state.selected = Some(ChainRef::Track(id));
}

fn new_track_zone(ui: &mut egui::Ui, session: &mut Session, state: &mut UiState, rect: Rect) {
    let zone = ui.allocate_rect(rect, Sense::hover());
    let hovering = zone.dnd_hover_payload::<BrowserItem>().is_some();
    if hovering {
        ui.painter().rect_stroke(
            rect.shrink(2.0),
            2.0,
            Stroke::new(1.0, ACCENT),
            StrokeKind::Inside,
        );
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            "Drop to create a track",
            theme::font(11.0),
            ACCENT,
        );
    }
    let resp = zone;
    let plus = Rect::from_min_size(
        rect.min + Vec2::new(3.0, 2.0),
        Vec2::new(ADD_COL_W - 6.0, HEADER_H - 4.0),
    );
    let plus_resp = ui.allocate_rect(plus, Sense::click());
    if !hovering {
        if plus_resp.hovered() {
            ui.painter().rect_filled(plus, 1.0, RAISED);
        }
        icons::plus(
            ui.painter(),
            plus,
            if plus_resp.hovered() {
                hex(0xe0e0e0)
            } else {
                hex(0x6a6a6a)
            },
        );
    }
    if let Some(item) = resp.dnd_release_payload::<BrowserItem>() {
        let instrument = matches!(&*item, BrowserItem::Plugin(p) if p.is_instrument)
            || matches!(&*item, BrowserItem::Builtin(b) if rlive_devices::find(b).is_some_and(|b| b.role == rlive_devices::DeviceRole::Instrument));
        let kind = if instrument {
            TrackKind::Midi
        } else {
            TrackKind::Audio
        };
        let id = session.add_track(kind, None);
        if instrument {
            // A dropped instrument replaces the default synth.
            let existing: Vec<_> = session
                .devices(ChainRef::Track(id))
                .iter()
                .map(|d| d.id)
                .collect();
            for d in existing {
                session.remove_device(ChainRef::Track(id), d);
            }
        }
        drop_on_track(session, state, id, &item, None);
    }
    info(
        resp,
        "Drop a device, plugin or sample here to create a track",
    );
    if info(plus_resp, "Add a MIDI track (Cmd+T)").clicked() {
        let id = session.add_track(TrackKind::Midi, None);
        state.selected = Some(ChainRef::Track(id));
    }
}
