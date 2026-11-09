//! Which clips the editor shows, how they line up, and their loop bars (manual pp. 289-292).

use super::{Geo, UiState};
use crate::session::{ClipRef, Session};
use crate::ui::clip_view;
use crate::ui::theme::*;
use crate::ui::widgets::{info, rgb};
use egui::{Color32, Pos2, Rect, Sense, Stroke};
use rlive_project::clip_edit as edit;
use rlive_project::note_view::{pick_shown, realign_span, ClipSpan, MAX_SHOWN};
use rlive_project::Clip;

/// One clip in the editor.
pub struct Shown {
    pub r: ClipRef,
    pub clip: Clip,
    pub span: ClipSpan,
    pub color: Color32,
}

fn single(session: &Session, cref: ClipRef) -> Vec<Shown> {
    session
        .clip_at(cref)
        .map(|clip| Shown {
            r: cref,
            clip: clip.clone(),
            span: ClipSpan::Content,
            color: rgb(clip.color),
        })
        .into_iter()
        .collect()
}

/// The clips shown, and the stretch of timeline they cover.
pub fn gather(session: &Session, state: &UiState, cref: ClipRef) -> (Vec<Shown>, (u64, u64)) {
    let group = clip_view::targets(session, state, cref);
    let midi: Vec<ClipRef> = group
        .into_iter()
        .filter(|r| session.clip_at(*r).is_some_and(Clip::is_midi))
        .collect();
    if midi.len() < 2 {
        let list = single(session, cref);
        let limit = list.first().map_or(0, |s| super::content_limit(&s.clip));
        return (list, (0, limit));
    }
    match cref {
        ClipRef::Slot { .. } => gather_session(session, cref, midi),
        ClipRef::Arr { .. } => gather_arrangement(session, cref, midi),
    }
}

/// Session clips must all be looped. In order of track, then scene.
fn gather_session(
    session: &Session,
    cref: ClipRef,
    midi: Vec<ClipRef>,
) -> (Vec<Shown>, (u64, u64)) {
    let looped: Vec<ClipRef> = midi
        .into_iter()
        .filter(|r| session.clip_at(*r).is_some_and(|c| c.looped))
        .collect();
    if !looped.contains(&cref) || looped.len() < 2 {
        let list = single(session, cref);
        let limit = list.first().map_or(0, |s| super::content_limit(&s.clip));
        return (list, (0, limit));
    }
    let mut ordered = looped;
    ordered.sort_by_key(|r| {
        (
            session.project.track_index(r.track()).unwrap_or(usize::MAX),
            r.as_slot().map_or(0, |(_, slot)| slot),
        )
    });
    let chosen = pick_shown(&ordered, cref);
    let clips: Vec<(ClipRef, Clip)> = chosen
        .iter()
        .filter_map(|r| Some((*r, session.clip_at(*r)?.clone())))
        .collect();
    let lead = clips
        .iter()
        .map(|(_, c)| c.region().lead().max(0.0) as u64)
        .max()
        .unwrap_or(0);
    let lengths: Vec<u64> = clips.iter().map(|(_, c)| c.length).collect();
    let end = lead + realign_span(&lengths);
    let list = clips
        .into_iter()
        .map(|(r, clip)| Shown {
            r,
            span: ClipSpan::Placed {
                region: clip.region(),
                origin: 0.0,
                from: 0.0,
                to: end as f64,
            },
            color: rgb(clip.color),
            clip,
        })
        .collect();
    (list, (0, end))
}

/// Timeline clips from up to eight tracks; the timeline is song time, first clip to last.
fn gather_arrangement(
    session: &Session,
    cref: ClipRef,
    midi: Vec<ClipRef>,
) -> (Vec<Shown>, (u64, u64)) {
    let mut tracks: Vec<rlive_core::TrackId> = Vec::new();
    for r in &midi {
        if !tracks.contains(&r.track()) {
            tracks.push(r.track());
        }
    }
    tracks.sort_by_key(|t| session.project.track_index(*t).unwrap_or(usize::MAX));
    tracks.truncate(MAX_SHOWN);
    let mut items: Vec<(usize, u64, Shown)> = Vec::new();
    for r in midi.into_iter().filter(|r| tracks.contains(&r.track())) {
        let ClipRef::Arr { track, id } = r else {
            continue;
        };
        let Some(a) = session.project.track(track).and_then(|t| t.arr_clip(id)) else {
            continue;
        };
        items.push((
            session.project.track_index(track).unwrap_or(usize::MAX),
            a.start,
            Shown {
                r,
                span: ClipSpan::Placed {
                    region: a.clip.region(),
                    origin: a.start as f64 - a.run_offset() as f64,
                    from: a.start as f64,
                    to: a.end() as f64,
                },
                color: rgb(a.clip.color),
                clip: a.clip.clone(),
            },
        ));
    }
    if !items.iter().any(|(_, _, s)| s.r == cref) {
        let list = single(session, cref);
        let limit = list.first().map_or(0, |s| super::content_limit(&s.clip));
        return (list, (0, limit));
    }
    items.sort_by_key(|(track, start, _)| (*track, *start));
    let from = items
        .iter()
        .map(|(_, _, s)| match s.span {
            ClipSpan::Placed { from, .. } => from as u64,
            ClipSpan::Content => 0,
        })
        .min()
        .unwrap_or(0);
    let to = items
        .iter()
        .map(|(_, _, s)| match s.span {
            ClipSpan::Placed { to, .. } => to as u64,
            ClipSpan::Content => 0,
        })
        .max()
        .unwrap_or(0);
    (items.into_iter().map(|(_, _, s)| s).collect(), (from, to))
}

