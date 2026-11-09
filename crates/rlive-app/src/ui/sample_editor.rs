//! The Sample tab of the Clip View: the waveform with its Warp Markers and transients, and the
//! warp, pitch and gain controls.

use super::clip_view::markers::{self, Axis, MARKER_H, SCRUB_H};
use super::clip_view::{self, EditorMode};
use super::keymap::FocusArea;
use super::theme::*;
use super::widgets::{self, info, rgb};
use super::UiState;
use crate::session::{ClipRef, Session, WarpView};
use egui::{Align2, CursorIcon, Key, Modifiers, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use rlive_core::warp::WarpMap;
use rlive_core::PPQ;
use rlive_project::warp_edit::{self, WarpFrom, WarpOp};
use rlive_project::{BeatsLoop, BeatsPreserve, WarpMode};
use std::path::{Path, PathBuf};

const RULER_H: f32 = 16.0;
const LANE_H: f32 = 16.0;
/// How close the pointer must be to a marker or transient to be on it, in points.
const GRAB_PX: f32 = 6.0;
/// Bar lengths Warp As offers.
const WARP_AS_BARS: [u32; 5] = [1, 2, 4, 8, 16];

/// Quantize grids: label and ticks.
const QUANTIZE_GRIDS: [(&str, u64); 8] = [
    ("1/4", PPQ),
    ("1/8", PPQ / 2),
    ("1/16", PPQ / 4),
    ("1/32", PPQ / 8),
    ("1/4T", PPQ * 2 / 3),
    ("1/8T", PPQ / 3),
    ("1/16T", PPQ / 6),
    ("1/32T", PPQ / 12),
];

/// What Beats Preserve can be set to, besides transients and a bar: label and grid in ticks.
const PRESERVE_GRIDS: [(&str, u32); 5] = [
    ("1/2", 2 * PPQ as u32),
    ("1/4", PPQ as u32),
    ("1/8", PPQ as u32 / 2),
    ("1/16", PPQ as u32 / 4),
    ("1/32", PPQ as u32 / 8),
];

const MODES: [WarpMode; 6] = [
    WarpMode::Beats,
    WarpMode::Tones,
    WarpMode::Texture,
    WarpMode::RePitch,
    WarpMode::Complex,
    WarpMode::ComplexPro,
];

/// The label a warp mode goes by in a chooser, saying so when it doesn't stretch yet.
pub fn mode_label(mode: WarpMode) -> String {
    let (name, built) = match mode {
        WarpMode::Beats => ("Beats", true),
        WarpMode::Tones => ("Tones", false),
        WarpMode::Texture => ("Texture", false),
        WarpMode::RePitch => ("Re-Pitch", true),
        WarpMode::Complex => ("Complex", false),
        WarpMode::ComplexPro => ("Complex Pro", false),
    };
    if built {
        name.to_string()
    } else {
        format!("{name} (not yet available)")
    }
}

/// A drag in progress on the waveform.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Drag {
    /// A marker going to a new place in the clip.
    Marker { tick: f64 },
    /// The audio moving under a marker.
    Slide {
        tick: f64,
        seconds: f64,
        from_x: f32,
    },
    /// A pseudo-marker, which becomes a real marker the moment it moves.
    Pseudo { seconds: f64, adjacent: bool },
    /// A transient going to a new place in the audio.
    Transient { seconds: f64 },
    /// A time selection growing from where it started.
    Select { anchor: f64 },
}

/// The state of the Sample tab.
#[derive(Default)]
pub struct SampleState {
    /// The clip the rest of this state is about.
    clip: Option<ClipRef>,
    /// The tick of the selected marker.
    selected_marker: Option<f64>,
    selected_transient: Option<f64>,
    /// Where new markers go, in seconds into the file.
    insert: Option<f64>,
    /// A stretch of the audio, in seconds, its ends in either order.
    selection: Option<(f64, f64)>,
    drag: Option<Drag>,
    /// Where the context menu was opened, in seconds.
    menu_at: Option<f64>,
    /// What part of the clip the editor shows: first tick and how many. `None` shows all of it.
    pub view: Option<(f64, f64)>,
    /// Views to go back to, for each zoom in.
    pub zoom_back: Vec<Option<(f64, f64)>>,
    /// The time ruler is being dragged, and where its zoom started.
    ruler_zoom: Option<(f64, f64, f32, f32)>,
    quantize_grid: u64,
    /// 0 to 100.
    quantize_amount: f32,
}

impl SampleState {
    pub fn new() -> Self {
        Self {
            quantize_grid: PPQ / 4,
            quantize_amount: 100.0,
            ..Self::default()
        }
    }

    /// What's selected belongs to one clip; another clip starts with none of it.
    fn follow_clip(&mut self, clip: ClipRef) {
        if self.clip != Some(clip) {
            self.clip = Some(clip);
            self.selected_marker = None;
            self.selected_transient = None;
            self.insert = None;
            self.selection = None;
            self.drag = None;
            self.menu_at = None;
            self.view = None;
            self.zoom_back.clear();
        }
    }

    /// The selection as ticks, which `tick_of` maps seconds to.
    pub fn selection_seconds(&self) -> Option<(f64, f64)> {
        self.selection
    }

