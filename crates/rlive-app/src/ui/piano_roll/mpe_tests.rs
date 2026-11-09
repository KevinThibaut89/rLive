//! The MPE tab driven through egui events: each gesture as a user would make it, then what it
//! did to the notes and the undo history.

use super::mpe_input;
use super::*;
use crate::ui::clip_view::EditorMode;
use egui::{Event, PointerButton, RawInput};
use rlive_project::mpe_edit::Dim;
use rlive_project::{Breakpoint, NoteExpression};

const AREA: Rect = Rect {
    min: Pos2::new(0.0, 0.0),
    max: Pos2::new(1000.0, 700.0),
};

struct Rig {
    _engine: rlive_engine::Engine,
    s: Session,
    ui: UiState,
    r: ClipRef,
    ctx: egui::Context,
    time: f64,
    down: bool,
}

fn rig(notes: Vec<NoteData>) -> Rig {
    let (engine, handle) = rlive_engine::create_engine(48_000.0, 512);
    let mut s = Session::new(
        handle,
        rlive_project::Project::new_default(),
        rlive_plugin_host::scanner::PluginCache::default(),
    );
    let t = s.add_track(rlive_project::TrackKind::Midi, None);
    let mut c = Clip::new_midi("c", [200, 100, 50], 2, Default::default());
    *c.notes_mut().unwrap() = notes;
    s.set_clip(t, 0, Some(c));
    s.commit_edits(false);
    let mut ui = UiState::initial(&s, &crate::settings::Settings::default(), String::new());
    let r = ClipRef::slot(t, 0);
    ui.sel.slots = vec![(t, 0)];
    ui.selected_clip = Some(r);
    ui.clip_view.mode = EditorMode::Mpe;
    ui.piano.foreground = Some(r);
    ui.piano.selected = vec![0];
    ui.piano.reset_view = false;
    ui.piano.px_per_beat = 100.0;
    ui.piano.key_h = 12.0;
    ui.piano.scroll = Vec2::new(0.0, 12.0 * 60.0);
    let mut rig = Rig {
        _engine: engine,
        s,
        ui,
        r,
        ctx: egui::Context::default(),
        time: 0.0,
        down: false,
    };
    rig.frame(Vec::new());
    rig
}

impl Rig {
    fn frame(&mut self, events: Vec<Event>) {
        let raw = RawInput {
            events,
            time: Some(self.time),
            screen_rect: Some(AREA),
            ..RawInput::default()
        };
        let (s, ui, r) = (&mut self.s, &mut self.ui, self.r);
        let mut out = self.ctx.run_ui(raw, |root| {
            note_editor(root, AREA, s, ui, r);
        });
        out.textures_delta.clear();
        self.time += 0.02;
        self.s.commit_edits(self.down);
    }

    /// Screen position of `tick` ticks into a 1-beat note at `key`.
    fn grid_rect(&self) -> Rect {
        let top = AREA.top() + RULER_H + MARKER_H + SCRUB_H + STRETCH_H;
        let bottom = AREA.bottom() - self.ui.piano.mpe.total_height() - LANE_BAR_H;
        Rect::from_min_max(
            Pos2::new(AREA.left() + KEYBOARD_W, top),
            Pos2::new(AREA.right(), bottom),
        )
    }

    fn pitch_pos(&self, tick: f64, key: u8, semis: f64) -> Pos2 {
        let g = self.grid_rect();
        let row = 127 - key as usize;
        let ps = &self.ui.piano;
        Pos2::new(
            g.left() + (tick / PPQ as f64) as f32 * ps.px_per_beat - ps.scroll.x,
            g.top() + row as f32 * ps.key_h - ps.scroll.y + ps.key_h / 2.0
                - semis as f32 * ps.key_h,
        )
    }

    /// Screen position in the Slide lane (the top lane).
    fn slide_pos(&self, tick: f64, value: f64) -> Pos2 {
        let g = self.grid_rect();
        let top = AREA.bottom() - self.ui.piano.mpe.total_height();
        let plot = Rect::from_min_max(
            Pos2::new(g.left(), top),
            Pos2::new(AREA.right(), top + self.ui.piano.mpe.heights[0]),
        );
        let ps = &self.ui.piano;
        Pos2::new(
            g.left() + (tick / PPQ as f64) as f32 * ps.px_per_beat - ps.scroll.x,
            plot.bottom() - 4.0 - value as f32 * (plot.height() - 8.0),
        )
    }

