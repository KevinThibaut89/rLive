//! Session view, laid out like Ableton Live:
//!
//! ```text
//! ┌ track headers (sticky) ──────┬ returns ┬ Master ┐
//! │ clip grid (scrolls ↕ and ↔)   │         │ scenes │
//! ├ clip stop buttons (sticky) ──┼         ┼ StopAll┤
//! └ mixer strips (sticky, resize) ┴ strips  ┴ master ┘
//! ```
//!
//! Track widths and slot heights adapt to the window; headers, stop row, mixer, return tracks and
//! the Master/scene column stay put while the grid scrolls.

use super::automation;
use super::devices::rack::ChainDrag;
use super::icons;
use super::in_out;
use super::info_view::edit_info_item;
use super::keymap::{self, Platform};
use super::rack_mixer::{self, ChainColumn, ColumnRects, MixFold};
use super::scene_view::{self, SceneRows};
use super::session_select::{click_slot, select_one, selected_slots};
use super::session_status;
use super::theme::{self, *};
use super::widgets::{self, info, info_note, rgb, Led};
use super::{BrowserItem, Detail, UiState};
use crate::session::{can_arm, ChainRef, ClipRef, InfoTarget, Session};
use egui::{
    Align2, Color32, Layout, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, UiBuilder, Vec2,
};
use rlive_core::param::{mixer, EnvTarget, ParamId, ParamOwner, ParamRef};
use rlive_core::TrackId;
use rlive_engine::mix_cell;
use rlive_engine::shared::{NO_SLOT, STOP_QUEUED};
use rlive_project::{ArrangementClip, TrackDelay, TrackKind, PALETTE, SEND_OFF_DB};
use std::sync::atomic::Ordering;

const HEADER_H: f32 = 22.0;
const STOP_ROW_H: f32 = 20.0;
const MIN_TRACK_W: f32 = 84.0;
const MAX_TRACK_W: f32 = 150.0;
const MIN_SLOT_H: f32 = 20.0;
const MAX_SLOT_H: f32 = 26.0;
const ADD_COL_W: f32 = 36.0;
const SCROLLBAR: f32 = 6.0;
const FOLD_W: f32 = 14.0;
/// Room each send takes in a mixer strip.
const SEND_RADIUS: f32 = 9.0;
const SEND_H: f32 = 26.0;

/// Updates the multi-selection for a click on a track.
fn click_track(state: &mut UiState, ui: &egui::Ui, id: TrackId) {
    state.devices.mixer_selection.clear();
    let m = ui.input(|i| i.modifiers);
    if m.shift || m.command {
        match state.selected_tracks.iter().position(|t| *t == id) {
            Some(i) => {
                state.selected_tracks.remove(i);
            }
            None => state.selected_tracks.push(id),
        }
    } else {
        state.selected_tracks = vec![id];
    }
    state.selected = Some(ChainRef::Track(id));
    state.detail = Detail::Devices;
}

/// Layout state that persists across frames.
pub struct SessionLayout {
    pub scroll: Vec2,
    pub mixer_h: f32,
    pub show_mixer: bool,
    /// The mixer's In/Out section.
    pub show_in_out: bool,
    /// The Crossfade Assign buttons and the crossfader.
    pub show_crossfader: bool,
}

impl Default for SessionLayout {
    fn default() -> Self {
        Self {
            scroll: Vec2::ZERO,
            mixer_h: 190.0,
            show_mixer: true,
            show_in_out: false,
            show_crossfader: false,
        }
    }
}

/// The first empty slot after `slot`, adding a scene if there's none.
pub fn next_free_slot(session: &mut Session, track: TrackId, slot: usize) -> usize {
    let scenes = session.project.scenes.len();
    match (slot + 1..scenes).find(|s| session.clip(track, *s).is_none()) {
        Some(t) => t,
        None => {
            session.add_scene();
            session.project.scenes.len() - 1
        }
    }
}

/// Enter on a selected scene.
pub(super) fn launch_selected_scene(session: &mut Session, state: &mut UiState, scene: usize) {
    scene_view::launch_scene(session, state, scene);
}

pub fn duplicate_clip(session: &mut Session, track: TrackId, slot: usize) {
    let Some(clip) = session.clip(track, slot).cloned() else {
        return;
    };
    let target = next_free_slot(session, track, slot);
    session.set_clip(track, target, Some(clip));
}

/// A column of the session grid: a track, or a chain of its rack mixer.
enum Column {
    Track(TrackId, usize),
    Chain(Box<ChainColumn>),
}

/// Geometry of the view for this frame.
struct Geo {
    track_w: f32,
    slot_h: f32,
    tracks_left: f32,
    tracks_right: f32,
    return_w: f32,
    returns: Rect,
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

/// A mixer control of a track, as an envelope addresses it.
fn mixer_target(track: TrackId, id: ParamId) -> EnvTarget {
    EnvTarget::Param(ParamRef {
        owner: ParamOwner::Track(track),
        id,
    })
}

pub(super) fn child(ui: &mut egui::Ui, clip: Rect, rect: Rect, layout: Layout) -> egui::Ui {
    let mut c = ui.new_child(UiBuilder::new().max_rect(rect).layout(layout));
    c.set_clip_rect(clip.intersect(ui.clip_rect()));
    c
}

pub fn show(ui: &mut egui::Ui, session: &mut Session, state: &mut UiState) {
    let full = ui.max_rect();
    let bg = ui.allocate_rect(full, Sense::click());
    ui.painter().rect_filled(full, 0.0, pal().grid_bg);

    // Only what isn't folded away inside a group gets a column.
    let visible = session.project.visible_tracks();
    let mut grid_columns = Vec::with_capacity(visible.len());
    for &i in &visible {
        let track = &session.project.tracks[i];
        grid_columns.push(Column::Track(track.id, i));
        for chain in rack_mixer::chain_columns(track.id, &track.devices, &state.devices.mixer_open)
        {
            grid_columns.push(Column::Chain(Box::new(chain)));
        }
    }
    let n_tracks = grid_columns.len();
    let n_scenes = session.project.scenes.len();
    let n_returns = session.project.returns.len();

    // ---- geometry
    let master_w = (full.width() * 0.13).clamp(112.0, 150.0) + state.sel.scene_fields_w;
    let master_x = full.right() - master_w;
    let return_w = if n_returns == 0 {
        0.0
    } else {
        (full.width() * 0.3 / n_returns as f32)
            .clamp(64.0, 92.0)
            .floor()
    };
    let tracks_right = master_x - return_w * n_returns as f32;
    let mixer_h = if state.layout.show_mixer {
        // On short windows the mixer gives room to the clip grid.
        state
            .layout
            .mixer_h
            .clamp(90.0, (full.height() * 0.42).max(90.0))
    } else {
        0.0
    };
    // The In/Out section is added to the mixer's height, so the strips keep their room.
    let in_out_h = if state.layout.show_mixer && state.layout.show_in_out {
        in_out::IN_OUT_H
    } else {
        0.0
    };
    let mixer_h = mixer_h + in_out_h;
    let mixer = Rect::from_min_max(
        Pos2::new(full.left(), full.bottom() - mixer_h),
        Pos2::new(tracks_right, full.bottom()),
    );
    let stop_row = Rect::from_min_max(
        Pos2::new(full.left(), mixer.top() - STOP_ROW_H),
        Pos2::new(tracks_right, mixer.top()),
    );
    let header = Rect::from_min_max(full.min, Pos2::new(tracks_right, full.top() + HEADER_H));
    let grid = Rect::from_min_max(
        Pos2::new(full.left(), header.bottom()),
        Pos2::new(tracks_right, stop_row.top()),
    );
    let avail_w = (tracks_right - full.left() - ADD_COL_W).max(10.0);
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
        tracks_right,
        return_w,
        returns: Rect::from_min_max(
            Pos2::new(tracks_right, full.top()),
            Pos2::new(master_x, full.bottom()),
        ),
        master: Rect::from_min_max(Pos2::new(master_x, full.top()), full.max),
        header,
        grid,
        stop_row,
        mixer,
        compact_mixer: mixer_h - in_out_h < 140.0,
    };

