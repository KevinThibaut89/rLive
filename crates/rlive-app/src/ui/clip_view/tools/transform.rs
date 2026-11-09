//! The Transform panel's settings for each tool.

use super::controls::{choice, count, float, int, menu, percent, rate, switch};
use super::displays;
use crate::session::{ClipRef, Session};
use crate::ui::theme::*;
use crate::ui::widgets::{self, info};
use crate::ui::UiState;
use egui::Vec2;
use rlive_core::PPQ;
use rlive_project::clip_edit::PitchScale;
use rlive_project::midi_tools::{
    lfo_balance, transform_scope, ArpStyle, Articulation, Chop, Dimension, GracePitch, LfoShape,
    LfoTarget, OrnamentKind, Transform,
};

pub(super) fn settings(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    cref: ClipRef,
    scale: Option<PitchScale>,
) {
    let steps = if scale.is_some() { " deg" } else { " st" };
    let clip = session.clip_at(cref).cloned();
    let (notes, range) = match &clip {
        Some(c) => {
            let s = transform_scope(
                c.notes(),
                &state.piano.selected,
                state.piano.time,
                state.piano.time_only,
                (c.loop_start, c.loop_end()),
            );
            (
                s.picks
                    .iter()
                    .filter_map(|i| c.notes().get(*i).cloned())
                    .collect(),
                s.range,
            )
        }
        None => (Vec::new(), (0, 0)),
    };
    let notes: Vec<rlive_project::NoteData> = notes;
    let picks = notes.len();
    let grid = state.piano.grid.max(1);
    let piano_q = state.piano.quantize;
    let index = state.clip_view.midi_tools.transform;
    let mut warp_point = state.clip_view.midi_tools.warp_point;
    let tool = &mut state.clip_view.midi_tools.transforms[index];
    match tool {
        Transform::Arpeggiate(t) => {
            let styles: Vec<(ArpStyle, &str)> =
                ArpStyle::ALL.iter().map(|s| (*s, s.label())).collect();
            menu(
                ui,
                "arp_style",
                "Style",
                &mut t.style,
                &styles,
                "Style — the order of the notes",
            );
            int(
                ui,
                "Distance",
                &mut t.distance,
                -24,
                24,
                steps,
                "Distance — between transposed passes",
            );
            count(
                ui,
                "Steps",
                &mut t.steps,
                0,
                8,
                "Steps — how many transposed passes",
            );
            rate(
                ui,
                "arp_rate",
                "Rate",
                &mut t.rate,
                "Rate — how fast, and how long the notes are",
            );
            percent(
                ui,
                "Gate",
                &mut t.gate,
                0.01,
                2.0,
                "Gate — note length, as a share of the rate",
            );
        }
        Transform::Chop(t) => chop(ui, t),
        Transform::Connect(t) => {
            count(
                ui,
                "Spread",
                &mut t.spread,
                0,
                24,
                "Spread — the most a new note strays in pitch",
            );
            percent(
                ui,
                "Density",
                &mut t.density,
                0.0,
                1.0,
                "Density — how much of the gaps gets filled",
            );
            rate(
                ui,
                "connect_rate",
                "Rate",
                &mut t.rate,
                "Rate — how long the new notes are",
            );
            percent(
                ui,
                "Tie",
                &mut t.tie,
                0.0,
                1.0,
                "Tie — the chance a new note runs on to the next",
            );
        }
        Transform::Glissando(t) => {
            displays::glissando(ui, t);
            percent(
                ui,
                "Start",
                &mut t.start,
                0.0,
                1.0,
                "Start — where in the note the bend begins",
            );
            float(
                ui,
                "Curve",
                &mut t.curve,
                -1.0,
                1.0,
                "",
                "Curve — the shape of the bend",
            );
        }
        Transform::Lfo(t) => {
            let targets: Vec<(LfoTarget, &str)> =
                LfoTarget::ALL.iter().map(|s| (*s, s.label())).collect();
            menu(
                ui,
                "lfo_target",
                "Target",
                &mut t.target,
                &targets,
                "Target — which expression the LFO writes",
            );
            displays::lfo(ui, t);
            let (a0, d0) = (t.attack, t.decay);
            percent(
                ui,
                "Attack",
                &mut t.attack,
                0.0,
                1.0,
                "Attack — fade in, across the oscillation",
            );
            percent(
                ui,
                "Decay",
                &mut t.decay,
                0.0,
                1.0,
                "Decay — fade out, across the oscillation",
            );
            let moved_attack = t.attack != a0 || t.decay == d0;
            (t.attack, t.decay) = lfo_balance(t.attack, t.decay, moved_attack);
            let shapes: Vec<(LfoShape, &str)> =
                LfoShape::ALL.iter().map(|s| (*s, s.label())).collect();
            menu(
                ui,
                "lfo_shape",
                "Type",
                &mut t.shape_type,
                &shapes,
                "Shape Type",
            );
            float(
                ui,
                "Shape",
                &mut t.shape,
                -1.0,
                1.0,
                "",
                "Shape — bends the wave",
            );
            if t.shape_type == LfoShape::Random
                && info(ui.small_button("Reseed"), "Reseed — a new random wave").clicked()
            {
                t.reseed = t.reseed.wrapping_add(1);
            }
            rate(
                ui,
                "lfo_rate",
                "Rate",
                &mut t.rate,
                "Rate — one cycle, from 1 to 1/128",
            );
            percent(
                ui,
                "Time Shift",
                &mut t.time_shift,
                -1.0,
                1.0,
                "Time Shift — delays the start, or moves the phase",
            );
            let (amount, base, unit) = match t.target {
                LfoTarget::Pitch => (96.0, 48.0, " st"),
                LfoTarget::Slide | LfoTarget::Pressure => (127.0, 127.0, ""),
            };
            float(
                ui,
                "Amount",
                &mut t.amount,
                -amount,
                amount,
                unit,
                "Amount — how far the wave moves",
            );
            float(
                ui,
                "Base",
                &mut t.base,
                -base,
                base,
                unit,
                "Amplitude Base — the value it moves from",
            );
        }
        Transform::Ornament(t) => {
            choice(
                ui,
                &mut t.kind,
                &[(OrnamentKind::Flam, "Flam"), (OrnamentKind::Grace, "Grace")],
                "Ornament — flam or grace notes",
            );
            match t.kind {
                OrnamentKind::Flam => {
                    percent(
                        ui,
                        "Position",
                        &mut t.flam_position,
                        -1.0,
                        1.0,
                        "Position — above zero replaces the note's start; below, goes before it",
                    );
                    percent(
                        ui,
                        "Velocity",
                        &mut t.flam_velocity,
                        0.0,
                        1.0,
                        "Velocity — of the flam, against the note",
                    );
                }
                OrnamentKind::Grace => {
                    choice(
                        ui,
                        &mut t.grace_pitch,
                        &[
                            (GracePitch::High, "High"),
                            (GracePitch::Same, "Same"),
                            (GracePitch::Low, "Low"),
                        ],
                        "Pitch — of every other grace note",
                    );
                    percent(
                        ui,
                        "Position",
                        &mut t.grace_position,
                        -1.0,
                        1.0,
                        "Position — above zero replaces the note's start; below, goes before it",
                    );
                    percent(
                        ui,
                        "Velocity",
                        &mut t.grace_velocity,
                        0.0,
                        1.0,
                        "Velocity — of the grace notes, against the note",
                    );
                    percent(
                        ui,
                        "Chance",
                        &mut t.grace_chance,
                        0.0,
                        1.0,
                        "Chance — of each grace note, against the note's",
                    );
                    count(
                        ui,
                        "Amount",
                        &mut t.grace_amount,
                        1,
                        32,
                        "Amount — how many grace notes",
                    );
                }
            }
        }
        Transform::Quantize(t) => {
            t.settings = piano_q;
            switch(
                ui,
                &mut t.use_grid,
                "Grid",
                "Current Grid — quantize to the grid rather than the value",
            );
            if !t.use_grid {
                rate(
                    ui,
                    "quantize_rate",
                    "Value",
                    &mut t.settings.grid,
                    "Value — what to quantize to",
                );
            }
            ui.horizontal(|ui| {
                switch(ui, &mut t.settings.starts, "Starts", "Quantize note starts");
                switch(ui, &mut t.settings.ends, "Ends", "Quantize note ends");
            });
            percent(
                ui,
                "Amount",
                &mut t.settings.amount,
                0.0,
                1.0,
                "Amount — how far notes move",
            );
        }
        Transform::Recombine(t) => {
            let dims: Vec<(Dimension, &str)> =
                Dimension::ALL.iter().map(|d| (*d, d.label())).collect();
            menu(
                ui,
                "recombine_dim",
                "Dimension",
                &mut t.dimension,
                &dims,
                "Dimension — what is exchanged",
            );
            let most = t
                .most_steps(picks, range.1.saturating_sub(range.0), grid)
                .max(0);
            displays::columns(ui, &notes, t, most);
            ui.horizontal(|ui| {
                switch(
                    ui,
                    &mut t.shuffle,
                    "Shuffle",
                    "Shuffle — a new order each time Apply is pressed",
                );
                switch(ui, &mut t.mirror, "Mirror", "Mirror — reverse order");
            });
            if t.dimension == Dimension::Position {
                switch(
                    ui,
                    &mut t.rotate_on_grid,
                    "On Grid",
                    "Rotate on Grid — count steps in grid cells, not notes",
                );
            }
            let most = t
                .most_steps(picks, range.1.saturating_sub(range.0), grid)
                .max(0);
            ui.horizontal(|ui| {
                if info(ui.small_button("<"), "Rotate Step Down").clicked() {
                    t.rotate = (t.rotate - 1).max(-most);
                }
                if info(ui.small_button(">"), "Rotate Step Up").clicked() {
                    t.rotate = (t.rotate + 1).min(most);
                }
            });
            int(
                ui,
                "Rotation",
                &mut t.rotate,
                -most,
                most,
                "",
                "Rotation Steps — above zero clockwise",
            );
        }
        Transform::Span(t) => {
            choice(
                ui,
                &mut t.articulation,
                &[
                    (Articulation::Legato, "Legato"),
                    (Articulation::Tenuto, "Tenuto"),
                    (Articulation::Staccato, "Stacc."),
                ],
                "Articulation",
            );
            percent(
                ui,
                "Offset",
                &mut t.offset,
                -1.0,
                1.0,
                "Offset — moves note ends, up to a grid step",
            );
            percent(
                ui,
                "Variation",
                &mut t.variation,
                -1.0,
                1.0,
                "Variation — above zero shortens at random; below zero lengthens",
            );
        }
        Transform::Strum(t) => {
            displays::strum(ui, t);
            percent(
                ui,
                "Low",
                &mut t.low,
                -1.0,
                1.0,
                "Strum Low — how far the lowest note moves",
            );
            percent(
                ui,
                "High",
                &mut t.high,
                -1.0,
                1.0,
                "Strum High — how far the highest note moves",
            );
            percent(
                ui,
                "Tension",
                &mut t.tension,
                -1.0,
                1.0,
                "Tension — bunches the notes toward an end",
            );
        }
        Transform::TimeWarp(t) => {
            displays::warp(ui, t, &mut warp_point);
            ui.horizontal(|ui| {
                for (i, p) in t.points.iter_mut().enumerate() {
                    let on = p.on;
                    let r = widgets::state_button(
                        ui,
                        Vec2::new(28.0, 15.0),
                        on,
                        pal().accent,
                        &format!("{}", i + 1),
                    );
                    if info(r, "Breakpoint — on or off").clicked() {
                        p.on = !on;
                        warp_point = i;
                    }
                }
            });
            let at_least_one = t.points.iter().any(|p| p.on);
            if !at_least_one {
                t.points[warp_point.min(2)].on = true;
            }
            if let Some(p) = t.points.get_mut(warp_point) {
                percent(ui, "Time", &mut p.time, 0.0, 1.0, "Breakpoint Time");
                float(
                    ui,
                    "Speed",
                    &mut p.speed,
                    0.25,
                    4.0,
                    "x",
                    "Breakpoint Speed",
                );
            }
            ui.horizontal(|ui| {
                switch(
                    ui,
                    &mut t.quantize,
                    "Quantize",
                    "Quantize — put warped notes on the grid",
                );
                switch(ui, &mut t.preserve_range, "Preserve", "Preserve Time Range");
            });
            switch(
                ui,
                &mut t.include_note_end,
                "Note End",
                "Include Note End — warp note ends too",
            );
        }
        Transform::VelocityShaper(t) => {
            displays::velocity_env(ui, &mut t.envelope);
            let (mut lo, mut hi) = (f64::from(t.min) * 127.0, f64::from(t.max) * 127.0);
            float(ui, "Minimum", &mut lo, 1.0, 127.0, "", "Minimum Velocity");
            float(ui, "Maximum", &mut hi, 1.0, 127.0, "", "Maximum Velocity");
            t.min = (lo / 127.0) as f32;
            t.max = (hi / 127.0) as f32;
            count(
                ui,
                "Loop",
                &mut t.loops,
                1,
                16,
                "Loop — how many times the shape runs",
            );
            int(
                ui,
                "Rotate",
                &mut t.rotate,
                -64,
                64,
                "",
                "Rotate — steps the shape moves",
            );
            let mut grid_on = t.division.is_none();
            switch(ui, &mut grid_on, "Grid", "Division — one grid step");
            if grid_on {
                t.division = None;
            } else {
                let mut d = t.division.unwrap_or(PPQ / 4);
                rate(
                    ui,
                    "velshape_div",
                    "Division",
                    &mut d,
                    "Division — the size of a step",
                );
                t.division = Some(d);
            }
        }
    }
    state.clip_view.midi_tools.warp_point = warp_point;
    if let Transform::Quantize(t) = &state.clip_view.midi_tools.transforms[index] {
        state.piano.quantize = t.settings;
    }
}