    fn move_to(&mut self, pos: Pos2) {
        self.frame(vec![Event::PointerMoved(pos)]);
    }

    fn button(&mut self, pos: Pos2, pressed: bool, modifiers: egui::Modifiers) {
        self.down = pressed;
        self.frame(vec![Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers,
        }]);
    }

    fn click(&mut self, pos: Pos2) {
        self.move_to(pos);
        self.button(pos, true, egui::Modifiers::NONE);
        self.button(pos, false, egui::Modifiers::NONE);
        self.frame(Vec::new());
        self.time += 1.0;
    }

    fn drag(&mut self, from: Pos2, to: Pos2, modifiers: egui::Modifiers) {
        self.move_to(from);
        self.ui_input_modifiers(modifiers);
        self.button(from, true, modifiers);
        let steps = 5;
        for i in 1..=steps {
            let pos = from + (to - from) * (i as f32 / steps as f32);
            self.frame_mod(vec![Event::PointerMoved(pos)], modifiers);
        }
        self.down = false;
        self.frame_mod(
            vec![Event::PointerButton {
                pos: to,
                button: PointerButton::Primary,
                pressed: false,
                modifiers,
            }],
            modifiers,
        );
        self.frame_mod(Vec::new(), PLAIN);
        self.time += 1.0;
    }

    fn ui_input_modifiers(&mut self, modifiers: egui::Modifiers) {
        self.frame_mod(Vec::new(), modifiers);
    }

    fn frame_mod(&mut self, events: Vec<Event>, modifiers: egui::Modifiers) {
        let mut events = events;
        events.insert(0, Event::ModifiersChanged(modifiers));
        let raw = RawInput {
            events,
            time: Some(self.time),
            screen_rect: Some(AREA),
            ..RawInput::default()
        };
        let (s, ui, r) = (&mut self.s, &mut self.ui, self.r);
        let mut out = self.ctx.run_ui(raw, |root| {
            note_editor(root, AREA, s, ui, r);
        });
        out.textures_delta.clear();
        self.time += 0.02;
        self.s.commit_edits(self.down);
    }

    fn note(&self) -> NoteData {
        self.s.clip_at(self.r).unwrap().notes()[0].clone()
    }

    fn points(&self, dim: Dim) -> Vec<Breakpoint> {
        dim.points(&self.note()).to_vec()
    }
}

fn note_with(pitch: &[(f64, f64)]) -> NoteData {
    let mut n = NoteData::new(0, 4 * PPQ, 60, 0.8);
    n.expr = Some(Box::new(NoteExpression {
        pitch: pitch.iter().map(|&(t, v)| Breakpoint::new(t, v)).collect(),
        ..Default::default()
    }));
    n
}

const PLAIN: egui::Modifiers = egui::Modifiers::NONE;

#[test]
fn a_click_on_the_line_adds_a_point_and_a_click_on_a_point_takes_it_away() {
    let mut r = rig(vec![NoteData::new(0, 4 * PPQ, 60, 0.8)]);
    let pos = r.pitch_pos(960.0, 60, 0.0);
    r.click(pos);
    let points = r.points(Dim::Pitch);
    assert_eq!(points.len(), 1, "{points:?}");
    assert!((points[0].time - 960.0).abs() < 5.0);
    assert!(points[0].value.abs() < 0.1, "on the dashed line at rest");

    r.click(pos);
    assert!(r.points(Dim::Pitch).is_empty(), "clicked again: it's gone");
    assert!(
        r.note().expr.is_none(),
        "with nothing left the note carries nothing"
    );
}

#[test]
fn dragging_a_point_moves_it_and_is_one_undo_step() {
    let n = note_with(&[(0.0, 0.0), (960.0, 0.0)]);
    let mut r = rig(vec![n.clone()]);
    let from = r.pitch_pos(960.0, 60, 0.0);
    let to = r.pitch_pos(1200.0, 60, 2.0);
    r.drag(from, to, PLAIN);
    let moved = r.points(Dim::Pitch);
    assert!((moved[1].value - 2.0).abs() < 0.2, "{moved:?}");
    assert!((moved[1].time - 1200.0).abs() < 8.0, "{moved:?}");
    assert!(r.s.undo());
    assert_eq!(r.note(), n, "one undo takes back the whole drag");
}

