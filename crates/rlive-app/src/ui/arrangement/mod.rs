//! The Arrangement view: clips on a linear timeline, one lane per track.

pub mod fades;
pub mod grid;
pub mod ruler;
pub mod selection;
pub mod view;

use super::automation::{self, AutomationState, LaneKey};
use super::commands::bounce;
use super::icons;
use super::info_view::edit_info_item;
use super::keymap::{self, Platform};
use super::session_view::{self, ClipDrag};
use super::theme::{self, *};
use super::timeline::{self, TimeAxis};
use super::widgets::{self, info, info_note, rgb};
use super::{BrowserItem, Detail, UiState};
use crate::session::{can_arm, ArrMove, ChainRef, ClipRef, InfoTarget, Session, TrimEdge};
use egui::{Align2, Color32, CursorIcon, Layout, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use fades::{FadeDrag, FadeTarget};
use grid::GridState;
use rlive_audio_io::SampleStatus;
use rlive_core::time::BarMap;
use rlive_core::{ArrClipId, TrackId, PPQ};
use rlive_project::{ArrangementClip, Clip, ClipContent, TrackKind};
pub use selection::TimeSel;
use std::sync::atomic::Ordering;
use view::{Follow, ZoomHistory};

const LOOP_H: f32 = 11.0;
const RULER_H: f32 = LOOP_H + ruler::BEAT_H + ruler::SCRUB_H;
const OVERVIEW_H: f32 = 16.0;
const TIME_RULER_H: f32 = 16.0;
/// A folded track's height.
const FOLDED_H: f32 = 22.0;
/// The tallest a track can be dragged.
const MAX_TRACK_H: f32 = 420.0;
/// How far an object's pull reaches when snapping, in screen points.
const SNAP_PX: f32 = 6.0;
const HEADER_W: f32 = 176.0;
const SCROLLBAR: f32 = 8.0;
const CLIP_TITLE_H: f32 = 13.0;
/// Rows at the bottom for the return tracks and the Master.
const RETURN_ROW_H: f32 = 24.0;
const EDGE_W: f32 = 6.0;
const ADD_ZONE_H: f32 = 64.0;
/// How tall a clip must be for its waveform to split into left and right.
const MIN_STEREO_H: f32 = 36.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum DragKind {
    Move,
    TrimLeft,
    TrimRight,
    /// Slides the clip's content under its edges.
    Slide,
    /// Selects time: over a clip's body or the background.
    Time,
    /// Pans the view with the mouse.
    Pan,
}

struct ClipDragState {
    kind: DragKind,
    origin: Pos2,
    /// The clip grabbed, whose start snaps to the grid.
    anchor: Option<u64>,
    clips: Vec<(TrackId, ArrangementClip)>,
    /// Where snapping can catch: other clips' edges, locators and signature markers.
    objects: Vec<u64>,
    /// Where a time selection started, and in which lane.
    time: Option<(f64, LaneKey)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LoopDrag {
    Whole,
    Start,
    End,
}

pub struct ArrangementState {
    pub px_per_beat: f32,
    pub scroll: Vec2,
    pub track_h: f32,
    pub selection: Vec<(TrackId, ArrClipId)>,
    /// A time selection across clip and automation lanes, or the insert marker on them.
    pub time: Option<TimeSel>,
    pub grid: GridState,
    /// The loop brace has the arrow keys.
    pub loop_selected: bool,
    /// A track header has the keyboard, not the lanes.
    pub header_selected: bool,
    pub selected_locator: Option<usize>,
    pub selected_signature: Option<usize>,
    pub selected_fade: Option<FadeTarget>,
    fade_drag: Option<FadeDrag>,
    /// F is held, which shows fade handles in Automation Mode.
    pub fades_key: bool,
    pub show_overview: bool,
    pub follow: Follow,
    pub zoom_history: ZoomHistory,
    /// Waveform Vertical Zoom, for every audio clip in the Arrangement.
    pub wave_zoom: f32,
    pub renaming_locator: Option<(usize, String)>,
    pub editing_signature: Option<(usize, String)>,
    /// Where Insert Silence will insert, and how many bars the dialog says.
    pub inserting_silence: Option<(u64, String)>,
    /// When the scrub area was pressed, where, and whether it's repeating yet.
    scrub_hold: Option<(f64, f64, bool)>,
    /// Width of the lanes, for keyboard commands that frame things.
    lanes_width: f32,
    /// The lanes' height, for Optimize Height.
    lanes_height: f32,
    drag: Option<ClipDragState>,
    loop_drag: Option<(LoopDrag, f32, u64, u64)>,
    pub(in crate::ui) context: Option<(TrackId, ArrClipId)>,
    /// Automation Mode and what's being edited in its lanes.
    pub auto: AutomationState,
}

impl ArrangementState {
    /// Whether clips are being dragged.
    pub fn dragging_clips(&self) -> bool {
        self.drag.is_some()
    }

    /// Lets go of a clip drag without it doing anything.
    pub fn cancel_clip_drag(&mut self) {
        self.drag = None;
    }

    /// Zooms the time ruler around the left of the visible range.
    pub fn zoom_ruler(&mut self, factor: f32) {
        let beat = self.scroll.x / self.px_per_beat;
        self.px_per_beat = (self.px_per_beat * factor).clamp(1.0, 240.0);
        self.scroll.x = beat * self.px_per_beat;
    }

    /// Whether something is selected that commands can act on: clips or a span of time.
    pub fn has_selection(&self) -> bool {
        !self.selection.is_empty() || self.time.as_ref().is_some_and(|t| !t.is_point())
    }

    /// The span a command applies to: the time selection, or the clips selected.
    pub fn selected_span(&self, session: &Session) -> Option<(u64, u64)> {
        match &self.time {
            Some(t) if !t.is_point() => Some(t.ticks()),
            _ => selection::clips_span(session, &self.selection),
        }
    }

    /// The tracks a command applies to: the selection's, or the insert marker's.
    pub fn selected_tracks(&self) -> Vec<TrackId> {
        match &self.time {
            Some(t) if !t.lanes.is_empty() => t.tracks(),
            _ => {
                let mut out: Vec<TrackId> = self.selection.iter().map(|(t, _)| *t).collect();
                out.dedup();
                out
            }
        }
    }

    /// Selects everything in the loop, the shortcut a click on the brace is.
    pub fn select_loop(&mut self, session: &Session) {
        self.loop_selected = true;
        self.header_selected = false;
        let a = &session.project.arrangement;
        let (s, e) = (a.loop_start as f64, a.loop_end as f64);
        self.selection.clear();
        self.time = Some(TimeSel {
            start: s,
            end: e,
            lanes: session
                .project
                .visible_tracks()
                .iter()
                .map(|&v| LaneKey {
                    chain: ChainRef::Track(session.project.tracks[v].id),
                    slot: None,
                })
                .collect(),
        });
    }

    /// Frames the selection, keeping the old view for Zoom Back. With no selection, everything.
    pub fn zoom_to_selection(&mut self, session: &Session, width: f32) {
        let Some((start, end)) = self.selected_span(session).filter(|(a, b)| b > a) else {
            let bar = session.project.time_signature.ticks_per_bar();
            let end = session.project.arrangement_end() + 2 * bar;
            let (px, scroll) = view::zoom_for(0, end.max(bar), width, 1.0, 240.0);
            self.zoom_history.push(self.px_per_beat, self.scroll.x);
            self.px_per_beat = px;
            self.scroll.x = scroll;
            return;
        };
        self.zoom_history.push(self.px_per_beat, self.scroll.x);
        let (px, scroll) = view::zoom_for(start, end, width, 1.0, 240.0);
        self.px_per_beat = px;
        self.scroll.x = scroll;
    }

    /// Steps back to the view before the last zoom.
    pub fn zoom_back(&mut self) {
        if let Some((px, scroll)) = self.zoom_history.pop() {
            self.px_per_beat = px;
            self.scroll.x = scroll;
        }
    }

    pub fn lanes_size(&self) -> Vec2 {
        Vec2::new(self.lanes_width, self.lanes_height)
    }
}

impl Default for ArrangementState {
    fn default() -> Self {
        Self {
            px_per_beat: 16.0,
            scroll: Vec2::ZERO,
            track_h: 54.0,
            selection: Vec::new(),
            time: None,
            grid: GridState::default(),
            loop_selected: false,
            header_selected: false,
            selected_locator: None,
            selected_signature: None,
            selected_fade: None,
            fade_drag: None,
            fades_key: false,
            show_overview: true,
            follow: Follow::default(),
            zoom_history: ZoomHistory::default(),
            wave_zoom: 1.0,
            renaming_locator: None,
            editing_signature: None,
            inserting_silence: None,
            scrub_hold: None,
            lanes_width: 0.0,
            lanes_height: 0.0,
            drag: None,
            loop_drag: None,
            context: None,
            auto: AutomationState::default(),
        }
    }
}

/// One clip on screen this frame.
#[derive(Clone, Copy)]
struct Hit {
    track: TrackId,
    id: ArrClipId,
    rect: Rect,
}

/// One track's rows of the Arrangement, top to bottom.
pub(super) struct Row {
    pub chain: ChainRef,
    /// Its index in the project's tracks, for a track with a clip lane.
    pub index: Option<usize>,
    /// Content position of the clip lane's top.
    pub top: f32,
    pub main_h: f32,
    /// Heights of the additional automation lanes below the clip lane.
    pub extra: Vec<f32>,
}

impl Row {
    pub fn bottom(&self) -> f32 {
        self.top + self.main_h + self.extra.iter().sum::<f32>()
    }
}

/// Where every row is this frame. Rows are in the order of the visible tracks.
pub(super) struct Rows {
    pub rows: Vec<Row>,
    pub total: f32,
}

impl Rows {
    fn build(session: &Session, state: &UiState) -> Self {
        let track_h = state.arr.track_h;
        let auto = state.arr.auto.mode;
        let height = |chain: ChainRef| match (chain, session.track_ui(chain)) {
            (ChainRef::Track(id), Some(ui)) => {
                let fold = session
                    .project
                    .track(id)
                    .is_some_and(|t| !t.is_group() && ui.folded);
                if fold {
                    FOLDED_H
                } else {
                    ui.height.unwrap_or(track_h)
                }
            }
            _ => track_h,
        };
        let mut rows = Vec::new();
        let mut top = 0.0;
        let mut add = |chain: ChainRef, index: Option<usize>, top: &mut f32| {
            let (main_h, extra) = if auto {
                automation::row_heights(session, chain, height(chain))
            } else {
                (height(chain), Vec::new())
            };
            let row = Row {
                chain,
                index,
                top: *top,
                main_h,
                extra,
            };
            *top = row.bottom();
            rows.push(row);
        };
        for v in session.project.visible_tracks() {
            let id = session.project.tracks[v].id;
            add(ChainRef::Track(id), Some(v), &mut top);
        }
        if auto {
            for r in &session.project.returns {
                add(ChainRef::Track(r.id), None, &mut top);
            }
            add(ChainRef::Master, None, &mut top);
        }
        Self { rows, total: top }
    }

    /// The clip lane of row `i`, on screen.
    pub fn main(&self, area: Rect, i: usize, scroll_y: f32) -> Rect {
        let row = &self.rows[i];
        Rect::from_min_size(
            Pos2::new(area.left(), area.top() + row.top - scroll_y),
            Vec2::new(area.width(), row.main_h),
        )
    }

    /// Additional lane `n` of row `i`, on screen.
    pub fn extra(&self, area: Rect, i: usize, n: usize, scroll_y: f32) -> Rect {
        let row = &self.rows[i];
        let top = row.top + row.main_h + row.extra[..n].iter().sum::<f32>();
        Rect::from_min_size(
            Pos2::new(area.left(), area.top() + top - scroll_y),
            Vec2::new(area.width(), row.extra[n]),
        )
    }

    /// The row at a screen height, if there is one.
    fn row_at(&self, area: Rect, y: f32, scroll_y: f32) -> Option<usize> {
        if y < area.top() {
            return None;
        }
        let content = y - area.top() + scroll_y;
        let i = self.rows.partition_point(|r| r.bottom() <= content);
        (i < self.rows.len()).then_some(i)
    }

    /// The lane at a screen height, if there is one: the row's clip lane in Automation Mode too.
    fn key_at(&self, area: Rect, y: f32, scroll_y: f32) -> Option<LaneKey> {
        let i = self.row_at(area, y, scroll_y)?;
        Some(LaneKey {
            chain: self.rows[i].chain,
            slot: None,
        })
    }

    /// Every clip lane, top to bottom.
    fn order(&self) -> Vec<LaneKey> {
        self.rows
            .iter()
            .map(|r| LaneKey {
                chain: r.chain,
                slot: None,
            })
            .collect()
    }

    /// The project index of the track at a screen height.
    fn track_at(&self, area: Rect, y: f32, scroll_y: f32) -> Option<usize> {
        self.rows[self.row_at(area, y, scroll_y)?].index
    }
}

pub fn show(ui: &mut egui::Ui, session: &mut Session, state: &mut UiState) {
    let full = ui.max_rect();
    ui.allocate_rect(full, Sense::hover());
    ui.painter().rect_filled(full, 0.0, pal().grid_bg);

    let sig = session.project.time_signature;
    let bar = sig.ticks_per_bar();
    let header_w = HEADER_W
        + if state.layout.show_in_out {
            super::in_out::ARR_IN_OUT_W
        } else {
            0.0
        };
    let lanes_right = full.right() - header_w;
    let overview_h = if state.arr.show_overview {
        OVERVIEW_H
    } else {
        0.0
    };
    let marker_h = if session.project.arrangement.signatures.is_empty() {
        0.0
    } else {
        ruler::SIGNATURE_H
    };
    let ruler_top = full.top() + overview_h;
    let ruler = Rect::from_min_max(
        Pos2::new(full.left(), ruler_top),
        Pos2::new(lanes_right, ruler_top + RULER_H + marker_h),
    );
    // Return tracks and the Master stay at the bottom, under the scrolling lanes.
    let auto_mode = state.arr.auto.mode;
    let bottom_rows = if auto_mode {
        0
    } else {
        session.project.returns.len() + 1
    };
    let bottom_h = bottom_rows as f32 * RETURN_ROW_H;
    let lanes_bottom = full.bottom() - SCROLLBAR - bottom_h - TIME_RULER_H;
    let lanes = Rect::from_min_max(
        Pos2::new(full.left(), ruler.bottom()),
        Pos2::new(lanes_right, lanes_bottom),
    );
    let headers = Rect::from_min_max(
        Pos2::new(lanes_right, ruler.bottom()),
        Pos2::new(full.right(), lanes_bottom),
    );
    state.arr.lanes_width = lanes.width();
    state.arr.lanes_height = lanes.height();

    wheel(ui, session, state, full, lanes);
    let rows = Rows::build(session, state);
    let content_h = rows.total + ADD_ZONE_H;
    let last = session
        .project
        .arrangement_end()
        .max(session.project.arrangement.loop_end);
    let content_beats = (last + 16 * bar) as f32 / PPQ as f32;
    let content_w = content_beats * state.arr.px_per_beat;
    let max_x = (content_w - lanes.width()).max(0.0);
    let max_y = (content_h - lanes.height()).max(0.0);
    state.arr.scroll.x = state.arr.scroll.x.clamp(0.0, max_x);
    state.arr.scroll.y = state.arr.scroll.y.clamp(0.0, max_y);

    let tick = session.engine.shared.tick.load();
    let playing = session.is_playing();
    follow_playhead(state, session, lanes, tick, playing);

    let axis = TimeAxis {
        left: lanes.left(),
        px_per_beat: state.arr.px_per_beat,
        scroll: state.arr.scroll.x,
    };
    let bars = session.project.bar_map();
    let grid_spacing = state.arr.grid.ticks(axis.px_per_beat, bar);

    if state.arr.show_overview {
        let strip = Rect::from_min_max(full.min, Pos2::new(lanes_right, ruler_top));
        ruler::overview(ui, session, state, strip, lanes.width(), content_beats);
    }
    paint_lane_backgrounds(ui, state, lanes, &rows);
    paint_grid(ui, lanes, &axis, grid_spacing, &bars);
    if session.project.arrangement.loop_enabled {
        let a = &session.project.arrangement;
        let (x0, x1) = (axis.x(a.loop_start as f64), axis.x(a.loop_end as f64));
        ui.painter_at(lanes).rect_filled(
            Rect::from_min_max(Pos2::new(x0, lanes.top()), Pos2::new(x1, lanes.bottom())),
            0.0,
            pal().overlay(6),
        );
    }
    let hits = paint_clips(ui, session, state, lanes, &axis, &rows);
    lane_interaction(
        ui,
        session,
        state,
        lanes,
        &axis,
        grid_spacing,
        &bars,
        &hits,
        &rows,
    );
    if auto_mode {
        automation::show_lanes(ui, session, state, lanes, &axis, grid_spacing, &rows);
    } else {
        // Only moves made in Automation Mode change what its lanes show.
        let _ = session.take_touched_control();
    }
    paint_time_selection(ui, state, lanes, &axis, &rows);
    fades::show(ui, session, state, &axis, &hits, &rows, lanes);
    ruler::show(ui, session, state, ruler, &axis, grid_spacing, bar);
    mode_buttons(
        ui,
        session,
        state,
        Rect::from_min_max(
            Pos2::new(lanes_right, full.top()),
            Pos2::new(full.right(), ruler.bottom()),
        ),
    );

    // The playhead and insert marker sit over everything in the lanes.
    let p = ui.painter_at(Rect::from_min_max(ruler.min, lanes.max));
    let marker = session.insert_marker();
    let mx = axis.x(marker);
    p.line_segment(
        [Pos2::new(mx, ruler.bottom()), Pos2::new(mx, lanes.bottom())],
        Stroke::new(1.0, pal().overlay(90)),
    );
    if playing {
        let x = axis.x(tick);
        p.line_segment(
            [Pos2::new(x, ruler.top()), Pos2::new(x, lanes.bottom())],
            Stroke::new(1.0, shade(0xf0f0f0)),
        );
    }

    track_headers(ui, session, state, headers, &rows);
    if auto_mode {
        automation::show_headers(ui, session, state, headers, &rows);
    }
    if !auto_mode {
        return_rows(
            ui,
            session,
            state,
            Rect::from_min_max(
                Pos2::new(full.left(), lanes.bottom() + TIME_RULER_H),
                Pos2::new(full.right(), lanes.bottom() + TIME_RULER_H + bottom_h),
            ),
            lanes_right,
        );
    }
    let grid_text = state.arr.grid.label(axis.px_per_beat, bar);
    ruler::time_ruler(
        ui,
        session,
        state,
        Rect::from_min_size(
            Pos2::new(full.left(), lanes.bottom()),
            Vec2::new(lanes_right - full.left(), TIME_RULER_H),
        ),
        &axis,
        &grid_text,
    );
    let add_y = lanes.top() + rows.total - state.arr.scroll.y;
    if add_y < lanes.bottom() {
        let zone = Rect::from_min_max(
            Pos2::new(headers.left(), add_y),
            Pos2::new(headers.right(), lanes.bottom()),
        );
        session_view::new_track_zone(ui, session, state, zone);
    }
    let p = ui.painter();
    p.line_segment(
        [headers.left_top(), Pos2::new(headers.left(), full.bottom())],
        Stroke::new(1.0, pal().separator),
    );
    scrollbars(
        ui, state, lanes, headers, full, content_w, content_h, max_x, max_y,
    );
    ruler::editors(ui.ctx(), session, state);
}

fn wheel(ui: &mut egui::Ui, session: &mut Session, state: &mut UiState, full: Rect, lanes: Rect) {
    if !ui.rect_contains_pointer(full) {
        return;
    }
    let (delta, m, pointer, zoom) = ui.input(|i| {
        (
            i.smooth_scroll_delta,
            i.modifiers,
            i.pointer.hover_pos(),
            i.zoom_delta(),
        )
    });
    let a = &mut state.arr;
    if m.command && delta.y != 0.0 {
        let axis = TimeAxis {
            left: lanes.left(),
            px_per_beat: a.px_per_beat,
            scroll: a.scroll.x,
        };
        let new = (a.px_per_beat * (1.0 + delta.y * 0.004)).clamp(1.0, 240.0);
        a.scroll.x = axis.scroll_after_zoom(pointer.map_or(lanes.left(), |p| p.x), new);
        a.px_per_beat = new;
    } else if m.alt && (delta.y != 0.0 || zoom != 1.0) {
        let dh = delta.y * 0.2 + (zoom - 1.0) * 80.0;
        let rows = Rows::build(session, state);
        let targets: Vec<ChainRef> = match state
            .arr
            .time
            .as_ref()
            .filter(|t| !t.is_point())
            .map(|t| t.lanes.iter().map(|l| l.chain).collect::<Vec<_>>())
        {
            Some(chains) if !chains.is_empty() => chains,
            _ => pointer
                .and_then(|p| rows.key_at(lanes, p.y, state.arr.scroll.y))
                .map(|k| k.chain)
                .into_iter()
                .collect(),
        };
        let fallback = state.arr.track_h;
        for chain in targets {
            session.edit_track_ui(chain, |ui| {
                let h = ui.height.unwrap_or(fallback) + dh;
                ui.height = Some(h.clamp(FOLDED_H, MAX_TRACK_H));
            });
        }
    } else if m.shift {
        a.scroll.x -= delta.y + delta.x;
        if delta.y + delta.x != 0.0 {
            a.follow.pause();
        }
    } else {
        a.scroll.x -= delta.x;
        a.scroll.y -= delta.y;
        if delta.x != 0.0 {
            a.follow.pause();
        }
    }
}

/// While playing, turn the page when the playhead leaves the view.
fn follow_playhead(state: &mut UiState, session: &Session, lanes: Rect, tick: f64, playing: bool) {
    if session.history.pending.is_some() && !session.is_recording() && playing {
        state.arr.follow.pause();
    }
    if !state.arr.follow.update(playing) || state.arr.drag.is_some() || state.arr.auto.dragging() {
        return;
    }
    let scrolls = state.follow_scrolls;
    let a = &mut state.arr;
    let x = (tick / PPQ as f64) as f32 * a.px_per_beat;
    a.scroll.x = view::follow_scroll(a.scroll.x, x, lanes.width(), scrolls);
}

fn paint_lane_backgrounds(ui: &egui::Ui, state: &UiState, lanes: Rect, rows: &Rows) {
    let p = ui.painter_at(lanes);
    let scroll_y = state.arr.scroll.y;
    for (i, row) in rows.rows.iter().enumerate() {
        let r = Rect::from_min_size(
            rows.main(lanes, i, scroll_y).min,
            Vec2::new(lanes.width(), row.bottom() - row.top),
        );
        if r.bottom() < lanes.top() || r.top() > lanes.bottom() {
            continue;
        }
        let fill = if state.selected == Some(row.chain) && row.chain != ChainRef::Master {
            shade(0x2e2e2e)
        } else if i % 2 == 0 {
            shade(0x282828)
        } else {
            shade(0x252525)
        };
        p.rect_filled(r, 0.0, fill);
        p.line_segment(
            [r.left_bottom(), r.right_bottom()],
            Stroke::new(1.0, pal().separator),
        );
    }
}

fn paint_grid(ui: &egui::Ui, lanes: Rect, axis: &TimeAxis, grid: u64, bars: &BarMap) {
    let p = ui.painter_at(lanes);
    let mut t = grid::grid_floor(axis.tick(lanes.left()), grid, bars) as f64;
    // A signature marker restarts the grid, so each line is worked out from the one before.
    let mut guard = 0;
    while axis.x(t) <= lanes.right() && guard < 8192 {
        guard += 1;
        let x = axis.x(t);
        let barline = bars.bar_at(t).start == t;
        let c = if barline {
            pal().grid_line(shade(0x141414))
        } else {
            pal().grid_line(shade(0x202020))
        };
        p.line_segment(
            [Pos2::new(x, lanes.top()), Pos2::new(x, lanes.bottom())],
            Stroke::new(1.0, c),
        );
        let mut next = t + grid as f64;
        let seg = bars.segment_start(t);
        let next_seg = bars.segment_start(next);
        if next_seg != seg && next_seg > t {
            next = next_seg;
        }
        t = next;
    }
}

fn paint_clips(
    ui: &egui::Ui,
    session: &Session,
    state: &UiState,
    lanes: Rect,
    axis: &TimeAxis,
    rows: &Rows,
) -> Vec<Hit> {
    let p = ui.painter_at(lanes);
    let mut hits = Vec::new();
    let edge = session
        .edge_fades
        .then(|| crate::session::edge_fade_ticks(session.project.tempo));
    for (i, row) in rows.rows.iter().enumerate() {
        let Some(index) = row.index else {
            continue;
        };
        let t = &session.project.tracks[index];
        let lane = rows.main(lanes, i, state.arr.scroll.y);
        if lane.bottom() < lanes.top() || lane.top() > lanes.bottom() {
            continue;
        }
        let shown = t.effective_fades(edge);
        if t.is_group() {
            paint_group_overview(&p, session, t.id, lane, lanes, axis);
        }
        for (c, f) in t.arrangement.iter().zip(&shown) {
            let (x0, x1) = (axis.x(c.start as f64), axis.x(c.end() as f64));
            if x1 < lanes.left() || x0 > lanes.right() {
                continue;
            }
            let rect = Rect::from_min_max(
                Pos2::new(x0, lane.top() + 1.0),
                Pos2::new(x1.max(x0 + 2.0), lane.bottom() - 1.0),
            );
            let selected = state.arr.selection.contains(&(t.id, c.id));
            paint_clip(
                &p,
                session,
                c,
                rect,
                lanes,
                axis,
                selected,
                t.mute,
                state.arr.wave_zoom,
            );
            if c.is_audio() && rect.height() >= fades::MIN_HEIGHT {
                fades::paint_ramps(&p, rect, c, f, axis, lanes);
            }
            hits.push(Hit {
                track: t.id,
                id: c.id,
                rect,
            });
        }
        if let Some(from) = session.take_recording_from(t.id) {
            let now = session.engine.shared.tick.load();
            let bar = Rect::from_min_max(
                Pos2::new(axis.x(from).max(lanes.left()), lane.top() + 1.0),
                Pos2::new(axis.x(now).min(lanes.right()), lane.bottom() - 1.0),
            );
            p.rect_filled(bar, 0.0, pal().arm_red.linear_multiply(0.4));
        }
    }
    hits
}

#[allow(clippy::too_many_arguments)]
fn paint_clip(
    p: &egui::Painter,
    session: &Session,
    c: &ArrangementClip,
    rect: Rect,
    lanes: Rect,
    axis: &TimeAxis,
    selected: bool,
    muted: bool,
    wave_zoom: f32,
) {
    let mut color = rgb(c.clip.color);
    if muted || !c.clip.active {
        color = theme::darken(color, 0.45);
    }
    let body = if session.content_waiting_for_sample(&c.clip.content) {
        pal().panel_dark
    } else {
        theme::darken(color, 0.55)
    };
    p.rect_filled(rect, 1.0, body);
    if c.clip.frozen.is_some() {
        bounce::frozen_glaze(p, rect);
    }
    let title = Rect::from_min_max(
        rect.min,
        Pos2::new(rect.right(), (rect.top() + CLIP_TITLE_H).min(rect.bottom())),
    );
    p.rect_filled(title, 1.0, color);
    let text_x = rect.left().max(lanes.left()) + 3.0;
    if rect.right() - text_x > 12.0 {
        let galley = p.layout_no_wrap(
            c.clip.name.clone(),
            theme::font(10.0),
            theme::text_on(color),
        );
        p.with_clip_rect(title.intersect(lanes)).galley(
            Pos2::new(text_x, title.center().y - galley.size().y / 2.0),
            galley,
            Color32::WHITE,
        );
    }
    let content = Rect::from_min_max(Pos2::new(rect.left(), title.bottom()), rect.max);
    if c.clip.frozen_tail {
        bounce::frozen_hatch(&p.with_clip_rect(rect.intersect(lanes)), content);
    }
    if content.height() > 6.0 {
        match &c.clip.content {
            ClipContent::Midi { .. } => paint_notes(p, c, content, lanes, axis, color),
            ClipContent::Audio { file, .. } => paint_waveform(
                p, session, c, &file.path, content, lanes, axis, color, wave_zoom,
            ),
        }
    }
    p.line_segment(
        [rect.right_top(), rect.right_bottom()],
        Stroke::new(1.0, pal().separator),
    );
    if selected {
        p.rect_stroke(
            rect,
            1.0,
            Stroke::new(1.0, Color32::WHITE),
            StrokeKind::Inside,
        );
    }
}

/// A group shows its members' clips as thin bars in its own lane.
fn paint_group_overview(
    p: &egui::Painter,
    session: &Session,
    group: TrackId,
    lane: Rect,
    lanes: Rect,
    axis: &TimeAxis,
) {
    let tracks = session.project.group_contents(group);
    let tracks: Vec<_> = tracks
        .iter()
        .filter_map(|id| session.project.track(*id))
        .filter(|t| !t.is_group())
        .collect();
    let rows = tracks.len().max(1) as f32;
    let row_h = ((lane.height() - 4.0) / rows).clamp(2.0, 9.0);
    for (row, t) in tracks.iter().enumerate() {
        let y = lane.top() + 2.0 + row as f32 * row_h;
        for c in &t.arrangement {
            let (x0, x1) = (axis.x(c.start as f64), axis.x(c.end() as f64));
            if x1 < lanes.left() || x0 > lanes.right() {
                continue;
            }
            let bar = Rect::from_min_max(
                Pos2::new(x0, y),
                Pos2::new(x1.max(x0 + 2.0), y + (row_h - 1.0).max(1.0)),
            );
            p.rect_filled(bar, 0.0, theme::darken(rgb(c.clip.color), 0.2));
        }
    }
}

/// Where the content at timeline tick `t` comes from, in clip-local ticks.
fn content_tick(c: &ArrangementClip, t: u64) -> Option<u64> {
    let local = c.offset + t.saturating_sub(c.start);
    if c.clip.looped && c.clip.length > 0 {
        Some(local % c.clip.length)
    } else {
        (local < c.clip.length).then_some(local)
    }
}

fn paint_notes(
    p: &egui::Painter,
    c: &ArrangementClip,
    content: Rect,
    lanes: Rect,
    axis: &TimeAxis,
    color: Color32,
) {
    let notes = c.clip.notes();
    let Some((lo, hi)) = notes.iter().fold(None, |acc: Option<(u8, u8)>, n| {
        Some(acc.map_or((n.key, n.key), |(l, h)| (l.min(n.key), h.max(n.key))))
    }) else {
        return;
    };
    let span = (hi - lo).max(11) as f32 + 1.0;
    let row = (content.height() - 2.0) / span;
    let length = c.clip.length.max(1);
    let shape_color = theme::darken(color, 0.15);
    // Draw each repeat of the content that falls inside the clip.
    let mut pass_start = c.start as i64 - (c.offset % length) as i64;
    let end = c.end() as i64;
    while pass_start < end {
        for n in notes {
            let on = pass_start + n.start as i64;
            let off = on + n.length as i64;
            if off <= c.start as i64 || on >= end {
                continue;
            }
            let x0 = axis.x(on.max(c.start as i64) as f64);
            let x1 = axis.x(off.min(end) as f64);
            if x1 < lanes.left() || x0 > lanes.right() {
                continue;
            }
            let y = content.bottom() - 1.0 - ((n.key - lo) as f32 + 1.0) * row;
            p.rect_filled(
                Rect::from_min_size(
                    Pos2::new(x0, y),
                    Vec2::new((x1 - x0).max(1.0), row.max(1.5)),
                ),
                0.0,
                shape_color,
            );
        }
        if !c.clip.looped {
            break;
        }
        pass_start += length as i64;
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_waveform(
    p: &egui::Painter,
    session: &Session,
    c: &ArrangementClip,
    path: &std::path::Path,
    content: Rect,
    lanes: Rect,
    axis: &TimeAxis,
    color: Color32,
    wave_zoom: f32,
) {
    if let Some(SampleStatus::Failed(_)) = session.samples.status(path) {
        let galley = p.layout_no_wrap(
            "Missing or unreadable file".to_string(),
            theme::font(10.0),
            pal().text_dim,
        );
        let at = Pos2::new(
            content.left().max(lanes.left()) + 3.0,
            content.center().y - galley.size().y / 2.0,
        );
        p.galley(at, galley, Color32::WHITE);
        return;
    }
    // Greyed until the sample has loaded, as Live does while analysing.
    let loading = !session.samples.is_ready(path);
    let Some((peaks, sample_rate)) = session.samples.peaks(path) else {
        p.line_segment(
            [
                Pos2::new(content.left().max(lanes.left()), content.center().y),
                Pos2::new(content.right().min(lanes.right()), content.center().y),
            ],
            Stroke::new(1.0, pal().text_faint),
        );
        return;
    };
    let shape = if loading {
        pal().text_faint
    } else {
        theme::darken(color, 0.15)
    };
    let frames_per_tick = 60.0 / session.project.tempo / PPQ as f64 * sample_rate;
    let (from, to) = (
        content.left().max(lanes.left()) as i32,
        content.right().min(lanes.right()) as i32,
    );
    let split = peaks.is_stereo() && content.height() >= MIN_STEREO_H;
    let waves = if split {
        vec![
            (
                Rect::from_min_max(content.min, Pos2::new(content.right(), content.center().y)),
                0usize,
            ),
            (
                Rect::from_min_max(Pos2::new(content.left(), content.center().y), content.max),
                1usize,
            ),
        ]
    } else {
        vec![(content, 0usize)]
    };
    for (wave, channel) in waves {
        let mid = wave.center().y;
        let half = wave.height() / 2.0 - 1.0;
        for px in from..to {
            let t0 = axis.tick(px as f32).max(c.start as f64) as u64;
            let t1 = axis.tick(px as f32 + 1.0).max(c.start as f64) as u64;
            let (Some(a), Some(b)) = (content_tick(c, t0), content_tick(c, t1.max(t0 + 1))) else {
                continue;
            };
            let (a, b) = if b > a { (a, b) } else { (a, a + 1) };
            let (start, end) = (
                (a as f64 * frames_per_tick) as usize,
                (b as f64 * frames_per_tick) as usize + 1,
            );
            let (lo, hi) = if split {
                peaks.min_max(channel, start, end)
            } else {
                peaks.min_max_mixed(start, end)
            };
            p.line_segment(
                [
                    Pos2::new(
                        px as f32 + 0.5,
                        mid - (hi * wave_zoom).clamp(-1.0, 1.0) * half,
                    ),
                    Pos2::new(
                        px as f32 + 0.5,
                        mid - (lo * wave_zoom).clamp(-1.0, 1.0) * half + 1.0,
                    ),
                ],
                Stroke::new(1.0, shape),
            );
        }
    }
}

/// A Shift+Alt drag over a clip's content slides it under its edges.
fn slides(m: egui::Modifiers) -> bool {
    m.shift && m.alt
}

/// Shift, in the title bar with Alt, picks out time within a clip.
fn selects_in_title(m: egui::Modifiers) -> bool {
    m.shift && m.alt
}

/// Pans the view from anywhere in the lanes.
fn pans(m: egui::Modifiers) -> bool {
    m.alt && m.command
}

fn clip_zone(h: &Hit, pos: Pos2, m: egui::Modifiers) -> DragKind {
    let in_title = pos.y <= h.rect.top() + CLIP_TITLE_H;
    if !in_title {
        return if slides(m) {
            DragKind::Slide
        } else {
            DragKind::Time
        };
    }
    if selects_in_title(m) {
        DragKind::Time
    } else if h.rect.width() >= EDGE_W * 2.5 && pos.x < h.rect.left() + EDGE_W {
        DragKind::TrimLeft
    } else if h.rect.width() >= EDGE_W * 2.5 && pos.x > h.rect.right() - EDGE_W {
        DragKind::TrimRight
    } else {
        DragKind::Move
    }
}

#[allow(clippy::too_many_arguments)]
fn lane_interaction(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    lanes: Rect,
    axis: &TimeAxis,
    grid: u64,
    bars: &BarMap,
    hits: &[Hit],
    rows: &Rows,
) {
    let resp = ui.interact(
        lanes,
        ui.scope_id().with("arr_lanes"),
        Sense::click_and_drag(),
    );
    let m = ui.input(|i| i.modifiers);
    let snapping = state.arr.grid.snapping(m, Platform::current());
    let hit = |pos: Pos2| hits.iter().rev().find(|h| h.rect.contains(pos));
    if let Some(pos) = resp.hover_pos() {
        if pans(m) {
            ui.ctx().set_cursor_icon(CursorIcon::Grab);
        } else if let Some(h) = hit(pos) {
            if matches!(
                clip_zone(h, pos, m),
                DragKind::TrimLeft | DragKind::TrimRight
            ) {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
            }
        }
    }

    let sy = state.arr.scroll.y;
    let pointer = resp.interact_pointer_pos();
    let tolerance = SNAP_PX as f64 / (axis.px_per_beat as f64 / PPQ as f64);
    let snap_at = |x: f32, objects: &[u64]| {
        grid::snap_tick(axis.tick(x), grid, snapping, bars, objects, tolerance)
    };
    let lane_order = rows.order();

    if resp.double_clicked() {
        if let Some(pos) = pointer {
            match hit(pos) {
                Some(hit) => open_clip(state, hit.track, hit.id, true),
                None => create_midi_clip(session, state, lanes, axis, grid, bars, pos, rows),
            }
        }
    } else if resp.clicked() {
        if let Some(pos) = pointer {
            state.arr.loop_selected = false;
            state.arr.header_selected = false;
            state.arr.selected_locator = None;
            state.arr.selected_signature = None;
            state.arr.selected_fade = None;
            if let Some(i) = rows.track_at(lanes, pos.y, sy) {
                let id = session.project.tracks[i].id;
                state.selected = Some(ChainRef::Track(id));
                state.selected_tracks = vec![id];
            }
            let tick = snap_at(pos.x, &[]) as f64;
            let clicked_clip = hit(pos).copied();
            let on_title = clicked_clip.is_some_and(|h| pos.y <= h.rect.top() + CLIP_TITLE_H);
            match (clicked_clip, on_title) {
                (Some(hit), true) => {
                    let key = (hit.track, hit.id);
                    if m.shift || m.command {
                        toggle(&mut state.arr.selection, key);
                    } else {
                        state.arr.selection = vec![key];
                    }
                    let span = selection::clips_span(session, &state.arr.selection);
                    let lane = rows.key_at(lanes, pos.y, sy);
                    state.arr.time = span.zip(lane).map(|((s, e), lane)| TimeSel {
                        start: s as f64,
                        end: e as f64,
                        lanes: vec![lane],
                    });
                    open_clip(state, hit.track, hit.id, false);
                }
                _ => {
                    state.arr.selection.clear();
                    let lane = rows.key_at(lanes, pos.y, sy);
                    if let Some(lane) = lane {
                        let extended = (m.shift)
                            .then_some(state.arr.time.as_ref())
                            .flatten()
                            .map(|t| selection::extended(t, tick, lane, &lane_order));
                        state.arr.time = Some(extended.unwrap_or(TimeSel::point(tick, lane)));
                    }
                    if !m.shift && !session.is_playing() {
                        session.seek(tick);
                    }
                    if let Some(hit) = clicked_clip {
                        open_clip(state, hit.track, hit.id, false);
                    }
                }
            }
        }
    }
    if resp.secondary_clicked() {
        if let Some(pos) = pointer {
            state.arr.context = hit(pos).map(|h| (h.track, h.id));
            if let Some(h) = hit(pos) {
                if !state.arr.selection.contains(&(h.track, h.id)) {
                    state.arr.selection = vec![(h.track, h.id)];
                }
            }
        }
    }
    resp.context_menu(|ui| clip_context_menu(ui, session, state));

    if resp.drag_started() {
        if let Some(pos) = pointer {
            let start = begin_drag(session, state, hit(pos), pos, m, lanes, rows, &snap_at);
            state.arr.loop_selected = false;
            state.arr.drag = Some(start);
        }
    }

    let mut preview = Vec::new();
    if let (Some(d), Some(pos)) = (&state.arr.drag, pointer) {
        preview = drag_preview(
            session, state, d, pos, axis, grid, bars, snapping, tolerance, lanes, rows,
        );
    }
    let p = ui.painter_at(lanes);
    for ghost in &preview {
        p.rect_filled(ghost.rect, 1.0, pal().overlay(45));
        p.rect_stroke(
            ghost.rect,
            1.0,
            Stroke::new(1.0, pal().text),
            StrokeKind::Inside,
        );
    }
    if let (Some(d), Some(pos)) = (&state.arr.drag, pointer) {
        match d.kind {
            DragKind::Time => {
                if let Some((anchor, origin)) = d.time {
                    let tick = snap_at(pos.x, &[]) as f64;
                    let lane = rows.key_at(lanes, pos.y, sy).unwrap_or(origin);
                    state.arr.selection.clear();
                    state.arr.time = Some(TimeSel {
                        start: anchor.min(tick),
                        end: anchor.max(tick),
                        lanes: selection::lanes_between(origin, lane, &lane_order),
                    });
                }
            }
            DragKind::Pan => {
                let d = resp.drag_delta();
                state.arr.scroll -= d;
                state.arr.follow.pause();
            }
            DragKind::Slide => {
                if let Some((track, base)) = d.clips.first().cloned() {
                    let delta = ((pos.x - d.origin.x) / axis.px_per_beat) as f64 * PPQ as f64;
                    let delta = grid::snap_tick(delta.abs(), grid, snapping, bars, &[], 0.0) as i64
                        * delta.signum() as i64;
                    session.arr_slide(track, &base, delta);
                }
            }
            _ => {}
        }
    }
    if resp.drag_stopped() {
        if let Some(d) = state.arr.drag.take() {
            finish_drag(
                session,
                state,
                d,
                &preview,
                keymap::copies_on_drag(m, Platform::current()),
            );
        }
    }

    drops(session, state, &resp, lanes, axis, grid, snapping, rows);
    let hovered = resp.hover_pos().and_then(|pos| hit(pos).copied());
    let target = hovered.map(|h| ClipRef::Arr {
        track: h.track,
        id: h.id,
    });
    let resp = match target.and_then(|r| session.clip_at(r)) {
        Some(c) => {
            let text = format!(
                "Clip '{}' — drag the title bar to move, drag its body to select time, double-click to edit, right-click for options",
                c.name
            );
            info(resp, text)
        }
        None => info(
            resp,
            "Arrangement — click to place the insert marker · drag selects time · drag clips by their title bar (Alt = copy, Cmd = no snap) · drag edges to trim · double-click to edit or add · Cmd+wheel zooms · Alt+wheel sets lane height",
        ),
    };
    if let Some(note) = target.and_then(|r| session.info_text(InfoTarget::Clip(r))) {
        info_note(&resp, note);
    }
}

fn toggle(selection: &mut Vec<(TrackId, ArrClipId)>, key: (TrackId, ArrClipId)) {
    match selection.iter().position(|s| *s == key) {
        Some(i) => {
            selection.remove(i);
        }
        None => selection.push(key),
    }
}

fn open_clip(state: &mut UiState, track: TrackId, id: ArrClipId, reveal: bool) {
    let cref = ClipRef::Arr { track, id };
    if state.selected_clip != Some(cref) {
        state.piano.reset_view = true;
        state.piano.selected.clear();
    }
    state.selected_clip = Some(cref);
    state.detail = Detail::Clip;
    if reveal {
        state.show_detail = true;
    }
}

#[allow(clippy::too_many_arguments)]
fn create_midi_clip(
    session: &mut Session,
    state: &mut UiState,
    lanes: Rect,
    axis: &TimeAxis,
    grid: u64,
    bars: &BarMap,
    pos: Pos2,
    rows: &Rows,
) {
    let Some(i) = rows.track_at(lanes, pos.y, state.arr.scroll.y) else {
        return;
    };
    let t = &session.project.tracks[i];
    if t.kind != TrackKind::Midi {
        return;
    }
    let id = t.id;
    let color = session.new_clip_color(id);
    let sig = session.project.time_signature;
    let start = grid::grid_floor(axis.tick(pos.x), grid, bars);
    let placed = ArrangementClip::new(Clip::new_midi("MIDI Clip", color, 1, sig), start);
    let clip_id = placed.id;
    session.arr_place(id, placed);
    state.arr.selection = vec![(id, clip_id)];
    open_clip(state, id, clip_id, true);
}

/// Everything clips can snap to, other than the clips in `moving`.
fn snap_objects(session: &Session, moving: &[ArrClipId]) -> Vec<u64> {
    let mut objects = Vec::new();
    for t in &session.project.tracks {
        for c in &t.arrangement {
            if !moving.contains(&c.id) {
                objects.push(c.start);
                objects.push(c.end());
            }
        }
    }
    let a = &session.project.arrangement;
    objects.extend(a.locators.iter().map(|l| l.tick));
    objects.extend(a.signatures.iter().map(|m| m.tick));
    objects
}

#[allow(clippy::too_many_arguments)]
fn begin_drag(
    session: &Session,
    state: &mut UiState,
    hit: Option<&Hit>,
    pos: Pos2,
    m: egui::Modifiers,
    lanes: Rect,
    rows: &Rows,
    snap_at: &dyn Fn(f32, &[u64]) -> u64,
) -> ClipDragState {
    let sy = state.arr.scroll.y;
    let time_drag = |pos: Pos2| ClipDragState {
        kind: DragKind::Time,
        origin: pos,
        anchor: None,
        clips: Vec::new(),
        objects: Vec::new(),
        time: rows
            .key_at(lanes, pos.y, sy)
            .map(|k| (snap_at(pos.x, &[]) as f64, k)),
    };
    if pans(m) {
        return ClipDragState {
            kind: DragKind::Pan,
            origin: pos,
            anchor: None,
            clips: Vec::new(),
            objects: Vec::new(),
            time: None,
        };
    }
    let Some(h) = hit else {
        return time_drag(pos);
    };
    let kind = clip_zone(h, pos, m);
    if kind == DragKind::Time {
        return time_drag(pos);
    }
    if !state.arr.selection.contains(&(h.track, h.id)) {
        state.arr.selection = vec![(h.track, h.id)];
    }
    let find = |t: TrackId, id: ArrClipId| session.project.track(t)?.arr_clip(id).cloned();
    let clips: Vec<(TrackId, ArrangementClip)> = if kind == DragKind::Move {
        state
            .arr
            .selection
            .iter()
            .filter_map(|(t, id)| find(*t, *id).map(|c| (*t, c)))
            .collect()
    } else {
        find(h.track, h.id)
            .map(|c| (h.track, c))
            .into_iter()
            .collect()
    };
    let anchor = find(h.track, h.id).map(|c| match kind {
        DragKind::TrimRight => c.end(),
        _ => c.start,
    });
    let moving: Vec<ArrClipId> = clips.iter().map(|(_, c)| c.id).collect();
    ClipDragState {
        kind,
        origin: pos,
        anchor,
        clips,
        objects: snap_objects(session, &moving),
        time: None,
    }
}

/// Where a clip would land when the drag ends.
struct Ghost {
    rect: Rect,
    from: TrackId,
    to: TrackId,
    clip: ArrangementClip,
}

#[allow(clippy::too_many_arguments)]
fn drag_preview(
    session: &Session,
    state: &UiState,
    d: &ClipDragState,
    pos: Pos2,
    axis: &TimeAxis,
    grid: u64,
    bars: &BarMap,
    snapping: bool,
    tolerance: f64,
    lanes: Rect,
    rows: &Rows,
) -> Vec<Ghost> {
    let Some(anchor) = d.anchor else {
        return Vec::new();
    };
    if matches!(d.kind, DragKind::Time | DragKind::Slide | DragKind::Pan) {
        return Vec::new();
    }
    let target = anchor as f64 + ((pos.x - d.origin.x) / axis.px_per_beat) as f64 * PPQ as f64;
    let snapped = grid::snap_tick(target, grid, snapping, bars, &d.objects, tolerance) as i64;
    let mut dt = snapped - anchor as i64;
    let visible = session.project.visible_tracks();
    let tracks: Vec<&rlive_project::Track> = visible
        .iter()
        .map(|&v| &session.project.tracks[v])
        .collect();
    let index = |t: TrackId| tracks.iter().position(|o| o.id == t);
    let mut dlane = 0i32;
    if d.kind == DragKind::Move {
        let earliest = d.clips.iter().map(|(_, c)| c.start).min().unwrap_or(0) as i64;
        dt = dt.max(-earliest);
        let sy = state.arr.scroll.y;
        dlane = match (
            rows.row_at(lanes, pos.y, sy),
            rows.row_at(lanes, d.origin.y, sy),
        ) {
            (Some(to), Some(from)) => to as i32 - from as i32,
            _ => 0,
        };
        let fits = d.clips.iter().all(|(t, _)| {
            index(*t).is_some_and(|i| {
                let to = i as i32 + dlane;
                to >= 0
                    && (to as usize) < tracks.len()
                    && tracks[to as usize].kind == tracks[i].kind
            })
        });
        if !fits {
            dlane = 0;
        }
    }
    d.clips
        .iter()
        .filter_map(|(t, c)| {
            let i = (index(*t)? as i32 + dlane) as usize;
            let to = tracks.get(i)?.id;
            let mut clip = c.clone();
            match d.kind {
                DragKind::Move => clip.start = (c.start as i64 + dt).max(0) as u64,
                DragKind::TrimLeft => clip.trim_left((c.start as i64 + dt).max(0) as u64),
                DragKind::TrimRight => clip.trim_right((c.end() as i64 + dt).max(0) as u64),
                DragKind::Time | DragKind::Slide | DragKind::Pan => return None,
            }
            let lane = rows.main(lanes, i, state.arr.scroll.y);
            let rect = Rect::from_min_max(
                Pos2::new(axis.x(clip.start as f64), lane.top() + 1.0),
                Pos2::new(axis.x(clip.end() as f64), lane.bottom() - 1.0),
            );
            Some(Ghost {
                rect,
                from: *t,
                to,
                clip,
            })
        })
        .collect()
}

fn finish_drag(
    session: &mut Session,
    state: &mut UiState,
    d: ClipDragState,
    preview: &[Ghost],
    copy: bool,
) {
    match d.kind {
        DragKind::Time | DragKind::Pan | DragKind::Slide => {}
        DragKind::Move => {
            let moves: Vec<ArrMove> = preview
                .iter()
                .map(|g| ArrMove {
                    track: g.from,
                    id: g.clip.id,
                    to_track: g.to,
                    start: g.clip.start,
                })
                .collect();
            let unchanged = !copy
                && preview
                    .iter()
                    .zip(&d.clips)
                    .all(|(g, (_, c))| g.from == g.to && g.clip.start == c.start);
            if !unchanged {
                state.arr.selection = session.arr_move(&moves, copy);
                state.arr.time = None;
            }
        }
        DragKind::TrimLeft | DragKind::TrimRight => {
            for g in preview {
                let edge = if d.kind == DragKind::TrimLeft {
                    TrimEdge::Left(g.clip.start)
                } else {
                    TrimEdge::Right(g.clip.end())
                };
                session.arr_trim(g.from, g.clip.id, edge);
            }
        }
    }
}

fn clip_context_menu(ui: &mut egui::Ui, session: &mut Session, state: &mut UiState) {
    bounce::arrangement_menu_items(ui, session, state);
    if let Some((track, id)) = state.arr.context {
        if ui.button("Edit").clicked() {
            open_clip(state, track, id, true);
            ui.close();
        }
        edit_info_item(
            ui,
            session,
            state,
            InfoTarget::Clip(ClipRef::Arr { track, id }),
        );
        if ui.button("Duplicate").clicked() {
            let items = state.arr.selection.clone();
            state.arr.selection = session.arr_duplicate(&items);
            ui.close();
        }
        if ui.button("Delete").clicked() {
            let items = std::mem::take(&mut state.arr.selection);
            session.arr_delete(&items);
            ui.close();
        }
        if ui.button("Split").clicked() {
            let at = session.insert_marker() as u64;
            session.arr_split(&[track], &[at]);
            ui.close();
        }
        if ui.button("Deactivate / Activate").clicked() {
            let items = state.arr.selection.clone();
            session.arr_toggle_active(&items);
            ui.close();
        }
        let leading = session.arr_is_leading(&state.arr.selection);
        let can_lead = leading || session.arr_can_lead(&state.arr.selection);
        let label = if leading {
            "Follow Tempo"
        } else {
            "Lead Tempo"
        };
        if ui
            .add_enabled(can_lead, egui::Button::new(label))
            .on_disabled_hover_text("Only warped audio clips can lead the tempo")
            .clicked()
        {
            let items = state.arr.selection.clone();
            session.arr_set_tempo_leader(&items, !leading);
            ui.close();
        }
        let hint = if session.arr_can_reverse(&state.arr.selection) {
            "Arrives with A9"
        } else {
            "Only audio clips can be reversed; arrives with A9"
        };
        ui.add_enabled(false, egui::Button::new("Reverse Clip(s)"))
            .on_disabled_hover_text(hint);
        if ui.button("Delete Fades").clicked() {
            let items = state.arr.selection.clone();
            session.arr_delete_fades(&items);
            ui.close();
        }
        ui.separator();
    }
    grid_menu(ui, state);
}

/// The context menu's grid section.
fn grid_menu(ui: &mut egui::Ui, state: &mut UiState) {
    let g = &mut state.arr.grid;
    ui.menu_button("Adaptive Grid", |ui| {
        for (label, level) in [
            ("Narrowest", 2),
            ("Narrow", 1),
            ("Medium", 0),
            ("Wide", -1),
            ("Widest", -2),
        ] {
            if ui
                .selectable_label(!g.fixed && g.level == level, label)
                .clicked()
            {
                g.fixed = false;
                g.level = level;
                ui.close();
            }
        }
    });
    ui.menu_button("Fixed Grid", |ui| {
        for (label, ticks) in [
            ("8 Bars", 8 * 4 * PPQ),
            ("4 Bars", 4 * 4 * PPQ),
            ("2 Bars", 2 * 4 * PPQ),
            ("1 Bar", 4 * PPQ),
            ("1/2", 2 * PPQ),
            ("1/4", PPQ),
            ("1/8", PPQ / 2),
            ("1/16", PPQ / 4),
            ("1/32", PPQ / 8),
        ] {
            if ui
                .selectable_label(g.fixed && g.width == ticks, label)
                .clicked()
            {
                g.fixed = true;
                g.width = ticks;
                ui.close();
            }
        }
    });
    if ui.selectable_label(g.triplet, "Triplet Grid").clicked() {
        g.triplet = !g.triplet;
        ui.close();
    }
    if ui.selectable_label(g.snap, "Snap to Grid").clicked() {
        g.snap = !g.snap;
        ui.close();
    }
}

#[allow(clippy::too_many_arguments)]
fn drops(
    session: &mut Session,
    state: &mut UiState,
    resp: &egui::Response,
    lanes: Rect,
    axis: &TimeAxis,
    grid: u64,
    snapping: bool,
    rows: &Rows,
) {
    let sy = state.arr.scroll.y;
    let hovering = resp.dnd_hover_payload::<BrowserItem>().is_some()
        || resp.dnd_hover_payload::<ClipDrag>().is_some();
    if !hovering {
        return;
    }
    let Some(pos) = resp.ctx.input(|i| i.pointer.hover_pos()) else {
        return;
    };
    let Some(i) = rows.track_at(lanes, pos.y, sy) else {
        return;
    };
    let target = session.project.tracks[i].id;
    let kind = session.project.tracks[i].kind;
    let start = timeline::snap(axis.tick(pos.x), grid, snapping).max(0) as u64;
    let x = axis.x(start as f64);
    let row = session
        .project
        .visible_tracks()
        .iter()
        .position(|&v| v == i)
        .unwrap_or(0);
    let lane = rows.main(lanes, row, sy);
    resp.ctx.layer_painter(resp.layer_id).line_segment(
        [Pos2::new(x, lane.top()), Pos2::new(x, lane.bottom())],
        Stroke::new(2.0, pal().accent),
    );
    if let Some(item) = resp.dnd_release_payload::<BrowserItem>() {
        match &*item {
            BrowserItem::Sample(path) if kind == TrackKind::Audio => {
                match session.audio_clip_from_file(path, target) {
                    Ok(c) => place_new(session, state, target, c, start),
                    Err(e) => state.status = e,
                }
            }
            other => session_view::drop_on_track(session, state, target, other, None),
        }
    }
    if let Some(src) = resp.dnd_release_payload::<ClipDrag>() {
        if let Some(c) = session.clip(src.track, src.slot).cloned() {
            if c.is_midi() == (kind == TrackKind::Midi) {
                place_new(session, state, target, c, start);
            } else {
                state.status = "That clip doesn't fit on this kind of track".into();
            }
        }
    }
}

fn place_new(session: &mut Session, state: &mut UiState, track: TrackId, clip: Clip, start: u64) {
    let placed = ArrangementClip::new(clip, start);
    let id = placed.id;
    session.arr_place(track, placed);
    state.arr.selection = vec![(track, id)];
    state.selected = Some(ChainRef::Track(track));
}

/// The time selection and insert marker on the clip lanes.
fn paint_time_selection(ui: &egui::Ui, state: &UiState, lanes: Rect, axis: &TimeAxis, rows: &Rows) {
    let Some(sel) = state.arr.time.as_ref() else {
        return;
    };
    if state.arr.auto.mode {
        return;
    }
    let p = ui.painter_at(lanes);
    for (i, _) in rows.rows.iter().enumerate() {
        let key = LaneKey {
            chain: rows.rows[i].chain,
            slot: None,
        };
        if !sel.lanes.contains(&key) {
            continue;
        }
        let lane = rows.main(lanes, i, state.arr.scroll.y);
        let (x0, x1) = (axis.x(sel.start), axis.x(sel.end));
        if sel.is_point() {
            p.line_segment(
                [Pos2::new(x0, lane.top()), Pos2::new(x0, lane.bottom())],
                Stroke::new(1.5, pal().select),
            );
        } else {
            let r = Rect::from_min_max(Pos2::new(x0, lane.top()), Pos2::new(x1, lane.bottom()));
            p.rect_filled(r, 0.0, pal().select.gamma_multiply(0.22));
            p.rect_stroke(r, 0.0, Stroke::new(1.0, pal().select), StrokeKind::Inside);
        }
    }
}

fn track_headers(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    headers: Rect,
    rows: &Rows,
) {
    for (row, entry) in rows.rows.iter().enumerate() {
        let (Some(i), ChainRef::Track(id)) = (entry.index, entry.chain) else {
            continue;
        };
        let lane = rows.main(headers, row, state.arr.scroll.y);
        if lane.bottom() < headers.top() || lane.top() > headers.bottom() {
            continue;
        }
        track_header(ui, session, state, headers, lane, id, i);
    }
}

/// The toggles above the track headers: Automation Mode and Lock Envelopes.
fn mode_buttons(ui: &mut egui::Ui, session: &mut Session, state: &mut UiState, area: Rect) {
    let p = ui.painter_at(area);
    p.rect_filled(area, 0.0, shade(0x333333));
    let row = Rect::from_min_size(
        area.left_top() + Vec2::new(6.0, 5.0),
        Vec2::new(area.width() - 12.0, 18.0),
    );
    let mut bu = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(row)
            .layout(Layout::left_to_right(egui::Align::Center)),
    );
    bu.spacing_mut().item_spacing.x = 4.0;
    let on = state.arr.auto.mode;
    let r = widgets::state_button(
        &mut bu,
        Vec2::new(78.0, 18.0),
        on,
        pal().arm_red,
        "Automation",
    );
    if info(
        r,
        "Automation Mode — show automation lanes under each track (A)",
    )
    .clicked()
    {
        state.arr.auto.mode = !on;
    }
    let locked = session.project.arrangement.lock_envelopes;
    let r = widgets::state_button(&mut bu, Vec2::new(44.0, 18.0), locked, pal().accent, "Lock");
    if info(
        r,
        "Lock Envelopes — envelopes stay on the timeline when clips move",
    )
    .clicked()
    {
        session.set_lock_envelopes(!locked);
    }
    locator_buttons(ui, session, state, area);
}

/// Previous, Set and Next, and the Optimize toggles and waveform zoom beside them.
fn locator_buttons(ui: &mut egui::Ui, session: &mut Session, state: &mut UiState, area: Rect) {
    let row = Rect::from_min_size(
        area.left_top() + Vec2::new(6.0, 26.0),
        Vec2::new(area.width() - 12.0, 16.0),
    );
    let mut bu = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(row)
            .layout(Layout::left_to_right(egui::Align::Center)),
    );
    bu.spacing_mut().item_spacing.x = 3.0;
    let size = Vec2::new(18.0, 16.0);
    if info(
        widgets::state_button(&mut bu, size, false, pal().accent, "<"),
        "Previous Locator — jump to the locator before the song position, quantized",
    )
    .clicked()
    {
        session.prev_locator();
    }
    let selected = state.arr.selected_locator;
    let label = if selected.is_some() { "Del" } else { "Set" };
    if info(
        widgets::state_button(&mut bu, Vec2::new(26.0, 16.0), false, pal().accent, label),
        "Set Locator — add a locator, quantized while playing; deletes the selected locator",
    )
    .clicked()
    {
        match selected {
            Some(i) => {
                session.delete_locator(i);
                state.arr.selected_locator = None;
            }
            None => {
                let at = state
                    .arr
                    .time
                    .as_ref()
                    .map_or(session.insert_marker() as u64, |t| t.start as u64);
                state.arr.selected_locator = Some(session.set_locator(at));
            }
        }
    }
    if info(
        widgets::state_button(&mut bu, size, false, pal().accent, ">"),
        "Next Locator — jump to the locator after the song position, quantized",
    )
    .clicked()
    {
        session.next_locator();
    }
    bu.add_space(6.0);
    let mut zoom = state.arr.wave_zoom;
    let r = bu.add(egui::DragValue::new(&mut zoom).range(0.5..=8.0).speed(0.02));
    if r.changed() {
        state.arr.wave_zoom = zoom;
    }
    info(r, "Waveform Vertical Zoom — enlarges every audio waveform");
}

/// The rows below the lanes: one for each return track, then the Master.
fn return_rows(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    area: Rect,
    lanes_right: f32,
) {
    let p = ui.painter().clone();
    p.rect_filled(area, 0.0, pal().panel_dark);
    p.line_segment(
        [area.left_top(), area.right_top()],
        Stroke::new(2.0, pal().separator),
    );
    let returns: Vec<TrackId> = session.project.returns.iter().map(|t| t.id).collect();
    for (i, id) in returns.iter().enumerate() {
        let row = Rect::from_min_size(
            Pos2::new(lanes_right, area.top() + i as f32 * RETURN_ROW_H),
            Vec2::new(area.right() - lanes_right, RETURN_ROW_H),
        );
        return_row(ui, session, state, row, *id, i);
    }
    let row = Rect::from_min_size(
        Pos2::new(lanes_right, area.bottom() - RETURN_ROW_H),
        Vec2::new(area.right() - lanes_right, RETURN_ROW_H),
    );
    let selected = state.selected == Some(ChainRef::Master);
    let resp = ui.allocate_rect(row, Sense::click());
    p.rect_filled(
        row,
        0.0,
        if selected {
            shade(0x3b3b3b)
        } else {
            shade(0x303030)
        },
    );
    p.text(
        row.left_center() + Vec2::new(10.0, 0.0),
        Align2::LEFT_CENTER,
        "Master",
        theme::font_bold(11.0),
        pal().text,
    );
    if resp.clicked() {
        state.selected = Some(ChainRef::Master);
        state.detail = Detail::Devices;
    }
    info(resp, "Master track — click to see its devices");
}

fn return_row(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    row: Rect,
    id: TrackId,
    index: usize,
) {
    let Some(t) = session.project.track(id) else {
        return;
    };
    let (name, color, mute, solo) = (t.name.clone(), t.color, t.mute, t.solo);
    let selected = state.selected == Some(ChainRef::Track(id));
    let resp = ui.allocate_rect(row, Sense::click());
    let p = ui.painter().clone();
    p.rect_filled(
        row,
        0.0,
        if selected {
            shade(0x3b3b3b)
        } else {
            shade(0x303030)
        },
    );
    p.line_segment(
        [row.left_bottom(), row.right_bottom()],
        Stroke::new(1.0, pal().separator),
    );
    p.rect_filled(
        Rect::from_min_size(row.min, Vec2::new(6.0, row.height())),
        0.0,
        rgb(color),
    );
    let label = Rect::from_min_size(
        row.min + Vec2::new(10.0, 0.0),
        Vec2::new(row.width() - 80.0, row.height()),
    );
    p.with_clip_rect(label).text(
        label.left_center(),
        Align2::LEFT_CENTER,
        name.as_str(),
        theme::font_bold(10.5),
        pal().text,
    );
    if state.layout.show_in_out {
        let area = Rect::from_min_max(
            Pos2::new(row.right() - HEADER_W - 66.0, row.top()),
            Pos2::new(row.right() - 72.0, row.bottom()),
        );
        super::in_out::header_section(ui, session, state, id, area);
    }
    let row_h = RETURN_ROW_H - 6.0;
    let letter = (b'A' + index as u8) as char;
    let mute_r = Rect::from_min_size(
        Pos2::new(row.right() - 68.0, row.top() + 3.0),
        Vec2::new(30.0, row_h),
    );
    let solo_r = Rect::from_min_size(
        Pos2::new(row.right() - 35.0, row.top() + 3.0),
        Vec2::new(30.0, row_h),
    );
    let mut mu = ui.new_child(egui::UiBuilder::new().max_rect(mute_r));
    let r = widgets::state_button(
        &mut mu,
        mute_r.size(),
        !mute,
        pal().activator,
        &letter.to_string(),
    );
    if info(r, "Track Activator — click to mute/unmute").clicked() {
        if let Some(t) = session.project.track_mut(id) {
            t.mute = !mute;
        }
        session.sync_mix(id);
    }
    let mut su = ui.new_child(egui::UiBuilder::new().max_rect(solo_r));
    let r = widgets::state_button(&mut su, solo_r.size(), solo, pal().solo_blue, "S");
    if info(
        r,
        "Solo the return — every other track's main output goes silent",
    )
    .clicked()
    {
        let additive = su.input(|i| i.modifiers.command);
        session_view::toggle_solo(session, id, solo, additive);
    }
    if resp.clicked() {
        state.selected = Some(ChainRef::Track(id));
        state.detail = Detail::Devices;
    }
    let resp = info(
        resp,
        format!("Return track '{name}' — click to select, right-click for options"),
    );
    info_note(
        &resp,
        session.info_text(InfoTarget::Track(id)).unwrap_or_default(),
    );
    resp.context_menu(|ui| session_view::return_menu(ui, session, state, id, &name));
}

fn track_header(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    headers: Rect,
    rect: Rect,
    id: TrackId,
    index: usize,
) {
    let Some(t) = session.project.track(id) else {
        return;
    };
    let (name, color, mute, solo, arm, kind, folded) = (
        t.name.clone(),
        t.color,
        t.mute,
        t.solo,
        t.arm,
        t.kind,
        t.ui.folded,
    );
    let holds_solo = kind == TrackKind::Group && session.project.holds_solo(id);
    let depth = session.project.group_depth(id);
    let selected = state.selected == Some(ChainRef::Track(id));
    let mut cu = session_view::child(
        ui,
        headers,
        rect,
        Layout::left_to_right(egui::Align::Center),
    );
    let resp = cu.allocate_rect(rect, Sense::click());
    let p = cu.painter().clone();
    p.rect_filled(
        rect,
        0.0,
        if selected {
            shade(0x3b3b3b)
        } else {
            shade(0x303030)
        },
    );
    p.line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        Stroke::new(1.0, pal().separator),
    );
    let swatch = Rect::from_min_size(rect.min, Vec2::new(6.0, rect.height()));
    p.rect_filled(swatch, 0.0, rgb(color));
    if session.is_frozen(id) {
        bounce::frozen_badge(&p, rect.right_top());
    }
    let indent = depth as f32 * 8.0;
    let in_out_w = if state.layout.show_in_out {
        super::in_out::ARR_IN_OUT_W
    } else {
        0.0
    };
    let inner = Rect::from_min_max(
        rect.min + Vec2::new(10.0 + indent, 4.0),
        rect.max - Vec2::new(10.0 + in_out_w, 4.0),
    );
    if in_out_w > 0.0 {
        let area = Rect::from_min_size(
            Pos2::new(rect.right() - in_out_w - 4.0, rect.top()),
            Vec2::new(in_out_w, rect.height()),
        );
        super::in_out::header_section(ui, session, state, id, area);
    }
    let fold_w = 16.0;
    if kind == TrackKind::Group {
        icons::fold(
            &p,
            Rect::from_min_size(inner.min, Vec2::splat(16.0)),
            !folded,
            pal().text_dim,
        );
    }
    let top = Rect::from_min_size(
        inner.min + Vec2::new(fold_w, 0.0),
        Vec2::new(inner.width() - fold_w, 16.0),
    );
    if matches!(&state.renaming, Some((tid, _)) if *tid == id) {
        let mut done = false;
        if let Some((_, text)) = &mut state.renaming {
            let r = cu.put(
                top,
                egui::TextEdit::singleline(text).font(theme::font(11.0)),
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
        let galley = p.layout_no_wrap(name.clone(), theme::font_bold(11.0), pal().text);
        p.with_clip_rect(top.intersect(headers)).galley(
            top.left_center() - Vec2::new(0.0, galley.size().y / 2.0),
            galley,
            pal().text,
        );
    }

    let btn_h = 16.0;
    if rect.height() >= 36.0 {
        let row = Rect::from_min_size(
            Pos2::new(inner.left(), inner.top() + 18.0),
            Vec2::new(inner.width(), btn_h),
        );
        let mut bu = session_view::child(
            &mut cu,
            headers,
            row,
            Layout::left_to_right(egui::Align::Center),
        );
        bu.spacing_mut().item_spacing.x = 2.0;
        let bw = 30.0;
        let r = widgets::state_button(
            &mut bu,
            Vec2::new(bw, btn_h),
            !mute,
            pal().activator,
            &(index + 1).to_string(),
        );
        if info(r, "Track Activator — click to mute/unmute").clicked() {
            if let Some(t) = session.project.track_mut(id) {
                t.mute = !mute;
            }
            session.sync_mix(id);
        }
        let solo_label = if session.project.cue.cue_mode {
            "Cue"
        } else {
            "S"
        };
        let r = widgets::state_button(
            &mut bu,
            Vec2::new(bw, btn_h),
            solo,
            pal().solo_blue,
            solo_label,
        );
        if holds_solo && !solo {
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
        let r = info(r, "Solo — Cmd-click to solo several tracks");
        if r.clicked() {
            session_view::toggle_solo(session, id, solo, bu.input(|i| i.modifiers.command));
        }
        if kind != TrackKind::Group && can_arm(&session.project, id) {
            let r = widgets::icon_button(
                &mut bu,
                Vec2::new(bw, btn_h),
                arm,
                pal().arm_red,
                icons::record,
            );
            if info(r, "Arm — record onto this track (Cmd-click to arm several)").clicked() {
                let exclusive = session.record.exclusive_arm && !bu.input(|i| i.modifiers.command);
                session.set_arm(id, !arm, exclusive);
                state.selected = Some(ChainRef::Track(id));
            }
        }
    }
    let fold_rect = Rect::from_min_size(inner.min, Vec2::splat(16.0));
    if kind != TrackKind::Group {
        icons::fold(&p, fold_rect, !folded, pal().text_faint);
    }
    let on_fold = resp
        .interact_pointer_pos()
        .is_some_and(|pos| fold_rect.contains(pos));
    if resp.clicked() && on_fold {
        if kind == TrackKind::Group {
            session.set_folded(id, !folded);
        } else {
            set_track_folded(session, id, !folded, cu.input(|i| i.modifiers.alt));
        }
    } else if resp.clicked() {
        state.arr.header_selected = true;
        state.arr.selection.clear();
        state.arr.time = None;
        let m = cu.input(|i| i.modifiers);
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
    if let Some(item) = resp.dnd_release_payload::<BrowserItem>() {
        session_view::drop_on_track(session, state, id, &item, None);
    }
    let resp = info(
        resp,
        format!("Track '{name}' — click to select, right-click for options"),
    );
    info_note(
        &resp,
        session.info_text(InfoTarget::Track(id)).unwrap_or_default(),
    );
    resp.context_menu(|ui| session_view::track_menu(ui, session, state, id, &name));

    // The split line below a track: drag it to resize; Alt resizes them all.
    if rect.height() >= FOLDED_H {
        let split = Rect::from_min_max(
            Pos2::new(rect.left(), rect.bottom() - 3.0),
            Pos2::new(rect.right(), rect.bottom() + 1.0),
        );
        let drag = cu.interact(split, cu.scope_id().with(("split", id)), Sense::drag());
        if drag.hovered() || drag.dragged() {
            cu.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
        }
        if drag.dragged() {
            let dh = drag.drag_delta().y;
            let all = cu.input(|i| i.modifiers.alt);
            let fallback = state.arr.track_h;
            if all {
                let ids: Vec<TrackId> = session.project.tracks.iter().map(|t| t.id).collect();
                for t in ids {
                    resize_track(session, t, dh, fallback);
                }
            } else {
                resize_track(session, id, dh, fallback);
            }
        }
    }
}

fn resize_track(session: &mut Session, id: TrackId, dh: f32, fallback: f32) {
    session.edit_track_ui(ChainRef::Track(id), |ui| {
        let h = ui.height.unwrap_or(fallback) + dh;
        ui.height = Some(h.clamp(FOLDED_H, MAX_TRACK_H));
        ui.folded = false;
    });
}

/// Folds a track (not a group, whose folding hides its members), or all of them with Alt.
fn set_track_folded(session: &mut Session, id: TrackId, folded: bool, all: bool) {
    let ids: Vec<TrackId> = if all {
        session
            .project
            .tracks
            .iter()
            .filter(|t| !t.is_group())
            .map(|t| t.id)
            .collect()
    } else {
        vec![id]
    };
    for t in ids {
        session.edit_track_ui(ChainRef::Track(t), |ui| ui.folded = folded);
    }
}

#[allow(clippy::too_many_arguments)]
fn scrollbars(
    ui: &mut egui::Ui,
    state: &mut UiState,
    lanes: Rect,
    headers: Rect,
    full: Rect,
    content_w: f32,
    content_h: f32,
    max_x: f32,
    max_y: f32,
) {
    let thumb_color =
        |r: &egui::Response| pal().overlay(if r.hovered() || r.dragged() { 90 } else { 45 });
    if max_x > 0.0 {
        let track = Rect::from_min_max(
            Pos2::new(lanes.left(), full.bottom() - SCROLLBAR + 1.0),
            Pos2::new(lanes.right(), full.bottom() - 1.0),
        );
        let w = (track.width() * lanes.width() / content_w).max(20.0);
        let x = track.left() + (track.width() - w) * (state.arr.scroll.x / max_x);
        let thumb = Rect::from_min_size(Pos2::new(x, track.top()), Vec2::new(w, track.height()));
        let resp = ui.allocate_rect(track, Sense::drag());
        ui.painter().rect_filled(thumb, 3.0, thumb_color(&resp));
        if resp.dragged() {
            state.arr.scroll.x += resp.drag_delta().x / (track.width() - w).max(1.0) * max_x;
            state.arr.follow.pause();
        }
    }
    if max_y > 0.0 {
        let track = Rect::from_min_max(
            Pos2::new(headers.right() - SCROLLBAR, headers.top()),
            Pos2::new(headers.right() - 1.0, headers.bottom()),
        );
        let h = (track.height() * lanes.height() / content_h).max(20.0);
        let y = track.top() + (track.height() - h) * (state.arr.scroll.y / max_y);
        let thumb = Rect::from_min_size(Pos2::new(track.left(), y), Vec2::new(track.width(), h));
        let resp = ui.allocate_rect(track, Sense::drag());
        ui.painter().rect_filled(thumb, 3.0, thumb_color(&resp));
        if resp.dragged() {
            state.arr.scroll.y += resp.drag_delta().y / (track.height() - h).max(1.0) * max_y;
        }
    }
}

/// Whether any track with timeline content has been taken over by a Session clip.
pub fn overridden(session: &Session) -> bool {
    session.project.tracks.iter().any(|t| {
        !t.arrangement.is_empty()
            && session
                .track_shared
                .get(&t.id)
                .is_some_and(|s| !s.follow_arrangement.load(Ordering::Relaxed))
    })
}
