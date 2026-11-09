//! The Transform and Generate panels (manual ch. 11): which tool, its settings, and Auto Apply.
//! What the tools do is in [`rlive_project::midi_tools`]; this module decides when they run.

mod controls;
mod displays;
mod generate;
mod transform;

use crate::session::{ClipRef, Session};
use crate::ui::clip_view::EditorMode;
use crate::ui::piano_roll::{self, Shown};
use crate::ui::theme::*;
use crate::ui::widgets::{self, info};
use crate::ui::UiState;
use egui::{RichText, Vec2};
use rlive_project::midi_tools::{
    apply_generator, apply_transform, generate_range, transform_scope, Applied, Generator,
    ToolContext, Transform,
};
use rlive_project::mpe_edit::BEND_RANGE;
use rlive_project::NoteData;

/// Which panel a tool is in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Transform,
    Generate,
}

/// What a tool's settings were, to notice when they change.
#[derive(Clone, PartialEq)]
enum Seen {
    T(Transform),
    G(Generator),
}

/// The notes of one clip while a tool is adjusting them.
#[derive(Clone)]
struct ClipRun {
    r: ClipRef,
    /// As they were when the tool first touched them: what Auto Apply goes back to.
    original: Vec<NoteData>,
    original_selected: Vec<usize>,
    /// What each change of a setting starts again from.
    base: Vec<NoteData>,
    picks: Vec<usize>,
    cx: ToolContext,
    result: Vec<NoteData>,
    result_selected: Vec<usize>,
}

/// A tool in the middle of being adjusted: its results replace each other until something else
/// touches the notes.
struct Run {
    side: Side,
    tool: usize,
    foreground: ClipRef,
    time: Option<(u64, u64)>,
    time_only: bool,
    seed: u64,
    clips: Vec<ClipRun>,
}

pub struct ToolsState {
    /// The panel in front when they share one.
    pub side: Side,
    /// Auto Apply, per panel.
    pub auto_apply: [bool; 2],
    pub transforms: Vec<Transform>,
    pub transform: usize,
    pub generators: Vec<Generator>,
    pub generator: usize,
    /// Euclidean is showing its Voices rather than its Pattern.
    pub voices_tab: bool,
    /// Which Time Warp breakpoint the sliders show.
    pub warp_point: usize,
    /// A piano-ruler pick in progress, and the key it started on.
    pub pick_from: Option<u8>,
    /// The Generate panel is showing, so the piano ruler can pick for it.
    pub picking: bool,
    run: Option<Run>,
    seen: [Option<Seen>; 2],
    next_seed: u64,
}

impl Default for ToolsState {
    fn default() -> Self {
        Self {
            side: Side::Transform,
            auto_apply: [true, true],
            transforms: Transform::all(),
            transform: 0,
            generators: Generator::all(),
            generator: 0,
            voices_tab: false,
            warp_point: 0,
            pick_from: None,
            picking: false,
            run: None,
            seen: [None, None],
            next_seed: 0x1357_9bdf,
        }
    }
}

fn slot(side: Side) -> usize {
    match side {
        Side::Transform => 0,
        Side::Generate => 1,
    }
}

impl ToolsState {
    fn fresh_seed(&mut self) -> u64 {
        self.next_seed = self
            .next_seed
            .wrapping_mul(0x9e37_79b9_7f4a_7c15)
            .wrapping_add(1);
        self.next_seed
    }

    fn seen_now(&self, side: Side) -> Seen {
        match side {
            Side::Transform => Seen::T(self.transforms[self.transform].clone()),
            Side::Generate => Seen::G(self.generators[self.generator].clone()),
        }
    }

