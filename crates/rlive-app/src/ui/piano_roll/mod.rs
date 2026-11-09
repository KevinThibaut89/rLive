//! The MIDI Note Editor (manual ch. 10): rulers, note grid, Velocity, Release Velocity and Chance
//! lanes, Find and Select, Note Stretch, folding and several clips at once.
//!
//! This file holds the editor's state, the edits menu and keyboard commands reach, and the layout.
//! Painting, gestures and lanes are in their own files; the arithmetic is in
//! [`rlive_project::note_edit`], [`rlive_project::note_select`] and [`rlive_project::note_view`].

mod filters;
mod gestures;
mod lanes;
mod paint;
mod rulers;
mod shown;

pub use filters::FilterState;
pub use gestures::apply_chop;
pub use lanes::{commit_typed, LaneState};
pub use rulers::{fit_keys, zoom_time};
pub use shown::Shown;

use super::clip_view::markers::{self, Axis, MARKER_H, SCRUB_H};
use super::theme::*;
use super::UiState;
use crate::session::{ClipRef, Session};
use egui::{Pos2, Rect, Vec2};
use gestures::Gesture;
use rlive_core::{MidiMessage, PPQ};
use rlive_project::note_edit::{self, Quantize};
use rlive_project::note_view::Spelling;
use rlive_project::{Clip, NoteData};
use std::collections::HashMap;

pub(super) const KEYBOARD_W: f32 = 48.0;
const RULER_H: f32 = 20.0;
const STRETCH_H: f32 = 10.0;
const LANE_BAR_H: f32 = 20.0;
const MIN_PPB: f32 = 10.0;
const MAX_PPB: f32 = 800.0;
pub(super) const MIN_KEY_H: f32 = 5.0;
pub(super) const MAX_KEY_H: f32 = 40.0;

/// A chop being adjusted: what the notes were and how many parts they're in now.
pub struct Chop {
    pub base: Vec<NoteData>,
    pub picks: Vec<usize>,
    pub parts: usize,
}

pub struct PianoState {
    pub px_per_beat: f32,
    pub key_h: f32,
    pub grid: u64,
    pub reset_view: bool,
    /// Selected notes of the clip in the foreground, by index.
    pub selected: Vec<usize>,
    /// Selected notes in the other clips shown with it.
    pub others: HashMap<ClipRef, Vec<usize>>,
    /// Where Paste puts notes: the insert marker, on the editor's timeline.
    pub insert_tick: Option<u64>,
    /// The time selection, on the editor's timeline.
    pub time: Option<(u64, u64)>,
    /// Enter has switched the selection to the time span: notes in it aren't selected.
    pub time_only: bool,
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
    /// How tall the grid was last drawn.
    pub view_height: f32,
    /// The key tracks last drawn, highest first.
    pub rows: Vec<u8>,
    /// Fold to Notes, applying to every clip in the Set.
    pub fold_notes: bool,
    /// Fold to Scale, applying to every clip in the Set.
    pub fold_scale: bool,
    /// Highlight Scales, applying to every clip in the Set.
    pub highlight: bool,
    pub spelling: Spelling,
    /// The Preview switch, for every MIDI track.
    pub preview_on: bool,
    pub focus_mode: bool,
    /// N is held: Focus Mode the other way for a moment.
    pub focus_held: bool,
    pub lanes: LaneState,
    pub filters: FilterState,
    /// Draw Mode draws on one key at a time unless Alt is held.
    pub pitch_lock: bool,
    /// The type Cmd+G makes: the one last chosen.
    pub last_group_one: bool,
    pub quantize: Quantize,
    pub show_quantize: bool,
    pub chop: Option<Chop>,
    /// The last new note's length.
    pub last_length: u64,
    pub loop_bar_h: f32,
    /// Loop bars selected together, which Duplicate and a resize act on.
    pub bars: Vec<ClipRef>,
    /// The inactive clip whose loop bar the pointer is over, shown as if active.
    pub bar_hover: Option<ClipRef>,
    /// The clip the selection belongs to.
    pub foreground: Option<ClipRef>,
    /// When N went down, to tell a tap (toggle) from a hold (for a moment).
    pub focus_down_at: Option<f64>,
    /// Commands the context menu has picked, for the app to run.
    pub queued: Vec<crate::ui::commands::MenuCommand>,
    pub(self) gesture: Option<Gesture>,
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
            others: HashMap::new(),
            insert_tick: None,
            time: None,
            time_only: false,
            new_velocity: 0.8,
            scroll: Vec2::ZERO,
            snap_off: false,
            fixed_grid: false,
            zoom_back: Vec::new(),
            view_width: 0.0,
            view_height: 0.0,
            rows: (0..=127u8).rev().collect(),
            fold_notes: false,
            fold_scale: false,
            highlight: false,
            spelling: Spelling::Sharps,
            preview_on: true,
            focus_mode: false,
            focus_held: false,
            lanes: LaneState::default(),
            filters: FilterState::default(),
            pitch_lock: false,
            last_group_one: false,
            quantize: Quantize {
                grid: PPQ / 4,
                amount: 1.0,
                starts: true,
                ends: false,
            },
            show_quantize: false,
            chop: None,
            last_length: PPQ / 4,
            loop_bar_h: 14.0,
            bars: Vec::new(),
            bar_hover: None,
            foreground: None,
            focus_down_at: None,
            queued: Vec::new(),
            gesture: None,
            preview: None,
            preview_from_keys: false,
            preview_deadline: 0.0,
        }
    }
}

