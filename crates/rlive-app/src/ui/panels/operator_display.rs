//! Operator's display: oscillator tabs with the envelope and harmonics editors, the filter's
//! response, the LFO and pitch envelopes, and the algorithm picker.

use super::{PanelCtx, PanelDef};
use crate::ui::theme::{self, *};
use egui::{Align2, CornerRadius, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use rlive_core::ParamId;
use rlive_devices::devices::b12::operator as op;
use rlive_devices::devices::b12::operator_waves as waves;
use rlive_devices::dsp::filter::{Circuit, Filter, FilterKind, FilterSettings, FilterSlope};

pub const PANEL: PanelDef = PanelDef {
    name: "operator_display",
    size: Vec2::new(560.0, 200.0),
    views: op::VIEW_LEN,
    draw,
};

const TABS: [&str; 8] = ["A", "B", "C", "D", "Filter", "Pitch", "LFO", "Global"];
const SAMPLE_RATE: f32 = 48_000.0;
const TAB_H: f32 = 18.0;

#[derive(Clone, Copy, PartialEq)]
enum Sub {
    Envelope,
    Harmonics,
}

fn draw(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    let id = ui.scope_id().with(("operator", panel.device.id));
    let mut tab: usize = ui.data(|d| d.get_temp(id)).unwrap_or(7);
    let sub: Sub = ui
        .data(|d| d.get_temp::<u8>(id.with("sub")))
        .map_or(Sub::Envelope, |v| {
            if v == 1 {
                Sub::Harmonics
            } else {
                Sub::Envelope
            }
        });

    let tab_w = (rect.width() / TABS.len() as f32).floor();
    for (i, label) in TABS.iter().enumerate() {
        let r = Rect::from_min_size(
            rect.min + Vec2::new(tab_w * i as f32, 0.0),
            Vec2::new(tab_w - 2.0, TAB_H - 2.0),
        );
        let response = ui.interact(r, id.with(("tab", i)), Sense::click());
        if response.clicked() {
            tab = i;
        }
        let p = ui.painter();
        p.rect_filled(
            r,
            CornerRadius::same(2),
            if tab == i {
                pal().accent
            } else if response.hovered() {
                pal().raised_hover
            } else {
                pal().raised
            },
        );
        p.text(
            r.center(),
            Align2::CENTER_CENTER,
            label,
            theme::font(10.0),
            if tab == i {
                pal().text_dark
            } else {
                pal().text
            },
        );
    }
    ui.data_mut(|d| d.insert_temp(id, tab));

    let body = Rect::from_min_max(rect.min + Vec2::new(0.0, TAB_H + 2.0), rect.max);
    match tab {
        0..=3 => oscillator_view(ui, panel, tab, body, sub, id),
        4 => {
            let filter_mode = ui
                .data(|d| d.get_temp::<u8>(id.with("filter_view")))
                .unwrap_or(0);
            filter_view(ui, panel, body, filter_mode, id);
        }
        5 => envelope_editor(ui, panel, op::P_ENV, body, id.with("pe"), EnvKind::Pitch),
        6 => envelope_editor(ui, panel, op::L_ENV, body, id.with("le"), EnvKind::Lfo),
        _ => algorithm_view(ui, panel, body, id),
    }
}

// ----------------------------------------------------------------------- oscillator and wave

fn oscillator_view(
    ui: &mut egui::Ui,
    panel: &mut PanelCtx,
    osc: usize,
    rect: Rect,
    sub: Sub,
    id: egui::Id,
) {
    let toggle = Rect::from_min_size(
        rect.right_top() + Vec2::new(-62.0, 0.0),
        Vec2::new(60.0, 15.0),
    );
    let response = ui.interact(toggle, id.with(("sub_toggle", osc)), Sense::click());
    if response.clicked() {
        let next = if sub == Sub::Envelope { 1u8 } else { 0u8 };
        ui.data_mut(|d| d.insert_temp(id.with("sub"), next));
    }
    let p = ui.painter();
    p.rect_filled(toggle, CornerRadius::same(2), pal().raised);
    p.text(
        toggle.center(),
        Align2::CENTER_CENTER,
        if sub == Sub::Envelope {
            "Harmonics"
        } else {
            "Envelope"
        },
        theme::font(9.0),
        pal().text,
    );
    let area = Rect::from_min_max(rect.min, Pos2::new(rect.right(), rect.bottom()));
    match sub {
        Sub::Envelope => envelope_editor(
            ui,
            panel,
            op::OSC_BASE[osc] + op::O_ENV,
            area,
            id.with(("osc_env", osc)),
            EnvKind::Osc,
        ),
        Sub::Harmonics => harmonics_editor(ui, panel, osc, area, id),
    }
    if let Some(wave) = panel.value(ParamId(op::OSC_BASE[osc] + op::O_WAVE)) {
        let wave = (wave.round() as usize).min(waves::WAVE_NAMES.len() - 1);
        ui.painter().text(
            rect.left_top() + Vec2::new(4.0, 2.0),
            Align2::LEFT_TOP,
            waves::WAVE_NAMES[wave],
            theme::font(9.0),
            pal().text_dim,
        );
        let current = state(panel).ops[osc].clone();
        preview(ui, rect, wave, &current);
    }
}

/// The small overview of the wave, next to its name.
fn preview(ui: &egui::Ui, rect: Rect, wave: usize, user: &waves::UserWave) {
    let area = Rect::from_min_size(
        rect.left_top() + Vec2::new(70.0, 1.0),
        Vec2::new(60.0, 13.0),
    );
    let partials = if wave == waves::W_USER {
        user.partials.clone()
    } else {
        waves::builtin_partials(wave).to_vec()
    };
    let mut values = [0.0f32; 60];
    for (i, v) in values.iter_mut().enumerate() {
        let x = i as f32 / 60.0 * std::f32::consts::TAU;
        *v = partials
            .iter()
            .enumerate()
            .map(|(k, a)| a * ((k + 1) as f32 * x).sin())
            .sum();
    }
    let peak = values.iter().fold(1e-6f32, |m, v| m.max(v.abs()));
    let points: Vec<Pos2> = values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            Pos2::new(
                area.left() + i as f32,
                area.center().y - v / peak * area.height() * 0.45,
            )
        })
        .collect();
    let p = ui.painter();
    p.rect_filled(area, CornerRadius::same(1), pal().knob_track);
    p.add(egui::Shape::line(points, Stroke::new(1.0, pal().accent)));
}