    /// Where new markers go, in seconds.
    pub fn insert_seconds(&self) -> Option<f64> {
        self.insert
    }

    /// Selects a stretch of the audio, in seconds.
    pub fn select(&mut self, from: f64, to: f64) {
        self.selection = Some((from, to));
    }

    /// Whether the Delete key has something here to act on.
    pub fn has_selection(&self) -> bool {
        self.selected_marker.is_some()
            || self.selected_transient.is_some()
            || self.selection.is_some()
    }
}

/// Maps between the time axis of the tab, in clip ticks, and seconds in the file.
struct Timing {
    map: Option<WarpMap>,
    ticks_per_second: f64,
}

impl Timing {
    fn seconds(&self, tick: f64) -> f64 {
        match &self.map {
            Some(m) => m.tick_to_src(tick),
            None => tick / self.ticks_per_second,
        }
    }

    fn tick(&self, seconds: f64) -> f64 {
        match &self.map {
            Some(m) => m.src_to_tick(seconds),
            None => seconds * self.ticks_per_second,
        }
    }
}

fn clips_for_edit(session: &Session, state: &UiState, cref: ClipRef) -> Vec<ClipRef> {
    let selected: Vec<ClipRef> = state
        .arr
        .selection
        .iter()
        .map(|&(track, id)| ClipRef::Arr { track, id })
        .collect();
    session.warp_group(cref, &selected)
}

/// The clip's whole panel: controls above, waveform below.
pub fn show(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    cref: ClipRef,
    path: &Path,
) {
    let Some(view) = session.warp_view(cref) else {
        status_only(ui, session, path);
        return;
    };
    state.sample.follow_clip(cref);
    let group = clips_for_edit(session, state, cref);
    details_header(ui, session, state, cref);
    waveform(ui, session, state, cref, &group, path, &view);
}

/// The samples of the clips selected, in one line (manual p. 230).
fn details_header(ui: &mut egui::Ui, session: &Session, state: &mut UiState, cref: ClipRef) {
    let group = clip_view::targets(session, state, cref);
    let paths: Vec<PathBuf> = group
        .iter()
        .filter_map(|r| match &session.clip_at(*r)?.content {
            rlive_project::ClipContent::Audio { file, .. } => Some(file.path.clone()),
            rlive_project::ClipContent::Midi { .. } => None,
        })
        .collect();
    let Some((name, text)) = clip_view::details_line(&mut state.clip_view, &paths) else {
        return;
    };
    ui.horizontal(|ui| {
        let r = ui.add(
            egui::Label::new(RichText::new(name).color(pal().text).small())
                .sense(Sense::click())
                .truncate(),
        );
        if info(r, "Sample — click to find it in the browser").clicked() {
            if let Some(path) = paths.first() {
                clip_view::show_in_browser(state, path);
            }
        }
        ui.label(RichText::new(text).small().color(pal().text_dim));
    });
}

/// The warp settings of the Audio Utilities panel.
pub(super) fn utilities(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    cref: ClipRef,
) {
    let Some(view) = session.warp_view(cref) else {
        ui.label(RichText::new("Loading…").color(pal().text_faint));
        return;
    };
    let group = clips_for_edit(session, state, cref);
    controls(ui, session, &group, &view);
}

/// The Transform panel of an audio clip: Quantize, to a grid and by an amount.
pub(super) fn quantize_panel(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    cref: ClipRef,
) {
    let Some(view) = session.warp_view(cref) else {
        return;
    };
    let group = clips_for_edit(session, state, cref);
    quantize_row(ui, session, state, &group, &view);
}

/// All there is to say about a clip whose file hasn't been read, or can't be.
fn status_only(ui: &mut egui::Ui, session: &Session, path: &Path) {
    widgets::section_label(ui, "SAMPLE");
    ui.label(RichText::new(path.display().to_string()).color(pal().text_dim));
    match session.samples.status(path) {
        Some(rlive_audio_io::SampleStatus::Failed(reason)) => {
            ui.label(RichText::new(reason).color(pal().arm_red));
        }
        _ => {
            ui.label(RichText::new("Loading…").color(pal().text_faint));
        }
    }
}

fn controls(ui: &mut egui::Ui, session: &mut Session, group: &[ClipRef], view: &WarpView) {
    let warp = &view.warp;
    ui.horizontal_wrapped(|ui| {
        let r = widgets::state_button(
            ui,
            Vec2::new(44.0, 16.0),
            warp.enabled,
            pal().accent,
            "Warp",
        );
        if info(
            r,
            "Warp — play the sample in sync with the Set tempo, or at its original speed",
        )
        .clicked()
        {
            session.set_warp_enabled(group, !warp.enabled);
        }
        let mut mode = warp.mode;
        let r = egui::ComboBox::from_id_salt("warp_mode")
            .width(80.0)
            .selected_text(mode_label(warp.mode))
            .show_ui(ui, |ui| {
                for m in MODES {
                    ui.selectable_value(&mut mode, m, mode_label(m));
                }
            });
        info(
            r.response,
            "Warp Mode — how the sample is stretched to the tempo",
        );
        if mode != warp.mode {
            session.set_warp_mode(group, mode);
        }
        mode_params(ui, session, group, view);
        bpm_field(ui, session, group, view);
    });
}

