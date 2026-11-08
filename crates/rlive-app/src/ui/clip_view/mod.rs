//! The Clip View: its title bar, its panels, and the editor beside them (manual ch. 8).

use super::keymap::FocusArea;
use super::theme::*;
use super::widgets::{self, info, rgb};
use super::{clip_envelopes, piano_roll, sample_editor, BrowserItem, UiState};
use crate::session::{ClipRef, Session};
use egui::{Pos2, Rect, RichText, Sense, Stroke, UiBuilder, Vec2};
use rlive_audio_io::AudioInfo;
use rlive_core::PPQ;
use rlive_project::{ClipContent, PALETTE};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

pub mod markers;
mod properties;

const TITLE_H: f32 = 22.0;
const TAB_H: f32 = 21.0;
const OVERVIEW_H: f32 = 10.0;
const PANEL_W: f32 = 172.0;
const VERTICAL_W: f32 = 204.0;
/// Clip View heights from here up put the panels in one column, when that's left automatic.
const AUTO_VERTICAL_H: f32 = 380.0;

/// How the panels are laid out (manual p. 205).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum PanelLayout {
    #[default]
    Automatic,
    Horizontal,
    Vertical,
}

impl PanelLayout {
    /// Whether the panels stand in one column at this height.
    pub fn vertical_at(self, height: f32) -> bool {
        match self {
            Self::Horizontal => false,
            Self::Vertical => true,
            Self::Automatic => height >= AUTO_VERTICAL_H,
        }
    }
}

/// What the editor beside the panels shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum EditorMode {
    /// The Sample Editor, or the MIDI Note Editor.
    #[default]
    Content,
    Envelope,
    Mpe,
}

impl EditorMode {
    /// The next tab, skipping MPE for audio clips, which don't have one.
    pub fn next(self, midi: bool) -> Self {
        match self {
            Self::Content => Self::Envelope,
            Self::Envelope if midi => Self::Mpe,
            Self::Envelope | Self::Mpe => Self::Content,
        }
    }
}

/// What the arrow keys move.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MarkerSel {
    Start,
    End,
    Loop,
}

/// The Clip View's own state.
#[derive(Default)]
pub struct ClipViewState {
    pub layout: PanelLayout,
    /// The panels are folded away.
    pub folded: bool,
    pub mode: EditorMode,
    /// Which marker the arrow keys move, if any.
    pub marker: Option<MarkerSel>,
    pub(super) grab: Option<markers::MarkerGrab>,
    /// The quantized position a held scrub is on.
    pub(super) scrubbing: Option<u64>,
    pub follow: bool,
    pub show_overview: bool,
    /// Clip View and Device View together, the Clip View on top.
    pub stacked: bool,
    /// The view's top border is being dragged, and where it began.
    pub(super) resizing: bool,
    /// The Clip View fills the Detail View's whole height.
    pub full_size: bool,
    /// Permanent Scrub Areas, mirrored from the settings.
    pub permanent_scrub: bool,
    /// The name being typed in the title bar.
    rename: Option<String>,
    details: HashMap<PathBuf, Option<AudioInfo>>,
    pub(super) tools: properties::ToolState,
}

impl ClipViewState {
    pub fn with_layout(layout: PanelLayout) -> Self {
        Self {
            layout,
            ..Self::default()
        }
    }

    /// Shows `[from, to)` ticks, remembering how it was shown so Zoom Back can return.
    pub fn zoom_to(
        &mut self,
        piano: &mut piano_roll::PianoState,
        sample: &mut sample_editor::SampleState,
        from: u64,
        to: u64,
    ) {
        sample.zoom_back.push(sample.view);
        sample.view = Some((from as f64, (to - from) as f64));
        piano.zoom_back.push((piano.px_per_beat, piano.scroll));
        let width = piano.view_width.max(1.0);
        piano.px_per_beat = (width / ((to - from) as f32 / PPQ as f32)).clamp(10.0, 800.0);
        piano.scroll.x = from as f32 / PPQ as f32 * piano.px_per_beat;
    }

