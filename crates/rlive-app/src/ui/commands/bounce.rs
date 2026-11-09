//! Bounce, Freeze and Consolidate, from the menus and the keyboard.

use super::{Entry, Entry::Item, MenuCommand as M, MenuState};
use crate::session::{BounceSource, ChainRef, ClipRef, PasteTarget, Session};
use crate::ui::session_select::selected_slots;
use crate::ui::{theme, MainView, RliveApp, UiState};
use egui::{Color32, Painter, Pos2, Rect, Stroke, Vec2};
use rlive_core::TrackId;

super::domain! {
    /// Committing material to audio.
    Bounce {
        ToNewTrack,
        InPlace,
        PasteBounced,
        Freeze,
        Flatten,
        Consolidate,
        ConsolidateToScene,
    }
}

pub const EDIT_ITEMS: [Entry; 6] = [
    Item("Freeze Track", M::Bounce(Bounce::Freeze)),
    Item("Flatten Track", M::Bounce(Bounce::Flatten)),
    Item("Bounce Track in Place", M::Bounce(Bounce::InPlace)),
    Item("Bounce to New Track", M::Bounce(Bounce::ToNewTrack)),
    Item("Paste Bounced Audio", M::Bounce(Bounce::PasteBounced)),
    Item("Consolidate", M::Bounce(Bounce::Consolidate)),
];

pub const CONSOLIDATE_TO_SCENE: Entry = Item(
    "Consolidate Time to New Scene",
    M::Bounce(Bounce::ConsolidateToScene),
);

/// What the current selection would bounce, in the view on show.
pub fn source_of(session: &Session, state: &UiState) -> Option<BounceSource> {
    if state.main_view == MainView::Session {
        if let Some(slot) = state.group_slot {
            return Some(BounceSource::Slots(vec![slot]));
        }
        let slots = selected_slots(session, state);
        return (!slots.is_empty()).then_some(BounceSource::Slots(slots));
    }
    if let Some((start, end)) = selected_span(state) {
        return Some(BounceSource::Span {
            tracks: state.arr.selected_tracks(),
            start,
            end,
        });
    }
    (!state.arr.selection.is_empty()).then(|| BounceSource::Clips(state.arr.selection.clone()))
}

/// A span of the Arrangement selected as time rather than as clips.
fn selected_span(state: &UiState) -> Option<(u64, u64)> {
    state
        .arr
        .time
        .as_ref()
        .filter(|t| !t.is_point() && state.arr.selection.is_empty())
        .map(|t| t.ticks())
}

/// What the selection would consolidate: its tracks, and the time it covers.
fn consolidate_range(
    session: &Session,
    state: &UiState,
) -> Option<(Vec<crate::session::LaneSel>, u64, u64)> {
    if let Some((start, end)) = selected_span(state) {
        return Some((state.arr.selected_lanes(session), start, end));
    }
    let clips: Vec<(TrackId, u64, u64)> = state
        .arr
        .selection
        .iter()
        .filter_map(|(t, id)| {
            let c = session.project.track(*t)?.arr_clip(*id)?;
            Some((*t, c.start, c.end()))
        })
        .collect();
    let start = clips.iter().map(|c| c.1).min()?;
    let end = clips.iter().map(|c| c.2).max()?;
    // The clips may be on a take lane: consolidating happens there.
    let lanes = state.arr.selected_lanes(session);
    Some((lanes, start, end))
}

fn paste_target(session: &Session, state: &UiState) -> Option<PasteTarget> {
    let Some(ChainRef::Track(track)) = state.selected else {
        return None;
    };
    let at = session.insert_marker() as u64;
    // With the insert marker on a take lane, the audio goes there.
    let lane = state
        .arr
        .time
        .as_ref()
        .filter(|t| t.is_point())
        .and_then(|t| t.lanes.first())
        .and_then(|k| k.take);
    Some(match lane {
        Some(lane) => PasteTarget::TakeLane { track, lane, at },
        None => PasteTarget::Main { track, at },
    })
}

fn report(state: &mut UiState, result: Result<(), String>) {
    if let Err(e) = result {
        state.status = e;
    }
}