impl PianoState {
    pub fn has_selection(&self) -> bool {
        !self.selected.is_empty() || self.others.values().any(|s| !s.is_empty())
    }

    /// Focus Mode as it is this moment, with the N key held or not.
    pub fn focused(&self) -> bool {
        self.focus_mode != self.focus_held
    }

    /// Whether a drag or stroke is in progress.
    pub fn busy(&self) -> bool {
        self.gesture.is_some()
    }

    /// Clears the selection of notes and time, and the insert marker with it if `all`.
    pub fn deselect(&mut self, all: bool) {
        self.selected.clear();
        self.others.clear();
        self.time = None;
        self.time_only = false;
        if all {
            self.insert_tick = None;
        }
    }
}

// ------------------------------------------------------------- what the Edit menu reaches

/// The MIDI notes of the selected clip, if any.
pub(super) fn selected_notes(session: &Session, state: &UiState) -> Option<Vec<NoteData>> {
    let clip = session.clip_at(state.selected_clip?)?;
    clip.is_midi().then(|| clip.notes().to_vec())
}

/// Runs an edit on the selected notes of every clip that can be edited now: the foreground clip
/// alone in Focus Mode, or all of them.
pub fn edit_selected(
    session: &mut Session,
    state: &mut UiState,
    label: &'static str,
    mut f: impl FnMut(&mut Vec<NoteData>, &mut Vec<usize>),
) {
    let Some(r) = state.selected_clip else {
        return;
    };
    let mut clips = vec![(r, state.piano.selected.clone())];
    if !state.piano.focused() {
        clips.extend(state.piano.others.iter().map(|(c, s)| (*c, s.clone())));
    }
    for (clip, mut sel) in clips {
        session.edit_clips(&[clip], label, |c| {
            if let Some(n) = c.notes_mut() {
                f(n, &mut sel);
            }
        });
        if clip == r {
            state.piano.selected = sel;
        } else {
            state.piano.others.insert(clip, sel);
        }
    }
}

/// Runs a tool on the chosen notes of the foreground clip only, as the Velocity and Chance lanes
/// do (manual p. 290).
pub fn edit_foreground(
    session: &mut Session,
    state: &mut UiState,
    label: &'static str,
    mut f: impl FnMut(&mut Vec<NoteData>, &[usize]),
) {
    let Some(r) = state.selected_clip else {
        return;
    };
    let sel = state.piano.selected.clone();
    session.edit_notes(r, label, |n| f(n, &sel));
}