    // ---- scrolling
    let content_w = n_tracks as f32 * track_w + ADD_COL_W;
    let content_h = n_scenes as f32 * slot_h;
    let max_x = (content_w - (tracks_right - full.left())).max(0.0);
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

    // A launch button comes up wherever the pointer has got to.
    if let Some((track, slot)) = state.sel.held {
        if ui.input(|i| !i.pointer.primary_down()) {
            state.sel.held = None;
            session.clip_up(track, slot);
        }
    }

    if let Some((track, slot)) = state.sel.held_key {
        if ui.input(|i| !i.key_down(egui::Key::Enter)) {
            state.sel.held_key = None;
            session.clip_up(track, slot);
        }
    }

    // ---- tracks
    for (col, column) in grid_columns.iter().enumerate() {
        let x = g.track_x(col, scroll.x);
        if x + track_w < full.left() || x > g.tracks_right {
            continue; // off screen
        }
        match column {
            Column::Track(id, i) => {
                track_column(ui, session, state, &g, *id, *i, x, scroll.y, blink);
            }
            Column::Chain(chain) => chain_column(ui, session, state, &g, chain, x),
        }
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
        Stroke::new(1.0, pal().separator),
    );
    p.line_segment(
        [g.stop_row.left_top(), g.stop_row.right_top()],
        Stroke::new(1.0, pal().separator),
    );
    if n_returns > 0 {
        p.line_segment(
            [g.returns.left_top(), g.returns.left_bottom()],
            Stroke::new(2.0, pal().separator),
        );
    }
    p.line_segment(
        [g.master.left_top(), g.master.left_bottom()],
        Stroke::new(2.0, pal().separator),
    );

    let returns: Vec<TrackId> = session.project.returns.iter().map(|t| t.id).collect();
    for (i, id) in returns.iter().enumerate() {
        return_column(ui, session, state, &g, *id, i);
    }
    master_column(ui, session, state, &g, scroll.y, blink);
    mixer_splitter(ui, state, &g);
    scrollbars(ui, state, &g, full, content_w, content_h, max_x, max_y);

    rubber_band(ui, session, state, &g, &grid_columns, scroll);
    drop_os_files(ui, session, state, &g, &grid_columns, scroll);

    if bg.clicked() {
        state.selected_clip = None;
        state.group_slot = None;
        state.sel.clear();
    }
}

/// The track column and scene under a point in the grid.
fn grid_cell(
    g: &Geo,
    columns: &[Column],
    scroll: Vec2,
    pos: Pos2,
    n_scenes: usize,
) -> Option<(TrackId, usize)> {
    if !g.grid.contains(pos) || n_scenes == 0 {
        return None;
    }
    let col = ((pos.x - g.tracks_left + scroll.x) / g.track_w).floor();
    let row = ((pos.y - g.grid.top() + scroll.y) / g.slot_h).floor();
    if col < 0.0 || row < 0.0 || row as usize >= n_scenes {
        return None;
    }
    match columns.get(col as usize)? {
        Column::Track(id, _) => Some((*id, row as usize)),
        Column::Chain(_) => None,
    }
}

/// Dragging from an empty slot selects everything it passes over.
fn rubber_band(
    ui: &mut egui::Ui,
    session: &Session,
    state: &mut UiState,
    g: &Geo,
    columns: &[Column],
    scroll: Vec2,
) {
    let Some(start) = state.sel.rubber else {
        return;
    };
    let Some(pos) = ui.input(|i| i.pointer.latest_pos()) else {
        return;
    };
    let band = Rect::from_two_pos(start, pos);
    let n = session.project.scenes.len();
    let mut slots = Vec::new();
    for (c, column) in columns.iter().enumerate() {
        let Column::Track(id, _) = column else {
            continue;
        };
        if session.project.track(*id).is_none_or(|t| t.is_group()) {
            continue;
        }
        let x = g.track_x(c, scroll.x);
        for row in 0..n {
            let r = Rect::from_min_size(
                Pos2::new(x, g.slot_y(row, scroll.y)),
                Vec2::new(g.track_w - 1.0, g.slot_h - 1.0),
            );
            if r.intersects(band) {
                slots.push((*id, row));
            }
        }
    }
    ui.painter().rect_stroke(
        band,
        0.0,
        Stroke::new(1.0, pal().accent),
        StrokeKind::Inside,
    );
    if !slots.is_empty() {
        state.sel.scenes.clear();
        state.selected_clip = slots
            .first()
            .map(|(t, s)| ClipRef::slot(*t, *s))
            .filter(|r| session.clip_at(*r).is_some());
        state.sel.slots = slots;
    }
    if ui.input(|i| !i.pointer.primary_down()) {
        state.sel.rubber = None;
    }
}

/// Files dropped on the grid from outside the app (manual p. 195).
fn drop_os_files(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    g: &Geo,
    columns: &[Column],
    scroll: Vec2,
) {
    let paths: Vec<std::path::PathBuf> = ui.input(|i| {
        i.raw
            .dropped_files
            .iter()
            .map(|f| f.path().to_path_buf())
            .collect()
    });
    if paths.is_empty() {
        return;
    }
    let n = session.project.scenes.len();
    let (pos, spread) = ui.input(|i| (i.pointer.latest_pos(), i.modifiers.command));
    let target = pos
        .and_then(|p| grid_cell(g, columns, scroll, p, n))
        .or_else(|| state.selected_clip.and_then(ClipRef::as_slot));
    let Some((track, slot)) = target else {
        return;
    };
    let placed = session.drop_files(&paths, track, slot, spread);
    if placed.is_empty() {
        state.status = "Drop audio files onto an audio track".into();
    }
    state.sel.slots = placed;
}

/// A chain of a rack mixer: a header, an empty stretch where clips would go, and its strip.
fn chain_column(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    g: &Geo,
    chain: &ChainColumn,
    x: f32,
) {
    let w = g.track_w;
    let rects = ColumnRects {
        header: Rect::from_min_size(
            Pos2::new(x, g.header.top()),
            Vec2::new(w - 1.0, HEADER_H - 1.0),
        ),
        grid: Rect::from_min_max(
            Pos2::new(x, g.grid.top()),
            Pos2::new(x + w, g.stop_row.bottom()),
        ),
        strip: Rect::from_min_max(
            Pos2::new(x, g.mixer.top()),
            Pos2::new(x + w - 1.0, g.mixer.bottom()),
        ),
        clip: Rect::from_min_max(
            Pos2::new(g.tracks_left, g.header.top()),
            Pos2::new(g.tracks_right, g.mixer.bottom()),
        ),
        compact: g.compact_mixer,
    };
    rack_mixer::chain_column(ui, session, state, chain, rects);
}

