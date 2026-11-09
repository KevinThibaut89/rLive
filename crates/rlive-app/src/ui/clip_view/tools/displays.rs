//! The tools' little pictures, which can mostly be dragged.

use crate::ui::theme::*;
use crate::ui::widgets::info;
use egui::{Color32, Pos2, Rect, Sense, Stroke, Vec2};
use rlive_core::envelope::curve_shape;
use rlive_project::midi_tools::{
    tonnetz, ChordDef, Dimension, Euclidean, Glissando, Lfo, Recombine, Strum, TimeWarp,
    SHAPE_POINTS,
};
use rlive_project::NoteData;

const H: f32 = 48.0;

fn area(ui: &mut egui::Ui, h: f32, sense: Sense) -> (Rect, egui::Response) {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), h), sense);
    ui.painter().rect_filled(rect, 1.0, shade(0x262626));
    (rect, resp)
}

fn to_unit(rect: Rect, pos: Pos2) -> (f64, f64) {
    (
        (((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0)) as f64,
        (((rect.bottom() - pos.y) / rect.height()).clamp(0.0, 1.0)) as f64,
    )
}

fn at(rect: Rect, x: f64, y: f64) -> Pos2 {
    Pos2::new(
        rect.left() + x as f32 * rect.width(),
        rect.bottom() - y as f32 * rect.height(),
    )
}

fn line(p: &egui::Painter, points: Vec<Pos2>, color: Color32) {
    p.add(egui::Shape::line(points, Stroke::new(1.5, color)));
}

/// Velocity Shaper's envelope: click to add a breakpoint, drag to move it, double-click to remove.
pub fn velocity_env(ui: &mut egui::Ui, points: &mut Vec<(f64, f64)>) {
    let (rect, resp) = area(ui, H, Sense::click_and_drag());
    let p = ui.painter_at(rect);
    line(
        &p,
        points.iter().map(|(x, y)| at(rect, *x, *y)).collect(),
        pal().accent,
    );
    for (x, y) in points.iter() {
        p.circle_filled(at(rect, *x, *y), 3.0, pal().text);
    }
    let pointer = resp.interact_pointer_pos();
    let nearest = |pts: &[(f64, f64)], pos: Pos2| {
        pts.iter()
            .enumerate()
            .map(|(i, (x, y))| (i, (at(rect, *x, *y) - pos).length()))
            .filter(|(_, d)| *d < 8.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    };
    if let Some(pos) = pointer {
        if resp.double_clicked() {
            if let Some(i) = nearest(points, pos) {
                if points.len() > 2 {
                    points.remove(i);
                }
            }
        } else if resp.clicked() && nearest(points, pos).is_none() {
            points.push(to_unit(rect, pos));
            points.sort_by(|a, b| a.0.total_cmp(&b.0));
        } else if resp.dragged() {
            let grabbed = ui.ctx().data_mut(|d| {
                let id = resp.id.with("grab");
                if resp.drag_started() {
                    let i = nearest(points, pos);
                    d.insert_temp(id, i);
                    i
                } else {
                    d.get_temp::<Option<usize>>(id).flatten()
                }
            });
            if let Some(i) = grabbed.filter(|i| *i < points.len()) {
                let mut v = to_unit(rect, pos);
                // The two ends keep their places in time.
                if i == 0 {
                    v.0 = 0.0;
                } else if i == points.len() - 1 {
                    v.0 = 1.0;
                } else {
                    v.0 = v.0.clamp(points[i - 1].0, points[i + 1].0);
                }
                points[i] = v;
            }
        }
    }
    info(
        resp,
        "Envelope — click to add a breakpoint, double-click to remove one, drag to shape",
    );
}

/// Shape's drawing: drag to draw levels.
pub fn shape(ui: &mut egui::Ui, levels: &mut Vec<f64>, purple: bool) {
    let color = if purple { hex(0xa77bff) } else { pal().accent };
    let (rect, resp) = area(ui, H, Sense::click_and_drag());
    if resp.dragged() || resp.drag_started() || resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let (x, y) = to_unit(rect, pos);
            let cell = ((x * SHAPE_POINTS as f64) as usize).min(SHAPE_POINTS - 1);
            levels.resize(SHAPE_POINTS, 0.5);
            levels[cell] = y;
        }
    }
    let p = ui.painter_at(rect);
    let last = levels.len().saturating_sub(1).max(1) as f64;
    line(
        &p,
        levels
            .iter()
            .enumerate()
            .map(|(i, v)| at(rect, i as f64 / last, *v))
            .collect(),
        color,
    );
    info(resp, "Shape — draw the notes' contour");
}

