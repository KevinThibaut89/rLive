//! The clip launch controls, and the Follow Action controls clips and scenes share.

use super::session_select::launch_targets;
use super::theme::{self, *};
use super::widgets::{self, info};
use super::UiState;
use crate::session::{ClipRef, Session};
use egui::{Layout, Rect, RichText, UiBuilder, Vec2};
use rlive_core::{FollowAction, FollowActions, LaunchMode, Quantize, TimeSignature};

/// How wide the launch section is.
const STRIP_W: f32 = 176.0;
/// The share of the panel it doesn't need.
const MIN_REST_W: f32 = 240.0;

/// Draws the Clip View, with the launch section down its left side for a Session clip.
pub fn with_launch_strip(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    clip_view: impl FnOnce(&mut egui::Ui, &mut Session, &mut UiState),
) {
    let full = ui.max_rect();
    let targets = launch_targets(state);
    let shows_launch = !targets.is_empty()
        && targets.iter().all(|r| r.as_slot().is_some())
        && full.width() > STRIP_W + MIN_REST_W;
    if !shows_launch {
        clip_view(ui, session, state);
        return;
    }
    let strip = Rect::from_min_size(full.min, Vec2::new(STRIP_W, full.height()));
    let rest = Rect::from_min_max(egui::pos2(full.left() + STRIP_W, full.top()), full.max);
    let mut su = ui.new_child(
        UiBuilder::new()
            .max_rect(strip.shrink2(Vec2::new(6.0, 4.0)))
            .layout(Layout::top_down(egui::Align::Min)),
    );
    su.set_clip_rect(strip);
    egui::ScrollArea::vertical()
        .id_salt("clip_launch")
        .auto_shrink([false, false])
        .show(&mut su, |ui| launch_section(ui, session, &targets));
    ui.painter().line_segment(
        [strip.right_top(), strip.right_bottom()],
        egui::Stroke::new(1.0, pal().separator),
    );
    let mut cu = ui.new_child(
        UiBuilder::new()
            .max_rect(rest)
            .layout(Layout::top_down(egui::Align::Min)),
    );
    clip_view(&mut cu, session, state);
}

fn section_title(ui: &mut egui::Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .font(theme::font_bold(10.5))
            .color(pal().text_dim),
    );
}

fn row<R>(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.add_sized(
            [54.0, 16.0],
            egui::Label::new(
                RichText::new(label)
                    .font(theme::font(10.5))
                    .color(pal().text_dim),
            ),
        );
        add(ui)
    })
    .inner
}

fn quantize_label(q: Option<Quantize>) -> &'static str {
    q.map_or("Global", |q| q.label())
}

