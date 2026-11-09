//! The Generate panel's settings for each tool.

use super::controls::{choice, count, float, int, key, menu, percent, range_u8, rate, velocity};
use super::displays;
use crate::session::{ClipRef, Session};
use crate::ui::theme::*;
use crate::ui::widgets::{self, info};
use crate::ui::UiState;
use egui::Vec2;
use rlive_core::PPQ;
use rlive_project::clip_edit::PitchScale;
use rlive_project::midi_tools::{
    parse_bank, pattern_count, Euclidean, Generator, ShapePreset, Stacks,
};

pub(super) fn settings(
    ui: &mut egui::Ui,
    state: &mut UiState,
    cref: ClipRef,
    session: &mut Session,
    scale: Option<PitchScale>,
) {
    let span = session.clip_at(cref).map_or(0, |c| {
        state
            .piano
            .time
            .filter(|(a, b)| b > a)
            .map_or(c.length, |(a, b)| b - a)
    });
    let index = state.clip_view.midi_tools.generator;
    let mut voices_tab = state.clip_view.midi_tools.voices_tab;
    let purple = scale.is_some();
    let tool = &mut state.clip_view.midi_tools.generators[index];
    match tool {
        Generator::Rhythm(t) => {
            key(
                ui,
                "Pitch",
                &mut t.pitch,
                "Pitch — or Alt-click the piano ruler",
            );
            rate(
                ui,
                "rhythm_step",
                "Step",
                &mut t.step_duration,
                "Step Duration — how long a step is",
            );
            let most = t.most_steps(span.max(1));
            count(
                ui,
                "Steps",
                &mut t.steps,
                1,
                most,
                "Steps — how long the pattern is",
            );
            count(
                ui,
                "Density",
                &mut t.density,
                1,
                t.steps.max(1),
                "Density — how many notes",
            );
            let top = pattern_count(t.steps.max(1), t.density.max(1)).saturating_sub(1);
            let mut pattern = t.pattern as i64;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Pattern").color(pal().text_dim).small());
                let r = ui.add(
                    egui::DragValue::new(&mut pattern)
                        .range(0..=top as i64)
                        .speed(0.5),
                );
                info(r, "Pattern — which arrangement of the notes");
            });
            t.pattern = pattern.max(0) as u64;
            displays::steps(ui, &t.steps_on(span.max(1)));
            percent(
                ui,
                "Split",
                &mut t.split,
                0.0,
                1.0,
                "Split — the chance a step divides in half",
            );
            int(
                ui,
                "Shift",
                &mut t.shift,
                -16,
                16,
                "",
                "Shift — moves the notes along the steps",
            );
            velocity(ui, "Velocity", &mut t.velocity, "Velocity");
            velocity(
                ui,
                "Accent",
                &mut t.accent,
                "Accent — velocity of accented notes",
            );
            count(
                ui,
                "Frequency",
                &mut t.accent_frequency,
                0,
                t.density.max(1),
                "Accent Frequency — every this-many notes",
            );
            int(
                ui,
                "Offset",
                &mut t.accent_offset,
                -16,
                16,
                "",
                "Accent Offset — moves the accents",
            );
        }
        Generator::Seed(t) => {
            range_u8(
                ui,
                "Pitch",
                &mut t.pitch,
                purple,
                "Pitch Range — or Alt-drag on the piano ruler",
            );
            let (mut lo, mut hi) = (t.duration.0 as f64, t.duration.1 as f64);
            float(
                ui,
                "Dur. Min",
                &mut lo,
                (PPQ / 32) as f64,
                (4 * PPQ) as f64,
                "",
                "Duration Range — shortest",
            );
            float(
                ui,
                "Dur. Max",
                &mut hi,
                (PPQ / 32) as f64,
                (4 * PPQ) as f64,
                "",
                "Duration Range — longest",
            );
            t.duration = (lo.min(hi) as u64, lo.max(hi) as u64);
            velocity(
                ui,
                "Vel. Min",
                &mut t.velocity.0,
                "Velocity Range — softest",
            );
            velocity(
                ui,
                "Vel. Max",
                &mut t.velocity.1,
                "Velocity Range — hardest",
            );
            count(
                ui,
                "Voices",
                &mut t.voices,
                1,
                16,
                "Voices — the most notes at once",
            );
            percent(
                ui,
                "Density",
                &mut t.density,
                0.0,
                1.0,
                "Density — notes, as a share of the pitch range",
            );
        }
        Generator::Shape(t) => {
            displays::shape(ui, &mut t.levels, purple);
            let mut preset = None;
            let presets: Vec<(Option<ShapePreset>, &str)> = std::iter::once((None, "Drawn"))
                .chain(ShapePreset::ALL.iter().map(|p| (Some(*p), p.label())))
                .collect();
            menu(
                ui,
                "shape_preset",
                "Preset",
                &mut preset,
                &presets,
                "Shape Presets",
            );
            if let Some(p) = preset {
                t.levels = p.levels();
            }
            range_u8(
                ui,
                "Pitch",
                &mut t.pitch,
                purple,
                "Pitch Range — or Alt-drag on the piano ruler",
            );
            rate(
                ui,
                "shape_rate",
                "Rate",
                &mut t.rate,
                "Rate — the shortest note",
            );
            percent(
                ui,
                "Tie",
                &mut t.tie,
                0.0,
                1.0,
                "Tie — the chance a note runs on to the next",
            );
            percent(
                ui,
                "Density",
                &mut t.density,
                0.0,
                1.0,
                "Density — how much of the shape gets notes",
            );
            percent(
                ui,
                "Jitter",
                &mut t.jitter,
                0.0,
                1.0,
                "Jitter — strays from the shape, at random",
            );
        }
        Generator::Stacks(t) => stacks(ui, t, scale),
        Generator::Euclidean(t) => {
            euclidean(ui, t, &mut voices_tab);
        }
    }
    state.clip_view.midi_tools.voices_tab = voices_tab;
}