/// The notes selected, in every clip that can be edited now, earliest at tick 0.
pub fn copy_selection(
    session: &Session,
    state: &mut UiState,
) -> Option<Vec<(ClipRef, Vec<NoteData>)>> {
    let r = state.selected_clip?;
    let mut sets = vec![(r, state.piano.selected.clone())];
    if !state.piano.focused() {
        sets.extend(state.piano.others.iter().map(|(c, s)| (*c, s.clone())));
    }
    let mut picked: Vec<(ClipRef, Vec<NoteData>)> = sets
        .iter()
        .filter_map(|(c, s)| {
            let clip = session.clip_at(*c)?;
            let notes: Vec<NoteData> = s
                .iter()
                .filter_map(|i| clip.notes().get(*i).cloned())
                .collect();
            (!notes.is_empty()).then_some((*c, notes))
        })
        .collect();
    let first = picked
        .iter()
        .flat_map(|(_, n)| n.iter().map(|x| x.start))
        .min()?;
    let end = picked
        .iter()
        .flat_map(|(_, n)| n.iter().map(|x| x.start + x.length))
        .max()?;
    for (_, notes) in &mut picked {
        for n in notes {
            n.start -= first;
        }
    }
    // A plain Paste lands right after what was copied.
    state.piano.insert_tick = Some(end);
    Some(picked)
}

pub fn delete_selected(session: &mut Session, state: &mut UiState) {
    edit_selected(session, state, "Delete Notes", |notes, sel| {
        let gone = std::mem::take(sel);
        let mut i = 0;
        notes.retain(|_| {
            i += 1;
            !gone.contains(&(i - 1))
        });
    });
    state.piano.others.clear();
}

pub fn select_all(session: &Session, state: &mut UiState) {
    if let Some(notes) = selected_notes(session, state) {
        state.piano.selected = (0..notes.len()).collect();
    }
    if state.piano.focused() {
        return;
    }
    let Some(r) = state.selected_clip else {
        return;
    };
    let group = shown::gather(session, state, r).0;
    state.piano.others = group
        .iter()
        .filter(|s| s.r != r)
        .map(|s| (s.r, (0..s.clip.notes().len()).collect()))
        .collect();
}

/// Adds `notes` (starting at tick 0) to clip `r` at `at`, dropping any past the clip end. Returns
/// where they went.
fn insert_notes(session: &mut Session, r: ClipRef, notes: &[NoteData], at: u64) -> Vec<usize> {
    let Some(clip) = session.clip_at(r) else {
        return Vec::new();
    };
    let limit = content_limit(clip);
    let first_new = clip.notes().len();
    let added: Vec<NoteData> = notes
        .iter()
        .map(|n| NoteData {
            start: n.start + at,
            ..n.clone()
        })
        .filter(|n| n.start < limit)
        .collect();
    let count = added.len();
    let mut winners = Vec::new();
    session.edit_notes(r, "Paste Notes", |all| {
        all.extend(added.iter().cloned());
        let new: Vec<usize> = (first_new..first_new + count).collect();
        winners = note_edit::resolve_overlaps(all, &new);
    });
    winners
}