    /// One step back through the zooms.
    pub fn zoom_back(
        &mut self,
        piano: &mut piano_roll::PianoState,
        sample: &mut sample_editor::SampleState,
    ) {
        if let Some(view) = sample.zoom_back.pop() {
            sample.view = view;
        }
        if let Some((ppb, scroll)) = piano.zoom_back.pop() {
            piano.px_per_beat = ppb;
            piano.scroll = scroll;
        }
    }

    /// Shows the whole clip.
    pub fn fit_width(
        &mut self,
        piano: &mut piano_roll::PianoState,
        sample: &mut sample_editor::SampleState,
    ) {
        sample.view = None;
        sample.zoom_back.clear();
        piano.zoom_back.clear();
        piano.reset_view = true;
    }
}

/// The clips an edit in the Clip View goes to: all those selected together with the one it shows.
/// The Session's selection and the Arrangement's are the ones their views draw.
pub fn targets(session: &Session, state: &UiState, cref: ClipRef) -> Vec<ClipRef> {
    let chosen: Vec<ClipRef> = match cref {
        ClipRef::Slot { .. } => state
            .sel
            .slots
            .iter()
            .map(|&(track, slot)| ClipRef::slot(track, slot))
            .collect(),
        ClipRef::Arr { .. } => state
            .arr
            .selection
            .iter()
            .map(|&(track, id)| ClipRef::Arr { track, id })
            .collect(),
    };
    with_shown(chosen, cref, |r| session.clip_at(r).is_some())
}

/// What's selected, if the clip shown is among it, without any empty slots; else just that clip.
fn with_shown(
    chosen: Vec<ClipRef>,
    shown: ClipRef,
    exists: impl Fn(ClipRef) -> bool,
) -> Vec<ClipRef> {
    if chosen.contains(&shown) {
        chosen.into_iter().filter(|r| exists(*r)).collect()
    } else {
        vec![shown]
    }
}

/// The Clip View, or nothing if there's no clip to show.
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
    let group = targets(session, state, cref);
    let vertical = state.clip_view.layout.vertical_at(full.height());
    let folded = state.clip_view.folded;

    // Panels.
    let panels_w = if folded {
        (full.width() * 0.2).clamp(120.0, VERTICAL_W)
    } else if vertical {
        VERTICAL_W
    } else {
        let across = PANEL_W * properties::panel_count(&clip) as f32;
        across.min((full.width() - 320.0).max(PANEL_W))
    };
    let panels = Rect::from_min_size(full.min, Vec2::new(panels_w, full.height()));
    ui.painter().rect_filled(panels, 0.0, pal().panel);
    let title = Rect::from_min_size(panels.min, Vec2::new(panels_w, TITLE_H));
    title_bar(ui, title, session, state, cref, &group);
    if !folded {
        let body = Rect::from_min_max(Pos2::new(panels.left(), title.bottom()), panels.max);
        let mut pu = ui.new_child(UiBuilder::new().max_rect(body));
        pu.set_clip_rect(body.intersect(ui.clip_rect()));
        properties::show(&mut pu, session, state, cref, &group, vertical);
    }
    edge_handle(ui, panels, state, vertical);

    // Editor.
    let editor = Rect::from_min_max(Pos2::new(panels.right() + 1.0, full.top()), full.max);
    let tabs = Rect::from_min_size(
        editor.min + Vec2::new(8.0, 3.0),
        Vec2::new(editor.width() - 16.0, TAB_H - 4.0),
    );
    tab_row(ui, tabs, session, state, cref, clip.is_midi());
    let mut top = tabs.bottom() + 2.0;
    if state.clip_view.show_overview
        && clip.is_midi()
        && state.clip_view.mode == EditorMode::Content
    {
        let strip = Rect::from_min_max(
            Pos2::new(editor.left() + 8.0, top),
            Pos2::new(editor.right() - 8.0, top + OVERVIEW_H - 2.0),
        );
        top += OVERVIEW_H;
        let piano = &mut state.piano;
        let per_px = PPQ as f64 / piano.px_per_beat.max(1.0) as f64;
        let start = piano.scroll.x as f64 * per_px;
        let ticks = piano.view_width as f64 * per_px;
        let total = clip.loop_end().max(clip.end_marker()) as f64;
        if let Some(to) = markers::overview_strip(ui, strip, total, start, ticks) {
            piano.scroll.x = (to / per_px) as f32;
        }
    }
    let body = Rect::from_min_max(Pos2::new(editor.left(), top), editor.max);
    state.env.open = state.clip_view.mode == EditorMode::Envelope;
    match state.clip_view.mode {
        EditorMode::Envelope => clip_envelopes::show(ui, session, state, cref, body),
        EditorMode::Mpe if clip.is_midi() => {
            ui.painter().text(
                body.center(),
                egui::Align2::CENTER_CENTER,
                "The MPE Editor arrives with C10",
                font(12.0),
                pal().text_faint,
            );
        }
        _ => match &clip.content {
            ClipContent::Audio { file, .. } => {
                let mut eu = ui.new_child(UiBuilder::new().max_rect(body.shrink(8.0)));
                sample_editor::show(&mut eu, session, state, cref, &file.path);
            }
            ClipContent::Midi { .. } => piano_roll::note_editor(ui, body, session, state, cref),
        },
    }
    drop_sample_on_clip(ui, session, cref, full);
}