fn state(panel: &PanelCtx) -> waves::WavesState {
    waves::WavesState::parse(panel.state_text())
}

fn harmonics_editor(ui: &mut egui::Ui, panel: &mut PanelCtx, osc: usize, rect: Rect, id: egui::Id) {
    let plot = Rect::from_min_max(
        rect.min + Vec2::new(0.0, 16.0),
        rect.max - Vec2::new(76.0, 0.0),
    );
    let controls = Rect::from_min_max(Pos2::new(plot.right() + 4.0, plot.top()), rect.max);
    let mut current = state(panel);
    let wave = current.ops[osc].clone();
    let count = wave.count.clamp(1, waves::PARTIALS);

    let p = ui.painter();
    p.rect_filled(plot, CornerRadius::same(2), pal().knob_track);
    let bar = plot.width() / count as f32;
    for k in 0..count {
        let amp = wave.partials.get(k).copied().unwrap_or(0.0).clamp(0.0, 1.0);
        let x = plot.left() + k as f32 * bar;
        p.rect_filled(
            Rect::from_min_max(
                Pos2::new(x + 0.5, plot.bottom() - amp * plot.height()),
                Pos2::new(x + bar - 0.5, plot.bottom()),
            ),
            CornerRadius::ZERO,
            pal().accent,
        );
    }

    let bars = ui.interact(plot, id.with(("bars", osc)), Sense::click_and_drag());
    let locked = id.with(("locked", osc));
    let mut edited = false;
    if bars.drag_started() {
        ui.data_mut(|d| d.remove::<usize>(locked));
    }
    if bars.dragged() || bars.clicked() {
        if let Some(pointer) = bars.interact_pointer_pos() {
            let mut k = (((pointer.x - plot.left()) / bar) as usize).min(count - 1);
            if ui.input(|i| i.modifiers.shift) {
                k = ui.data_mut(|d| *d.get_temp_mut_or_insert_with(locked, || k));
            }
            let amp = (1.0 - (pointer.y - plot.top()) / plot.height()).clamp(0.0, 1.0);
            let allowed = match wave.restrict {
                1 => (k + 1).is_multiple_of(2),
                2 => (k + 1) % 2 == 1,
                _ => true,
            };
            if allowed {
                let target = &mut current.ops[osc];
                let wave_param = ParamId(op::OSC_BASE[osc] + op::O_WAVE);
                let selected = panel.value(wave_param).unwrap_or(0.0).round() as usize;
                if selected != waves::W_USER {
                    target.partials = waves::builtin_partials(selected).to_vec();
                    panel.set(wave_param, waves::W_USER as f64);
                }
                target.partials[k] = amp;
                edited = true;
            }
        }
    }

    bars.context_menu(|ui| {
        for (i, label) in ["All Harmonics", "Even Only", "Odd Only"]
            .into_iter()
            .enumerate()
        {
            if ui.radio(current.ops[osc].restrict == i, label).clicked() {
                current.ops[osc].restrict = i;
                edited = true;
                ui.close();
            }
        }
        ui.separator();
        let mut normalize = current.ops[osc].normalize;
        if ui.checkbox(&mut normalize, "Normalize").changed() {
            current.ops[osc].normalize = normalize;
            edited = true;
            ui.close();
        }
        ui.separator();
        if ui.button("Export .ams...").clicked() {
            export_ams(&current.ops[osc]);
            ui.close();
        }
        if ui.button("Import .ams...").clicked() {
            if let Some(imported) = import_ams() {
                current.ops[osc] = imported;
                panel.set(
                    ParamId(op::OSC_BASE[osc] + op::O_WAVE),
                    waves::W_USER as f64,
                );
                edited = true;
            }
            ui.close();
        }
        ui.separator();
        for (target, name) in ["A", "B", "C", "D"].iter().enumerate() {
            if target == osc {
                continue;
            }
            if ui.button(format!("Copy Waveform to {name}")).clicked() {
                current.ops[target] = current.ops[osc].clone();
                let wave_id = ParamId(op::OSC_BASE[osc] + op::O_WAVE);
                if let Some(value) = panel.value(wave_id) {
                    panel.set(ParamId(op::OSC_BASE[target] + op::O_WAVE), value);
                }
                edited = true;
                ui.close();
            }
        }
    });

    // 16/32/64 and Repeat.
    let mut y = controls.top();
    for n in [16usize, 32, 64] {
        let r = Rect::from_min_size(
            Pos2::new(controls.left(), y),
            Vec2::new(controls.width() - 2.0, 15.0),
        );
        let response = ui.interact(r, id.with(("count", osc, n)), Sense::click());
        if response.clicked() {
            current.ops[osc].count = n;
            edited = true;
        }
        let selected = wave.count == n;
        let p = ui.painter();
        p.rect_filled(
            r,
            CornerRadius::same(2),
            if selected { pal().accent } else { pal().raised },
        );
        p.text(
            r.center(),
            Align2::CENTER_CENTER,
            n.to_string(),
            theme::font(9.0),
            if selected {
                pal().text_dark
            } else {
                pal().text
            },
        );
        y += 17.0;
    }
    y += 3.0;
    let repeat_label = if wave.repeat == 0 {
        "Repeat Off".to_string()
    } else {
        format!("Repeat {}", wave.repeat)
    };
    let r = Rect::from_min_size(
        Pos2::new(controls.left(), y),
        Vec2::new(controls.width() - 2.0, 15.0),
    );
    let response = ui.interact(r, id.with(("repeat", osc)), Sense::click());
    if response.clicked() {
        current.ops[osc].repeat = (wave.repeat + 1) % (waves::REPEAT_MAX + 1);
        edited = true;
    }
    let p = ui.painter();
    p.rect_filled(r, CornerRadius::same(2), pal().raised);
    p.text(
        r.center(),
        Align2::CENTER_CENTER,
        repeat_label,
        theme::font(9.0),
        pal().text,
    );

    if edited {
        panel.set_state(current.to_text());
    }
}