/// Puts clipboard notes back at the insert marker.
pub fn paste_notes(session: &mut Session, state: &mut UiState, notes: &[(ClipRef, Vec<NoteData>)]) {
    let Some(r) = state.selected_clip else {
        return;
    };
    let at = state.piano.insert_tick.unwrap_or(0);
    let shown_clips = shown::gather(session, state, r).0;
    let shown_refs: Vec<ClipRef> = shown_clips.iter().map(|s| s.r).collect();
    let copied: Vec<ClipRef> = notes.iter().map(|(c, _)| *c).collect();
    let same = !copied.is_empty()
        && rlive_project::note_view::paste_keeps_clips(&copied, &copied[0], &shown_refs, &r);
    state.piano.others.clear();
    state.piano.selected.clear();
    let mut end = at;
    for (from, list) in notes {
        let target = if same { *from } else { r };
        let content = shown_clips
            .iter()
            .find(|s| s.r == target)
            .map_or(at, |s| s.span.content_at(at as f64) as u64);
        let added = insert_notes(session, target, list, content);
        end = end.max(
            list.iter()
                .map(|n| at + n.start + n.length)
                .max()
                .unwrap_or(at),
        );
        if target == r {
            state.piano.selected.extend(added);
        } else {
            state.piano.others.entry(target).or_default().extend(added);
        }
    }
    state.piano.insert_tick = Some(end);
}

pub fn duplicate_selection(session: &mut Session, state: &mut UiState) {
    if let Some(copied) = copy_selection(session, state) {
        paste_notes(session, state, &copied);
    }
}

/// How far a clip's content can reach: past its loop if its end marker is.
pub(super) fn content_limit(c: &Clip) -> u64 {
    c.loop_end().max(c.end_marker()).max(c.length)
}

pub(super) fn is_black(key: u8) -> bool {
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

/// Grid line step that keeps lines at least ~8 px apart.
pub(super) fn grid_step(grid: u64, ppb: f32) -> u64 {
    let mut step = grid.max(1);
    while (step as f32 / PPQ as f32) * ppb < 8.0 {
        step *= 2;
    }
    step
}

// ------------------------------------------------------------- view arithmetic

/// Where things are on the note grid.
#[derive(Clone)]
pub(super) struct Geo {
    pub grid: Rect,
    pub ppb: f32,
    pub kh: f32,
    pub scroll: Vec2,
    /// The key tracks shown, highest first.
    pub rows: Vec<u8>,
}

impl Geo {
    pub fn x(&self, t: f64) -> f32 {
        self.grid.left() + (t / PPQ as f64) as f32 * self.ppb - self.scroll.x
    }

    pub fn tick(&self, x: f32) -> f64 {
        ((x - self.grid.left() + self.scroll.x) / self.ppb * PPQ as f32) as f64
    }

    pub fn row_of(&self, key: u8) -> Option<usize> {
        self.rows.iter().position(|k| *k == key)
    }

    pub fn y(&self, row: usize) -> f32 {
        self.grid.top() + row as f32 * self.kh - self.scroll.y
    }

    /// The key track at height `y`, if there is one there.
    pub fn key_at(&self, y: f32) -> Option<u8> {
        let row = ((y - self.grid.top() + self.scroll.y) / self.kh).floor();
        (row >= 0.0)
            .then(|| self.rows.get(row as usize).copied())
            .flatten()
    }

    /// The nearest key track to height `y`, even past the top or bottom.
    pub fn key_near(&self, y: f32) -> u8 {
        let row = ((y - self.grid.top() + self.scroll.y) / self.kh).floor() as i64;
        let last = self.rows.len().saturating_sub(1) as i64;
        self.rows
            .get(row.clamp(0, last) as usize)
            .copied()
            .unwrap_or(60)
    }

    pub fn note_rect(&self, t: f64, length: u64, key: u8) -> Option<Rect> {
        let row = self.row_of(key)?;
        Some(Rect::from_min_size(
            Pos2::new(self.x(t), self.y(row)),
            Vec2::new(((length as f32 / PPQ as f32) * self.ppb).max(3.0), self.kh),
        ))
    }
}

/// Which part of a note the pointer is on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Zone {
    Start,
    Body,
    End,
}

/// The part of a note at `x`. An edge is up to six pixels wide, or less on a short note.
pub(super) fn zone(r: Rect, x: f32) -> Zone {
    let edge = 6.0_f32.min(r.width() * 0.3);
    if x > r.right() - edge {
        Zone::End
    } else if x < r.left() + edge {
        Zone::Start
    } else {
        Zone::Body
    }
}