/// Starts freezing `id`, or unfreezes it.
fn toggle_freeze(session: &mut Session, state: &mut UiState, id: TrackId) {
    if session.is_frozen(id) {
        session.unfreeze_track(id);
    } else {
        report(state, session.freeze_track(id));
    }
}

/// Freeze entries for a track's title bar or a clip's menu, Bounce in Place below them.
pub fn track_menu_items(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    id: TrackId,
) {
    let frozen = session.is_frozen(id);
    let group = session.project.track(id).is_some_and(|t| t.is_group());
    if !group {
        let label = if frozen {
            "Unfreeze Track"
        } else {
            "Freeze Track"
        };
        if ui
            .add_enabled(frozen || session.can_freeze(id), egui::Button::new(label))
            .clicked()
        {
            toggle_freeze(session, state, id);
            ui.close();
        }
        if ui
            .add_enabled(frozen, egui::Button::new("Flatten Track"))
            .clicked()
        {
            report(state, session.flatten_track(id));
            ui.close();
        }
    }
    let label = if group {
        "Bounce Group in Place"
    } else {
        "Bounce Track in Place"
    };
    if ui
        .add_enabled(session.can_bounce_track(id), egui::Button::new(label))
        .clicked()
    {
        report(state, session.bounce_in_place(id));
        ui.close();
    }
}

fn paste_item(ui: &mut egui::Ui, session: &mut Session, state: &mut UiState) {
    let target = paste_target(session, state);
    let can = target.is_some_and(|t| session.can_paste_bounced(t))
        && state
            .bounce_copy
            .as_ref()
            .is_some_and(|s| session.bounce_source_exists(s));
    if ui
        .add_enabled(can, egui::Button::new("Paste Bounced Audio"))
        .clicked()
    {
        if let (Some(source), Some(target)) = (state.bounce_copy.clone(), target) {
            report(state, session.paste_bounced(&source, target));
        }
        ui.close();
    }
}

/// Bounce entries for a Session clip's menu.
pub fn slot_menu_items(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    id: TrackId,
    slot: usize,
) {
    if ui.button("Bounce to New Track   ⌘B").clicked() {
        let mut slots = selected_slots(session, state);
        if !slots.contains(&(id, slot)) {
            slots = vec![(id, slot)];
        }
        report(
            state,
            session.bounce_to_new_track(BounceSource::Slots(slots)),
        );
        ui.close();
    }
    frozen_clip_item(ui, session, state, ClipRef::slot(id, slot), None);
}

/// A group slot's menu: Bounce Group to New Track.
pub fn group_slot_menu_items(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    id: TrackId,
    slot: usize,
) {
    if ui.button("Bounce Group to New Track   ⌘B").clicked() {
        report(
            state,
            session.bounce_to_new_track(BounceSource::Slots(vec![(id, slot)])),
        );
        ui.close();
    }
}

/// Bounce and Consolidate entries for the Arrangement's context menu.
pub fn arrangement_menu_items(ui: &mut egui::Ui, session: &mut Session, state: &mut UiState) {
    let range = consolidate_range(session, state);
    let source = source_of(session, state);
    let can_consolidate = range
        .as_ref()
        .is_some_and(|(t, a, b)| session.can_consolidate_lanes(t, *a, *b));
    if ui
        .add_enabled(can_consolidate, egui::Button::new("Consolidate   ⌘J"))
        .clicked()
    {
        if let Some((tracks, start, end)) = &range {
            report(state, session.consolidate_lanes(tracks, *start, *end));
        }
        ui.close();
    }
    let span = selected_span(state);
    if ui
        .add_enabled(
            span.is_some(),
            egui::Button::new("Consolidate Time to New Scene"),
        )
        .clicked()
    {
        if let Some((start, end)) = span {
            let below = scene_below(state);
            report(
                state,
                session.consolidate_time_to_new_scene(below, start, end),
            );
        }
        ui.close();
    }
    if ui
        .add_enabled(
            source.is_some(),
            egui::Button::new("Bounce to New Track   ⌘B"),
        )
        .clicked()
    {
        if let Some(source) = source {
            report(state, session.bounce_to_new_track(source));
        }
        ui.close();
    }
    paste_item(ui, session, state);
    if let Some((track, id)) = state.arr.context {
        frozen_clip_item(ui, session, state, ClipRef::Arr { track, id }, span);
    }
    ui.separator();
}