fn chop(ui: &mut egui::Ui, t: &mut Chop) {
    count(ui, "Parts", &mut t.parts, 2, 64, "Parts — how many");
    let (lo, hi) = t.gaps_range();
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Gaps").color(pal().text_dim).small());
        let mut g = t.gaps.unwrap_or(0);
        let known = t.gaps.is_some();
        let r = ui.add(
            egui::DragValue::new(&mut g)
                .range(lo..=hi)
                .speed(0.1)
                .custom_formatter(move |v, _| {
                    if known {
                        format!("{v:.0}")
                    } else {
                        "-".to_string()
                    }
                }),
        );
        if info(
            r,
            "Gaps — above zero, notes before a gap; below, gaps after each note",
        )
        .changed()
        {
            t.set_gaps(g);
        }
    });
    let len = t.pattern_len();
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(1.0, 2.0);
        for i in 0..len {
            let lit = t.pattern[i];
            let color = if t.emphasis[i] {
                shade(0x6a6a6a)
            } else {
                pal().accent
            };
            let r = widgets::state_button(ui, Vec2::new(16.0, 14.0), lit, color, "");
            if info(r, "Pattern — a note or a gap").clicked() {
                t.toggle_part(i);
            }
        }
    });
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(1.0, 2.0);
        for i in 0..len {
            let on = t.emphasis[i];
            let r = widgets::state_button(ui, Vec2::new(16.0, 10.0), on, pal().text_dim, "");
            if info(r, "Emphasis — stretch this part").clicked() {
                t.emphasis[i] = !on;
            }
        }
    });
    count(
        ui,
        "Stretch",
        &mut t.stretch,
        2,
        8,
        "Stretch Chunk(s) — how much longer an emphasised part is",
    );
    percent(
        ui,
        "Variation",
        &mut t.variation,
        0.0,
        1.0,
        "Variation — moves starts and ends at random",
    );
}