fn tab_button(
    ui: &mut egui::Ui,
    width: f32,
    on: bool,
    label: &str,
    purpose: &str,
) -> egui::Response {
    let r = widgets::state_button(ui, Vec2::new(width, 15.0), on, pal().accent, label);
    info(r, purpose)
}

fn tab_row(
    ui: &mut egui::Ui,
    rect: Rect,
    session: &mut Session,
    state: &mut UiState,
    cref: ClipRef,
    midi: bool,
) {
    let mut bu = ui.new_child(UiBuilder::new().max_rect(rect));
    bu.horizontal(|ui| {
        let mode = state.clip_view.mode;
        let content = if midi { "Notes" } else { "Sample" };
        if tab_button(
            ui,
            58.0,
            mode == EditorMode::Content,
            content,
            "Clip contents — the waveform or the notes",
        )
        .clicked()
        {
            state.clip_view.mode = EditorMode::Content;
        }
        if tab_button(
            ui,
            70.0,
            mode == EditorMode::Envelope,
            "Envelopes",
            "Envelopes — clip envelopes: automate or modulate controls, sample settings or MIDI \
             controllers",
        )
        .clicked()
        {
            state.clip_view.mode = EditorMode::Envelope;
        }
        if midi
            && tab_button(
                ui,
                42.0,
                mode == EditorMode::Mpe,
                "MPE",
                "MPE — each note's pitch, slide and pressure",
            )
            .clicked()
        {
            state.clip_view.mode = EditorMode::Mpe;
        }
        ui.add_space(10.0);
        let follow = state.clip_view.follow;
        if tab_button(
            ui,
            44.0,
            follow,
            "Follow",
            "Follow — keep the playhead in view while the clip plays",
        )
        .clicked()
        {
            state.clip_view.follow = !follow;
        }
        if midi && mode == EditorMode::Content {
            properties::notes_row(ui, session, state, cref);
        }
    });
}