/// A frozen clip's way to a new frozen track, standing in for dragging it to the drop area.
fn frozen_clip_item(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    clip: ClipRef,
    span: Option<(u64, u64)>,
) {
    if !session.is_frozen(clip.track()) {
        return;
    }
    if ui.button("Frozen Clip to New Track").clicked() {
        report(
            state,
            session.frozen_clip_to_new_track(clip, span).map(|_| ()),
        );
        ui.close();
    }
}

/// The scene a new one goes below: the selected scene, or the last.
fn scene_below(state: &UiState) -> usize {
    state.sel.scenes.first().copied().unwrap_or(0)
}

/// A frozen clip's mark: a glaze over its body, for a clip or Session slot.
pub fn frozen_glaze(p: &Painter, rect: Rect) {
    p.rect_filled(rect, 1.0, ice().gamma_multiply(0.22));
}

/// Crosshatching across a frozen tail.
pub fn frozen_hatch(p: &Painter, rect: Rect) {
    let color = ice().gamma_multiply(0.45);
    let h = rect.height();
    let mut x = rect.left() - h;
    while x < rect.right() {
        for (from, to) in [
            (Pos2::new(x, rect.bottom()), Pos2::new(x + h, rect.top())),
            (Pos2::new(x, rect.top()), Pos2::new(x + h, rect.bottom())),
        ] {
            let (a, b) = (clamp_x(from, to, rect), clamp_x(to, from, rect));
            if let (Some(a), Some(b)) = (a, b) {
                p.line_segment([a, b], Stroke::new(1.0, color));
            }
        }
        x += 10.0;
    }
}

/// The point on the segment from `from` to `to` that is at `from`'s end of `rect` horizontally.
fn clamp_x(from: Pos2, to: Pos2, rect: Rect) -> Option<Pos2> {
    if (to.x - from.x).abs() < f32::EPSILON {
        return rect.contains(from).then_some(from);
    }
    let x = from.x.clamp(rect.left(), rect.right());
    let share = (x - from.x) / (to.x - from.x);
    let point = Pos2::new(x, from.y + (to.y - from.y) * share);
    (point.y >= rect.top() - 0.5 && point.y <= rect.bottom() + 0.5).then_some(point)
}

fn ice() -> Color32 {
    theme::shade(0x9fd4ff)
}

/// A small "Frozen" tag on a track's header.
pub fn frozen_badge(p: &Painter, right_top: Pos2) {
    let galley = p.layout_no_wrap("Frozen".into(), theme::font(9.0), Color32::BLACK);
    let rect = Rect::from_min_size(
        right_top - Vec2::new(galley.size().x + 8.0, -3.0),
        galley.size() + Vec2::new(6.0, 2.0),
    );
    p.rect_filled(rect, 2.0, ice());
    p.galley(rect.min + Vec2::new(3.0, 1.0), galley, Color32::BLACK);
}

impl RliveApp {
    pub(super) fn bounce_command(&mut self, c: Bounce) {
        let (session, state) = (&mut self.session, &mut self.ui);
        match c {
            Bounce::ToNewTrack => {
                if let Some(source) = source_of(session, state) {
                    report(state, session.bounce_to_new_track(source));
                }
            }
            Bounce::Consolidate => {
                if let Some((tracks, start, end)) = consolidate_range(session, state) {
                    report(state, session.consolidate_lanes(&tracks, start, end));
                }
            }
            Bounce::ConsolidateToScene => {
                if let Some((start, end)) = selected_span(state) {
                    let below = scene_below(state);
                    report(
                        state,
                        session.consolidate_time_to_new_scene(below, start, end),
                    );
                }
            }
            Bounce::PasteBounced => {
                if let (Some(source), Some(target)) =
                    (state.bounce_copy.clone(), paste_target(session, state))
                {
                    report(state, session.paste_bounced(&source, target));
                }
            }
            Bounce::Freeze | Bounce::Flatten | Bounce::InPlace => self.selected_track_command(c),
        }
    }

