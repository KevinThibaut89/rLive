//! The Track Status field above the mixer: what a track is doing right now (manual p. 194).

use super::icons;
use super::theme::{self, *};
use super::{Detail, UiState};
use crate::session::{ClipRef, Session};
use egui::{Align2, Pos2, Rect, Stroke, Vec2};
use rlive_core::TrackId;
use rlive_engine::shared::NO_SLOT;
use rlive_project::{Monitor, TrackKind};
use std::f32::consts::{FRAC_PI_2, TAU};
use std::sync::atomic::Ordering;

/// What the field shows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Status {
    Idle,
    /// A looping clip: how many times round, how many beats in a loop, and how far through.
    Loop {
        passes: u32,
        beats: f64,
        frac: f32,
    },
    /// A one-shot clip: seconds left, and how far through.
    OneShot {
        seconds: f64,
        frac: f32,
    },
    /// A group with something playing in it.
    GroupPie,
    /// The track is monitoring its input.
    Monitor {
        midi: bool,
    },
    Arrangement,
}

/// `m:ss`, rounded up so the last second reads 0:01 until it's over.
pub fn format_remaining(seconds: f64) -> String {
    let s = seconds.max(0.0).ceil() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// What the field says given what the track is playing.
pub fn clip_status(
    looped: bool,
    length_ticks: f64,
    elapsed: f64,
    ticks_per_second: f64,
    beat_ticks: f64,
) -> Status {
    let len = length_ticks.max(1.0);
    if looped {
        Status::Loop {
            passes: (elapsed / len).floor() as u32 + 1,
            beats: len / beat_ticks.max(1.0),
            frac: (elapsed % len / len) as f32,
        }
    } else {
        Status::OneShot {
            seconds: (len - elapsed).max(0.0) / ticks_per_second.max(1.0),
            frac: (elapsed / len).min(1.0) as f32,
        }
    }
}

fn status_of(session: &Session, id: TrackId) -> Status {
    let Some(t) = session.project.track(id) else {
        return Status::Idle;
    };
    if t.kind == TrackKind::Group {
        let playing = session.project.group_contents(id).iter().any(|c| {
            session
                .track_shared
                .get(c)
                .is_some_and(|s| s.playing_slot.load(Ordering::Relaxed) != NO_SLOT)
        });
        return if playing {
            Status::GroupPie
        } else {
            Status::Idle
        };
    }
    let Some(shared) = session.track_shared.get(&id) else {
        return Status::Idle;
    };
    let slot = shared.playing_slot.load(Ordering::Relaxed);
    let monitoring =
        t.routing.monitor == Monitor::In || (t.routing.monitor == Monitor::Auto && t.arm);
    if slot != NO_SLOT {
        if let Some(c) = session.clip(id, slot as usize) {
            let tempo = session.engine.shared.tempo.load();
            let sig = session.project.time_signature;
            return clip_status(
                c.looped,
                c.length as f64,
                shared.clip_elapsed.load(),
                tempo / 60.0 * rlive_core::PPQ as f64,
                sig.ticks_per_beat() as f64,
            );
        }
    }
    if monitoring && matches!(t.kind, TrackKind::Audio | TrackKind::Midi) {
        return Status::Monitor {
            midi: t.kind == TrackKind::Midi,
        };
    }
    if !t.arrangement.is_empty()
        && shared.follow_arrangement.load(Ordering::Relaxed)
        && session.is_playing()
    {
        return Status::Arrangement;
    }
    Status::Idle
}

fn pie(p: &egui::Painter, rect: Rect, frac: f32, color: egui::Color32) {
    let c = rect.center();
    let r = rect.height().min(rect.width()) * 0.42;
    p.circle_stroke(c, r, Stroke::new(1.0, color));
    let n = 24;
    let end = -FRAC_PI_2 + frac.clamp(0.0, 1.0) * TAU;
    let mut points = vec![c];
    for i in 0..=n {
        let a = -FRAC_PI_2 + (end + FRAC_PI_2) * i as f32 / n as f32;
        points.push(c + Vec2::angled(a) * r);
    }
    p.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
}

fn mini_arrangement(p: &egui::Painter, session: &Session, id: TrackId, rect: Rect) {
    let Some(t) = session.project.track(id) else {
        return;
    };
    let tick = session.engine.shared.tick.load();
    // Eight bars around the playhead, the playhead a quarter of the way in.
    let span = session.project.time_signature.ticks_per_bar() as f64 * 8.0;
    let from = tick - span * 0.25;
    let x_of = |t: f64| rect.left() + ((t - from) / span) as f32 * rect.width();
    p.rect_filled(rect, 0.0, pal().field_bg);
    for c in &t.arrangement {
        let (a, b) = (c.start as f64, c.end() as f64);
        if b < from || a > from + span {
            continue;
        }
        let r = Rect::from_min_max(
            Pos2::new(x_of(a).max(rect.left()), rect.top() + 2.0),
            Pos2::new(x_of(b).min(rect.right()), rect.bottom() - 2.0),
        );
        p.rect_filled(r, 0.0, super::widgets::rgb(c.clip.color));
    }
    let px = x_of(tick);
    p.line_segment(
        [Pos2::new(px, rect.top()), Pos2::new(px, rect.bottom())],
        Stroke::new(1.0, pal().text),
    );
}

/// Draws the field in `rect`. Clicking it opens the clip it's showing.
pub fn show(ui: &mut egui::Ui, session: &Session, state: &mut UiState, id: TrackId, rect: Rect) {
    let status = status_of(session, id);
    let resp = ui.allocate_rect(rect, egui::Sense::click());
    let p = ui.painter().clone();
    let dim = pal().text_dim;
    let font = theme::font(10.0);
    match status {
        Status::Idle => {}
        Status::Loop {
            passes,
            beats,
            frac,
        } => {
            let icon = Rect::from_center_size(rect.center(), Vec2::splat(rect.height()));
            pie(&p, icon, frac, pal().play_green);
            p.text(
                Pos2::new(icon.left() - 2.0, rect.center().y),
                Align2::RIGHT_CENTER,
                passes.to_string(),
                font.clone(),
                dim,
            );
            let beats = if beats.fract().abs() < 1e-6 {
                format!("{beats:.0}")
            } else {
                format!("{beats:.2}")
            };
            p.text(
                Pos2::new(icon.right() + 2.0, rect.center().y),
                Align2::LEFT_CENTER,
                beats,
                font,
                dim,
            );
        }
        Status::OneShot { seconds, frac } => {
            let bar = Rect::from_center_size(
                rect.center() + Vec2::new(-14.0, 0.0),
                Vec2::new(rect.width() * 0.4, 6.0),
            );
            p.rect_filled(bar, 0.0, pal().field_bg);
            p.rect_filled(
                Rect::from_min_size(bar.min, Vec2::new(bar.width() * frac, bar.height())),
                0.0,
                pal().play_green,
            );
            p.text(
                Pos2::new(bar.right() + 4.0, rect.center().y),
                Align2::LEFT_CENTER,
                format_remaining(seconds),
                font,
                dim,
            );
        }
        Status::GroupPie => {
            let icon = Rect::from_center_size(rect.center(), Vec2::splat(rect.height()));
            pie(&p, icon, 1.0, pal().play_green);
        }
        Status::Monitor { midi } => {
            let icon = Rect::from_center_size(rect.center(), Vec2::splat(rect.height()));
            if midi {
                icons::keyboard(&p, icon, dim);
            } else {
                icons::microphone(&p, icon, dim);
            }
        }
        Status::Arrangement => mini_arrangement(&p, session, id, rect.shrink2(Vec2::new(2.0, 1.0))),
    }
    let playing = session
        .track_shared
        .get(&id)
        .map_or(NO_SLOT, |s| s.playing_slot.load(Ordering::Relaxed));
    if resp.clicked() && playing != NO_SLOT {
        state.selected_clip = Some(ClipRef::slot(id, playing as usize));
        state.detail = Detail::Clip;
        state.show_detail = true;
    }
    super::widgets::info(
        resp,
        "Track Status — what this track is doing; click to open its clip",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_loop_shows_its_passes_and_length_in_beats() {
        let Status::Loop {
            passes,
            beats,
            frac,
        } = clip_status(true, 3840.0, 8640.0, 1920.0, 960.0)
        else {
            panic!()
        };
        assert_eq!(passes, 3);
        assert_eq!(beats, 4.0);
        assert_eq!(frac, 0.25);
    }

    #[test]
    fn a_one_shot_shows_what_is_left() {
        let Status::OneShot { seconds, frac } = clip_status(false, 3840.0, 1920.0, 1920.0, 960.0)
        else {
            panic!()
        };
        assert_eq!(seconds, 1.0);
        assert_eq!(frac, 0.5);
    }

    #[test]
    fn remaining_time_reads_minutes_and_seconds() {
        assert_eq!(format_remaining(61.0), "1:01");
        assert_eq!(format_remaining(0.2), "0:01");
        assert_eq!(format_remaining(0.0), "0:00");
    }
}