fn launch_section(ui: &mut egui::Ui, session: &mut Session, targets: &[ClipRef]) {
    let Some(first) = targets.first().and_then(|r| session.clip_at(*r)) else {
        return;
    };
    let launch = first.launch.clone();
    if targets.len() > 1 {
        ui.label(
            RichText::new(format!("{} Clips Selected", targets.len()))
                .font(theme::font_bold(10.5))
                .color(pal().accent),
        );
    }
    section_title(ui, "Launch");
    let mut mode = launch.mode;
    row(ui, "Mode", |ui| {
        egui::ComboBox::from_id_salt("launch_mode")
            .width(84.0)
            .selected_text(mode.label())
            .show_ui(ui, |ui| {
                for m in LaunchMode::ALL {
                    ui.selectable_value(&mut mode, m, m.label());
                }
            })
    });
    if mode != launch.mode {
        session.edit_launch(targets, |l| l.mode = mode);
    }
    let mut quantize = launch.quantize;
    row(ui, "Quantize", |ui| {
        egui::ComboBox::from_id_salt("launch_quantize")
            .width(84.0)
            .selected_text(quantize_label(quantize))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut quantize, None, "Global");
                for q in Quantize::ALL {
                    ui.selectable_value(&mut quantize, Some(q), q.label());
                }
            })
    });
    if quantize != launch.quantize {
        session.edit_launch(targets, |l| l.quantize = quantize);
    }
    let mut velocity = launch.velocity_amount * 100.0;
    row(ui, "Velocity", |ui| {
        ui.add(
            egui::DragValue::new(&mut velocity)
                .range(0.0..=100.0)
                .suffix(" %"),
        );
        let r = widgets::state_button(
            ui,
            Vec2::new(42.0, 16.0),
            launch.legato,
            pal().accent,
            "Legato",
        );
        if info(
            r,
            "Legato — takes over the play position of the clip it replaces",
        )
        .clicked()
        {
            let on = !launch.legato;
            session.edit_launch(targets, |l| l.legato = on);
        }
    });
    if (velocity / 100.0 - launch.velocity_amount).abs() > 1e-4 {
        let amount = (velocity / 100.0).clamp(0.0, 1.0);
        session.edit_launch(targets, |l| l.velocity_amount = amount);
    }
    if let Some(track) = targets[0].as_slot().map(|s| s.0) {
        row(ui, "Nudge", |ui| {
            let back = widgets::state_button(ui, Vec2::new(30.0, 16.0), false, pal().accent, "<");
            if info(
                back,
                "Nudge Backward — jump back by the global quantization",
            )
            .clicked()
            {
                session.nudge_clip(track, false);
            }
            let fwd = widgets::state_button(ui, Vec2::new(30.0, 16.0), false, pal().accent, ">");
            if info(fwd, "Nudge Forward — jump ahead by the global quantization").clicked() {
                session.nudge_clip(track, true);
            }
        });
    }
    ui.add_space(4.0);
    let sig = session.project.time_signature;
    if let Some(next) = follow_controls(ui, "clip", launch.follow, true, sig) {
        let before = launch.follow;
        session.edit_launch(targets, |l| merge_follow(&before, &next, &mut l.follow));
    }
}

/// Puts the fields the user changed into `into`, leaving the rest as each clip has them.
pub fn merge_follow(before: &FollowActions, after: &FollowActions, into: &mut FollowActions) {
    if before.enabled != after.enabled {
        crate::session::set_follow_on(into, after.enabled);
    }
    macro_rules! carry {
        ($($f:ident),*) => { $(if before.$f != after.$f { into.$f = after.$f; })* };
    }
    carry!(action_a, action_b, chance_a, chance_b, linked, multiplier, time);
}

fn action_row(ui: &mut egui::Ui, salt: &str, label: &str, action: &mut FollowAction) {
    row(ui, label, |ui| {
        egui::ComboBox::from_id_salt((salt, label))
            .width(66.0)
            .selected_text(action.label())
            .show_ui(ui, |ui| {
                for a in FollowAction::ALL {
                    let selected = std::mem::discriminant(action) == std::mem::discriminant(&a);
                    if ui.selectable_label(selected, a.label()).clicked() && !selected {
                        *action = a;
                    }
                }
            });
        if let FollowAction::Jump(target) = action {
            let mut shown = *target + 1;
            ui.add(egui::DragValue::new(&mut shown).range(1..=256));
            *target = shown.max(1) - 1;
        }
    });
}

/// A length in bars.beats.sixteenths as three fields. Returns the new length if it changed.
fn time_fields(ui: &mut egui::Ui, ticks: u64, sig: TimeSignature) -> u64 {
    let bar = sig.ticks_per_bar().max(1);
    let beat = sig.ticks_per_beat().max(1);
    let sixteenth = (rlive_core::PPQ / 4).max(1);
    let mut b = ticks / bar;
    let mut be = (ticks % bar) / beat;
    let mut s = (ticks % beat) / sixteenth;
    ui.spacing_mut().item_spacing.x = 2.0;
    ui.add(egui::DragValue::new(&mut b).range(0..=999));
    ui.add(egui::DragValue::new(&mut be).range(0..=98));
    ui.add(egui::DragValue::new(&mut s).range(0..=15));
    (b * bar + be * beat + s * sixteenth).max(1)
}