    fn selected_track_command(&mut self, c: Bounce) {
        let ids = self.selected_track_ids();
        match c {
            Bounce::Freeze => {
                let (frozen, thawed): (Vec<_>, Vec<_>) =
                    ids.into_iter().partition(|t| self.session.is_frozen(*t));
                // Each freeze is a job of its own, started as the one before finishes.
                self.ui.pending_freezes = thawed;
                for t in frozen {
                    self.session.unfreeze_track(t);
                }
            }
            Bounce::Flatten => {
                for t in ids {
                    let result = self.session.flatten_track(t);
                    report(&mut self.ui, result);
                }
            }
            Bounce::InPlace => {
                if let Some(t) = ids.first() {
                    let result = self.session.bounce_in_place(*t);
                    report(&mut self.ui, result);
                }
            }
            _ => {}
        }
    }

    pub(super) fn bounce_state(&self, s: &mut MenuState) {
        let session = &self.session;
        let ids = self.selected_track_ids();
        let any_frozen = ids.iter().any(|t| session.is_frozen(*t));
        let on = |c: Bounce| M::Bounce(c);
        s.enable(
            on(Bounce::Freeze),
            ids.iter()
                .any(|t| session.is_frozen(*t) || session.can_freeze(*t)),
        );
        s.freeze_title = if any_frozen {
            "Unfreeze Track"
        } else {
            "Freeze Track"
        };
        s.enable(on(Bounce::Flatten), any_frozen);
        s.enable(
            on(Bounce::InPlace),
            ids.first().is_some_and(|t| session.can_bounce_track(*t)),
        );
        s.enable(
            on(Bounce::ToNewTrack),
            source_of(session, &self.ui).is_some(),
        );
        let range = consolidate_range(session, &self.ui);
        s.enable(
            on(Bounce::Consolidate),
            range
                .as_ref()
                .is_some_and(|(t, a, b)| session.can_consolidate_lanes(t, *a, *b)),
        );
        s.enable(
            on(Bounce::ConsolidateToScene),
            selected_span(&self.ui).is_some(),
        );
        let can_paste = paste_target(session, &self.ui)
            .is_some_and(|t| session.can_paste_bounced(t))
            && self
                .ui
                .bounce_copy
                .as_ref()
                .is_some_and(|c| session.bounce_source_exists(c));
        s.enable(on(Bounce::PasteBounced), can_paste);
    }

    /// Whether the Detail View is showing something on a frozen track, and so can't change it.
    pub(in crate::ui) fn detail_is_frozen(&self, stacked: bool) -> bool {
        let clip = || {
            self.ui
                .selected_clip
                .is_some_and(|c| self.session.is_frozen(c.track()))
        };
        let devices = || {
            matches!(
                self.ui.selected,
                Some(ChainRef::Track(t)) if self.session.is_frozen(t)
            )
        };
        match self.ui.detail {
            _ if stacked => clip() || devices(),
            crate::ui::Detail::Clip => clip(),
            crate::ui::Detail::Devices => devices(),
            crate::ui::Detail::Scene => false,
        }
    }

    /// Starts the next of a run of freezes once the job before it has finished.
    pub(in crate::ui) fn run_pending_freezes(&mut self) {
        if self.session.render_status().is_some() {
            return;
        }
        while !self.ui.pending_freezes.is_empty() {
            let id = self.ui.pending_freezes.remove(0);
            if self.session.can_freeze(id) {
                let result = self.session.freeze_track(id);
                report(&mut self.ui, result);
                break;
            }
        }
    }

    /// The progress of a bounce, freeze or consolidate, over the app, which keeps running under it.
    pub(in crate::ui) fn render_progress(&mut self, ctx: &egui::Context) {
        let Some(status) = self.session.render_status() else {
            return;
        };
        ctx.request_repaint();
        let mut cancel = false;
        egui::Modal::new(egui::Id::unique("render job")).show(ctx, |ui| {
            ui.set_width(280.0);
            ui.label(status.label);
            ui.add(egui::ProgressBar::new(status.fraction).show_percentage());
            if status.realtime {
                ui.small("Rendering in real time for external hardware");
            }
            cancel = ui.button("Cancel").clicked();
        });
        if cancel {
            self.session.cancel_render();
            self.ui.pending_freezes.clear();
        }
    }
}