    fn label(&self, side: Side) -> &'static str {
        match side {
            Side::Transform => self.transforms[self.transform].name(),
            Side::Generate => self.generators[self.generator].name(),
        }
    }

    fn tool_index(&self, side: Side) -> usize {
        match side {
            Side::Transform => self.transform,
            Side::Generate => self.generator,
        }
    }

    /// Whether this panel's tool is at its defaults, which greys out Reset.
    pub fn at_defaults(&self, side: Side) -> bool {
        match side {
            Side::Transform => {
                let t = &self.transforms[self.transform];
                *t == t.defaults()
            }
            Side::Generate => {
                let t = &self.generators[self.generator];
                *t == t.defaults()
            }
        }
    }

    /// Reset: this tool's settings go back to their defaults. The notes are left alone.
    pub fn reset(&mut self, side: Side) {
        match side {
            Side::Transform => {
                let t = &mut self.transforms[self.transform];
                *t = t.defaults();
            }
            Side::Generate => {
                let t = &mut self.generators[self.generator];
                *t = t.defaults();
            }
        }
        self.run = None;
        self.seen[slot(side)] = Some(self.seen_now(side));
    }

    /// Chooses a tool. What the last one did stays as it is.
    pub fn choose(&mut self, side: Side, tool: usize) {
        match side {
            Side::Transform => self.transform = tool.min(self.transforms.len() - 1),
            Side::Generate => self.generator = tool.min(self.generators.len() - 1),
        }
        self.run = None;
        self.seen[slot(side)] = Some(self.seen_now(side));
    }

    /// Takes the pitch a generator is set to from the piano ruler (manual pp. 322, 324, 327).
    pub fn pick_keys(&mut self, from: u8, to: u8) {
        let (lo, hi) = (from.min(to), from.max(to));
        match &mut self.generators[self.generator] {
            Generator::Rhythm(t) => t.pitch = to,
            Generator::Seed(t) => t.pitch = (lo, hi),
            Generator::Shape(t) => t.pitch = (lo, hi),
            Generator::Stacks(t) => {
                if let Some(c) = t.chords.get_mut(t.current) {
                    c.root = to;
                }
            }
            Generator::Euclidean(_) => {}
        }
    }

    /// Whether the piano ruler is picking for a tool at the moment.
    pub fn picks_keys(&self, mode: EditorMode) -> bool {
        mode == EditorMode::Content
            && self.picking
            && !matches!(self.generators[self.generator], Generator::Euclidean(_))
    }
}

/// The time selection as positions in one clip, or as they are in a clip on its own timeline.
fn content_time(shown: &Shown, time: (u64, u64)) -> (u64, u64) {
    let from = shown.span.content_at(time.0 as f64).max(0.0) as u64;
    (from, from + (time.1 - time.0))
}

fn context(
    session: &Session,
    state: &UiState,
    r: ClipRef,
    range: (u64, u64),
    end: u64,
) -> Option<ToolContext> {
    let clip = session.clip_at(r)?;
    Some(ToolContext {
        range,
        end,
        grid: state.piano.grid,
        scale: Session::pitch_scale(clip),
        seed: 0,
        bend_range: BEND_RANGE,
    })
}

fn clip_run(
    r: ClipRef,
    notes: &[NoteData],
    selected: &[usize],
    picks: Vec<usize>,
    cx: ToolContext,
) -> ClipRun {
    ClipRun {
        r,
        original: notes.to_vec(),
        original_selected: selected.to_vec(),
        base: notes.to_vec(),
        picks,
        cx,
        result: notes.to_vec(),
        result_selected: selected.to_vec(),
    }
}

/// Where a Transformation works: the foreground clip, and with Focus off every other clip that
/// has notes selected, in its own time.
fn transform_clips(session: &Session, state: &UiState, r: ClipRef) -> Vec<ClipRun> {
    let ps = &state.piano;
    let (shown, _) = piano_roll::gather(session, state, r);
    let mut runs = Vec::new();
    for s in &shown {
        let selected: Vec<usize> = if s.r == r {
            ps.selected.clone()
        } else if !ps.focused() {
            ps.others.get(&s.r).cloned().unwrap_or_default()
        } else {
            Vec::new()
        };
        if s.r != r && selected.is_empty() {
            continue;
        }
        let notes = s.clip.notes();
        let time = ps.time.filter(|(a, b)| b > a).map(|t| content_time(s, t));
        let loop_range = (s.clip.loop_start, s.clip.loop_end());
        let scope = transform_scope(notes, &selected, time, ps.time_only, loop_range);
        if scope.picks.is_empty() {
            continue;
        }
        if let Some(cx) = context(session, state, s.r, scope.range, scope.end) {
            runs.push(clip_run(s.r, notes, &selected, scope.picks, cx));
        }
    }
    runs
}

/// A Generator always writes into the foreground clip.
fn generate_clips(session: &Session, state: &UiState, r: ClipRef) -> Vec<ClipRun> {
    let (shown, _) = piano_roll::gather(session, state, r);
    let Some(s) = shown.iter().find(|x| x.r == r) else {
        return Vec::new();
    };
    let time = state
        .piano
        .time
        .filter(|(a, b)| b > a)
        .map(|t| content_time(s, t));
    let range = generate_range(time, (s.clip.loop_start, s.clip.loop_end()));
    context(session, state, r, range, range.1)
        .map(|cx| clip_run(r, s.clip.notes(), &state.piano.selected, Vec::new(), cx))
        .into_iter()
        .collect()
}