/// The pixels a time zoom must keep still: the time under `pointer` stays where it is.
pub fn zoom_about(scroll_x: f32, px: f32, ppb: f32, factor: f32) -> (f32, f32) {
    let new = (ppb * factor).clamp(MIN_PPB, MAX_PPB);
    let at = (px + scroll_x) / ppb;
    (new, at * new - px)
}

/// Sends the audition requests a frame made, if the Preview switch is on.
struct Preview {
    on: Option<u8>,
    off: bool,
    from_keys: bool,
    deadline: f64,
}

fn play_previews(
    session: &mut Session,
    state: &mut UiState,
    track: rlive_core::TrackId,
    p: Preview,
) {
    let off = |key: u8| MidiMessage::NoteOff {
        channel: 0,
        key,
        velocity: 0.5,
        note_id: 0,
    };
    if p.off {
        if let Some(k) = state.piano.preview.take() {
            session.midi_in(track, off(k));
        }
    }
    let Some(k) = p.on.filter(|_| state.piano.preview_on) else {
        return;
    };
    state.piano.preview_from_keys = p.from_keys;
    state.piano.preview_deadline = p.deadline;
    if let Some(old) = state.piano.preview.replace(k) {
        session.midi_in(track, off(old));
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

/// N taps Focus Mode, or held down flips it just for as long (manual p. 290).
fn focus_key(ui: &egui::Ui, state: &mut UiState, now: f64) {
    if ui.ctx().egui_wants_keyboard_input() || state.computer_keys {
        return;
    }
    let (pressed, released, modified) = ui.input(|i| {
        (
            i.key_pressed(egui::Key::N),
            i.key_released(egui::Key::N),
            i.modifiers.any(),
        )
    });
    let ps = &mut state.piano;
    if pressed && !modified {
        ps.focus_held = true;
        ps.focus_down_at = Some(now);
    }
    if released {
        if ps.focus_down_at.take().is_some_and(|at| now - at < 0.3) {
            ps.focus_mode = !ps.focus_mode;
        }
        ps.focus_held = false;
    }
}

/// The piano's visible keys, foldings applied.
fn visible_rows(session: &Session, state: &UiState, shown: &[Shown], fg: usize) -> Vec<u8> {
    let mut used: Vec<u8> = shown
        .iter()
        .flat_map(|s| s.clip.notes().iter().map(|n| n.key))
        .collect();
    used.sort_unstable();
    used.dedup();
    let scale = Session::pitch_scale(&shown[fg].clip).filter(|_| state.piano.fold_scale);
    let pads = rulers::drum_pads(session, shown[fg].r.track());
    let pad_keys: Vec<u8> = pads.iter().map(|(k, _)| *k).collect();
    rlive_project::note_view::visible_keys(&used, state.piano.fold_notes, scale, &pad_keys)
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
    session.note_recent_scale(&clip.scale);
    let (shown, span) = shown::gather(session, state, cref);
    let Some(fg) = shown.iter().position(|s| s.r == cref) else {
        return;
    };
    let multi = shown.len() > 1;
    let sig = clip.signature.unwrap_or(session.project.time_signature);
    let limit = if multi { span.1 } else { content_limit(&clip) };
    let now = ui.input(|i| i.time);
    focus_key(ui, state, now);
    // The selection is of this clip; anything else selected was of some other view.
    if state.piano.foreground != Some(cref) {
        state.piano.foreground = Some(cref);
        state.piano.selected.clear();
    }
    let ps = &mut state.piano;
    ps.others.retain(|r, _| shown.iter().any(|s| s.r == *r));
    ps.bars.retain(|r| shown.iter().any(|s| s.r == *r));

    // ---- regions, top to bottom
    let mut top = area.top();
    let filter_rect = state.piano.filters.open.then(|| {
        let r = Rect::from_min_size(area.min, Vec2::new(area.width(), filters::HEIGHT));
        top = r.bottom();
        r
    });
    let bars_rect = multi.then(|| {
        let h = shown.len() as f32 * state.piano.loop_bar_h;
        let r = Rect::from_min_max(
            Pos2::new(area.left() + KEYBOARD_W, top + 4.0),
            Pos2::new(area.right(), top + 4.0 + h),
        );
        top = r.bottom();
        r
    });
    let ruler = Rect::from_min_max(
        Pos2::new(area.left() + KEYBOARD_W, top),
        Pos2::new(area.right(), top + RULER_H),
    );
    let mut below_ruler = ruler.bottom();
    let marker_strip = (!multi).then(|| {
        let r = Rect::from_min_max(
            Pos2::new(ruler.left(), below_ruler),
            Pos2::new(ruler.right(), below_ruler + MARKER_H),
        );
        below_ruler = r.bottom();
        r
    });
    let scrub_strip = (!multi).then(|| {
        let r = Rect::from_min_max(
            Pos2::new(ruler.left(), below_ruler),
            Pos2::new(ruler.right(), below_ruler + SCRUB_H),
        );
        below_ruler = r.bottom();
        r
    });
    let stretch_strip = Rect::from_min_max(
        Pos2::new(ruler.left(), below_ruler),
        Pos2::new(ruler.right(), below_ruler + STRETCH_H),
    );
    let compact = area.height() < 210.0;
    let lanes_h = if compact {
        0.0
    } else {
        state.piano.lanes.total_height()
    };
    let lane_area = Rect::from_min_max(Pos2::new(area.left(), area.bottom() - lanes_h), area.max);
    let grid = Rect::from_min_max(
        Pos2::new(area.left() + KEYBOARD_W, stretch_strip.bottom()),
        Pos2::new(
            area.right(),
            (lane_area.top() - LANE_BAR_H).max(stretch_strip.bottom() + 40.0),
        ),
    );
    let keys = Rect::from_min_max(
        Pos2::new(area.left(), grid.top()),
        Pos2::new(grid.left(), grid.bottom()),
    );

    let rows = visible_rows(session, state, &shown, fg);
    let total_h = rows.len() as f32 * state.piano.key_h;
    let ps = &mut state.piano;
    ps.view_width = grid.width();
    ps.view_height = grid.height();
    ps.rows.clone_from(&rows);
    let beats = (limit.saturating_sub(span.0)) as f32 / PPQ as f32;
    if ps.reset_view {
        ps.px_per_beat = ((grid.width() - 20.0) / beats.max(1.0)).clamp(MIN_PPB, 160.0);
        let key_center = {
            let notes = clip.notes();
            if notes.is_empty() {
                60
            } else {
                (notes.iter().map(|n| n.key as u32).sum::<u32>() / notes.len() as u32) as u8
            }
        };
        let row = rows.iter().position(|k| *k <= key_center).unwrap_or(0) as f32;
        ps.scroll = Vec2::new(
            span.0 as f32 / PPQ as f32 * ps.px_per_beat,
            row * ps.key_h - grid.height() / 2.0,
        );
        ps.reset_view = false;
    }

    // ---- wheel over the note area: scroll; Shift = horizontal; Cmd/Ctrl = time zoom around the
    // pointer; Alt = key track zoom, or lane height over the lanes
    if ui.rect_contains_pointer(Rect::from_min_max(keys.min, area.max)) {
        let (delta, shift, zoom_mod, alt, pointer) = ui.input(|i| {
            (
                i.smooth_scroll_delta,
                i.modifiers.shift,
                i.modifiers.command,
                i.modifiers.alt,
                i.pointer.hover_pos(),
            )
        });
        let over_lanes = pointer.is_some_and(|p| p.y > lane_area.top());
        if alt && over_lanes && delta.y != 0.0 {
            ps.lanes.height = (ps.lanes.height + delta.y * 0.3).clamp(20.0, 200.0);
        } else if alt && delta.y != 0.0 {
            ps.key_h = (ps.key_h * (1.0 + delta.y * 0.004)).clamp(MIN_KEY_H, MAX_KEY_H);
        } else if zoom_mod && delta.y != 0.0 {
            let px = pointer.map_or(0.0, |p| p.x - grid.left());
            let (ppb, scroll) = zoom_about(ps.scroll.x, px, ps.px_per_beat, 1.0 + delta.y * 0.004);
            ps.px_per_beat = ppb;
            ps.scroll.x = scroll;
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
    let ps = &mut state.piano;
    let content_w = (limit as f32 / PPQ as f32 + 4.0) * ps.px_per_beat;
    ps.scroll.x = ps.scroll.x.clamp(0.0, (content_w - grid.width()).max(0.0));
    ps.scroll.y = ps.scroll.y.clamp(0.0, (total_h - grid.height()).max(0.0));
    let geo = Geo {
        grid,
        ppb: ps.px_per_beat,
        kh: ps.key_h,
        scroll: ps.scroll,
        rows,
    };

    let mut preview = Preview {
        on: None,
        off: false,
        from_keys: false,
        deadline: 0.0,
    };
    if ps.preview.is_some()
        && !ps.preview_from_keys
        && ps.gesture.is_none()
        && now > ps.preview_deadline
    {
        preview.off = true;
    }

    paint::grid(ui, &geo, session, state, &shown, fg, sig);
    rulers::time_ruler(ui, ruler, &geo, state, sig, &shown, fg, span);
    if let Some(strip) = marker_strip {
        let view = Axis {
            left: grid.left(),
            width: grid.width(),
            start: (geo.scroll.x / geo.ppb * PPQ as f32) as f64,
            ticks: (grid.width() / geo.ppb * PPQ as f32) as f64,
        };
        let snap_grid = state.piano.grid;
        markers::marker_strip(ui, strip, session, state, cref, &view, snap_grid);
        if let Some(scrub) = scrub_strip {
            markers::scrub_strip(ui, scrub, session, state, cref, &view);
        }
    }
    gestures::stretch_markers(ui, stretch_strip, &geo, session, state, cref, &shown[fg]);
    if let Some(rect) = bars_rect {
        shown::loop_bars(ui, rect, &geo, session, state, &shown, fg, span);
    }
    rulers::keys(ui, keys, &geo, session, state, &shown, fg, &mut preview);
    ui.painter().rect_filled(
        Rect::from_min_max(
            Pos2::new(area.left(), top),
            Pos2::new(grid.left(), grid.top()),
        ),
        0.0,
        shade(0x333333),
    );
    rulers::preview_switch(
        ui,
        Rect::from_min_size(
            Pos2::new(area.left() + 3.0, top + 2.0),
            Vec2::new(KEYBOARD_W - 6.0, 15.0),
        ),
        state,
    );

    gestures::grid(
        ui,
        &geo,
        session,
        state,
        &shown,
        fg,
        limit,
        now,
        &mut preview,
    );
    if let Some(rect) = filter_rect {
        filters::strip(ui, rect, session, state, &shown, fg);
    }
    if lanes_h > 0.0 {
        let bar = Rect::from_min_max(
            Pos2::new(area.left(), lane_area.top() - LANE_BAR_H),
            Pos2::new(area.right(), lane_area.top()),
        );
        lanes::bar(ui, bar, session, state, fg, &shown);
        lanes::lanes(ui, lane_area, &geo, session, state, &shown[fg], now);
    }

    let track = cref.track();
    play_previews(session, state, track, preview);

    if state.piano.show_quantize {
        lanes::quantize_window(ui, session, state);
    }
}

#[cfg(test)]
mod tests;