#[test]
fn shift_drags_vertically_at_a_finer_resolution() {
    let n = note_with(&[(0.0, 0.0), (960.0, 0.0)]);
    let mut r = rig(vec![n]);
    let from = r.pitch_pos(960.0, 60, 0.0);
    let to = from + Vec2::new(0.0, -40.0);
    r.drag(from, to, egui::Modifiers::SHIFT);
    let v = r.points(Dim::Pitch)[1].value;
    assert!(
        v > 0.0 && v < 2.0,
        "well short of the 3.3 st a plain drag gives: {v}"
    );
}

#[test]
fn alt_dragging_a_segment_curves_it_and_alt_double_click_straightens_it() {
    let n = note_with(&[(0.0, 0.0), (1920.0, 4.0)]);
    let mut r = rig(vec![n]);
    let on_line = r.pitch_pos(960.0, 60, 2.0);
    r.drag(
        on_line,
        on_line + Vec2::new(0.0, 30.0),
        egui::Modifiers::ALT,
    );
    assert_ne!(r.points(Dim::Pitch)[0].curve, 0.0);
}

#[test]
fn a_breakpoint_snaps_to_semitones_with_the_modifier_when_the_grid_is_off() {
    let n = note_with(&[(0.0, 0.0), (960.0, 0.0)]);
    let mut r = rig(vec![n]);
    let from = r.pitch_pos(960.0, 60, 0.0);
    let to = r.pitch_pos(960.0, 60, 1.6);
    r.drag(
        from,
        to,
        egui::Modifiers::MAC_CMD | egui::Modifiers::COMMAND,
    );
    let v = r.points(Dim::Pitch)[1].value;
    assert_eq!(v, v.round(), "{v}");
}

#[test]
fn slide_points_are_edited_in_their_lane() {
    let mut r = rig(vec![NoteData::new(0, 4 * PPQ, 60, 0.8)]);
    let pos = r.slide_pos(480.0, Dim::Slide.rest());
    r.click(pos);
    assert_eq!(r.points(Dim::Slide).len(), 1);
    let from = r.slide_pos(r.points(Dim::Slide)[0].time, r.points(Dim::Slide)[0].value);
    let to = r.slide_pos(r.points(Dim::Slide)[0].time, 0.9);
    r.drag(from, to, PLAIN);
    let v = r.points(Dim::Slide)[0].value;
    assert!((v - 0.9).abs() < 0.05, "{v}");
}

#[test]
fn outside_the_note_a_drag_scales_the_envelope() {
    let mut n = NoteData::new(0, 2 * PPQ, 60, 0.8);
    n.expr = Some(Box::new(NoteExpression {
        slide: vec![Breakpoint::new(0.0, 0.4), Breakpoint::new(960.0, 0.8)],
        ..Default::default()
    }));
    let mut r = rig(vec![n]);
    // A click in the grid clear of any note.
    let away = r.pitch_pos(3.5 * PPQ as f64, 64, 0.0);
    r.click(away);
    assert!(r.ui.piano.mpe.scale_mode);
    assert_eq!(r.ui.piano.selected, vec![0], "the note stays selected");
    let at = r.slide_pos(480.0, 0.6);
    let up = at + Vec2::new(0.0, -13.0);
    r.drag(at, up, PLAIN);
    let p = r.points(Dim::Slide);
    assert!(p[0].value > 0.4, "scaled up: {p:?}");
    let ratio = p[1].value / p[0].value;
    assert!((ratio - 2.0).abs() < 1e-4, "the shape is kept: {p:?}");
    // And a second click outside clears the selection.
    r.click(away);
    assert!(r.ui.piano.selected.is_empty());
}

#[test]
fn draw_mode_draws_on_the_note_and_is_one_step() {
    let n = NoteData::new(0, 4 * PPQ, 60, 0.8);
    let mut r = rig(vec![n.clone()]);
    r.ui.arr.auto.draw = true;
    let from = r.slide_pos(100.0, 0.2);
    let to = r.slide_pos(1600.0, 0.7);
    r.drag(from, to, PLAIN);
    let p = r.points(Dim::Slide);
    assert!(p.len() > 2, "{p:?}");
    assert!(p.first().unwrap().value < p.last().unwrap().value);
    assert!(r.s.undo());
    assert_eq!(r.note(), n);
}