/// The Follow Action controls. Returns the settings if the user changed any.
pub fn follow_controls(
    ui: &mut egui::Ui,
    salt: &str,
    follow: FollowActions,
    clip: bool,
    sig: TimeSignature,
) -> Option<FollowActions> {
    let mut next = follow;
    ui.horizontal(|ui| {
        section_title(ui, "Follow Action");
        let r = widgets::state_button(
            ui,
            Vec2::new(30.0, 16.0),
            follow.enabled,
            pal().accent,
            "On",
        );
        if info(
            r,
            "Follow Action — on or off for this clip or scene (Shift+Enter)",
        )
        .clicked()
        {
            next.enabled = !follow.enabled;
            if next.enabled
                && follow.action_a == FollowAction::None
                && follow.action_b == FollowAction::None
            {
                next.action_a = FollowAction::Next;
            }
        }
    });
    let enabled = next.enabled;
    ui.add_enabled_ui(enabled, |ui| {
        action_row(ui, salt, "A", &mut next.action_a);
        action_row(ui, salt, "B", &mut next.action_b);
        row(ui, "Chance", |ui| {
            let mut a = next.chance_a * 100.0;
            let mut b = next.chance_b * 100.0;
            ui.spacing_mut().item_spacing.x = 2.0;
            if ui
                .add(egui::DragValue::new(&mut a).range(0.0..=100.0))
                .changed()
            {
                next.chance_a = a / 100.0;
                next.chance_b = 1.0 - next.chance_a;
            }
            let share_of = |f: &FollowActions| {
                let sum = f.chance_a + f.chance_b;
                if sum > 0.0 {
                    f.chance_a / sum
                } else {
                    0.5
                }
            };
            let mut share = share_of(&next);
            let slider = egui::Slider::new(&mut share, 1.0..=0.0)
                .show_value(false)
                .trailing_fill(false);
            if ui
                .add_sized([34.0, 14.0], slider)
                .on_hover_text("Moves the chance between A and B")
                .changed()
            {
                next.set_chance_share(share);
            }
            if ui
                .add(egui::DragValue::new(&mut b).range(0.0..=100.0))
                .changed()
            {
                next.chance_b = b / 100.0;
                next.chance_a = 1.0 - next.chance_b;
            }
        });
        if clip {
            row(ui, "Time", |ui| {
                let r = widgets::state_button(
                    ui,
                    Vec2::new(44.0, 16.0),
                    next.linked,
                    pal().accent,
                    if next.linked { "Linked" } else { "Unlinked" },
                );
                if info(
                    r,
                    "Linked fires at the end of the clip; Unlinked after the Follow Time",
                )
                .clicked()
                {
                    next.linked = !next.linked;
                }
                if next.linked {
                    let mut m = next.multiplier.max(1);
                    ui.add(egui::DragValue::new(&mut m).range(1..=999).prefix("x"));
                    next.multiplier = m;
                } else {
                    next.time = time_fields(ui, next.time, sig);
                }
            });
        } else {
            row(ui, "Time", |ui| next.time = time_fields(ui, next.time, sig));
        }
    });
    (next != follow).then_some(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_multi_clip_edit_only_touches_what_was_changed() {
        let before = FollowActions::default();
        let after = FollowActions {
            action_b: FollowAction::Stop,
            ..before
        };
        let mut other = FollowActions {
            action_a: FollowAction::Next,
            time: 123,
            ..FollowActions::default()
        };
        merge_follow(&before, &after, &mut other);
        assert_eq!(other.action_a, FollowAction::Next, "theirs is kept");
        assert_eq!(other.time, 123);
        assert_eq!(other.action_b, FollowAction::Stop);
    }
}
