//! Painting the note grid.

use super::{grid_step, is_black, Geo, Shown, UiState, Zone};
use crate::session::{ClipRef, Session};
use crate::ui::theme::*;
use egui::{Color32, Pos2, Rect, Shape, Stroke, StrokeKind, Vec2};
use rlive_core::{TimeSignature, PPQ};
use rlive_project::note_view::{spell_in_scale, ClipSpan};
use rlive_project::NoteData;

fn draw_chance_mark(p: &egui::Painter, r: Rect, kh: f32) {
    if kh < 8.0 {
        return;
    }
    let s = (kh * 0.5).min(7.0);
    p.add(Shape::convex_polygon(
        vec![
            r.left_top(),
            r.left_top() + Vec2::new(s, 0.0),
            r.left_top() + Vec2::new(0.0, s),
        ],
        shade(0x101010),
        Stroke::NONE,
    ));
}

#[allow(clippy::too_many_arguments)]
fn draw_note(
    p: &egui::Painter,
    geo: &Geo,
    n: &NoteData,
    t: f64,
    fill: Color32,
    outline: Option<Color32>,
) {
    let Some(r) = geo.note_rect(t, n.length, n.key) else {
        return;
    };
    let r = r.shrink2(Vec2::new(0.5, 0.5));
    if !r.intersects(geo.grid) {
        return;
    }
    p.rect_filled(r, 1.0, fill);
    p.rect_stroke(
        r,
        1.0,
        Stroke::new(1.0, outline.unwrap_or(Color32::from_black_alpha(170))),
        StrokeKind::Inside,
    );
    if n.probability < 1.0 || n.group.is_some() {
        draw_chance_mark(p, r, geo.kh);
    }
}

/// A note in the MPE tab: the selected ones are see-through and the rest are grey (manual p. 332).
fn mpe_fill(color: Color32, selected: bool) -> Color32 {
    if selected {
        color.gamma_multiply(0.35)
    } else {
        shade(0x4c4c4c)
    }
}