fn export_ams(wave: &waves::UserWave) {
    if let Some(path) = rfd::FileDialog::new()
        .add_filter("Ableton Waveform", &["ams"])
        .save_file()
    {
        let path = path.with_extension("ams");
        if let Err(err) = std::fs::write(&path, waves::to_ams(wave)) {
            eprintln!("Could not write {}: {err}", path.display());
        }
    }
}

fn import_ams() -> Option<waves::UserWave> {
    let path = rfd::FileDialog::new()
        .add_filter("Ableton Waveform", &["ams", "wav"])
        .pick_file()?;
    waves::from_ams(&std::fs::read(path).ok()?)
}

// ------------------------------------------------------------------------------ envelopes

#[derive(Clone, Copy, PartialEq)]
enum EnvKind {
    Osc,
    Pitch,
    Lfo,
    Filter,
}

fn envelope_editor(
    ui: &mut egui::Ui,
    panel: &mut PanelCtx,
    base: u32,
    rect: Rect,
    id: egui::Id,
    kind: EnvKind,
) {
    let plot = rect.shrink2(Vec2::new(4.0, 2.0));
    let plot = Rect::from_min_max(plot.min + Vec2::new(0.0, 14.0), plot.max);
    let p = ui.painter();
    p.rect_filled(plot, CornerRadius::same(2), pal().knob_track);

    let attack_id = ParamId(base + op::E_ATTACK);
    let decay_id = ParamId(base + op::E_DECAY);
    let release_id = ParamId(base + op::E_RELEASE);
    let initial_id = ParamId(base + op::E_INITIAL);
    let peak_id = ParamId(base + op::E_PEAK);
    let sustain_id = ParamId(base + op::E_SUSTAIN);
    let end_id = ParamId(base + op::E_END);

    let Some(level_info) = panel.param(initial_id).cloned() else {
        return;
    };
    let time_x = |ctx: &PanelCtx, param: ParamId, start: f32, span: f32| {
        let n = ctx
            .param(param)
            .map_or(0.0, |i| i.to_normalized(ctx.value(param).unwrap_or(0.0)))
            as f32;
        start + n * span
    };
    let level_y = |ctx: &PanelCtx, param: ParamId| {
        let v = ctx.value(param).unwrap_or(level_info.default);
        let n = level_info.to_normalized(v) as f32;
        plot.bottom() - n * plot.height()
    };

    let width = plot.width();
    let hold = width * 0.08;
    let segment = (width - hold) / 3.0;
    let start = plot.left();
    let (x_a, x_d) = (
        time_x(panel, attack_id, start, segment),
        time_x(panel, decay_id, start + segment, segment),
    );
    let x_s = x_d + hold;
    let x_r = time_x(panel, release_id, x_s, (plot.right() - x_s).max(1.0));

    let (y_i, y_p, y_s) = (
        level_y(panel, initial_id),
        level_y(panel, peak_id),
        level_y(panel, sustain_id),
    );
    let y_e = if kind == EnvKind::Osc {
        plot.bottom()
    } else {
        level_y(panel, end_id)
    };

    let points = [
        Pos2::new(start, y_i),
        Pos2::new(x_a, y_p),
        Pos2::new(x_d, y_s),
        Pos2::new(x_s, y_s),
        Pos2::new(x_r, y_e),
    ];
    let p = ui.painter();
    p.add(egui::Shape::line(
        points.to_vec(),
        Stroke::new(1.5, pal().accent),
    ));

    // Handles: Initial (level), Peak (attack time and level), Sustain (decay time and level),
    // Release end (release time and End level).
    let handle_at = [points[0], points[1], points[2], points[4]];
    let mut changed: [Option<(ParamId, f64)>; 2] = [None, None];
    for (i, centre) in handle_at.iter().enumerate() {
        let r = Rect::from_center_size(*centre, Vec2::splat(12.0));
        let response = ui.interact(r, id.with(("env_handle", i)), Sense::drag());
        let p = ui.painter();
        p.circle_filled(
            *centre,
            if response.hovered() || response.dragged() {
                4.0
            } else {
                3.0
            },
            pal().text,
        );
        if response.dragged() {
            if let Some(pointer) = response.interact_pointer_pos() {
                let ny = ((plot.bottom() - pointer.y) / plot.height()).clamp(0.0, 1.0) as f64;
                let level = level_info.from_normalized(ny);
                let nx_of =
                    |from: f32, span: f32| (((pointer.x - from) / span).clamp(0.0, 1.0)) as f64;
                match i {
                    0 => changed[0] = Some((initial_id, level)),
                    1 => {
                        changed[0] = Some((peak_id, level));
                        if let Some(info) = panel.param(attack_id) {
                            changed[1] =
                                Some((attack_id, info.from_normalized(nx_of(start, segment))));
                        }
                    }
                    2 => {
                        changed[0] = Some((sustain_id, level));
                        if let Some(info) = panel.param(decay_id) {
                            changed[1] = Some((
                                decay_id,
                                info.from_normalized(nx_of(start + segment, segment)),
                            ));
                        }
                    }
                    _ => {
                        if kind != EnvKind::Osc {
                            changed[0] = Some((end_id, level));
                        }
                        if let Some(info) = panel.param(release_id) {
                            changed[1] = Some((
                                release_id,
                                info.from_normalized(nx_of(x_s, (plot.right() - x_s).max(1.0))),
                            ));
                        }
                    }
                }
            }
        }
    }
    for (param, value) in changed.into_iter().flatten() {
        panel.set(param, value);
    }

    // The slope handles between the breakpoints, for the filter and pitch envelopes.
    if matches!(kind, EnvKind::Filter | EnvKind::Pitch) {
        let slopes = [
            (
                base + op::E_A_SLOPE,
                (points[0] + points[1].to_vec2()) / 2.0,
            ),
            (
                base + op::E_D_SLOPE,
                (points[1] + points[2].to_vec2()) / 2.0,
            ),
            (
                base + op::E_R_SLOPE,
                (points[3] + points[4].to_vec2()) / 2.0,
            ),
        ];
        for (slope_id, centre) in slopes {
            let param = ParamId(slope_id);
            let r = Rect::from_center_size(centre, Vec2::splat(9.0));
            let response = ui.interact(r, id.with(("slope", slope_id)), Sense::drag());
            ui.painter().rect_stroke(
                r.shrink(2.0),
                CornerRadius::same(1),
                Stroke::new(1.0, pal().text_dim),
                StrokeKind::Inside,
            );
            if response.dragged() {
                if let Some(info) = panel.param(param) {
                    let value = panel.value(param).unwrap_or(0.0) - response.drag_delta().y as f64;
                    panel.set(param, value.clamp(info.min, info.max));
                }
            }
        }
    }

    // The live level, while a note plays.
    let view_index = match kind {
        EnvKind::Filter => Some(4),
        EnvKind::Pitch => Some(5),
        EnvKind::Lfo => Some(6),
        EnvKind::Osc => None,
    };
    if let (Some(index), Some(reader)) = (view_index, panel.view) {
        let mut view = [0.0f32; op::VIEW_LEN];
        if reader.read(op::VIEW, &mut view) {
            let n = level_info.to_normalized(view[index] as f64) as f32;
            let y = plot.bottom() - n * plot.height();
            ui.painter().line_segment(
                [Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)],
                Stroke::new(1.0, pal().play_green),
            );
        }
    }

    // Envelope commands in the context menu.
    let background = ui.interact(plot, id.with("env_menu"), Sense::click());
    background.context_menu(|ui| {
        let levels = [initial_id, peak_id, sustain_id, end_id];
        for (label, target) in [
            ("All Max", level_info.max),
            ("All Min", level_info.min),
            ("All Mid", (level_info.max + level_info.min) * 0.5),
        ] {
            if ui.button(label).clicked() {
                for param in levels {
                    panel.set(param, target);
                }
                ui.close();
            }
        }
        ui.separator();
        for (target, name) in ["A", "B", "C", "D"].iter().enumerate() {
            let other = op::OSC_BASE[target] + op::O_ENV;
            if other == base {
                continue;
            }
            if kind == EnvKind::Osc && ui.button(format!("Copy Envelope to {name}")).clicked() {
                for offset in [
                    op::E_ATTACK,
                    op::E_DECAY,
                    op::E_RELEASE,
                    op::E_INITIAL,
                    op::E_PEAK,
                    op::E_SUSTAIN,
                    op::E_LOOP,
                    op::E_REPEAT,
                    op::E_LOOP_TIME,
                    op::E_TIME_VEL,
                ] {
                    if let Some(value) = panel.value(ParamId(base + offset)) {
                        panel.set(ParamId(other + offset), value);
                    }
                }
                ui.close();
            }
        }
    });
}