fn stacks(ui: &mut egui::Ui, t: &mut Stacks, scale: Option<PitchScale>) {
    let mut remove = false;
    let mut add = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(2.0, 2.0);
        for (i, c) in t.chords.iter().enumerate() {
            let name = t.bank.chords.get(c.chord).map_or("?", |d| d.name.as_str());
            let on = t.current == i;
            let label = format!("{}", i + 1);
            let r = widgets::state_button(ui, Vec2::new(18.0, 16.0), on, pal().accent, &label);
            if info(r, format!("Chord — {name}")).clicked() {
                t.current = i;
            }
        }
        add = info(ui.small_button("+"), "Add Chord").clicked();
        remove = info(ui.small_button("-"), "Delete Chord").clicked();
    });
    if add {
        t.add_chord();
    }
    if remove {
        t.remove_chord();
    }
    let chords = t.bank.chords.clone();
    let current = t.current.min(t.chords.len() - 1);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(3.0, 3.0);
        for (i, def) in chords.iter().enumerate() {
            let on = t.chords[current].chord == i;
            if displays::chord_pad(ui, def, on) {
                t.chords[current].chord = i;
            }
        }
    });
    let roots = Stacks::roots(scale);
    let c = &mut t.chords[current];
    let mut pos = roots.iter().position(|k| *k >= c.root).unwrap_or(0) as i32;
    let last = roots.len() as i32 - 1;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Root").color(pal().text_dim).small());
        let list = roots.clone();
        let r = ui.add(
            egui::DragValue::new(&mut pos)
                .range(0..=last)
                .speed(0.15)
                .custom_formatter(move |v, _| {
                    let key = list[(v as usize).min(list.len() - 1)];
                    rlive_project::note_view::spell(
                        key,
                        rlive_project::note_view::Spelling::Sharps,
                        None,
                    )
                }),
        );
        info(r, "Chord Root — or Alt-click the piano ruler");
    });
    c.root = roots[pos.clamp(0, last) as usize];
    let chord = c.chord;
    let most = t.most_inversions(chord);
    let c = &mut t.chords[current];
    int(
        ui,
        "Inversion",
        &mut c.inversion,
        -most,
        most,
        "",
        "Chord Inversion — below zero is an octave lower",
    );
    count(
        ui,
        "Duration",
        &mut c.duration,
        1,
        8,
        "Chord Duration — in eighths of its slot",
    );
    count(
        ui,
        "Offset",
        &mut c.offset,
        0,
        7,
        "Offset — in eighths of its slot",
    );
    if info(
        ui.small_button("Load Bank..."),
        "Chord Bank — load a .stacks file",
    )
    .clicked()
    {
        load_bank(t);
    }
}