fn mode_params(ui: &mut egui::Ui, session: &mut Session, group: &[ClipRef], view: &WarpView) {
    let warp = &view.warp;
    let bar = session.project.time_signature.ticks_per_bar() as u32;
    if warp.mode != WarpMode::Beats {
        if warp.mode != WarpMode::RePitch {
            ui.label(RichText::new("plays at original speed until A10").color(pal().text_faint));
        }
        return;
    }
    let mut params = warp.params;
    let preserve_label = |p: BeatsPreserve| match p {
        BeatsPreserve::Transients => "Transients".to_string(),
        BeatsPreserve::Grid(t) if t == bar => "1 Bar".to_string(),
        BeatsPreserve::Grid(t) => PRESERVE_GRIDS
            .iter()
            .find(|(_, g)| *g == t)
            .map_or_else(|| format!("{t} ticks"), |(n, _)| (*n).to_string()),
    };
    ui.label(RichText::new("Preserve").color(pal().text_dim));
    let r = egui::ComboBox::from_id_salt("preserve")
        .width(76.0)
        .selected_text(preserve_label(params.preserve))
        .show_ui(ui, |ui| {
            ui.selectable_value(
                &mut params.preserve,
                BeatsPreserve::Transients,
                "Transients",
            );
            ui.selectable_value(&mut params.preserve, BeatsPreserve::Grid(bar), "1 Bar");
            for (name, grid) in PRESERVE_GRIDS {
                ui.selectable_value(&mut params.preserve, BeatsPreserve::Grid(grid), name);
            }
        });
    info(
        r.response,
        "Preserve — keep the audio's transients, or its divisions on a grid",
    );
    let r = egui::ComboBox::from_id_salt("loop_mode")
        .width(100.0)
        .selected_text(match params.beats_loop {
            BeatsLoop::Off => "Loop Off",
            BeatsLoop::Forward => "Loop Forward",
            BeatsLoop::BackAndForth => "Back-and-Forth",
        })
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut params.beats_loop, BeatsLoop::Off, "Loop Off");
            ui.selectable_value(&mut params.beats_loop, BeatsLoop::Forward, "Loop Forward");
            ui.selectable_value(
                &mut params.beats_loop,
                BeatsLoop::BackAndForth,
                "Back-and-Forth",
            );
        });
    info(
        r.response,
        "Transient Loop Mode — what fills the time after a segment runs out",
    );
    ui.label(RichText::new("Envelope").color(pal().text_dim));
    let r = ui.add(
        egui::DragValue::new(&mut params.transient_envelope)
            .range(0.0..=100.0)
            .speed(0.5)
            .max_decimals(0),
    );
    info(
        r,
        "Transient Envelope — the volume fade across each segment; 100 is no fade",
    );
    if params != warp.params {
        session.set_warp_params(group, params);
    }
}

fn bpm_field(ui: &mut egui::Ui, session: &mut Session, group: &[ClipRef], view: &WarpView) {
    let Some(mut bpm) = warp_edit::clip_bpm(&view.warp) else {
        return;
    };
    let before = bpm;
    ui.label(RichText::new("BPM").color(pal().text_dim));
    let r = ui.add(
        egui::DragValue::new(&mut bpm)
            .range(20.0..=999.0)
            .speed(0.25)
            .fixed_decimals(2),
    );
    info(
        r,
        "Segment BPM — the sample's tempo; hold Shift for finer steps",
    );
    if bpm != before {
        session.apply_warp_op(group, &WarpOp::SetBpm { bpm });
    }
    for (label, factor, purpose) in [
        (
            "×2",
            2.0,
            "Double Tempo — the audio covers twice as many beats",
        ),
        (
            "÷2",
            0.5,
            "Halve Tempo — the audio covers half as many beats",
        ),
    ] {
        if info(ui.small_button(label), purpose).clicked() {
            session.apply_warp_op(group, &WarpOp::ScaleTempo { factor });
        }
    }
}

fn quantize_row(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    group: &[ClipRef],
    view: &WarpView,
) {
    ui.horizontal_wrapped(|ui| {
        let sample = &mut state.sample;
        let current = QUANTIZE_GRIDS
            .iter()
            .find(|(_, g)| *g == sample.quantize_grid)
            .map_or("1/16", |(n, _)| *n);
        ui.label(RichText::new("Quantize to").color(pal().text_dim));
        let r = egui::ComboBox::from_id_salt("audio_quantize_grid")
            .width(60.0)
            .selected_text(current)
            .show_ui(ui, |ui| {
                for (name, g) in QUANTIZE_GRIDS {
                    ui.selectable_value(&mut sample.quantize_grid, g, name);
                }
            });
        info(r.response, "Grid — where transients are pulled to");
        ui.label(RichText::new("Amount").color(pal().text_dim));
        let r = ui.add(
            egui::DragValue::new(&mut sample.quantize_amount)
                .range(0.0..=100.0)
                .speed(1.0)
                .max_decimals(0)
                .suffix(" %"),
        );
        info(r, "Amount — how far the transients are moved");
        if ui.button("Quantize").clicked() {
            run_quantize(session, sample, group, view);
        }
    });
}