/// Makes `r` the foreground clip, keeping what was selected in the others.
pub fn set_foreground(state: &mut UiState, r: ClipRef) {
    let Some(old) = state.selected_clip.filter(|o| *o != r) else {
        return;
    };
    let ps = &mut state.piano;
    let was = std::mem::take(&mut ps.selected);
    if !was.is_empty() {
        ps.others.insert(old, was);
    }
    ps.selected = ps.others.remove(&r).unwrap_or_default();
    ps.foreground = Some(r);
    state.selected_clip = Some(r);
}

/// One loop bar per clip: where it loops, its colour, and a grip to resize its loop.
#[allow(clippy::too_many_arguments)]
pub fn loop_bars(
    ui: &mut egui::Ui,
    rect: Rect,
    geo: &Geo,
    session: &mut Session,
    state: &mut UiState,
    shown: &[Shown],
    fg: usize,
    span: (u64, u64),
) {
    let h = state.piano.loop_bar_h;
    let focused = state.piano.focused();
    let p = ui.painter_at(rect);
    state.piano.bar_hover = None;
    let snap = state.piano.grid;
    for (i, s) in shown.iter().enumerate() {
        let bar = Rect::from_min_max(
            Pos2::new(rect.left(), rect.top() + i as f32 * h),
            Pos2::new(rect.right(), rect.top() + (i + 1) as f32 * h - 1.0),
        );
        let active = i == fg;
        let picked = state.piano.bars.contains(&s.r);
        let color = if active || !focused {
            s.color
        } else {
            shade(0x6a6a6a)
        };
        p.rect_filled(bar, 0.0, pal().panel_dark);
        let span_range = match s.span {
            ClipSpan::Placed { from, to, .. } => (from, to),
            ClipSpan::Content => (span.0 as f64, span.1 as f64),
        };
        // The timeline that falls outside this clip's loop is darker.
        p.rect_filled(
            Rect::from_x_y_ranges(
                geo.x(span_range.0).max(bar.left())..=geo.x(span_range.1).min(bar.right()),
                bar.y_range(),
            ),
            0.0,
            color.gamma_multiply(0.35),
        );
        let (start, loop_len) = (s.clip.loop_start as f64, s.clip.length as f64);
        let run = |content: f64| match s.span {
            ClipSpan::Placed { origin, region, .. } => {
                origin + region.run_of(content).unwrap_or(0.0)
            }
            ClipSpan::Content => content,
        };
        let (a, b) = (geo.x(run(start)), geo.x(run(start) + loop_len));
        let brace =
            Rect::from_x_y_ranges(a.max(bar.left())..=b.min(bar.right()).max(a), bar.y_range());
        let fill = if picked { shade(0x101010) } else { color };
        p.rect_filled(brace, 0.0, fill);
        if active {
            p.rect_stroke(
                bar,
                0.0,
                Stroke::new(1.0, pal().text),
                egui::StrokeKind::Inside,
            );
        }
        let resp = ui.interact(
            bar,
            ui.scope_id().with(("loop_bar", i)),
            Sense::click_and_drag(),
        );
        if resp.hovered() {
            state.piano.bar_hover = Some(s.r);
        }
        let (cmd, shift) = ui.input(|i| (i.modifiers.command, i.modifiers.shift));
        if resp.clicked() {
            let bars = &mut state.piano.bars;
            if focused {
                // In Focus Mode only the one clip's loop bar can be selected.
                *bars = vec![s.r];
            } else if cmd {
                if let Some(at) = bars.iter().position(|b| *b == s.r) {
                    bars.remove(at);
                } else {
                    bars.push(s.r);
                }
            } else if shift {
                let (lo, hi) = (fg.min(i), fg.max(i));
                *bars = shown[lo..=hi].iter().map(|x| x.r).collect();
            } else {
                bars.clear();
                bars.push(s.r);
            }
            set_foreground(state, s.r);
        }
        if resp.dragged() {
            let delta = (resp.drag_delta().x / geo.ppb) * rlive_core::PPQ as f32;
            if delta != 0.0 {
                let targets = if state.piano.bars.contains(&s.r) && !focused {
                    state.piano.bars.clone()
                } else {
                    vec![s.r]
                };
                let by = delta.round() as i64;
                session.edit_clips(&targets, "Loop Length", |c| {
                    let wanted = (c.length as i64 + by).max(snap as i64) as u64;
                    edit::set_loop_length(c, wanted);
                });
            }
        }
        resp.context_menu(|ui| {
            if ui.button("Duplicate Loop").clicked() {
                let list = if state.piano.bars.is_empty() {
                    vec![s.r]
                } else {
                    state.piano.bars.clone()
                };
                for r in list {
                    session.duplicate_loop(r);
                }
                ui.close();
            }
        });
        info(
            resp,
            "Loop Bar — click to edit this clip; drag to change its loop length; Cmd and Shift \
             select several",
        );
    }
    // The height of the bars, by the sliver above them.
    let grip = Rect::from_min_size(
        rect.min - egui::vec2(0.0, 4.0),
        egui::vec2(rect.width(), 4.0),
    );
    let resp = ui.interact(grip, ui.scope_id().with("loop_bar_grip"), Sense::drag());
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
    }
    if resp.dragged() {
        let n = shown.len().max(1) as f32;
        state.piano.loop_bar_h =
            (state.piano.loop_bar_h + resp.drag_delta().y / n).clamp(6.0, 30.0);
    }
}