/// A return track's column: its header, an empty stretch for the grid, and its mixer strip.
fn return_column(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    g: &Geo,
    id: TrackId,
    index: usize,
) {
    let Some(t) = session.project.track(id) else {
        return;
    };
    let (name, color) = (t.name.clone(), t.color);
    let selected = state.selected == Some(ChainRef::Track(id));
    let track_color = rgb(color);
    let x = g.returns.left() + index as f32 * g.return_w;
    let w = g.return_w - 1.0;
    let body = Rect::from_min_max(
        Pos2::new(x, g.header.bottom()),
        Pos2::new(x + w, g.mixer.top()),
    );
    ui.painter().rect_filled(body, 0.0, pal().panel_dark);
    let rect = Rect::from_min_size(Pos2::new(x, g.header.top()), Vec2::new(w, HEADER_H - 1.0));
    let resp = ui.allocate_rect(rect, Sense::click());
    let fill = if selected {
        track_color
    } else {
        theme::darken(track_color, 0.35)
    };
    ui.painter().rect_filled(rect, 0.0, fill);
    if selected {
        widgets::outline(ui.painter(), rect, shade(0xffffff));
    }
    let renaming = matches!(&state.renaming, Some((tid, _)) if *tid == id);
    if renaming {
        let mut done = false;
        if let Some((_, text)) = &mut state.renaming {
            let r = ui.put(
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
        let galley =
            ui.painter()
                .layout_no_wrap(name.clone(), theme::font_bold(11.0), theme::text_on(fill));
        ui.painter()
            .with_clip_rect(rect.intersect(ui.clip_rect()))
            .galley(
                Pos2::new(rect.left() + 5.0, rect.center().y - galley.size().y / 2.0),
                galley,
                Color32::WHITE,
            );
    }
    if resp.clicked() {
        click_track(state, ui, id);
    }
    if resp.double_clicked() {
        state.renaming = Some((id, name.clone()));
    }
    if let Some(item) = resp.dnd_release_payload::<BrowserItem>() {
        drop_on_track(session, state, id, &item, None);
    }
    let resp = info(
        resp,
        format!("Return track '{name}' — click to select, double-click to rename"),
    );
    info_note(
        &resp,
        session.info_text(InfoTarget::Track(id)).unwrap_or_default(),
    );
    resp.context_menu(|ui| return_menu(ui, session, state, id, &name));
    if g.mixer.height() > 0.0 {
        let rect = Rect::from_min_size(Pos2::new(x, g.mixer.top()), Vec2::new(w, g.mixer.height()));
        let col_clip = Rect::from_min_max(
            Pos2::new(g.returns.left(), g.mixer.top()),
            Pos2::new(g.returns.right(), g.mixer.bottom()),
        );
        let mut cu = child(ui, col_clip, rect, Layout::top_down(egui::Align::Center));
        let letter = ((b'A' + index as u8) as char).to_string();
        mixer_strip(
            &mut cu,
            session,
            state,
            id,
            &letter,
            rect,
            g.compact_mixer,
            selected,
            false,
        );
    }
}

pub(super) fn return_menu(
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
    edit_info_item(ui, session, state, InfoTarget::Track(id));
    ui.menu_button("Color", |ui| {
        if let Some(c) = color_picker(ui) {
            if let Some(t) = session.project.track_mut(id) {
                t.color = c;
            }
            session.dirty = true;
            ui.close();
        }
    });
    ui.separator();
    if ui.button("Insert Return Track").clicked() {
        if let Some(new) = session.add_return() {
            state.selected = Some(ChainRef::Track(new));
        }
        ui.close();
    }
    if ui
        .button(RichText::new("Delete Return Track").color(pal().arm_red))
        .clicked()
    {
        session.delete_return(id);
        if state.selected == Some(ChainRef::Track(id)) {
            state.selected = None;
        }
        ui.close();
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
    let (name, color, kind, arm, folded) = (t.name.clone(), t.color, t.kind, t.arm, t.ui.folded);
    let shared = session.track_shared.get(&id).cloned();
    let playing_slot = shared
        .as_ref()
        .map_or(NO_SLOT, |s| s.playing_slot.load(Ordering::Relaxed));
    let queued_slot = shared
        .as_ref()
        .map_or(NO_SLOT, |s| s.queued_slot.load(Ordering::Relaxed));
    let recording_slot = session.recording_slot(id);
    let rec_queued = shared
        .as_ref()
        .map(|s| s.record_queued_slot.load(Ordering::Relaxed))
        .filter(|s| *s != NO_SLOT)
        .map(|s| s as usize);
    let position = shared.as_ref().map_or(0.0, |s| s.clip_position.load());
    let selected = state.selected == Some(ChainRef::Track(id));
    let has_rack_mixer = rack_mixer::track_has_mixer(&t.devices);
    let rack_mixer_open = state.devices.mixer_open.contains(&MixFold::Track(id));
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
        ui.painter().rect_filled(r, 0.0, shade(0x262626));
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
        if let Some(group) = session.project.parent_group(id) {
            let group_color = session
                .project
                .track(group)
                .map_or(track_color, |t| rgb(t.color));
            cu.painter().rect_filled(
                Rect::from_min_size(rect.min, Vec2::new(rect.width(), 3.0)),
                0.0,
                group_color,
            );
        }
        if selected {
            widgets::outline(cu.painter(), rect, shade(0xffffff));
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
            let mut clip_r = rect.shrink2(Vec2::new(5.0, 0.0));
            if kind == TrackKind::Group || has_rack_mixer {
                icons::fold(
                    cu.painter(),
                    Rect::from_center_size(
                        Pos2::new(rect.left() + FOLD_W / 2.0, rect.center().y),
                        Vec2::splat(FOLD_W),
                    ),
                    if kind == TrackKind::Group {
                        !folded
                    } else {
                        rack_mixer_open
                    },
                    theme::text_on(fill),
                );
                clip_r.min.x += FOLD_W;
            }
            cu.painter()
                .with_clip_rect(clip_r.intersect(cu.clip_rect()))
                .galley(
                    Pos2::new(clip_r.left(), rect.center().y - galley.size().y / 2.0),
                    galley,
                    Color32::WHITE,
                );
        }
        let on_fold = (kind == TrackKind::Group || has_rack_mixer)
            && resp
                .interact_pointer_pos()
                .is_some_and(|pos| pos.x < rect.left() + FOLD_W);
        // A chain dragged over a track gives it focus, ready for a drop in its Device View.
        if resp.dnd_hover_payload::<ChainDrag>().is_some() {
            state.selected = Some(ChainRef::Track(id));
            state.detail = Detail::Devices;
        }
        if resp.clicked() {
            if on_fold && kind == TrackKind::Group {
                session.set_folded(id, !folded);
            } else if on_fold {
                let key = MixFold::Track(id);
                if !state.devices.mixer_open.remove(&key) {
                    state.devices.mixer_open.insert(key);
                }
            } else {
                click_track(state, ui, id);
            }
        }
        if resp.double_clicked() && !on_fold {
            state.renaming = Some((id, name.clone()));
        }
        resp.dnd_set_drag_payload(TrackDrag(id));
        if resp.dnd_hover_payload::<BrowserItem>().is_some()
            || resp.dnd_hover_payload::<TrackDrag>().is_some()
        {
            widgets::outline(cu.painter(), rect, pal().accent);
        }
        if let Some(item) = resp.dnd_release_payload::<BrowserItem>() {
            drop_on_track(session, state, id, &item, None);
        }
        if let Some(dragged) = resp.dnd_release_payload::<TrackDrag>() {
            if dragged.0 != id {
                drop_track(session, dragged.0, id, kind);
            }
        }
        let resp = info(
            resp,
            format!(
                "Track '{name}' — click to select, double-click to rename, right-click for options"
            ),
        );
        info_note(
            &resp,
            session.info_text(InfoTarget::Track(id)).unwrap_or_default(),
        );
        if session.is_frozen(id) {
            super::commands::bounce::frozen_badge(cu.painter(), rect.right_top());
        }
        resp.context_menu(|ui| track_menu(ui, session, state, id, &name));
    }

    // ---- clip slots
    let n_scenes = session.project.scenes.len();
    let first = ((scroll_y / g.slot_h).floor().max(0.0)) as usize;
    let last = (((scroll_y + g.grid.height()) / g.slot_h).ceil() as usize).min(n_scenes);
    for slot in first..last {
        let y = g.slot_y(slot, scroll_y);
        let rect = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w - 1.0, g.slot_h - 1.0));
        if kind == TrackKind::Group {
            group_slot(ui, session, state, g, col_clip, id, slot, rect);
            continue;
        }
        let clip_info = session
            .clip(id, slot)
            .map(|c| (c.name.clone(), c.color, c.length));
        let striped = session
            .clip(id, slot)
            .is_some_and(|c| c.launch.follow.assigned());
        let clip_active = session.clip(id, slot).is_none_or(|c| c.active);
        let mut cu = child(
            ui,
            g.grid.intersect(col_clip),
            rect,
            Layout::left_to_right(egui::Align::Center),
        );
        let resp = cu.allocate_rect(rect, Sense::click_and_drag());
        let p = cu.painter().clone();
        let is_sel = state.selected_clip == Some(ClipRef::slot(id, slot))
            || state.sel.slots.contains(&(id, slot))
            || state.group_slot.is_some_and(|(group, at)| {
                at == slot && session.project.ancestors(id).contains(&group)
            });
        let launch = Rect::from_min_size(rect.min, Vec2::new(g.slot_h.min(22.0), rect.height()));
        let over_launch = resp.hover_pos().is_some_and(|pos| launch.contains(pos));
        let is_playing = playing_slot == slot as i32;
        let is_queued = queued_slot == slot as i32;
        let recording_here = recording_slot == Some(slot);
        let waiting_for_sample = session.clip_waiting_for_sample(id, slot);
        let stop_button = session.has_stop_button(id, slot);

        match &clip_info {
            Some((cname, ccol, clen)) => {
                let fill = if clip_active {
                    rgb(*ccol)
                } else {
                    pal().raised
                };
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
                if session.clip(id, slot).is_some_and(|c| c.frozen.is_some()) {
                    super::commands::bounce::frozen_glaze(&p, rect);
                }
                let tri = if waiting_for_sample {
                    pal().text_faint
                } else if is_playing || (is_queued && blink) {
                    pal().play_green
                } else if over_launch {
                    hex(0xffffff)
                } else {
                    theme::text_on(fill)
                };
                if striped {
                    icons::play_striped(&p, launch, tri, theme::darken(fill, 0.25));
                } else {
                    icons::play(&p, launch, tri);
                }
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
                    widgets::outline(&p, rect, shade(0xffffff));
                }
            }
            None => {
                p.rect_filled(
                    rect,
                    0.0,
                    if resp.hovered() {
                        pal().slot_empty_hover
                    } else {
                        pal().slot_empty
                    },
                );
                let col = if queued_slot == STOP_QUEUED && blink {
                    pal().accent
                } else if over_launch {
                    shade(0xe0e0e0)
                } else {
                    shade(0x6a6a6a)
                };
                if recording_here {
                    p.rect_filled(rect, 0.0, shade(0x6a2020));
                    icons::record(&p, launch.shrink(2.0), pal().arm_red);
                } else if rec_queued == Some(slot) {
                    let color = if blink {
                        pal().arm_red
                    } else {
                        shade(0x8a4a4a)
                    };
                    icons::record_ring(&p, launch, color);
                } else if arm {
                    icons::record_ring(
                        &p,
                        launch,
                        if over_launch {
                            pal().arm_red
                        } else {
                            shade(0x8a4a4a)
                        },
                    );
                } else if stop_button {
                    icons::stop(&p, launch.shrink(3.0), col);
                }
                if is_sel {
                    widgets::outline(&p, rect, shade(0x9a9a9a));
                }
            }
        }
        if resp.dnd_hover_payload::<BrowserItem>().is_some()
            || resp.dnd_hover_payload::<ClipDrag>().is_some()
        {
            widgets::outline(&p, rect, pal().accent);
        }

        // A clip launches as the button goes down: Gate and Repeat need to see it held.
        if clip_info.is_some() && over_launch && cu.input(|i| i.pointer.primary_pressed()) {
            session.clip_down(id, slot, 1.0);
            state.sel.held = Some((id, slot));
            if state.sel.prefs.select_on_launch {
                state.selected = Some(ChainRef::Track(id));
                state.group_slot = None;
                select_one(state, id, slot);
                state.detail = Detail::Clip;
                state.piano.reset_view = true;
            }
        }
        if resp.drag_started() && clip_info.is_none() {
            state.sel.rubber = cu.input(|i| i.pointer.press_origin());
        }
        if resp.clicked() {
            state.group_slot = None;
            if over_launch {
                let queued = rec_queued == Some(slot);
                if clip_info.is_some() {
                    // Already launched when the button went down.
                } else if recording_here || queued {
                    state.selected = Some(ChainRef::Track(id));
                    session.close_slot_recording(id);
                } else if arm {
                    state.selected = Some(ChainRef::Track(id));
                    session.record_slot(id, slot);
                } else if stop_button {
                    state.selected = Some(ChainRef::Track(id));
                    session.stop_track(id);
                }
            } else {
                state.selected = Some(ChainRef::Track(id));
                let (shift, cmd) = cu.input(|i| (i.modifiers.shift, i.modifiers.command));
                click_slot(session, state, id, slot, shift, cmd);
                state.detail = if clip_info.is_some() {
                    Detail::Clip
                } else {
                    Detail::Devices
                };
                state.piano.reset_view = true;
            }
        }
        if resp.double_clicked() && !over_launch {
            let alt = ui.input(|i| i.modifiers.alt);
            if clip_info.is_none() && kind == TrackKind::Midi && alt {
                session.create_arm_and_launch(id, slot);
                state.selected_clip = Some(ClipRef::slot(id, slot));
                state.detail = Detail::Clip;
                state.show_detail = true;
                state.piano.reset_view = true;
            } else if clip_info.is_none() && kind == TrackKind::Midi {
                let color = session.new_clip_color(id);
                let c = session.new_midi_clip(color, session.project.time_signature);
                session.set_clip(id, slot, Some(c));
                select_one(state, id, slot);
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
                let moved = if state.sel.slots.contains(&(src.track, src.slot)) {
                    state.sel.slots.clone()
                } else {
                    vec![(src.track, src.slot)]
                };
                let index = |t: TrackId| session.project.track_index(t).unwrap_or(0) as isize;
                let tracks = index(id) - index(src.track);
                let scenes = slot as isize - src.slot as isize;
                let copy = cu.input(|i| keymap::copies_on_drag(i.modifiers, Platform::current()));
                let landed = session.move_slots(&moved, tracks, scenes, copy);
                if let Some((t, s)) = landed.first().copied() {
                    state.selected_clip = Some(ClipRef::slot(t, s));
                    state.sel.slots = landed;
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
        info_note(
            &resp,
            session
                .info_text(InfoTarget::Clip(ClipRef::slot(id, slot)))
                .unwrap_or_default(),
        );
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
                pal().slot_empty_hover
            } else {
                pal().panel_dark
            },
        );
        let stop_w = rect.width().min(22.0);
        let status_r = Rect::from_min_max(Pos2::new(rect.left() + stop_w, rect.top()), rect.max);
        session_status::show(&mut cu, session, state, id, status_r);
        let active =
            playing_slot != NO_SLOT || (kind == TrackKind::Group && group_is_playing(session, id));
        let col = if queued_slot == STOP_QUEUED && blink {
            pal().accent
        } else if active {
            shade(0xd0d0d0)
        } else {
            shade(0x5a5a5a)
        };
        icons::stop(
            cu.painter(),
            Rect::from_min_size(rect.min, Vec2::new(stop_w, rect.height())),
            col,
        );
        if kind == TrackKind::Group {
            if info(resp, "Group Stop — stop every clip in this group").clicked() {
                session.stop_group(id);
            }
        } else if info(resp, "Clip Stop — stop the clip playing on this track").clicked() {
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
            &(index + 1).to_string(),
            rect,
            g.compact_mixer,
            selected,
            kind != TrackKind::Group,
        );
    }
}