/// The notes a colour says: velocity as saturation, grey if deactivated or inactive.
fn note_fill(n: &NoteData, color: Color32, active: bool, selected: bool) -> Color32 {
    if selected {
        shade(0xb5e0ff)
    } else if n.muted {
        shade(0x555555)
    } else if !active {
        shade(0x5c5c5c)
    } else {
        color.gamma_multiply(0.45 + 0.55 * n.velocity)
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn grid(
    ui: &mut egui::Ui,
    geo: &Geo,
    session: &Session,
    state: &UiState,
    shown: &[Shown],
    fg: usize,
    sig: TimeSignature,
) {
    let p = ui.painter_at(geo.grid);
    let ps = &state.piano;
    p.rect_filled(geo.grid, 0.0, shade(0x2a2a2a));
    let clip = &shown[fg].clip;
    let scale = Session::pitch_scale(clip);
    let first = ((geo.scroll.y / geo.kh).floor() as usize).min(geo.rows.len());
    let last = (((geo.scroll.y + geo.grid.height()) / geo.kh).ceil() as usize).min(geo.rows.len());
    for row in first..last {
        let key = geo.rows[row];
        let y = geo.y(row);
        let r = Rect::from_min_size(
            Pos2::new(geo.grid.left(), y),
            Vec2::new(geo.grid.width(), geo.kh),
        );
        if ps.highlight && scale.is_some_and(|s| spell_in_scale(key, s)) {
            p.rect_filled(r, 0.0, pal().accent.gamma_multiply(0.12));
        } else if is_black(key) {
            p.rect_filled(r, 0.0, shade(0x242424));
        }
        if key.is_multiple_of(12) {
            p.line_segment(
                [r.left_bottom(), r.right_bottom()],
                Stroke::new(1.0, shade(0x171717)),
            );
        }
    }
    // Columns.
    let bar = sig.ticks_per_bar();
    let step = if ps.fixed_grid {
        ps.grid.max(1)
    } else {
        grid_step(ps.grid, geo.ppb)
    };
    let mut t = ((geo.scroll.x / geo.ppb * PPQ as f32) as u64 / step) * step;
    while geo.x(t as f64) <= geo.grid.right() {
        let x = geo.x(t as f64);
        let c = if t.is_multiple_of(bar) {
            pal().grid_line(shade(0x0f0f0f))
        } else if t.is_multiple_of(PPQ) {
            pal().grid_line(shade(0x1c1c1c))
        } else {
            pal().grid_line(shade(0x242424))
        };
        p.line_segment(
            [
                Pos2::new(x, geo.grid.top()),
                Pos2::new(x, geo.grid.bottom()),
            ],
            Stroke::new(1.0, c),
        );
        t += step;
    }
    // What doesn't play is dimmed.
    if shown.len() == 1 {
        let (from, to) = if clip.looped {
            (clip.start.min(clip.loop_start), clip.loop_end())
        } else {
            (clip.start, clip.end_marker())
        };
        for (a, b) in [
            (geo.grid.left(), geo.x(from as f64)),
            (geo.x(to as f64), geo.grid.right()),
        ] {
            if b > a {
                p.rect_filled(
                    Rect::from_x_y_ranges(
                        a.max(geo.grid.left())..=b.min(geo.grid.right()),
                        geo.grid.y_range(),
                    ),
                    0.0,
                    Color32::from_black_alpha(100),
                );
            }
        }
    }
    // The time selection.
    if let Some((a, b)) = ps.time {
        p.rect_filled(
            Rect::from_x_y_ranges(geo.x(a as f64)..=geo.x(b as f64), geo.grid.y_range()),
            0.0,
            Color32::from_rgba_unmultiplied(0xb5, 0xe0, 0xff, 32),
        );
    }
    let focused = ps.focused();
    let mpe_tab = state.clip_view.mode == crate::ui::clip_view::EditorMode::Mpe;
    let hovered_note = ps.bar_hover;
    let hover_group = ps
        .lanes
        .hover_note
        .and_then(|i| clip.notes().get(i))
        .and_then(|n| n.group.map(|g| g.id));
    let mut at = Vec::new();
    // Inactive clips first, so the foreground is on top.
    for (i, s) in shown.iter().enumerate() {
        let active = i == fg || !focused || hovered_note == Some(s.r);
        if i == fg {
            continue;
        }
        let chosen = ps.others.get(&s.r);
        for (ni, n) in s.clip.notes().iter().enumerate() {
            s.span.occurrences(n.start, &mut at);
            let selected = chosen.is_some_and(|c| c.contains(&ni));
            let fill = if mpe_tab {
                mpe_fill(s.color, selected)
            } else {
                note_fill(n, s.color, active, selected)
            };
            for t in &at {
                draw_note(&p, geo, n, *t, fill, None);
            }
        }
    }
    let s = &shown[fg];
    for (ni, n) in s.clip.notes().iter().enumerate() {
        s.span.occurrences(n.start, &mut at);
        let selected = ps.selected.contains(&ni);
        let fill = if mpe_tab {
            mpe_fill(s.color, selected)
        } else {
            note_fill(n, s.color, true, selected)
        };
        let outline = n
            .group
            .filter(|g| hover_group == Some(g.id))
            .map(|_| pal().text);
        for t in &at {
            draw_note(&p, geo, n, *t, fill, outline);
        }
    }
    // Insert marker.
    if let Some(t) = ps.insert_tick {
        let x = geo.x(t as f64);
        p.line_segment(
            [
                Pos2::new(x, geo.grid.top()),
                Pos2::new(x, geo.grid.bottom()),
            ],
            Stroke::new(1.0, pal().text),
        );
    }
    // Playhead.
    if let Some(at_tick) = session.clip_playhead(s.r) {
        let x = match s.span {
            ClipSpan::Content => geo.x(at_tick),
            ClipSpan::Placed { .. } => {
                let mut times = Vec::new();
                s.span.occurrences(at_tick as u64, &mut times);
                geo.x(times.first().copied().unwrap_or(0.0))
            }
        };
        p.line_segment(
            [
                Pos2::new(x, geo.grid.top()),
                Pos2::new(x, geo.grid.bottom()),
            ],
            Stroke::new(1.0, pal().play_green),
        );
    }
}

/// A note and its part under the pointer: which clip, which note, where.
pub(super) fn hit(
    geo: &Geo,
    shown: &[Shown],
    fg: usize,
    focused: bool,
    pos: Pos2,
) -> Option<(usize, usize, Zone)> {
    let mut at = Vec::new();
    let order = std::iter::once(fg).chain((0..shown.len()).filter(|i| *i != fg && !focused));
    for i in order {
        let s = &shown[i];
        for (ni, n) in s.clip.notes().iter().enumerate().rev() {
            s.span.occurrences(n.start, &mut at);
            for t in &at {
                let Some(r) = geo.note_rect(*t, n.length, n.key) else {
                    continue;
                };
                if r.contains(pos) {
                    return Some((i, ni, super::zone(r, pos.x)));
                }
            }
        }
    }
    None
}

/// A clip that shows at `t` on the track of `foreground`, when drawing crosses clip boundaries.
pub(super) fn clip_under(shown: &[Shown], foreground: ClipRef, t: f64) -> Option<&Shown> {
    let same_track = |s: &&Shown| s.r.track() == foreground.track();
    shown.iter().filter(same_track).find(|s| s.span.contains(t))
}