fn run_quantize(session: &mut Session, sample: &SampleState, group: &[ClipRef], view: &WarpView) {
    if !view.warp.enabled {
        return;
    }
    session.apply_warp_op(
        group,
        &WarpOp::Quantize {
            grid: sample.quantize_grid as f64,
            amount: sample.quantize_amount as f64 / 100.0,
            range: sample.selection,
        },
    );
}

#[allow(clippy::too_many_arguments)]
fn waveform(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    cref: ClipRef,
    group: &[ClipRef],
    path: &Path,
    view: &WarpView,
) {
    let warped = view.warp.enabled && view.warp.markers.len() >= 2;
    let timing = Timing {
        map: warped.then(|| warp_edit::seconds_map(&view.warp)).flatten(),
        ticks_per_second: session.project.tempo / 60.0 * PPQ as f64,
    };
    let clip = session.clip_at(cref).cloned();
    let clip_len = clip
        .as_ref()
        .map_or(0, |c| c.loop_end().max(c.end_marker())) as f64;
    let color = clip.as_ref().map_or(pal().accent, |c| rgb(c.color));
    let whole = timing.tick(view.seconds).max(clip_len).max(1.0);
    if state.clip_view.show_overview {
        let (strip, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 10.0), Sense::hover());
        let (start, ticks) = state.sample.view.unwrap_or((0.0, whole));
        if let Some(to) = markers::overview_strip(ui, strip, whole, start, ticks) {
            state.sample.view = Some((to, ticks));
        }
    }
    let height = (ui.available_height() - 18.0).max(64.0);
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), height),
        Sense::click_and_drag(),
    );
    if let Some(at) = session
        .clip_playhead(cref)
        .filter(|_| state.clip_view.follow)
    {
        follow_view(&mut state.sample, at, whole, state.follow_scrolls);
    }
    let (start, ticks) = state.sample.view.unwrap_or((0.0, whole));
    let axis = Axis {
        left: rect.left(),
        width: rect.width(),
        start,
        ticks,
    };

    let ruler = Rect::from_min_size(rect.min, Vec2::new(rect.width(), RULER_H));
    let marker_strip = Rect::from_min_size(
        Pos2::new(rect.left(), ruler.bottom()),
        Vec2::new(rect.width(), MARKER_H),
    );
    let scrub_strip = Rect::from_min_size(
        Pos2::new(rect.left(), marker_strip.bottom()),
        Vec2::new(rect.width(), SCRUB_H),
    );
    let lane = Rect::from_min_size(
        Pos2::new(rect.left(), scrub_strip.bottom()),
        Vec2::new(rect.width(), LANE_H),
    );
    let wave = Rect::from_min_max(Pos2::new(rect.left(), lane.bottom()), rect.max);
    let upper = rect.top() + rect.height() / 2.0;

    let p = ui.painter_at(rect);
    p.rect_filled(rect, 1.0, pal().grid_bg);
    p.rect_filled(lane, 0.0, pal().panel_dark);

    let sig = clip
        .as_ref()
        .and_then(|c| c.signature)
        .unwrap_or(session.project.time_signature);
    draw_ruler(&p, ruler, &axis, sig);
    draw_selection(&p, wave, &axis, &timing, state.sample.selection);
    let plays = clip.as_ref().map_or((0.0, clip_len), |c| {
        let r = rlive_project::clip_edit::crop_range(c);
        (r.0 as f64, r.1 as f64)
    });
    draw_waveform(&p, wave, &axis, &timing, session, path, color, plays);

    let hovered_pos = resp.hover_pos();
    let pseudo = hovered_pos.and_then(|at| {
        (at.y < wave.top() + LANE_H).then(|| {
            view.transients
                .iter()
                .copied()
                .find(|t| (axis.x(timing.tick(*t)) - at.x).abs() < GRAB_PX)
        })?
    });
    draw_transients(
        &p,
        lane,
        &axis,
        &timing,
        view,
        state.sample.selected_transient,
    );
    if warped {
        if let Some(t) = pseudo {
            draw_marker(&p, lane, axis.x(timing.tick(t)), pal().text_dim, false);
        }
        for m in &view.warp.markers {
            let selected = state.sample.selected_marker == Some(m.tick);
            let x = axis.x(m.tick);
            draw_marker(&p, lane, x, pal().activator, selected);
            p.line_segment(
                [Pos2::new(x, lane.bottom()), Pos2::new(x, rect.bottom())],
                Stroke::new(1.0, pal().activator.gamma_multiply(0.45)),
            );
        }
    }
    if let Some(s) = state.sample.insert {
        let x = axis.x(timing.tick(s));
        p.line_segment(
            [Pos2::new(x, wave.top()), Pos2::new(x, rect.bottom())],
            Stroke::new(1.0, pal().text),
        );
    }
    markers::draw_playhead(ui, wave, session, cref, &axis);

    let marker_under = |x: f32| {
        view.warp
            .markers
            .iter()
            .filter(|_| warped)
            .map(|m| ((axis.x(m.tick) - x).abs(), m.tick))
            .filter(|(d, _)| *d < GRAB_PX)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, tick)| tick)
    };
    let transient_under = |x: f32| {
        view.transients
            .iter()
            .copied()
            .find(|t| (axis.x(timing.tick(*t)) - x).abs() < GRAB_PX)
    };
    let modifiers = ui.input(|i| i.modifiers);
    let free = modifiers.command || modifiers.alt;
    let grid = state.sample.quantize_grid.max(1) as f64;
    let snap = state.piano.grid;
    markers::marker_strip(ui, marker_strip, session, state, cref, &axis, snap);
    markers::scrub_strip(ui, scrub_strip, session, state, cref, &axis);
    ruler_gesture(ui, ruler, session, state, cref, &axis, whole);

    if resp.secondary_clicked() {
        state.sample.menu_at = hovered_pos.map(|at| timing.seconds(axis.tick(at.x)));
    }
    if let Some(at) = hovered_pos {
        let in_lane = at.y < wave.top();
        if marker_under(at.x).is_some() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        } else if in_lane && pseudo.is_some() {
            ui.ctx().set_cursor_icon(CursorIcon::Grab);
        }
    }

    if resp.double_clicked() && warped {
        if let Some(at) = resp.interact_pointer_pos() {
            let tick = axis.tick(at.x);
            if let Some(marker) = marker_under(at.x) {
                session.apply_warp_op(group, &WarpOp::Delete { tick: marker });
                state.sample.selected_marker = None;
            } else if let Some(t) = transient_under(at.x).filter(|_| at.y < wave.top()) {
                session.apply_warp_op(
                    group,
                    &WarpOp::FromTransient {
                        seconds: t,
                        adjacent: modifiers.command,
                    },
                );
            } else if at.y < upper {
                session.apply_warp_op(
                    group,
                    &WarpOp::Add {
                        seconds: timing.seconds(tick),
                        tick,
                    },
                );
            }
        }
    } else if resp.clicked() {
        if let Some(at) = resp.interact_pointer_pos() {
            let sample = &mut state.sample;
            sample.selected_marker = marker_under(at.x);
            sample.selected_transient = transient_under(at.x).filter(|_| at.y < wave.top());
            if sample.selected_marker.is_none() && sample.selected_transient.is_none() {
                sample.insert = Some(timing.seconds(axis.tick(at.x)));
            }
            sample.selection = None;
        }
    }

    // With Permanent Scrub Areas on, the waveform's lower half is one (manual p. 227).
    if state.clip_view.permanent_scrub {
        let held = state.clip_view.scrubbing.is_some();
        let on_lower = resp.is_pointer_button_down_on()
            && !modifiers.shift
            && ui
                .input(|i| i.pointer.press_origin())
                .is_some_and(|o| o.y >= upper && state.sample.drag.is_none());
        if on_lower || held {
            markers::scrub_gesture(&resp, session, &mut state.clip_view, cref, &axis);
        }
    }
    if resp.drag_started() {
        let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or(rect.min);
        let sample = &mut state.sample;
        sample.drag = if let Some(tick) = marker_under(origin.x) {
            sample.selected_marker = Some(tick);
            Some(if modifiers.shift {
                Drag::Slide {
                    tick,
                    seconds: view
                        .warp
                        .markers
                        .iter()
                        .find(|m| m.tick == tick)
                        .map_or(0.0, |m| m.seconds),
                    from_x: origin.x,
                }
            } else {
                Drag::Marker { tick }
            })
        } else if let Some(t) = transient_under(origin.x).filter(|_| origin.y < wave.top()) {
            Some(if modifiers.shift {
                Drag::Transient { seconds: t }
            } else {
                Drag::Pseudo {
                    seconds: t,
                    adjacent: modifiers.command,
                }
            })
        } else if origin.y < upper || !state.clip_view.permanent_scrub || modifiers.shift {
            Some(Drag::Select {
                anchor: timing.seconds(axis.tick(origin.x)),
            })
        } else {
            None
        };
    }
    if resp.dragged() {
        if let (Some(drag), Some(at)) = (state.sample.drag, resp.interact_pointer_pos()) {
            let to_tick = |x: f32| {
                let tick = axis.tick(x);
                if free {
                    tick
                } else {
                    (tick / grid).round() * grid
                }
            };
            match drag {
                Drag::Marker { tick } => {
                    let to = to_tick(at.x);
                    if session.apply_warp_op(group, &WarpOp::Move { tick, to }) {
                        state.sample.drag = Some(Drag::Marker { tick: to });
                        state.sample.selected_marker = Some(to);
                    }
                }
                Drag::Slide {
                    tick,
                    seconds,
                    from_x,
                } => {
                    // Dragging the audio right pulls earlier audio under the marker.
                    let seconds_per_x = view.seconds / axis.width.max(1.0) as f64;
                    let to_seconds = seconds - (at.x - from_x) as f64 * seconds_per_x;
                    session.apply_warp_op(group, &WarpOp::Slide { tick, to_seconds });
                }
                Drag::Pseudo { seconds, adjacent } => {
                    let at_tick = timing.tick(seconds);
                    session.apply_warp_op(group, &WarpOp::FromTransient { seconds, adjacent });
                    state.sample.selected_marker = Some(at_tick);
                    state.sample.drag = Some(Drag::Marker { tick: at_tick });
                }
                Drag::Transient { seconds } => {
                    let to = timing.seconds(axis.tick(at.x));
                    if session.apply_warp_op(group, &WarpOp::MoveTransient { from: seconds, to }) {
                        state.sample.drag = Some(Drag::Transient { seconds: to });
                        state.sample.selected_transient = Some(to);
                    }
                }
                Drag::Select { anchor } => {
                    state.sample.selection = Some((anchor, timing.seconds(axis.tick(at.x))));
                    state.sample.selected_marker = None;
                }
            }
        }
    }
    if resp.drag_stopped() {
        state.sample.drag = None;
    }

    keyboard(ui, session, state, group, view, &timing, grid);
    context_menu(&resp, session, state, group, view);

    info(
        resp,
        "Sample — double-click the top half for a Warp Marker; drag a marker to stretch, Shift-drag \
         to slide the audio under it",
    );

    let warp_text = if warped {
        warp_edit::clip_bpm(&view.warp).map_or_else(String::new, |b| format!("{b:.2} BPM"))
    } else {
        "Warp off (plays at original speed)".to_string()
    };
    ui.label(
        RichText::new(format!(
            "{:.2} s · {} · {} transients",
            view.seconds,
            warp_text,
            view.transients.len()
        ))
        .small()
        .color(pal().text_dim),
    );
}