// --------------------------------------------------------------------------------- filter

fn filter_view(ui: &mut egui::Ui, panel: &mut PanelCtx, rect: Rect, mode: u8, id: egui::Id) {
    let toggle = Rect::from_min_size(
        rect.right_top() + Vec2::new(-62.0, 0.0),
        Vec2::new(60.0, 15.0),
    );
    let response = ui.interact(toggle, id.with("filter_toggle"), Sense::click());
    if response.clicked() {
        ui.data_mut(|d| d.insert_temp(id.with("filter_view"), 1 - mode.min(1)));
    }
    let p = ui.painter();
    p.rect_filled(toggle, CornerRadius::same(2), pal().raised);
    p.text(
        toggle.center(),
        Align2::CENTER_CENTER,
        if mode == 0 { "Filter" } else { "Envelope" },
        theme::font(9.0),
        pal().text,
    );
    if mode == 0 {
        envelope_editor(ui, panel, op::F_ENV, rect, id.with("fe"), EnvKind::Filter);
        return;
    }
    let plot = Rect::from_min_max(rect.min + Vec2::new(0.0, 16.0), rect.max);
    let p = ui.painter();
    p.rect_filled(plot, CornerRadius::same(2), pal().knob_track);
    let value =
        |ctx: &PanelCtx, id: u32, default: f64| ctx.value(ParamId(id)).unwrap_or(default) as f32;
    let kind = match value(panel, op::F_TYPE, 0.0).round() as usize {
        1 => FilterKind::HighPass,
        2 => FilterKind::BandPass,
        3 => FilterKind::Notch,
        4 => FilterKind::Morph,
        _ => FilterKind::LowPass,
    };
    let settings = FilterSettings {
        circuit: Circuit::Svf,
        kind,
        slope: if value(panel, op::F_SLOPE, 0.0).round() as usize == 1 {
            FilterSlope::Db24
        } else {
            FilterSlope::Db12
        },
        cutoff: value(panel, op::F_FREQ, 2000.0),
        resonance: value(panel, op::F_RES, 20.0) * 0.01 * 0.98,
        drive: 0.0,
        morph: value(panel, op::F_MORPH, 0.0) * 0.01,
    };
    let mut filter = Filter::new();
    filter.prepare(SAMPLE_RATE as f64);
    filter.set(&settings);
    let mut points = Vec::with_capacity(plot.width() as usize);
    for i in 0..plot.width() as usize {
        let t = i as f32 / plot.width();
        let hz = 20.0 * 1000f32.powf(t);
        let db: f32 = 20.0 * settings.magnitude(hz, SAMPLE_RATE).max(1e-6).log10();
        let y = plot.bottom() - ((db + 48.0) / 72.0).clamp(0.0, 1.0) * plot.height();
        points.push(Pos2::new(plot.left() + i as f32, y));
    }
    p.add(egui::Shape::line(points, Stroke::new(1.5, pal().accent)));
    let curve = ui.interact(plot, id.with("filter_curve"), Sense::click_and_drag());
    if curve.dragged() {
        if let Some(pointer) = curve.interact_pointer_pos() {
            let t = ((pointer.x - plot.left()) / plot.width()).clamp(0.0, 1.0);
            let hz = 20.0 * 1000f32.powf(t) as f64;
            panel.set(ParamId(op::F_FREQ), hz);
            let res =
                (1.0 - (pointer.y - plot.top()) / plot.height()).clamp(0.0, 1.0) as f64 * 100.0;
            panel.set(ParamId(op::F_RES), res);
        }
    }
    curve.context_menu(|ui| {
        if ui.button("Play By Key").clicked() {
            panel.set(ParamId(op::F_FREQ), 466.0);
            panel.set(ParamId(op::F_FREQ_KEY), 100.0);
            ui.close();
        }
    });
}