fn new_run(session: &Session, state: &mut UiState, side: Side) -> Option<Run> {
    let r = state.selected_clip?;
    let clips = match side {
        Side::Transform => transform_clips(session, state, r),
        Side::Generate => generate_clips(session, state, r),
    };
    if clips.is_empty() {
        return None;
    }
    let tools = &mut state.clip_view.midi_tools;
    Some(Run {
        side,
        tool: tools.tool_index(side),
        foreground: r,
        time: state.piano.time,
        time_only: state.piano.time_only,
        seed: tools.fresh_seed(),
        clips,
    })
}

/// Whether nothing has touched the notes since the run last wrote them.
fn still_holds(run: &Run, session: &Session, state: &UiState, side: Side) -> bool {
    let tools = &state.clip_view.midi_tools;
    run.side == side
        && run.tool == tools.tool_index(side)
        && Some(run.foreground) == state.selected_clip
        && run.time == state.piano.time
        && run.time_only == state.piano.time_only
        && run.clips.iter().all(|c| {
            let selected = if c.r == run.foreground {
                &state.piano.selected
            } else {
                match state.piano.others.get(&c.r) {
                    Some(s) => s,
                    None => return false,
                }
            };
            *selected == c.result_selected
                && session.clip_at(c.r).is_some_and(|x| x.notes() == c.result)
        })
}

fn run_tool(state: &UiState, side: Side, c: &ClipRun, seed: u64) -> Applied {
    let tools = &state.clip_view.midi_tools;
    let cx = ToolContext { seed, ..c.cx };
    match side {
        Side::Transform => {
            apply_transform(&tools.transforms[tools.transform], &c.base, &c.picks, &cx)
        }
        Side::Generate => apply_generator(&tools.generators[tools.generator], &c.base, &cx),
    }
}

fn set_selection(state: &mut UiState, foreground: ClipRef, r: ClipRef, selected: Vec<usize>) {
    if r == foreground {
        state.piano.selected = selected;
    } else {
        state.piano.others.insert(r, selected);
    }
}

/// Writes the tool's output for a run into its clips.
fn write_run(session: &mut Session, state: &mut UiState, side: Side, run: &mut Run) {
    let label = state.clip_view.midi_tools.label(side);
    let foreground = run.foreground;
    for c in &mut run.clips {
        let out = run_tool(state, side, c, run.seed);
        c.result = out.notes;
        c.result_selected = out.selected;
        let notes = c.result.clone();
        session.edit_notes(c.r, label, |n| n.clone_from(&notes));
        set_selection(state, foreground, c.r, c.result_selected.clone());
    }
}

/// A setting has changed: Auto Apply reworks the notes from where they were (manual p. 294).
pub fn settings_changed(session: &mut Session, state: &mut UiState, side: Side) {
    let kept = state
        .clip_view
        .midi_tools
        .run
        .take()
        .filter(|run| still_holds(run, session, state, side));
    let Some(mut run) = kept.or_else(|| new_run(session, state, side)) else {
        return;
    };
    write_run(session, state, side, &mut run);
    state.clip_view.midi_tools.run = Some(run);
}

/// The Apply button and its shortcut: the tool runs once more on the notes as they stand. With
/// Auto Apply on, later changes of its settings carry on from here (manual p. 295).
pub fn apply(session: &mut Session, state: &mut UiState, side: Side) {
    state.clip_view.midi_tools.side = side;
    let last = state
        .clip_view
        .midi_tools
        .run
        .take()
        .filter(|run| still_holds(run, session, state, side));
    let Some(mut run) = new_run(session, state, side) else {
        return;
    };
    if let Some(last) = &last {
        for c in &mut run.clips {
            if let Some(old) = last.clips.iter().find(|o| o.r == c.r) {
                c.original.clone_from(&old.original);
                c.original_selected.clone_from(&old.original_selected);
            }
        }
    }
    write_run(session, state, side, &mut run);
    if state.clip_view.midi_tools.auto_apply[slot(side)] {
        state.clip_view.midi_tools.run = Some(run);
    }
}

/// Auto Apply on or off. Off puts the notes back as they were before the tool touched them
/// (manual p. 294).
pub fn set_auto_apply(session: &mut Session, state: &mut UiState, side: Side, on: bool) {
    state.clip_view.midi_tools.auto_apply[slot(side)] = on;
    if on {
        return;
    }
    let held = state.clip_view.midi_tools.run.take();
    let Some(run) = held.filter(|run| still_holds(run, session, state, side)) else {
        return;
    };
    for c in &run.clips {
        let notes = c.original.clone();
        session.edit_notes(c.r, "Restore Notes", |n| n.clone_from(&notes));
        set_selection(state, run.foreground, c.r, c.original_selected.clone());
    }
}