/// Strum: two breakpoints. Each moves one end of the chord.
pub fn strum(ui: &mut egui::Ui, t: &mut Strum) {
    let (rect, resp) = area(ui, H, Sense::click_and_drag());
    let (x_low, x_high) = (0.5 + t.low / 2.0, 0.5 + t.high / 2.0);
    let p = ui.painter_at(rect);
    let steps = 24;
    let points: Vec<Pos2> = (0..=steps)
        .map(|i| {
            let y = i as f64 / steps as f64;
            let shaped = curve_shape(y, -t.tension as f32);
            let x = x_low + (x_high - x_low) * shaped;
            at(rect, x, y)
        })
        .collect();
    line(&p, points, pal().accent);
    p.circle_filled(at(rect, x_low, 0.0), 3.5, pal().text);
    p.circle_filled(at(rect, x_high, 1.0), 3.5, pal().text);
    if resp.dragged() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let (x, y) = to_unit(rect, pos);
            let v = ((x - 0.5) * 2.0).clamp(-1.0, 1.0);
            if y < 0.5 {
                t.low = v;
            } else {
                t.high = v;
            }
        }
    }
    info(
        resp,
        "Strum Position — drag a breakpoint to move that end of the chord",
    );
}

/// Glissando: the yellow breakpoint is the start; the line's height is its curve.
pub fn glissando(ui: &mut egui::Ui, t: &mut Glissando) {
    let (rect, resp) = area(ui, H, Sense::click_and_drag());
    let p = ui.painter_at(rect);
    let start = t.start.clamp(0.0, 1.0);
    let steps = 24;
    let mut points = vec![at(rect, 0.0, 0.0)];
    points.extend((0..=steps).map(|i| {
        let x = i as f64 / steps as f64;
        at(
            rect,
            start + (1.0 - start) * x,
            curve_shape(x, t.curve as f32),
        )
    }));
    line(&p, points, pal().accent);
    p.circle_filled(at(rect, start, 0.0), 3.5, hex(0xe8c33a));
    if resp.dragged() {
        let d = resp.drag_delta();
        t.start = (t.start + f64::from(d.x / rect.width())).clamp(0.0, 1.0);
        t.curve = (t.curve + f64::from(d.y) * 0.012).clamp(-1.0, 1.0);
    }
    info(
        resp,
        "Glissando — drag across to set where the bend begins, up or down to curve it",
    );
}

/// The LFO's curve. It's only to look at.
pub fn lfo(ui: &mut egui::Ui, t: &Lfo) {
    let (rect, _) = area(ui, H, Sense::hover());
    let p = ui.painter_at(rect);
    line(
        &p,
        t.preview()
            .iter()
            .map(|(x, y)| at(rect, *x, (y + 1.0) / 2.0))
            .collect(),
        pal().accent,
    );
}

/// Time Warp's speed curve.
pub fn warp(ui: &mut egui::Ui, t: &mut TimeWarp, selected: &mut usize) {
    let (rect, resp) = area(ui, H, Sense::click_and_drag());
    let y_of = |speed: f64| 0.5 + (speed.log2() / 4.0).clamp(-0.5, 0.5);
    let speed_of = |y: f64| 2f64.powf((y - 0.5) * 4.0).clamp(0.25, 4.0);
    let mut sorted: Vec<(usize, f64, f64)> = t
        .points
        .iter()
        .enumerate()
        .filter(|(_, p)| p.on)
        .map(|(i, p)| (i, p.time, y_of(p.speed)))
        .collect();
    sorted.sort_by(|a, b| a.1.total_cmp(&b.1));
    let p = ui.painter_at(rect);
    p.line_segment(
        [at(rect, 0.0, 0.5), at(rect, 1.0, 0.5)],
        Stroke::new(1.0, shade(0x404040)),
    );
    let mut path = Vec::new();
    if let Some((_, _, y)) = sorted.first() {
        path.push(at(rect, 0.0, *y));
    }
    path.extend(sorted.iter().map(|(_, x, y)| at(rect, *x, *y)));
    if let Some((_, _, y)) = sorted.last() {
        path.push(at(rect, 1.0, *y));
    }
    line(&p, path, pal().accent);
    for (i, x, y) in &sorted {
        let c = if *i == *selected {
            pal().text
        } else {
            pal().text_dim
        };
        p.circle_filled(at(rect, *x, *y), 3.5, c);
    }
    if let Some(pos) = resp.interact_pointer_pos() {
        if resp.drag_started() || resp.clicked() {
            if let Some((i, _, _)) = sorted.iter().min_by(|a, b| {
                let d = |e: &(usize, f64, f64)| (at(rect, e.1, e.2) - pos).length();
                d(a).total_cmp(&d(b))
            }) {
                *selected = *i;
            }
        }
        if resp.dragged() {
            let (x, y) = to_unit(rect, pos);
            if let Some(p) = t.points.get_mut(*selected) {
                p.time = x;
                p.speed = speed_of(y);
            }
        }
    }
    info(resp, "Breakpoints — drag to set where and how fast");
}