/// A group track's slot: launches and stops what its members have in this scene.
#[allow(clippy::too_many_arguments)]
fn group_slot(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    g: &Geo,
    col_clip: Rect,
    id: TrackId,
    slot: usize,
    rect: Rect,
) {
    let mut cu = child(
        ui,
        g.grid.intersect(col_clip),
        rect,
        Layout::left_to_right(egui::Align::Center),
    );
    let resp = cu.allocate_rect(rect, Sense::click());
    let p = cu.painter().clone();
    let filled = session.group_slot_has_clip(id, slot);
    let launch = Rect::from_min_size(rect.min, Vec2::new(g.slot_h.min(22.0), rect.height()));
    let stop = Rect::from_min_size(
        Pos2::new(rect.right() - launch.width(), rect.top()),
        Vec2::new(launch.width(), rect.height()),
    );
    let over_launch = resp.hover_pos().is_some_and(|pos| launch.contains(pos));
    let over_stop = resp.hover_pos().is_some_and(|pos| stop.contains(pos));
    let is_sel = state.group_slot == Some((id, slot));
    if filled {
        // Shaded by the left-most clip in the scene.
        let color = session
            .project
            .group_contents(id)
            .iter()
            .find_map(|t| session.clip(*t, slot))
            .map_or(pal().raised, |c| rgb(c.color));
        p.rect_filled(rect, 0.0, theme::darken(color, 0.55));
        icons::play(
            &p,
            launch,
            if over_launch {
                shade(0xffffff)
            } else {
                shade(0xbdbdbd)
            },
        );
        icons::stop(
            &p,
            stop.shrink(3.0),
            if over_stop {
                shade(0xffffff)
            } else {
                shade(0x8a8a8a)
            },
        );
    } else {
        p.rect_filled(rect, 0.0, pal().slot_empty);
    }
    if is_sel {
        widgets::outline(&p, rect, shade(0xffffff));
    }
    if resp.clicked() && filled {
        if over_launch {
            session.launch_group_slot(id, slot);
        } else if over_stop {
            session.stop_group(id);
        } else {
            // Selects all of the group's clips in this scene.
            state.selected = Some(ChainRef::Track(id));
            state.selected_clip = None;
            state.group_slot = Some((id, slot));
        }
    }
    if filled {
        resp.context_menu(|ui| {
            super::commands::bounce::group_slot_menu_items(ui, session, state, id, slot);
        });
    }
    let text = if filled {
        "Group slot — ▶ launches its clips, ■ stops them, click selects them"
    } else {
        "No clips in this scene"
    };
    let _ = info(resp, text);
}