/// Runs Auto Apply if the panel's tool has changed since it last looked.
fn notice_changes(session: &mut Session, state: &mut UiState, side: Side) {
    let now = state.clip_view.midi_tools.seen_now(side);
    let i = slot(side);
    let was = state.clip_view.midi_tools.seen[i].replace(now);
    let changed = was.is_some_and(|w| w != state.clip_view.midi_tools.seen_now(side));
    if changed {
        // Apply's shortcut goes to the panel last touched.
        state.clip_view.midi_tools.side = side;
    }
    if changed && state.clip_view.midi_tools.auto_apply[i] {
        settings_changed(session, state, side);
    }
}

fn header(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    side: Side,
    names: &[&'static str],
) {
    let i = slot(side);
    let tools = &mut state.clip_view.midi_tools;
    let mut chosen = tools.tool_index(side);
    let r = egui::ComboBox::from_id_salt(("midi_tool", i))
        .width(ui.available_width() - 8.0)
        .selected_text(names[chosen])
        .show_ui(ui, |ui| {
            for (n, name) in names.iter().enumerate() {
                ui.selectable_value(&mut chosen, n, *name);
            }
        });
    info(r.response, "Tool — what this panel does to the notes");
    if chosen != tools.tool_index(side) {
        tools.choose(side, chosen);
    }
    ui.horizontal(|ui| {
        let auto = state.clip_view.midi_tools.auto_apply[i];
        let r = widgets::state_button(ui, Vec2::new(36.0, 16.0), auto, pal().accent, "Auto");
        if info(r, "Auto Apply — change the notes as you adjust the tool").clicked() {
            set_auto_apply(session, state, side, !auto);
        }
        let r = ui.add(egui::Button::new("Apply").small());
        if info(r, "Apply — run the tool on the notes (Cmd+Enter)").clicked() {
            apply(session, state, side);
        }
        let default = state.clip_view.midi_tools.at_defaults(side);
        let r = ui.add_enabled(!default, egui::Button::new("Reset").small());
        if info(r, "Reset — this tool's settings back to their defaults").clicked() {
            state.clip_view.midi_tools.reset(side);
        }
    });
}

/// One panel: its header and the current tool.
pub fn panel(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    cref: ClipRef,
    side: Side,
) {
    let Some(clip) = session.clip_at(cref).cloned() else {
        return;
    };
    let scale = Session::pitch_scale(&clip);
    if side == Side::Generate {
        let tools = &mut state.clip_view.midi_tools;
        tools.picking = true;
        // A root moved because the clip's scale changed isn't a setting being adjusted.
        if let Generator::Stacks(t) = &mut tools.generators[tools.generator] {
            let before = t.clone();
            t.follow_scale(scale);
            if *t != before {
                tools.seen[slot(side)] = Some(tools.seen_now(side));
            }
        }
    }
    notice_changes(session, state, side);
    match side {
        Side::Transform => {
            let names: Vec<&'static str> = state
                .clip_view
                .midi_tools
                .transforms
                .iter()
                .map(Transform::name)
                .collect();
            header(ui, session, state, side, &names);
            transform::settings(ui, session, state, cref, scale);
        }
        Side::Generate => {
            let names: Vec<&'static str> = state
                .clip_view
                .midi_tools
                .generators
                .iter()
                .map(Generator::name)
                .collect();
            header(ui, session, state, side, &names);
            generate::settings(ui, state, cref, session, scale);
        }
    }
    notice_changes(session, state, side);
    if scale.is_some() {
        ui.label(
            RichText::new("Pitch in scale degrees")
                .color(pal().text_faint)
                .small(),
        );
    }
}

/// The two panels side by side as one, in a row of panels that has no room for both.
pub fn shared_panel(ui: &mut egui::Ui, session: &mut Session, state: &mut UiState, cref: ClipRef) {
    ui.horizontal(|ui| {
        for (side, label) in [(Side::Transform, "Transform"), (Side::Generate, "Generate")] {
            let on = state.clip_view.midi_tools.side == side;
            if widgets::state_button(ui, Vec2::new(70.0, 16.0), on, pal().accent, label).clicked() {
                state.clip_view.midi_tools.side = side;
            }
        }
    });
    let side = state.clip_view.midi_tools.side;
    state.clip_view.midi_tools.picking = false;
    panel(ui, session, state, cref, side);
}

/// Double-click on a `.stacks` file: it replaces the Stacks generator's bank (manual p. 326).
pub fn load_chord_bank(tools: &mut ToolsState, path: &std::path::Path) -> Result<(), String> {
    let stacks = tools.generators.iter_mut().find_map(|g| match g {
        Generator::Stacks(t) => Some(t),
        _ => None,
    });
    let Some(stacks) = stacks else {
        return Err("There is no Stacks generator to load it into".to_string());
    };
    generate::read_bank(stacks, path)
}

#[cfg(test)]
mod tests;