fn keyboard(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    group: &[ClipRef],
    view: &WarpView,
    timing: &Timing,
    grid: f64,
) {
    if state.focus != FocusArea::Clip || ui.ctx().egui_wants_keyboard_input() {
        return;
    }
    let sample = &mut state.sample;
    let consume =
        |ui: &mut egui::Ui, mods: Modifiers, key: Key| ui.input_mut(|i| i.consume_key(mods, key));
    let delete = |ui: &mut egui::Ui, mods: Modifiers| {
        ui.input_mut(|i| i.consume_key(mods, Key::Delete) || i.consume_key(mods, Key::Backspace))
    };
    let command_shift = Modifiers::COMMAND | Modifiers::SHIFT;
    if consume(ui, command_shift, Key::I) {
        if let Some(seconds) = sample.insert {
            session.apply_warp_op(group, &WarpOp::AddTransient { seconds });
        }
    }
    if delete(ui, command_shift) {
        if let Some(seconds) = sample.selected_transient.take() {
            session.apply_warp_op(group, &WarpOp::DeleteTransient { seconds });
        }
    }
    if consume(ui, Modifiers::COMMAND, Key::I) && view.warp.enabled {
        if let Some((from, to)) = sample.selection {
            session.apply_warp_op(group, &WarpOp::InsertInRange { from, to });
        } else if let Some(seconds) = sample.insert {
            let tick = timing.tick(seconds);
            session.apply_warp_op(group, &WarpOp::Add { seconds, tick });
        }
    }
    if consume(ui, Modifiers::COMMAND, Key::U) {
        run_quantize(session, sample, group, view);
    }
    if consume(ui, command_shift, Key::U) {
        state.clip_view.mode = EditorMode::Content;
    }
    let selected = sample.selected_marker;
    let ticks: Vec<f64> = view.warp.markers.iter().map(|m| m.tick).collect();
    if consume(ui, Modifiers::COMMAND, Key::ArrowLeft) {
        sample.selected_marker = selected
            .map(|s| ticks.iter().copied().rfind(|t| *t < s).unwrap_or(s))
            .or(ticks.first().copied());
    }
    if consume(ui, Modifiers::COMMAND, Key::ArrowRight) {
        sample.selected_marker = selected
            .map(|s| ticks.iter().copied().find(|t| *t > s).unwrap_or(s))
            .or(ticks.first().copied());
    }
    if let Some(tick) = selected {
        for (key, by) in [(Key::ArrowLeft, -grid), (Key::ArrowRight, grid)] {
            if consume(ui, Modifiers::NONE, key)
                && session.apply_warp_op(group, &WarpOp::Nudge { tick, by })
            {
                sample.selected_marker = Some(tick + by);
            }
        }
    }
    if delete(ui, Modifiers::NONE) {
        if let Some(tick) = sample.selected_marker.take() {
            session.apply_warp_op(group, &WarpOp::Delete { tick });
        } else if let Some(seconds) = sample.selected_transient.take() {
            session.apply_warp_op(group, &WarpOp::DeleteTransient { seconds });
        } else if let Some((from, to)) = sample.selection.take() {
            let in_range: Vec<f64> = view
                .warp
                .markers
                .iter()
                .filter(|m| m.seconds >= from.min(to) && m.seconds <= from.max(to))
                .map(|m| m.tick)
                .collect();
            for tick in in_range {
                session.apply_warp_op(group, &WarpOp::Delete { tick });
            }
        }
    }
}