/// The right edge of the panels: dragging it far enough one way or the other changes which way
/// they run (manual p. 205).
fn edge_handle(ui: &mut egui::Ui, panels: Rect, state: &mut UiState, vertical: bool) {
    ui.painter().line_segment(
        [panels.right_top(), panels.right_bottom()],
        Stroke::new(1.0, pal().separator),
    );
    let handle = Rect::from_x_y_ranges(
        panels.right() - 3.0..=panels.right() + 3.0,
        panels.top() + TITLE_H..=panels.bottom(),
    );
    let resp = ui.interact(handle, ui.scope_id().with("panel_edge"), Sense::drag());
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    if resp.drag_stopped() {
        let moved = ui
            .input(|i| i.pointer.interact_pos())
            .map_or(0.0, |p| p.x - panels.right());
        if moved < -40.0 && !vertical {
            state.clip_view.layout = PanelLayout::Vertical;
        } else if moved > 40.0 && vertical {
            state.clip_view.layout = PanelLayout::Horizontal;
        }
    }
    info(
        resp,
        "Drag left to stack the panels, right to run them across",
    );
}

/// The Clip Activator, name and colour, and what else the title bar offers.
fn title_bar(
    ui: &mut egui::Ui,
    rect: Rect,
    session: &mut Session,
    state: &mut UiState,
    cref: ClipRef,
    group: &[ClipRef],
) {
    let Some(clip) = session.clip_at(cref).cloned() else {
        return;
    };
    let multi = group.len() > 1;
    let shared = session.multi_clip(group);
    let p = ui.painter_at(rect);
    if multi {
        // Stripes in the UI's colours: no one clip's colour says it for all of them.
        let bars = [pal().accent, shade(0x6a6a6a), pal().text_dim];
        for (i, x) in (0..(rect.width() / 8.0).ceil() as usize).enumerate() {
            let left = rect.left() + x as f32 * 8.0;
            p.rect_filled(
                Rect::from_x_y_ranges(left..=left + 8.0, rect.y_range()),
                0.0,
                bars[i % bars.len()],
            );
        }
    } else {
        p.rect_filled(rect, 0.0, rgb(clip.color));
    }
    let text_color = if multi {
        pal().text
    } else {
        text_on(rgb(clip.color))
    };
    let bg = ui.interact(rect, ui.scope_id().with("clip_title"), Sense::click());
    if bg.double_clicked() {
        state.clip_view.folded = !state.clip_view.folded;
    }
    let mut tu = ui.new_child(
        UiBuilder::new()
            .max_rect(rect.shrink2(Vec2::new(3.0, 2.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    tu.spacing_mut().item_spacing.x = 3.0;
    let (active, mixed) = match shared.active {
        Some((lo, hi)) => (hi, lo != hi),
        None => (clip.active, false),
    };
    let (dot, dresp) = tu.allocate_exact_size(Vec2::new(16.0, 16.0), Sense::click());
    tu.painter().circle(
        dot.center(),
        6.0,
        if mixed {
            shade(0x6a6a6a)
        } else if active {
            pal().accent
        } else {
            shade(0x3a3a3a)
        },
        Stroke::new(1.0, shade(0x101010)),
    );
    if info(
        dresp,
        "Clip Activator — switch the clip off so it doesn't play",
    )
    .clicked()
    {
        session.set_clips_active(group, !active);
    }
    if let Some(name) = state.clip_view.rename.as_mut() {
        let r = tu.add(egui::TextEdit::singleline(name).desired_width(f32::INFINITY));
        r.request_focus();
        if r.lost_focus() {
            let typed = state.clip_view.rename.take().unwrap_or_default();
            if tu.input(|i| !i.key_pressed(egui::Key::Escape)) {
                session.rename_clips(group, &typed);
            }
        }
    } else {
        let name = if multi {
            format!("{} Clips", group.len())
        } else {
            clip.name.clone()
        };
        tu.add(egui::Label::new(RichText::new(name).color(text_color)).truncate());
    }
    if !clip.is_midi() {
        let r = info(
            tu.small_button("Save"),
            "Save Default Clip — store these settings with the sample, for new clips of it",
        );
        if r.clicked() {
            for c in group {
                if let Err(reason) = session.save_clip_with_sample(*c) {
                    session.messages.push(reason);
                }
            }
        }
    }
    bg.context_menu(|ui| {
        if ui.button("Rename").clicked() {
            state.clip_view.rename = Some(clip.name.clone());
            ui.close();
        }
        let fold = if state.clip_view.folded {
            "Unfold"
        } else {
            "Fold"
        };
        if ui.button(fold).clicked() {
            state.clip_view.folded = !state.clip_view.folded;
            ui.close();
        }
        ui.separator();
        for (label, layout) in [
            (
                "Arrange Clip View Panels Automatically",
                PanelLayout::Automatic,
            ),
            (
                "Arrange Clip View Panels Horizontally",
                PanelLayout::Horizontal,
            ),
            ("Arrange Clip View Panels Vertically", PanelLayout::Vertical),
        ] {
            if ui
                .selectable_label(state.clip_view.layout == layout, label)
                .clicked()
            {
                state.clip_view.layout = layout;
                ui.close();
            }
        }
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::splat(2.0);
            for color in PALETTE {
                let (r, resp) = ui.allocate_exact_size(markers::SWATCH, Sense::click());
                ui.painter().rect_filled(r, 1.0, rgb(color));
                if resp.clicked() {
                    session.color_clips(group, color);
                    ui.close();
                }
            }
        });
        ui.separator();
        if ui.button("Assign Track Color to Clips").clicked() {
            let arrangement = cref.as_slot().is_none();
            session.assign_track_color(cref.track(), arrangement);
            ui.close();
        }
    });
    info(
        bg,
        "Clip Title Bar — double-click to fold the panels; right-click to rename or recolour",
    );
}

/// Dropping a sample anywhere on the Clip View puts it in the clip.
fn drop_sample_on_clip(ui: &mut egui::Ui, session: &mut Session, cref: ClipRef, area: Rect) {
    let id = ui.scope_id().with("clip_view_drop");
    let resp = ui.interact(area, id, Sense::hover());
    if let Some(item) = resp.dnd_release_payload::<BrowserItem>() {
        if let BrowserItem::Sample(path) = &*item {
            session.replace_clip_sample(cref, path);
        }
    }
}

/// What a sample says about itself, asked for once.
fn sample_details<'a>(
    state: &'a mut ClipViewState,
    path: &std::path::Path,
) -> &'a Option<AudioInfo> {
    state
        .details
        .entry(path.to_path_buf())
        .or_insert_with(|| rlive_audio_io::probe(path).ok())
}