fn group_is_playing(session: &Session, group: TrackId) -> bool {
    session.project.group_contents(group).iter().any(|t| {
        session
            .track_shared
            .get(t)
            .is_some_and(|s| s.playing_slot.load(Ordering::Relaxed) != NO_SLOT)
    })
}

/// Drops a dragged track on another's header: into a group, or to that place in the list.
fn drop_track(session: &mut Session, dragged: TrackId, target: TrackId, kind: TrackKind) {
    if kind == TrackKind::Group {
        session.move_into_group(dragged, target);
        return;
    }
    let Some(at) = session.project.track_index(target) else {
        return;
    };
    let block = session.project.track_block(dragged);
    let to = session.project.tracks[..at]
        .iter()
        .filter(|t| !block.contains(&t.id))
        .count();
    session.move_track(dragged, to);
}

#[allow(clippy::too_many_arguments)]
fn mixer_strip(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    id: TrackId,
    label: &str,
    rect: Rect,
    compact: bool,
    selected: bool,
    armable: bool,
) {
    let Some(t) = session.project.track(id) else {
        return;
    };
    let (vol, pan, mute, solo, arm, color) =
        (t.volume_db, t.pan, t.mute, t.solo, t.arm, rgb(t.color));
    let chain = ChainRef::Track(id);
    let is_return = t.kind == TrackKind::Return;
    let pre_fader = t.pre_fader_sends;
    let holds_solo = t.kind == TrackKind::Group && session.project.holds_solo(id);
    let sends: Vec<(TrackId, f32, bool)> = session
        .project
        .returns
        .iter()
        .map(|r| (r.id, t.send_level(r.id), t.send_enabled(r.id)))
        .collect();
    let solo_in_place = session.project.solo_in_place;
    let cue_mode = session.project.cue.cue_mode;
    let monitoring_in = matches!(t.kind, TrackKind::Audio | TrackKind::Midi)
        && t.routing.monitor == rlive_project::Monitor::In;
    let delay = t.delay;
    let meter = state.meters.get(&id).copied().unwrap_or_default();
    let p = ui.painter().clone();
    p.rect_filled(
        rect,
        0.0,
        if selected {
            shade(0x363636)
        } else {
            pal().panel
        },
    );
    p.line_segment(
        [rect.right_top(), rect.right_bottom()],
        Stroke::new(1.0, pal().separator),
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

    if !compact && state.layout.show_in_out {
        y = in_out::strip_section(ui, session, state, id, inner, y);
    }

    if !compact {
        if is_return {
            let toggle = Rect::from_center_size(
                Pos2::new(inner.center().x, y + 8.0),
                Vec2::new(inner.width().min(46.0), 15.0),
            );
            let mut bu = ui.new_child(UiBuilder::new().max_rect(toggle));
            let label = if pre_fader { "Pre" } else { "Post" };
            let r = widgets::state_button(&mut bu, toggle.size(), pre_fader, pal().accent, label);
            if info(
                r,
                "Pre/Post — whether sends feed this return before or after each track's mixer",
            )
            .clicked()
            {
                session.set_pre_fader_sends(id, !pre_fader);
            }
            y += 18.0;
        }
        let knob_r =
            Rect::from_center_size(Pos2::new(inner.center().x, y + 13.0), Vec2::new(28.0, 26.0));
        let mut ku = ui.new_child(UiBuilder::new().max_rect(knob_r));
        let shown = session
            .automated_cell_value(chain, mix_cell::PAN)
            .map_or(pan, |v| v as f32);
        if let Some(v) = widgets::pan_knob(&mut ku, shown) {
            new_mix.1 = v;
        }
        if let Some(moved) = session.modulated_cell_norm(chain, mix_cell::PAN) {
            let needle = (shown + 1.0) / 2.0;
            if let Some(span) = widgets::mod_ring_span(needle, moved) {
                widgets::paint_mod_ring(&p, knob_r.center(), 9.5, span);
            }
        }
        Led::of(session.cell_state(chain, mix_cell::PAN))
            .paint(&p, knob_r.left_top() + Vec2::new(2.0, 2.0));
        automation::control_zone(
            &ku,
            session,
            state,
            knob_r,
            chain,
            mixer_target(id, mixer::PAN),
        );
        y += 30.0;
        if !sends.is_empty() {
            y = send_knobs(ui, session, state, id, is_return, &sends, inner, y);
        }
        y = track_delay_row(ui, session, id, delay, inner, y);
    }

    // Fader + meter take the remaining height above the buttons and dB box.
    let assign_h = if state.layout.show_crossfader {
        in_out::ASSIGN_H
    } else {
        0.0
    };
    let fader_bottom = inner.bottom() - buttons_h - 18.0 - assign_h;
    let fader_w = (inner.width() * 0.5).clamp(26.0, 44.0);
    let fader_r = Rect::from_min_max(
        Pos2::new(inner.center().x - fader_w / 2.0, y),
        Pos2::new(inner.center().x + fader_w / 2.0, fader_bottom.max(y + 20.0)),
    );
    let mut fu = ui.new_child(UiBuilder::new().max_rect(fader_r));
    let shown_vol = session
        .automated_cell_value(chain, mix_cell::VOLUME)
        .map_or(vol, |v| v as f32);
    if let Some(db) = widgets::fader(
        &mut fu,
        fader_r.size(),
        shown_vol,
        meter.get(),
        pal().accent,
    ) {
        new_mix.0 = db;
    }
    if let Some(norm) = session.modulated_cell_norm(chain, mix_cell::VOLUME) {
        let db = mixer::VOLUME_MIN_DB as f32
            + norm * (mixer::VOLUME_MAX_DB - mixer::VOLUME_MIN_DB) as f32;
        widgets::paint_fader_dot(&p, fader_r, db);
    }
    Led::of(session.cell_state(chain, mix_cell::VOLUME))
        .paint(&p, fader_r.left_top() + Vec2::new(-5.0, 4.0));
    automation::control_zone(
        &fu,
        session,
        state,
        fader_r,
        chain,
        mixer_target(id, mixer::VOLUME),
    );
    let db_r = Rect::from_center_size(
        Pos2::new(inner.center().x, fader_r.bottom() + 9.0),
        Vec2::new(inner.width().min(54.0), 14.0),
    );
    p.rect_filled(db_r, 1.0, pal().field_bg);
    let db_text = if shown_vol <= -70.0 {
        "-inf".to_string()
    } else {
        format!("{shown_vol:.1}")
    };
    p.text(
        db_r.center(),
        Align2::CENTER_CENTER,
        db_text,
        theme::font(10.0),
        pal().text,
    );

    // Activator / Solo / Arm. Monitoring In tints the Activator blue (manual p. 374).
    let activator_color = if monitoring_in {
        pal().solo_blue
    } else {
        pal().activator
    };
    let shown_mute = session
        .automated_cell_value(chain, mix_cell::MUTE)
        .map_or(mute, |v| v >= 0.5);
    let bw = ((inner.width() - 4.0) / 3.0).floor();
    if assign_h > 0.0 {
        let row = Rect::from_min_size(
            Pos2::new(inner.left(), inner.bottom() - buttons_h - assign_h),
            Vec2::new(inner.width(), assign_h - 2.0),
        );
        in_out::assign_row(ui, session, state, id, row);
    }
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
        !shown_mute,
        activator_color,
        label,
    );
    if info(r, "Track Activator — click to mute/unmute").clicked() {
        new_mix.2 = !shown_mute;
    }
    let r = widgets::state_button(
        &mut bu,
        Vec2::new(bw, buttons_h),
        solo,
        pal().solo_blue,
        if cue_mode { "Cue" } else { "S" },
    );
    if holds_solo && !solo {
        // A soloed track somewhere inside: the button lights up half way.
        let half =
            Rect::from_min_size(r.rect.min, Vec2::new(r.rect.width() / 2.0, r.rect.height()));
        p.rect_filled(half, 1.0, pal().solo_blue);
        p.text(
            r.rect.center(),
            Align2::CENTER_CENTER,
            "S",
            theme::font(10.5),
            pal().text,
        );
    }
    let r = info(
        r,
        "Solo — Cmd-click to solo several tracks; right-click for Solo in Place",
    );
    if r.clicked() {
        new_mix.3 = !solo;
        // Like Live: solo is exclusive unless Cmd is held.
        if !solo && !bu.input(|i| i.modifiers.command) {
            clear_other_solos(session, id);
        }
    }
    r.context_menu(|ui| {
        if ui
            .selectable_label(solo_in_place, "Solo in Place")
            .clicked()
        {
            session.set_solo_in_place(!solo_in_place);
            ui.close();
        }
    });
    if armable && !is_return && can_arm(&session.project, id) {
        let r = widgets::icon_button(
            &mut bu,
            Vec2::new(bw, buttons_h),
            arm,
            pal().arm_red,
            icons::record,
        );
        if info(r, "Arm — record onto this track (Cmd-click to arm several)").clicked() {
            let exclusive = session.record.exclusive_arm && !bu.input(|i| i.modifiers.command);
            let selection = if state.selected_tracks.contains(&id) {
                state.selected_tracks.clone()
            } else {
                vec![id]
            };
            for (i, track) in selection.iter().enumerate() {
                session.set_arm(*track, !arm, exclusive && i == 0);
            }
            state.selected = Some(ChainRef::Track(id));
        }
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

/// One knob per return track. Returns the height to carry on from.
#[allow(clippy::too_many_arguments)]
fn send_knobs(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    id: TrackId,
    is_return: bool,
    sends: &[(TrackId, f32, bool)],
    inner: Rect,
    y: f32,
) -> f32 {
    let per_row = ((inner.width() / (SEND_RADIUS * 2.0 + 6.0)).floor() as usize).max(1);
    let rows = sends.len().div_ceil(per_row).min(2);
    let shown = (per_row * rows).min(sends.len());
    let p = ui.painter().clone();
    for (n, (ret, db, enabled)) in sends.iter().take(shown).enumerate() {
        let col = n % per_row;
        let row = n / per_row;
        let cell_w = inner.width() / per_row as f32;
        let center = Pos2::new(
            inner.left() + (col as f32 + 0.5) * cell_w,
            y + SEND_RADIUS + 2.0 + row as f32 * SEND_H,
        );
        let hit = Rect::from_center_size(center, Vec2::splat(SEND_RADIUS * 2.0 + 4.0));
        let resp = ui.interact(
            hit,
            ui.scope_id().with(("send", id, n)),
            Sense::click_and_drag(),
        );
        let norm = ((*db as f64 - rlive_engine::SEND_MIN_DB)
            / (rlive_engine::SEND_MAX_DB - rlive_engine::SEND_MIN_DB)) as f32;
        let mut changed = None;
        if resp.dragged() && *enabled {
            let fine = ui.input(|i| i.modifiers.shift);
            let delta = -resp.drag_delta().y / if fine { 600.0 } else { 120.0 };
            let v = (norm + delta).clamp(0.0, 1.0);
            let db_new = rlive_engine::SEND_MIN_DB + v as f64 * (0.0 - rlive_engine::SEND_MIN_DB);
            changed = Some(db_new as f32);
        }
        if resp.double_clicked() && *enabled {
            changed = Some(SEND_OFF_DB);
        }
        let tone = if !*enabled {
            widgets::KnobTone::Disabled
        } else if resp.hovered() || resp.dragged() {
            widgets::KnobTone::Active
        } else {
            widgets::KnobTone::Normal
        };
        widgets::paint_knob(&p, center, SEND_RADIUS, norm, false, tone);
        if let Some(moved) = session.modulated_cell_norm(ChainRef::Track(id), mix_cell::send(n)) {
            if let Some(span) = widgets::mod_ring_span(norm, moved) {
                widgets::paint_mod_ring(&p, center, SEND_RADIUS, span);
            }
        }
        p.text(
            center + Vec2::new(0.0, SEND_RADIUS + 3.0),
            Align2::CENTER_CENTER,
            ((b'A' + n as u8) as char).to_string(),
            theme::font(9.0),
            if *enabled {
                pal().text_dim
            } else {
                pal().text_faint
            },
        );
        if let Some(value) = changed {
            session.set_send(id, *ret, value);
        }
        if !is_return {
            let target = mixer_target(id, ParamId(mixer::FIRST_SEND.0 + n as u32));
            automation::control_zone(ui, session, state, hit, ChainRef::Track(id), target);
        }
        let label = if *db <= SEND_OFF_DB + 0.5 {
            "-inf".to_string()
        } else {
            format!("{db:.1} dB")
        };
        let text = if *enabled {
            format!("Send {} — {label}", (b'A' + n as u8) as char)
        } else {
            format!(
                "Send {} — disabled to prevent feedback; right-click to enable",
                (b'A' + n as u8) as char
            )
        };
        let resp = info(resp, text);
        if is_return {
            let (ret, enabled) = (*ret, *enabled);
            resp.context_menu(|ui| {
                let label = if enabled {
                    "Disable Send"
                } else {
                    "Enable Send"
                };
                if ui.button(label).clicked() {
                    session.set_send_enabled(id, ret, !enabled);
                    ui.close();
                }
                if ui.button("Enable All Sends").clicked() {
                    session.enable_all_sends(id);
                    ui.close();
                }
            });
        }
    }
    y + rows as f32 * SEND_H
}

const DELAY_RANGE_MS: f32 = 500.0;
const DELAY_RANGE_SAMPLES: i32 = 48_000;

/// Track Delay: a value, dragged, and a toggle between milliseconds and samples. Disabled while
/// delay compensation is off, as Live has it. Returns the height to carry on from.
fn track_delay_row(
    ui: &mut egui::Ui,
    session: &mut Session,
    id: TrackId,
    delay: TrackDelay,
    inner: Rect,
    y: f32,
) -> f32 {
    let sample_rate = session.engine.sample_rate as f32;
    let row = Rect::from_min_size(Pos2::new(inner.left(), y), Vec2::new(inner.width(), 14.0));
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(row)
            .layout(Layout::left_to_right(egui::Align::Center)),
    );
    child.spacing_mut().item_spacing.x = 2.0;
    if !session.project.delay_compensation {
        child.disable();
    }
    let mut next = delay;
    match delay {
        TrackDelay::Samples(mut n) => {
            let drag = egui::DragValue::new(&mut n)
                .range(-DELAY_RANGE_SAMPLES..=DELAY_RANGE_SAMPLES)
                .speed(1.0);
            if child.add(drag).changed() {
                next = TrackDelay::Samples(n);
            }
        }
        other => {
            let mut ms = match other {
                TrackDelay::Ms(ms) => ms,
                _ => 0.0,
            };
            let drag = egui::DragValue::new(&mut ms)
                .range(-DELAY_RANGE_MS..=DELAY_RANGE_MS)
                .speed(0.25)
                .fixed_decimals(1);
            if child.add(drag).changed() {
                next = TrackDelay::Ms(ms);
            }
        }
    }
    let unit = if matches!(delay, TrackDelay::Samples(_)) {
        "smp"
    } else {
        "ms"
    };
    if child
        .button(egui::RichText::new(unit).font(theme::font(9.0)))
        .on_hover_text("Track Delay — switch between milliseconds and samples")
        .clicked()
    {
        next = match delay {
            TrackDelay::Samples(n) => TrackDelay::Ms(n as f32 / sample_rate * 1000.0),
            TrackDelay::Ms(ms) => TrackDelay::Samples((ms / 1000.0 * sample_rate).round() as i32),
            TrackDelay::None => TrackDelay::Samples(0),
        };
    }
    if next != delay {
        session.set_track_delay(id, next);
    }
    y + 16.0
}

fn clear_other_solos(session: &mut Session, id: TrackId) {
    let others: Vec<TrackId> = session
        .project
        .tracks
        .iter()
        .chain(&session.project.returns)
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

/// Live-style solo: exclusive unless `additive`.
pub(super) fn toggle_solo(session: &mut Session, id: TrackId, solo: bool, additive: bool) {
    if !solo && !additive {
        clear_other_solos(session, id);
    }
    if let Some(t) = session.project.track_mut(id) {
        t.solo = !solo;
    }
    session.sync_mix(id);
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
        pal().panel_dark,
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
            shade(0xbdbdbd)
        } else {
            shade(0x7a7a7a)
        },
    );
    p.text(
        hr.left_center() + Vec2::new(6.0, 0.0),
        Align2::LEFT_CENTER,
        "Master",
        theme::font_bold(11.0),
        pal().text_dark,
    );
    if resp.clicked() {
        state.selected = Some(ChainRef::Master);
        state.detail = if state.sel.scenes.is_empty() {
            Detail::Devices
        } else {
            Detail::Scene
        };
    }
    if let Some(item) = resp.dnd_release_payload::<BrowserItem>() {
        match &*item {
            BrowserItem::Builtin(t) => session.add_builtin(ChainRef::Master, t),
            BrowserItem::Plugin(p) => session.add_plugin(ChainRef::Master, p),
            BrowserItem::Sample(_) => {}
        }
    }
    let resp = info(
        resp,
        "Master track — scene launch buttons; drop effects here; drag its left edge for scene tempo",
    );
    resp.context_menu(|ui| {
        if ui.button("Cancel Scene Launch").clicked() {
            session.cancel_scene_launch();
            ui.close();
        }
    });
    // Dragging the left edge of the title reveals the Scene Tempo and Signature controls.
    let edge = Rect::from_min_size(Pos2::new(m.left() - 2.0, m.top()), Vec2::new(5.0, HEADER_H));
    let edge = ui.allocate_rect(edge, Sense::drag());
    if edge.hovered() || edge.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    if edge.dragged() {
        state.sel.scene_fields_w =
            (state.sel.scene_fields_w - edge.drag_delta().x).clamp(0.0, scene_view::FIELDS_W * 1.3);
    }

    // Scene launch buttons, aligned with the clip grid rows.
    let grid = Rect::from_min_max(
        Pos2::new(m.left(), g.grid.top()),
        Pos2::new(m.right(), g.grid.bottom()),
    );
    let n = session.project.scenes.len();
    let first = ((scroll_y / g.slot_h).floor().max(0.0)) as usize;
    let last = (((scroll_y + grid.height()) / g.slot_h).ceil() as usize).min(n);
    scene_view::rows(
        ui,
        session,
        state,
        &SceneRows {
            grid,
            left: m.left(),
            right: m.right(),
            first,
            last,
            scroll_y,
            slot_h: g.slot_h,
            blink,
        },
    );
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
            cu.painter().rect_filled(rect, 0.0, shade(0x333333));
        }
        icons::plus(
            cu.painter(),
            Rect::from_center_size(rect.center(), Vec2::splat(rect.height())),
            shade(0x7a7a7a),
        );
        if info(resp, "Add Scene — add a new row of slots").clicked() {
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
            pal().slot_empty_hover
        } else {
            shade(0x353535)
        },
    );
    icons::stop(
        &p,
        Rect::from_min_size(sr.min, Vec2::splat(sr.height())),
        shade(0xd0d0d0),
    );
    p.text(
        Pos2::new(sr.left() + sr.height() + 2.0, sr.center().y),
        Align2::LEFT_CENTER,
        "Stop Clips",
        theme::font(10.5),
        pal().text_dim,
    );
    if info(resp, "Stop All Clips — stop every clip in the Session").clicked() {
        session.stop_all_clips();
    }

    // Master mixer strip
    if g.mixer.height() > 0.0 {
        let rect = Rect::from_min_max(Pos2::new(m.left() + 1.0, g.mixer.top()), m.max);
        p.rect_filled(
            rect,
            0.0,
            if selected {
                shade(0x363636)
            } else {
                pal().panel
            },
        );
        let inner = rect.shrink2(Vec2::new(8.0, 8.0));
        let mut top = inner.top() + 2.0;
        if state.layout.show_in_out {
            top = in_out::main_section(ui, session, state, inner, top + 12.0);
        }
        let mut bottom = inner.bottom() - 20.0;
        if state.layout.show_crossfader {
            let row = Rect::from_min_max(Pos2::new(inner.left(), inner.bottom() - 14.0), inner.max);
            in_out::crossfader(ui, session, row);
            bottom -= 20.0;
        }
        let fader_r = Rect::from_min_max(
            Pos2::new(inner.center().x - 22.0, top),
            Pos2::new(inner.center().x + 22.0, bottom.max(top + 20.0)),
        );
        let mut fu = ui.new_child(UiBuilder::new().max_rect(fader_r));
        let vol = session
            .automated_cell_value(ChainRef::Master, mix_cell::VOLUME)
            .map_or(session.project.master.volume_db, |v| v as f32);
        Led::of(session.cell_state(ChainRef::Master, mix_cell::VOLUME))
            .paint(&p, fader_r.left_top() + Vec2::new(-5.0, 4.0));
        let master_volume = EnvTarget::Param(ParamRef {
            owner: ParamOwner::Master,
            id: mixer::VOLUME,
        });
        automation::control_zone(
            &fu,
            session,
            state,
            fader_r,
            ChainRef::Master,
            master_volume,
        );
        if let Some(db) = widgets::fader(
            &mut fu,
            fader_r.size(),
            vol,
            state.master_meter.get(),
            pal().accent,
        ) {
            session.set_master_volume(db);
        }
        let db_r = Rect::from_center_size(
            Pos2::new(inner.center().x, bottom + 12.0),
            Vec2::new(54.0, 14.0),
        );
        p.rect_filled(db_r, 1.0, pal().field_bg);
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
            pal().text,
        );
        p.text(
            Pos2::new(inner.left(), inner.top()),
            Align2::LEFT_TOP,
            "Master",
            theme::font(10.0),
            pal().text_dim,
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
            Stroke::new(2.0, pal().accent.gamma_multiply(0.6)),
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
            pal().overlay(if resp.hovered() || resp.dragged() {
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
            pal().overlay(if resp.hovered() || resp.dragged() {
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

/// A track header being dragged onto another, to reorder or group.
#[derive(Clone, Copy)]
struct TrackDrag(TrackId);

#[derive(Clone, Copy)]
pub(super) struct ClipDrag {
    pub track: TrackId,
    pub slot: usize,
}

pub(super) fn color_picker(ui: &mut egui::Ui) -> Option<[u8; 3]> {
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

pub(super) fn track_menu(
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
    edit_info_item(ui, session, state, InfoTarget::Track(id));
    ui.menu_button("Color", |ui| {
        if let Some(c) = color_picker(ui) {
            if let Some(t) = session.project.track_mut(id) {
                t.color = c;
            }
            session.dirty = true;
            ui.close();
        }
    });
    let is_group = session.project.track(id).is_some_and(|t| t.is_group());
    let selection = if state.selected_tracks.contains(&id) {
        state.selected_tracks.clone()
    } else {
        vec![id]
    };
    super::commands::bounce::track_menu_items(ui, session, state, id);
    if ui.button("Group Tracks   ⌘G").clicked() {
        if let Some(g) = session.group_tracks(&selection) {
            state.selected = Some(ChainRef::Track(g));
            state.selected_tracks = vec![g];
        }
        ui.close();
    }
    if ui.button("Assign Track Color to Clips").clicked() {
        session.assign_track_color(id, state.main_view == super::MainView::Arrangement);
        ui.close();
    }
    if is_group {
        if ui.button("Ungroup Tracks   ⌘⇧G").clicked() {
            session.ungroup(id);
            state.selected_tracks.clear();
            ui.close();
        }
        if ui
            .button("Assign Track Color to Grouped Tracks and Clips")
            .clicked()
        {
            session.assign_group_color(id, state.main_view == super::MainView::Session);
            ui.close();
        }
    }
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
        .button(RichText::new("Delete Track").color(pal().arm_red))
        .clicked()
    {
        session.delete_track(id);
        if state.selected == Some(ChainRef::Track(id)) {
            state.selected = None;
        }
        if state.selected_clip.is_some_and(|c| c.track() == id) {
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
        let stop_label = if session.has_stop_button(id, slot) {
            "Remove Stop Button"
        } else {
            "Add Stop Button"
        };
        if ui.button(stop_label).clicked() {
            session.toggle_stop_buttons(&[(id, slot)]);
            ui.close();
        }
        if kind == TrackKind::Midi && ui.button("Insert MIDI Clip").clicked() {
            let color = session.new_clip_color(id);
            let c = session.new_midi_clip(color, session.project.time_signature);
            session.set_clip(id, slot, Some(c));
            state.selected_clip = Some(ClipRef::slot(id, slot));
            state.detail = Detail::Clip;
            ui.close();
        }
        if kind == TrackKind::Audio && ui.button("Load Audio File…").clicked() {
            ui.close();
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Audio", rlive_audio_io::AUDIO_EXTENSIONS)
                .pick_file()
            {
                match session.audio_clip_from_file(&path, id) {
                    Ok(c) => session.set_clip(id, slot, Some(c)),
                    Err(e) => state.status = e,
                }
            }
        }
        return;
    }
    edit_info_item(
        ui,
        session,
        state,
        InfoTarget::Clip(ClipRef::slot(id, slot)),
    );
    if ui.button("Launch").clicked() {
        session.launch_clip(id, slot);
        ui.close();
    }
    if ui.button("Add to Arrangement at Marker").clicked() {
        if let Some(c) = session.clip(id, slot).cloned() {
            let at = session.insert_marker() as u64;
            session.arr_place(id, ArrangementClip::new(c, at));
        }
        ui.close();
    }
    if ui.button("Edit").clicked() {
        state.selected_clip = Some(ClipRef::slot(id, slot));
        state.detail = Detail::Clip;
        state.show_detail = true;
        ui.close();
    }
    if ui.button("Duplicate   ⌘D").clicked() {
        duplicate_clip(session, id, slot);
        ui.close();
    }
    super::commands::bounce::slot_menu_items(ui, session, state, id, slot);
    if ui.button("Create Follow Action Chain").clicked() {
        let mut slots = selected_slots(session, state);
        if !slots.contains(&(id, slot)) {
            slots = vec![(id, slot)];
        }
        session.create_follow_chain(&slots);
        ui.close();
    }
    let stop_label = if session.has_stop_button(id, slot) {
        "Remove Stop Button"
    } else {
        "Add Stop Button"
    };
    if ui.button(stop_label).clicked() {
        session.toggle_stop_buttons(&[(id, slot)]);
        ui.close();
    }
    let mut looped = session.clip(id, slot).is_some_and(|c| c.looped);
    let frozen = session.is_frozen(id);
    if ui
        .add_enabled(!frozen, egui::Checkbox::new(&mut looped, "Loop"))
        .changed()
    {
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
    if ui
        .button(RichText::new("Delete").color(pal().arm_red))
        .clicked()
    {
        session.set_clip(id, slot, None);
        if state.selected_clip == Some(ClipRef::slot(id, slot)) {
            state.selected_clip = None;
        }
        ui.close();
    }
}

pub(super) fn drop_on_track(
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
            match session.audio_clip_from_file(path, id) {
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
                    state.selected_clip = Some(ClipRef::slot(id, slot));
                }
                Err(e) => state.status = e,
            }
        }
    }
    state.selected = Some(ChainRef::Track(id));
}

pub(super) fn new_track_zone(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    rect: Rect,
) {
    let zone = ui.allocate_rect(rect, Sense::hover());
    rack_mixer::extract_zone(ui, &zone, session, state, rect);
    let hovering = zone.dnd_hover_payload::<BrowserItem>().is_some();
    if hovering {
        ui.painter().rect_stroke(
            rect.shrink(2.0),
            2.0,
            Stroke::new(1.0, pal().accent),
            StrokeKind::Inside,
        );
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            "Drop to create a track",
            theme::font(11.0),
            pal().accent,
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
            ui.painter().rect_filled(plus, 1.0, pal().raised);
        }
        icons::plus(
            ui.painter(),
            plus,
            if plus_resp.hovered() {
                shade(0xe0e0e0)
            } else {
                shade(0x6a6a6a)
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
        "Add Track — drop a device, plugin or sample here to create a track",
    );
    if info(plus_resp, "Add Track — add a MIDI track (Cmd+Shift+T)").clicked() {
        let id = session.add_track(TrackKind::Midi, None);
        state.selected = Some(ChainRef::Track(id));
    }
}