fn context_menu(
    resp: &egui::Response,
    session: &mut Session,
    state: &mut UiState,
    group: &[ClipRef],
    view: &WarpView,
) {
    let ticks_per_bar = session.project.time_signature.ticks_per_bar();
    let set_bpm = session.project.tempo;
    resp.context_menu(|ui| {
        let mut auto_reset = state.midi_auto_reset;
        if ui
            .checkbox(&mut auto_reset, "MIDI Envelope Auto-Reset")
            .changed()
        {
            state.toggle_midi_auto_reset = true;
            ui.close();
        }
        ui.separator();
        clip_view::crop_menu(ui, session, state, group[0]);
        ui.separator();
        let at = state.sample.menu_at.or(state.sample.insert).unwrap_or(0.0);
        let selected_seconds = state.sample.selected_marker.and_then(|t| {
            view.warp
                .markers
                .iter()
                .find(|m| m.tick == t)
                .map(|m| m.seconds)
        });
        let here = selected_seconds.unwrap_or(at);
        let warped = view.warp.enabled && view.warp.markers.len() >= 2;
        let mut run = |ui: &mut egui::Ui, op: WarpOp| {
            session.apply_warp_op(group, &op);
            ui.close();
        };
        if !warped {
            ui.label(RichText::new("Turn Warp on to place markers").color(pal().text_faint));
        }
        ui.add_enabled_ui(warped, |ui| {
            if ui.button("Set 1.1.1 Here").clicked() {
                run(ui, WarpOp::SetOneOneOne { seconds: here });
            }
            ui.separator();
            let entries = [
                ("Warp From Here".to_string(), WarpFrom::Auto),
                (
                    format!("Warp From Here (Start At {set_bpm:.2})"),
                    WarpFrom::StartAtSetTempo,
                ),
                ("Warp From Here (Straight)".to_string(), WarpFrom::Straight),
                (
                    format!("Warp {set_bpm:.2} BPM From Here"),
                    WarpFrom::SetTempo,
                ),
            ];
            for (label, how) in entries {
                if ui.button(label).clicked() {
                    run(ui, WarpOp::WarpFrom { seconds: here, how });
                }
            }
            ui.separator();
            ui.menu_button("Warp Sample As", |ui| {
                let suggested = warp_edit::suggest_bars(view.seconds, ticks_per_bar, set_bpm);
                for bars in WARP_AS_BARS {
                    let label = bar_label(bars, bars == suggested);
                    if ui.button(label).clicked() {
                        run(
                            ui,
                            WarpOp::WarpAs {
                                from: 0.0,
                                to: view.seconds,
                                bars,
                            },
                        );
                    }
                }
            });
            if let Some((a, b)) = state.sample.selection {
                let (from, to) = (a.min(b), a.max(b));
                ui.menu_button("Warp Selection As", |ui| {
                    let tempo = view.tracked_bpm.unwrap_or(set_bpm);
                    let suggested = warp_edit::suggest_bars(to - from, ticks_per_bar, tempo);
                    for bars in WARP_AS_BARS {
                        let label = bar_label(bars, bars == suggested);
                        if ui.button(label).clicked() {
                            run(ui, WarpOp::WarpAs { from, to, bars });
                        }
                    }
                });
                if ui.button("Insert Warp Markers").clicked() {
                    run(ui, WarpOp::InsertInRange { from, to });
                }
            }
            ui.separator();
            if ui.button("Reset Transients").clicked() {
                run(ui, WarpOp::ResetTransients);
            }
        });
    });
}