/// One line about the samples of the clips selected: name, rate, depth and channels. A value that
/// isn't the same for all of them is an asterisk (manual p. 230).
pub(super) fn details_line(
    state: &mut ClipViewState,
    paths: &[PathBuf],
) -> Option<(String, String)> {
    let first = paths.first()?;
    let infos: Vec<Option<AudioInfo>> = paths
        .iter()
        .map(|p| sample_details(state, p).clone())
        .collect();
    let same = |f: &dyn Fn(&AudioInfo) -> String| -> String {
        let values: Vec<String> = infos.iter().flatten().map(f).collect();
        match values.first() {
            Some(v) if values.iter().all(|o| o == v) => v.clone(),
            Some(_) => "*".into(),
            None => "?".into(),
        }
    };
    let names: std::collections::HashSet<_> = paths.iter().map(|p| p.file_name()).collect();
    let name = if names.len() > 1 {
        "*".to_string()
    } else {
        first
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
    };
    let rate = same(&|i: &AudioInfo| format!("{:.1} kHz", i.sample_rate as f64 / 1000.0));
    let bits =
        same(&|i: &AudioInfo| i.bits_per_sample.map_or("—".into(), |b| format!("{b} Bit")));
    let channels = same(&|i: &AudioInfo| match i.channels {
        1 => "Mono".to_string(),
        2 => "Stereo".to_string(),
        n => format!("{n} Ch"),
    });
    let mut text = format!("{rate} · {bits} · {channels}");
    if paths.len() > 1 {
        text.push_str(&format!(" · {} Samples", paths.len()));
    }
    Some((name, text))
}