// ------------------------------------------------------------------------------ algorithms

fn algorithm_view(ui: &mut egui::Ui, panel: &mut PanelCtx, rect: Rect, id: egui::Id) {
    let selected = panel.value(ParamId(op::G_ALGORITHM)).unwrap_or(0.0).round() as usize;
    let count = op::ALGORITHMS.len();
    let columns = 6;
    let cell = Vec2::new(rect.width() / columns as f32, rect.height() / 2.0);
    for (i, algorithm) in op::ALGORITHMS.iter().enumerate() {
        let (col, row) = (i % columns, i / columns);
        let r = Rect::from_min_size(
            rect.min + Vec2::new(cell.x * col as f32, cell.y * row as f32),
            cell,
        )
        .shrink(3.0);
        let response = ui.interact(r, id.with(("alg", i)), Sense::click());
        if response.clicked() {
            panel.set(ParamId(op::G_ALGORITHM), i as f64);
        }
        draw_algorithm(ui, r, algorithm, i == selected, response.hovered(), count);
    }
}

/// How many steps an oscillator is from the output: 0 for a carrier, otherwise one more than the
/// furthest oscillator it modulates.
fn depth(algorithm: &op::Algorithm, osc: usize, seen: u8) -> usize {
    if algorithm.carriers & (1 << osc) != 0
        && algorithm.modulators.iter().all(|m| m & (1 << osc) == 0)
    {
        return 0;
    }
    let mut d = 0;
    for target in 0..4 {
        if target != osc
            && algorithm.modulators[target] & (1 << osc) != 0
            && seen & (1 << target) == 0
        {
            d = d.max(1 + depth(algorithm, target, seen | (1 << osc)));
        }
    }
    d
}