/// The notes as columns, for Recombine's Dimension. Dragging across sets the Rotation Steps.
pub fn columns(ui: &mut egui::Ui, notes: &[NoteData], t: &mut Recombine, most: i32) {
    let (rect, resp) = area(ui, H, Sense::drag());
    if notes.is_empty() {
        return;
    }
    if resp.dragged() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let x = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            t.rotate = ((x * 2.0 - 1.0) * most as f32).round() as i32;
        }
    }
    let mut sorted = notes.to_vec();
    sorted.sort_by_key(|n| (n.start, n.key));
    let values: Vec<f64> = sorted
        .iter()
        .map(|n| match t.dimension {
            Dimension::Position => n.start as f64,
            Dimension::Pitch => f64::from(n.key),
            Dimension::Duration => n.length as f64,
            Dimension::Velocity => f64::from(n.velocity),
        })
        .collect();
    let (lo, hi) = values
        .iter()
        .fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
    let w = rect.width() / values.len() as f32;
    let p = ui.painter_at(rect);
    for (i, v) in values.iter().enumerate() {
        let h = if hi > lo {
            (v - lo) / (hi - lo) * 0.8 + 0.2
        } else {
            1.0
        };
        let left = rect.left() + i as f32 * w;
        let bar = Rect::from_min_max(
            Pos2::new(left + 1.0, rect.bottom() - h as f32 * rect.height()),
            Pos2::new(left + w - 1.0, rect.bottom()),
        );
        p.rect_filled(bar, 0.0, pal().accent);
    }
    info(resp, "Rotation Steps — drag across to rotate");
}

/// Which steps have a note.
pub fn steps(ui: &mut egui::Ui, on: &[bool]) {
    let (rect, _) = area(ui, 14.0, Sense::hover());
    if on.is_empty() {
        return;
    }
    let w = rect.width() / on.len() as f32;
    let p = ui.painter_at(rect);
    for (i, hit) in on.iter().enumerate() {
        let r = Rect::from_min_size(
            Pos2::new(rect.left() + i as f32 * w + 1.0, rect.top() + 2.0),
            Vec2::new(w - 2.0, rect.height() - 4.0),
        );
        p.rect_filled(r, 0.0, if *hit { pal().accent } else { shade(0x3a3a3a) });
    }
}

/// Euclidean's four voices, one row each.
pub fn euclid(ui: &mut egui::Ui, t: &Euclidean) {
    let (rect, resp) = area(ui, 44.0, Sense::hover());
    let steps = t.steps.max(1) as usize;
    let p = ui.painter_at(rect);
    let cell = Vec2::new(rect.width() / steps as f32, rect.height() / 4.0);
    for (row, v) in t.voices.iter().enumerate() {
        let pulses = v.pulses.min(t.steps);
        for s in 0..steps {
            let at_rotated = (s as i32 - v.rotation).rem_euclid(steps as i32) as usize;
            let hit = (at_rotated as u32 * pulses) % t.steps.max(1) < pulses;
            let r = Rect::from_min_size(
                rect.min + Vec2::new(s as f32 * cell.x + 1.0, row as f32 * cell.y + 1.0),
                cell - Vec2::splat(2.0),
            );
            let c = match (hit, v.on) {
                (true, true) => pal().accent,
                (true, false) => shade(0x6a6a6a),
                _ => shade(0x3a3a3a),
            };
            p.rect_filled(r, 0.0, c);
        }
    }
    info(resp, "Pattern — the voices across the steps");
}

/// A chord's pad: its notes on the Tonnetz. Returns whether it was clicked.
pub fn chord_pad(ui: &mut egui::Ui, def: &ChordDef, on: bool) -> bool {
    let size = Vec2::new(36.0, 30.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let fill = if on {
        pal().accent
    } else if resp.hovered() {
        pal().raised_hover
    } else {
        pal().raised
    };
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 1.0, fill);
    let places: Vec<(i32, i32)> = def.intervals.iter().map(|i| tonnetz(*i)).collect();
    let (min_a, max_a) = (
        places.iter().map(|x| x.0).min().unwrap_or(0),
        places.iter().map(|x| x.0).max().unwrap_or(0),
    );
    let (min_b, max_b) = (
        places.iter().map(|x| x.1).min().unwrap_or(0),
        places.iter().map(|x| x.1).max().unwrap_or(0),
    );
    let centre = rect.center();
    let x_of = |a: i32| centre.x + (a as f32 - (min_a + max_a) as f32 / 2.0) * 6.0;
    let y_of = |b: i32| centre.y - (b as f32 - (min_b + max_b) as f32 / 2.0) * 6.0;
    let dark = text_on(fill);
    let dots: Vec<Pos2> = places
        .iter()
        .map(|(a, b)| Pos2::new(x_of(*a), y_of(*b)))
        .collect();
    for pair in dots.windows(2) {
        p.line_segment([pair[0], pair[1]], Stroke::new(1.0, dark));
    }
    for d in &dots {
        p.circle_filled(*d, 2.0, dark);
    }
    let clicked = resp.clicked();
    info(resp, format!("Chord — {}", def.name));
    clicked
}