fn bar_label(bars: u32, suggested: bool) -> String {
    let unit = if bars == 1 { "Bar" } else { "Bars" };
    if suggested {
        format!("{bars} {unit} (suggested)")
    } else {
        format!("{bars} {unit}")
    }
}

fn draw_ruler(p: &egui::Painter, ruler: Rect, axis: &Axis, sig: rlive_core::TimeSignature) {
    let bar = sig.ticks_per_bar();
    let mut tick = 0u64;
    let mut number = 1;
    let step_px = axis.x(bar as f64) - axis.x(0.0);
    let every = ((48.0 / step_px.max(1.0)).ceil() as u64).max(1);
    while (tick as f64) <= axis.start + axis.ticks {
        let x = axis.x(tick as f64);
        if ((number - 1) as u64).is_multiple_of(every) {
            p.line_segment(
                [Pos2::new(x, ruler.top()), Pos2::new(x, ruler.bottom())],
                Stroke::new(1.0, pal().text_faint),
            );
            p.text(
                Pos2::new(x + 3.0, ruler.center().y),
                Align2::LEFT_CENTER,
                format!("{number}"),
                egui::FontId::proportional(10.0),
                pal().text_dim,
            );
        }
        tick += bar;
        number += 1;
    }
}

fn draw_selection(
    p: &egui::Painter,
    wave: Rect,
    axis: &Axis,
    timing: &Timing,
    selection: Option<(f64, f64)>,
) {
    if let Some((a, b)) = selection {
        let (a, b) = (axis.x(timing.tick(a)), axis.x(timing.tick(b)));
        let rect = Rect::from_x_y_ranges(a.min(b)..=a.max(b), wave.y_range());
        p.rect_filled(rect, 0.0, pal().select.gamma_multiply(0.25));
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_waveform(
    p: &egui::Painter,
    wave: Rect,
    axis: &Axis,
    timing: &Timing,
    session: &Session,
    path: &Path,
    color: egui::Color32,
    plays: (f64, f64),
) {
    let Some((peaks, sample_rate)) = session.samples.peaks(path) else {
        return;
    };
    let frames = peaks.frames().max(1);
    let channels = if peaks.is_stereo() { 2 } else { 1 };
    let color = if session.samples.is_ready(path) {
        color
    } else {
        pal().text_faint
    };
    let cols = wave.width() as usize;
    for ch in 0..channels {
        let lane = wave.height() / channels as f32;
        let mid = wave.top() + ch as f32 * lane + lane / 2.0;
        for x in 0..cols {
            let left = axis.tick(wave.left() + x as f32);
            let right = axis.tick(wave.left() + x as f32 + 1.0);
            let a = (timing.seconds(left) * sample_rate).max(0.0) as usize;
            let b = ((timing.seconds(right) * sample_rate).max(0.0) as usize)
                .max(a + 1)
                .min(frames);
            if a >= frames {
                continue;
            }
            let outside = left < plays.0 || left >= plays.1;
            let (lo, hi) = peaks.min_max(ch, a, b);
            let cx = wave.left() + x as f32;
            p.line_segment(
                [
                    Pos2::new(cx, mid - hi.clamp(-1.0, 1.0) * lane * 0.48),
                    Pos2::new(cx, mid - lo.clamp(-1.0, 1.0) * lane * 0.48),
                ],
                Stroke::new(
                    1.0,
                    if outside {
                        color.gamma_multiply(0.35)
                    } else {
                        color
                    },
                ),
            );
        }
    }
}

fn draw_transients(
    p: &egui::Painter,
    lane: Rect,
    axis: &Axis,
    timing: &Timing,
    view: &WarpView,
    selected: Option<f64>,
) {
    for t in &view.transients {
        let x = axis.x(timing.tick(*t));
        let (top, color) = if selected.is_some_and(|s| (s - t).abs() < 1e-6) {
            (lane.top(), pal().text)
        } else {
            (lane.bottom() - 7.0, pal().text_dim)
        };
        p.line_segment(
            [Pos2::new(x, top), Pos2::new(x, lane.bottom())],
            Stroke::new(1.0, color),
        );
    }
}

fn draw_marker(p: &egui::Painter, lane: Rect, x: f32, color: egui::Color32, selected: bool) {
    let head = Rect::from_center_size(
        Pos2::new(x, lane.center().y),
        Vec2::new(if selected { 9.0 } else { 7.0 }, lane.height() - 2.0),
    );
    p.rect_filled(head, 1.0, color);
    if selected {
        p.rect_stroke(
            head,
            1.0,
            Stroke::new(1.0, pal().text),
            egui::StrokeKind::Outside,
        );
    }
}

/// Scrolling the view to keep the playhead on screen, a page at a time.
fn follow_view(sample: &mut SampleState, at: f64, whole: f64, scrolls: bool) {
    let (start, ticks) = sample.view.unwrap_or((0.0, whole));
    let to = if scrolls {
        at - ticks / 3.0
    } else if at < start || at >= start + ticks {
        at
    } else {
        start
    };
    if to != start {
        sample.view = Some((to.clamp(0.0, (whole - ticks).max(0.0)), ticks));
    }
}

/// The time ruler: Shift-click scrubs; dragging up or down zooms around the pointer, and sideways
/// scrolls (manual p. 226).
fn ruler_gesture(
    ui: &mut egui::Ui,
    ruler: Rect,
    session: &mut Session,
    state: &mut UiState,
    cref: ClipRef,
    axis: &Axis,
    whole: f64,
) {
    let resp = ui.interact(
        ruler,
        ui.scope_id().with("sample_ruler"),
        Sense::click_and_drag(),
    );
    if resp.clicked() && ui.input(|i| i.modifiers.shift) {
        if let Some(pos) = resp.interact_pointer_pos() {
            session.scrub_clip(cref, axis.tick(pos.x), false);
        }
    }
    if resp.drag_started() {
        if let Some(pos) = resp.interact_pointer_pos() {
            state.sample.ruler_zoom = Some((axis.start, axis.ticks, pos.x, pos.y));
        }
    }
    if let (true, Some((start, ticks, x, y)), Some(pos)) = (
        resp.dragged(),
        state.sample.ruler_zoom,
        resp.interact_pointer_pos(),
    ) {
        let zoom = (1.0 + (pos.y - y) as f64 * 0.01).clamp(0.02, 50.0);
        let new_ticks = (ticks * zoom).clamp(whole / 2000.0, whole);
        let at = Axis {
            start,
            ticks,
            ..*axis
        }
        .tick(x);
        let scroll = (x - pos.x) as f64 / axis.width.max(1.0) as f64 * new_ticks;
        let share = ((x - axis.left) / axis.width.max(1.0)) as f64;
        let new_start = (at - share * new_ticks + scroll).clamp(0.0, (whole - new_ticks).max(0.0));
        state.sample.view = Some((new_start, new_ticks));
    }
    if resp.drag_stopped() {
        state.sample.ruler_zoom = None;
    }
    info(
        resp,
        "Time Ruler — drag up or down to zoom, sideways to scroll; Shift-click to scrub",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_that_do_not_stretch_yet_say_so() {
        assert_eq!(mode_label(WarpMode::Beats), "Beats");
        assert_eq!(mode_label(WarpMode::RePitch), "Re-Pitch");
        for mode in [
            WarpMode::Tones,
            WarpMode::Texture,
            WarpMode::Complex,
            WarpMode::ComplexPro,
        ] {
            assert!(mode_label(mode).ends_with("(not yet available)"));
        }
    }
}