fn draw_algorithm(
    ui: &egui::Ui,
    rect: Rect,
    algorithm: &op::Algorithm,
    selected: bool,
    hovered: bool,
    _count: usize,
) {
    let p = ui.painter();
    p.rect_filled(
        rect,
        CornerRadius::same(3),
        if selected {
            pal().accent
        } else if hovered {
            pal().raised_hover
        } else {
            pal().raised
        },
    );
    let levels: [usize; 4] = std::array::from_fn(|o| depth(algorithm, o, 0));
    let tallest = levels.iter().copied().max().unwrap_or(0) + 1;
    let mut per_level = [0usize; 5];
    let mut position = [Pos2::ZERO; 4];
    for o in 0..4 {
        let level = levels[o];
        let total = levels.iter().filter(|l| **l == level).count();
        let index = per_level[level];
        per_level[level] += 1;
        let x = rect.left() + rect.width() * (index as f32 + 1.0) / (total as f32 + 1.0);
        let y = rect.bottom() - rect.height() * (level as f32 + 0.5) / tallest as f32;
        position[o] = Pos2::new(x, y);
    }
    let link = Stroke::new(
        1.0,
        if selected {
            pal().text_dark
        } else {
            pal().text_dim
        },
    );
    for target in 0..4 {
        for source in 0..4 {
            if algorithm.modulators[target] & (1 << source) != 0 {
                p.line_segment([position[source], position[target]], link);
            }
        }
    }
    let names = ["A", "B", "C", "D"];
    for o in 0..4 {
        let box_size = Vec2::splat(13.0);
        let square = Rect::from_center_size(position[o], box_size);
        let carrier = algorithm.carriers & (1 << o) != 0;
        p.rect_filled(
            square,
            CornerRadius::same(2),
            if carrier {
                pal().text
            } else {
                pal().panel_dark
            },
        );
        p.text(
            square.center(),
            Align2::CENTER_CENTER,
            names[o],
            theme::font(9.0),
            if carrier { pal().text_dark } else { pal().text },
        );
    }
}