fn load_bank(t: &mut Stacks) {
    let Some(path) = rfd::FileDialog::new()
        .add_filter("Chord bank", &["stacks"])
        .pick_file()
    else {
        return;
    };
    // The panel has no message line, so a bank that can't be read just changes nothing.
    let _ = read_bank(t, &path);
}

/// Replaces a Stacks generator's bank with a `.stacks` file.
pub(super) fn read_bank(t: &mut Stacks, path: &std::path::Path) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let bank = parse_bank(&text)?;
    for c in &mut t.chords {
        c.chord = c.chord.min(bank.chords.len().saturating_sub(1));
    }
    t.bank = bank;
    Ok(())
}

fn euclidean(ui: &mut egui::Ui, t: &mut Euclidean, voices_tab: &mut bool) {
    choice(
        ui,
        voices_tab,
        &[(false, "Pattern"), (true, "Voices")],
        "Euclidean tabs",
    );
    let mut new_seed = None;
    let steps = t.steps;
    if *voices_tab {
        for (i, v) in t.voices.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(2.0, 2.0);
                if info(
                    widgets::state_button(
                        ui,
                        Vec2::new(16.0, 15.0),
                        v.on,
                        pal().accent,
                        &format!("{}", i + 1),
                    ),
                    "Voice — on or off",
                )
                .clicked()
                {
                    v.on = !v.on;
                }
                let mut pitch = i32::from(v.pitch);
                let r = ui.add(egui::DragValue::new(&mut pitch).range(0..=127).speed(0.2));
                info(r, "Voice Pitch");
                v.pitch = pitch.clamp(0, 127) as u8;
                let mut vel = v.velocity * 127.0;
                let r = ui.add(
                    egui::DragValue::new(&mut vel)
                        .range(1.0..=127.0)
                        .speed(0.5)
                        .max_decimals(0),
                );
                if info(r, "Voice Velocity").changed() {
                    v.velocity = vel / 127.0;
                }
            });
        }
        ui.horizontal(|ui| {
            let mut shift = 0;
            if info(ui.small_button("v"), "All voices down").clicked() {
                shift = -1;
            }
            if info(ui.small_button("^"), "All voices up").clicked() {
                shift = 1;
            }
            if shift != 0 {
                for v in &mut t.voices {
                    v.pitch = (i32::from(v.pitch) + shift).clamp(0, 127) as u8;
                }
            }
        });
    } else {
        displays::euclid(ui, t);
        for (i, v) in t.voices.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(2.0, 2.0);
                let r = widgets::state_button(
                    ui,
                    Vec2::new(16.0, 15.0),
                    v.on,
                    pal().accent,
                    &format!("{}", i + 1),
                );
                if info(r, "Voice — on or off").clicked() {
                    v.on = !v.on;
                }
                let r = ui.add(
                    egui::DragValue::new(&mut v.pulses)
                        .range(0..=steps)
                        .speed(0.1)
                        .prefix("x"),
                );
                info(r, "Hits — how many this voice spreads across the steps");
                let r = ui.add(
                    egui::DragValue::new(&mut v.rotation)
                        .range(-32..=32)
                        .speed(0.1),
                );
                info(r, "Rotation — moves this voice along the steps");
            });
        }
        if info(
            ui.small_button("Random"),
            "Randomize — every Rotation at random",
        )
        .clicked()
        {
            new_seed = Some(ui.input(|i| (i.time * 1000.0) as u64));
        }
    }
    if let Some(seed) = new_seed {
        t.randomize_rotations(seed);
    }
    count(
        ui,
        "Steps",
        &mut t.steps,
        1,
        32,
        "Steps — how long the pattern is",
    );
    let mut density = t.density;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Density").color(pal().text_dim).small());
        let r = ui.add(
            egui::DragValue::new(&mut density)
                .range(0..=32)
                .speed(0.1)
                .custom_formatter(|v, _| {
                    if v < 1.0 {
                        "Fill".into()
                    } else {
                        format!("{v:.0}")
                    }
                }),
        );
        info(
            r,
            "Density — how many times the pattern repeats; at zero it fills the span",
        );
    });
    t.density = density;
    rate(
        ui,
        "euclid_div",
        "Division",
        &mut t.division,
        "Division — how long a step is",
    );
}