/// The commands the editors' context menus share: Crop, and those about the sample's file.
pub(super) fn crop_menu(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    cref: ClipRef,
) {
    let Some(clip) = session.clip_at(cref).cloned() else {
        return;
    };
    let midi = clip.is_midi();
    let busy = session.clip_job_running();
    let crop = if midi {
        "Crop Clip"
    } else {
        "Crop Clip Sample"
    };
    if ui.add_enabled(!busy, egui::Button::new(crop)).clicked() {
        session.crop_clip(cref, None);
        ui.close();
    }
    if let Some(range) = time_selection(session, state, cref) {
        let label = if midi {
            "Crop Clip to Time Selection"
        } else {
            "Crop Clip Sample to Time Selection"
        };
        if ui.add_enabled(!busy, egui::Button::new(label)).clicked() {
            session.crop_clip(cref, Some(range));
            ui.close();
        }
    }
    if let ClipContent::Audio { file, .. } = &clip.content {
        ui.separator();
        if ui.button("Show in Browser").clicked() {
            show_in_browser(state, &file.path);
            ui.close();
        }
        ui.add_enabled(false, egui::Button::new("Show Similar Files"));
        if ui.button("Manage Sample File").clicked() {
            session.manage_sample_file(cref);
            state.status = "The File Manager arrives with C13".into();
            ui.close();
        }
    }
}

/// The time selection of the clip's editor, in ticks.
pub(super) fn time_selection(
    session: &Session,
    state: &UiState,
    cref: ClipRef,
) -> Option<(u64, u64)> {
    let clip = session.clip_at(cref)?;
    if clip.is_midi() {
        let notes = clip.notes();
        let chosen: Vec<&rlive_project::NoteData> = state
            .piano
            .selected
            .iter()
            .filter_map(|i| notes.get(*i))
            .collect();
        let from = chosen.iter().map(|n| n.start).min()?;
        let to = chosen.iter().map(|n| n.start + n.length).max()?;
        Some((from, to))
    } else {
        let (a, b) = state.sample.selection_seconds()?;
        let (a, b) = (a.min(b), a.max(b));
        Some((
            session.tick_of_seconds(clip, a) as u64,
            session.tick_of_seconds(clip, b) as u64,
        ))
    }
}

/// Reveals a sample in the browser.
pub(super) fn show_in_browser(state: &mut UiState, path: &std::path::Path) {
    state.show_browser = true;
    state.browser_category = super::browser::Category::Samples;
    state.browser_filter = path
        .file_stem()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    state.focus = FocusArea::Browser;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_layout_stands_the_panels_up_in_a_tall_view() {
        assert!(PanelLayout::Vertical.vertical_at(100.0));
        assert!(!PanelLayout::Horizontal.vertical_at(900.0));
        assert!(!PanelLayout::Automatic.vertical_at(300.0));
        assert!(PanelLayout::Automatic.vertical_at(500.0));
    }

    #[test]
    fn an_edit_goes_to_the_selection_only_if_the_shown_clip_is_in_it() {
        let t = rlive_core::TrackId::new();
        let (a, b, empty) = (
            ClipRef::slot(t, 0),
            ClipRef::slot(t, 1),
            ClipRef::slot(t, 2),
        );
        let exists = |r: ClipRef| r != empty;
        assert_eq!(with_shown(vec![a, b, empty], a, exists), vec![a, b]);
        assert_eq!(with_shown(vec![b], a, exists), vec![a]);
        assert_eq!(with_shown(vec![], a, exists), vec![a]);
    }

    #[test]
    fn the_tab_cycle_skips_mpe_for_audio() {
        assert_eq!(EditorMode::Content.next(true), EditorMode::Envelope);
        assert_eq!(EditorMode::Envelope.next(true), EditorMode::Mpe);
        assert_eq!(EditorMode::Mpe.next(true), EditorMode::Content);
        assert_eq!(EditorMode::Envelope.next(false), EditorMode::Content);
    }
}