#[test]
fn select_all_then_drag_moves_every_point_equally() {
    let n = note_with(&[(0.0, 0.0), (960.0, 1.0), (1920.0, 2.0)]);
    let mut r = rig(vec![n]);
    r.ui.piano.mpe.focus_dim = Dim::Pitch;
    assert!(mpe::select_all_breakpoints(&r.s, &mut r.ui));
    let from = r.pitch_pos(960.0, 60, 1.0);
    let to = r.pitch_pos(960.0, 60, 3.0);
    r.drag(from, to, PLAIN);
    let p = r.points(Dim::Pitch);
    assert!((p[0].value - 2.0).abs() < 0.2, "{p:?}");
    assert!((p[1].value - 3.0).abs() < 0.2, "{p:?}");
    assert!((p[2].value - 4.0).abs() < 0.2, "{p:?}");
}

#[test]
fn clear_all_envelopes_empties_the_selection_and_is_undoable() {
    let n = note_with(&[(0.0, 0.0), (960.0, 1.0)]);
    let mut r = rig(vec![n.clone()]);
    r.s.edit_notes(r.r, "Clear All Envelopes", |v| {
        rlive_project::mpe_edit::clear_envelopes(v, &[0])
    });
    assert!(r.note().expr.is_none());
    r.s.commit_edits(false);
    assert!(r.s.undo());
    assert_eq!(r.note(), n);
}

#[test]
fn pitch_cant_be_edited_under_fold() {
    let n = NoteData::new(0, 4 * PPQ, 60, 0.8);
    let mut r = rig(vec![n]);
    r.ui.piano.fold_notes = true;
    r.ui.piano.scroll = Vec2::ZERO;
    r.frame(Vec::new());
    assert!(mpe::pitch_hidden(&r.ui));
    let g = r.grid_rect();
    r.click(Pos2::new(g.left() + 100.0, g.top() + 6.0));
    assert!(
        r.points(Dim::Pitch).is_empty(),
        "the envelope isn't there to click on"
    );
}

#[test]
fn edit_value_and_add_value_set_points_exactly() {
    let n = note_with(&[(0.0, 0.0), (960.0, 1.0)]);
    let mut r = rig(vec![n]);
    let cref = r.r;
    mpe_input::apply_typed(
        &mut r.s,
        &mut r.ui,
        cref,
        &mpe_input::Typing {
            note: 0,
            dim: Dim::Pitch,
            point: Some(1),
            tick: 0.0,
            text: "5.5 st".into(),
            pos: Pos2::ZERO,
            fresh: false,
        },
    );
    assert_eq!(r.points(Dim::Pitch)[1].value, 5.5);
    mpe_input::apply_typed(
        &mut r.s,
        &mut r.ui,
        cref,
        &mpe_input::Typing {
            note: 0,
            dim: Dim::Slide,
            point: None,
            tick: 480.0,
            text: "127".into(),
            pos: Pos2::ZERO,
            fresh: false,
        },
    );
    let s = r.points(Dim::Slide);
    assert_eq!(
        (s[0].time, s[0].value),
        (480.0, 1.0),
        "added at the value typed"
    );
}

#[test]
fn the_triangle_shows_hides_and_shows_everything_with_alt() {
    let mut m = MpeState::default();
    assert_eq!(
        m.shown,
        [true, true, false, false],
        "Slide and Pressure by default"
    );
    m.toggle_lanes(false);
    assert!(m.hide_all);
    m.toggle_lanes(false);
    assert!(!m.hide_all);
    m.shown = [false; 4];
    m.toggle_lanes(false);
    assert_eq!(m.shown, [true; 4], "with none on, it brings them all");
    m.shown = [false, true, false, false];
    m.hide_all = true;
    m.toggle_lanes(true);
    assert_eq!(m.shown, [true; 4]);
    assert!(!m.hide_all);
    assert_eq!(MpeState::default().total_height(), 104.0);
}

#[test]
fn a_clips_mpe_grid_is_off_by_default_and_saved_with_it() {
    let mut r = rig(vec![NoteData::new(0, 480, 60, 0.8)]);
    assert!(!r.s.clip_at(r.r).unwrap().mpe_grid.on);
    mpe_input::set_grid(&mut r.s, r.r, true);
    assert!(r.s.clip_at(r.r).unwrap().mpe_grid.on);
    let text = serde_json::to_string(r.s.clip_at(r.r).unwrap()).unwrap();
    assert!(text.contains("mpe_grid"), "kept with the clip");
    assert!(!r.ui.piano.snap_off, "separate from the notes tab's");
}
