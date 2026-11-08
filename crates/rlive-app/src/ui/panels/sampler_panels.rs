//! Sampler's panels: the tabs (Sample, Pitch/Osc, Filter/Global, Modulation, MIDI) and the Zone
//! Editor with its layer list.

use super::sample_ui::*;
use super::{PanelCtx, PanelDef};
use crate::session::SamplingCommand;
use crate::ui::theme::*;
use egui::{DragValue, Pos2, Rect, RichText, ScrollArea, Sense, Stroke, StrokeKind, Vec2};
use rlive_core::ParamId;
use rlive_devices::devices::b10::common::WAVE_VALUES;
use rlive_devices::devices::b10::multi::{self as multi, Layer, RrMode, Zone, ZoneMap};
use rlive_devices::devices::b10::multi_params as sp;
use rlive_devices::sampler::p as old;
use std::hash::{Hash, Hasher};

pub const TABS: PanelDef = PanelDef {
    name: "sampler_tabs",
    size: Vec2::new(470.0, 205.0),
    views: multi::VIEW_LEN,
    draw: draw_tabs,
};

pub const ZONES: PanelDef = PanelDef {
    name: "sampler_zones",
    size: Vec2::new(560.0, 245.0),
    views: 0,
    draw: draw_zones,
};

/// The zone map parsed from the device's text, kept between frames.
#[derive(Default, Clone)]
struct CachedMap {
    hash: u64,
    map: ZoneMap,
}

fn zones(ui: &egui::Ui, panel: &PanelCtx) -> ZoneMap {
    let id = ui.scope_id().with(("zones-cache", panel.device.id));
    let text = panel.state_text().unwrap_or("");
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    let hash = hasher.finish();
    let cached: CachedMap = memory(ui, id);
    if cached.hash == hash {
        return cached.map;
    }
    let map = ZoneMap::parse((!text.is_empty()).then_some(text));
    remember(
        ui,
        id,
        CachedMap {
            hash,
            map: map.clone(),
        },
    );
    map
}

fn frame_rect(ui: &egui::Ui, rect: Rect) {
    ui.painter().rect_filled(rect, 1.0, pal().field_bg);
    ui.painter().rect_stroke(
        rect,
        1.0,
        Stroke::new(1.0, pal().separator),
        StrokeKind::Inside,
    );
}

fn params_tab(ui: &mut egui::Ui, panel: &mut PanelCtx, rows: &[&[(&str, u32)]]) {
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for row in rows {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for (label, id) in row.iter() {
                        param_control(ui, panel, *id, label, 110.0);
                    }
                });
            }
        });
}

fn env_rows(prefix: &str, base: u32) -> Vec<[(String, u32); 4]> {
    let at = |n: u32| base + n;
    vec![
        [
            (format!("{prefix} Init"), at(0)),
            (format!("{prefix} Peak"), at(1)),
            (format!("{prefix} Sus"), at(2)),
            (format!("{prefix} End"), at(3)),
        ],
        [
            (format!("{prefix} A"), at(4)),
            (format!("{prefix} D"), at(5)),
            (format!("{prefix} R"), at(6)),
            (format!("{prefix} Loop"), at(10)),
        ],
    ]
}

fn dynamic_rows(ui: &mut egui::Ui, panel: &mut PanelCtx, rows: &[[(String, u32); 4]]) {
    for row in rows {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for (label, id) in row.iter() {
                param_control(ui, panel, *id, label, 110.0);
            }
        });
    }
}

fn draw_tabs(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    frame_rect(ui, rect);
    let id = ui.scope_id().with(("sampler_tabs", panel.device.id));
    let mut tab: usize = memory(ui, id.with("tab"));
    let mut inner = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(3.0)));
    if let Some(next) = tab_strip(
        &mut inner,
        &["Sample", "Pitch/Osc", "Filter/Global", "Modulation", "MIDI"],
        tab,
    ) {
        tab = next;
        remember(ui, id.with("tab"), tab);
    }
    match tab {
        0 => sample_tab(&mut inner, panel),
        1 => {
            params_tab(
                &mut inner,
                panel,
                &[
                    &[
                        ("Osc", sp::OSC_ON),
                        ("Type", sp::OSC_TYPE),
                        ("Mode", sp::OSC_MODE),
                        ("Volume", sp::OSC_VOL),
                    ],
                    &[
                        ("Vol < Vel", sp::OSC_VOL_VEL),
                        ("Fixed", sp::OSC_FIXED),
                        ("Freq", sp::OSC_FREQ),
                        ("Multi", sp::OSC_MULTI),
                    ],
                    &[
                        ("Coarse", sp::OSC_COARSE),
                        ("Fine", sp::OSC_FINE),
                        ("Pitch Env", sp::PE_ON),
                        ("Amount", sp::PE_AMOUNT),
                    ],
                    &[
                        ("Spread", sp::SPREAD),
                        ("Transpose", old::TUNE),
                        ("Detune", sp::DETUNE),
                        ("Zn Shft", sp::ZONE_SHIFT),
                    ],
                    &[
                        ("Glide", sp::GLIDE),
                        ("Time", sp::GLIDE_TIME),
                        ("Root Key", old::ROOT),
                        ("Reverse", sp::REVERSE),
                    ],
                ],
            );
            dynamic_rows(&mut inner, panel, &env_rows("Pitch", sp::PE_BASE));
            dynamic_rows(&mut inner, panel, &env_rows("Osc", sp::OE_BASE));
        }
        2 => {
            params_tab(
                &mut inner,
                panel,
                &[
                    &[
                        ("Filter", sp::F_ON),
                        ("Type", sp::F_TYPE),
                        ("Circuit", sp::F_CIRCUIT),
                        ("Slope", sp::F_SLOPE),
                    ],
                    &[
                        ("Freq", sp::F_FREQ),
                        ("Res", sp::F_RES),
                        ("Drive", sp::F_DRIVE),
                        ("Morph", sp::F_MORPH),
                    ],
                    &[
                        ("F. Env", sp::FE_ON),
                        ("Amount", sp::FE_AMOUNT),
                        ("Shaper", sp::SHAPER_ON),
                        ("Type", sp::SHAPER_TYPE),
                    ],
                    &[
                        ("Amount", sp::SHAPER_AMOUNT),
                        ("Order", sp::SHAPER_ORDER),
                        ("Decay", sp::V_DECAY),
                        ("Sustain", sp::V_SUSTAIN),
                    ],
                    &[
                        ("Time", sp::TIME),
                        ("Time < Key", sp::TIME_KEY),
                        ("Pan < Rnd", sp::PAN_RND),
                        ("Voices", sp::VOICES),
                    ],
                    &[
                        ("Retrigger", sp::RETRIGGER),
                        ("Loop", sp::V_LOOP),
                        ("Repeat", sp::V_REPEAT),
                        ("Loop Time", sp::V_LOOP_TIME),
                    ],
                    &[
                        ("Attack", old::ATTACK),
                        ("Release", old::RELEASE),
                        ("Initial", sp::V_INITIAL),
                        ("Peak", sp::V_PEAK),
                    ],
                ],
            );
            dynamic_rows(&mut inner, panel, &env_rows("F.Env", sp::FE_BASE));
        }
        3 => {
            params_tab(
                &mut inner,
                panel,
                &[
                    &[
                        ("Aux Dest A", sp::AE_DEST_A),
                        ("Amt A", sp::AE_AMT_A),
                        ("Dest B", sp::AE_DEST_B),
                        ("Amt B", sp::AE_AMT_B),
                    ],
                    &[
                        ("LFO 1", sp::L1_ON),
                        ("Type", sp::L1_TYPE),
                        ("Rate", sp::L1_SYNC),
                        ("Freq", sp::L1_FREQ),
                    ],
                    &[
                        ("Beats", sp::L1_BEATS),
                        ("Attack", sp::L1_ATTACK),
                        ("Retrig", sp::L1_RETRIG),
                        ("Offset", sp::L1_OFFSET),
                    ],
                    &[
                        ("Vol", sp::L1_TO_VOL),
                        ("Pan", sp::L1_TO_PAN),
                        ("Filter", sp::L1_TO_FILTER),
                        ("Pitch", sp::L1_TO_PITCH),
                    ],
                    &[
                        ("LFO 2", sp::L2_ON),
                        ("Type", sp::L2_TYPE),
                        ("Rate", sp::L2_SYNC),
                        ("Freq", sp::L2_FREQ),
                    ],
                    &[
                        ("Stereo", sp::L2_STEREO),
                        ("Phase", sp::L2_PHASE),
                        ("Spin", sp::L2_SPIN),
                        ("Dest A", sp::L2_DEST_A),
                    ],
                    &[
                        ("Amt A", sp::L2_AMT_A),
                        ("Dest B", sp::L2_DEST_B),
                        ("Amt B", sp::L2_AMT_B),
                        ("Key", sp::L2_KEY),
                    ],
                    &[
                        ("LFO 3", sp::L3_ON),
                        ("Type", sp::L3_TYPE),
                        ("Rate", sp::L3_SYNC),
                        ("Freq", sp::L3_FREQ),
                    ],
                    &[
                        ("Stereo", sp::L3_STEREO),
                        ("Phase", sp::L3_PHASE),
                        ("Spin", sp::L3_SPIN),
                        ("Dest A", sp::L3_DEST_A),
                    ],
                    &[
                        ("Amt A", sp::L3_AMT_A),
                        ("Dest B", sp::L3_DEST_B),
                        ("Amt B", sp::L3_AMT_B),
                        ("Key", sp::L3_KEY),
                    ],
                ],
            );
            dynamic_rows(&mut inner, panel, &env_rows("Aux", sp::AE_BASE));
        }
        _ => {
            params_tab(
                &mut inner,
                panel,
                &[
                    &[
                        ("Key Dest A", sp::M_KEY_DEST_A),
                        ("Amt A", sp::M_KEY_AMT_A),
                        ("Dest B", sp::M_KEY_DEST_B),
                        ("Amt B", sp::M_KEY_AMT_B),
                    ],
                    &[
                        ("Vel Dest A", sp::M_VEL_DEST_A),
                        ("Amt A", sp::M_VEL_AMT_A),
                        ("Dest B", sp::M_VEL_DEST_B),
                        ("Amt B", sp::M_VEL_AMT_B),
                    ],
                    &[
                        ("Rel Dest A", sp::M_REL_DEST_A),
                        ("Amt A", sp::M_REL_AMT_A),
                        ("Dest B", sp::M_REL_DEST_B),
                        ("Amt B", sp::M_REL_AMT_B),
                    ],
                    &[
                        ("AT Dest A", sp::M_AT_DEST_A),
                        ("Amt A", sp::M_AT_AMT_A),
                        ("Dest B", sp::M_AT_DEST_B),
                        ("Amt B", sp::M_AT_AMT_B),
                    ],
                    &[
                        ("MW Dest A", sp::M_MW_DEST_A),
                        ("Amt A", sp::M_MW_AMT_A),
                        ("Dest B", sp::M_MW_DEST_B),
                        ("Amt B", sp::M_MW_AMT_B),
                    ],
                    &[
                        ("FC Dest A", sp::M_FC_DEST_A),
                        ("Amt A", sp::M_FC_AMT_A),
                        ("Dest B", sp::M_FC_DEST_B),
                        ("Amt B", sp::M_FC_AMT_B),
                    ],
                    &[
                        ("PB Dest A", sp::M_PB_DEST_A),
                        ("Amt A", sp::M_PB_AMT_A),
                        ("Dest B", sp::M_PB_DEST_B),
                        ("Amt B", sp::M_PB_AMT_B),
                    ],
                    &[
                        ("PB Range", sp::PB_RANGE),
                        ("Selector", sp::SELECTOR),
                        ("Reverse", sp::REVERSE),
                        ("Interp", sp::INTERPOLATION),
                    ],
                ],
            );
        }
    }
}

fn sample_tab(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 64.0), Sense::hover());
    frame_rect(ui, rect);
    let mut wave = [0.0f32; WAVE_VALUES];
    if let Some(v) = panel.view {
        v.read(multi::VIEW_WAVE, &mut wave);
    }
    draw_wave(ui, rect.shrink(1.0), &wave);
    params_tab(
        ui,
        panel,
        &[
            &[
                ("Reverse", sp::REVERSE),
                ("Snap", sp::SNAP),
                ("Interpolation", sp::INTERPOLATION),
                ("RAM", sp::RAM_MODE),
            ],
            &[
                ("Loop Fades", sp::CONST_POWER),
                ("Start", old::START),
                ("One-shot", old::ONE_SHOT),
                ("Selector", sp::SELECTOR),
            ],
        ],
    );
    ui.horizontal(|ui| {
        if small_button(ui, "To Simpler", 62.0).clicked() {
            panel.sampling(SamplingCommand::SamplerToSimpler);
        }
    });
}

fn zone_rect(lane: Rect, z: &Zone, scale: u8) -> Rect {
    let x = |v: u8| lane.left() + v as f32 / scale as f32 * lane.width();
    Rect::from_min_max(
        Pos2::new(x(z.lo), lane.top()),
        Pos2::new(x(z.hi), lane.bottom()),
    )
}

fn draw_zones(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    frame_rect(ui, rect);
    let id = ui.scope_id().with(("sampler_zones", panel.device.id));
    let mut map = zones(ui, panel);
    let original = map.clone();
    let mut changed = false;
    let selected = panel
        .value(ParamId(sp::LAYER_SELECT))
        .unwrap_or(0.0)
        .round() as usize;
    let selected = selected.min(map.layers.len().saturating_sub(1));

    let mut top = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(3.0)));
    let mut view: usize = memory(ui, id.with("view"));
    top.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        if let Some(next) = tab_strip(ui, &["Key", "Vel", "Sel"], view) {
            view = next;
            remember(ui, id.with("view"), view);
        }
        if crate::ui::widgets::state_button(
            ui,
            Vec2::new(30.0, 16.0),
            map.auto_select,
            pal().accent,
            "Auto",
        )
        .clicked()
        {
            map.auto_select = !map.auto_select;
            changed = true;
        }
        let label = if map.power_fades { "Pow" } else { "Lin" };
        if crate::ui::widgets::state_button(
            ui,
            Vec2::new(30.0, 16.0),
            map.power_fades,
            pal().accent,
            label,
        )
        .clicked()
        {
            map.power_fades = !map.power_fades;
            changed = true;
        }
        if crate::ui::widgets::state_button(
            ui,
            Vec2::new(26.0, 16.0),
            map.round_robin,
            pal().accent,
            "RR",
        )
        .clicked()
        {
            map.round_robin = !map.round_robin;
            changed = true;
        }
        let modes = [
            ("Forward", RrMode::Forward),
            ("Backward", RrMode::Backward),
            ("Other", RrMode::Other),
            ("Random", RrMode::Random),
        ];
        let current = modes
            .iter()
            .position(|(_, m)| *m == map.rr_mode)
            .unwrap_or(0);
        let mut picked = current;
        egui::ComboBox::from_id_salt(id.with("rr-mode"))
            .width(70.0)
            .selected_text(RichText::new(modes[current].0).size(10.0))
            .show_ui(ui, |ui| {
                for (i, (label, _)) in modes.iter().enumerate() {
                    ui.selectable_value(&mut picked, i, *label);
                }
            });
        if picked != current {
            map.rr_mode = modes[picked].1;
            changed = true;
        }
        let mut reset = map.rr_reset_beats as f32;
        let response = ui.add(
            DragValue::new(&mut reset)
                .speed(0.1)
                .range(0.0..=64.0)
                .suffix(" bt"),
        );
        if response.changed() {
            map.rr_reset_beats = reset as f64;
            changed = true;
        }
        if small_button(ui, "Add", 28.0).clicked() {
            let used: Vec<u32> = map.layers.iter().map(|l| l.slot).collect();
            let slot = (0..multi::MAX_LAYERS as u32)
                .find(|s| !used.contains(s))
                .unwrap_or(0);
            map.layers
                .push(Layer::new(&format!("Layer {}", slot + 1), slot));
            changed = true;
            panel.load_sample(slot as usize);
        }
        if small_button(ui, "Dup", 28.0).clicked() {
            if let Some(mut l) = map.layers.get(selected).cloned() {
                l.name.push_str(" copy");
                let used: Vec<u32> = map.layers.iter().map(|l| l.slot).collect();
                if let Some(slot) = (0..multi::MAX_LAYERS as u32).find(|s| !used.contains(s)) {
                    l.slot = slot;
                    map.layers.push(l);
                    changed = true;
                }
            }
        }
        if small_button(ui, "Del", 28.0).clicked() && !map.layers.is_empty() {
            map.layers.remove(selected);
            changed = true;
        }
        if small_button(ui, "Equal", 36.0).clicked() {
            map.distribute_equally();
            changed = true;
        }
        if small_button(ui, "Root", 32.0).clicked() {
            map.distribute_around_root();
            changed = true;
        }
        let sorts = ["Sort", "Name", "Key", "Vel", "Sel"];
        let mut sort_pick = 0usize;
        egui::ComboBox::from_id_salt(id.with("sort"))
            .width(42.0)
            .selected_text(RichText::new(sorts[0]).size(10.0))
            .show_ui(ui, |ui| {
                for (i, s) in sorts.iter().enumerate().skip(1) {
                    ui.selectable_value(&mut sort_pick, i, *s);
                }
            });
        match sort_pick {
            1 => map.layers.sort_by(|a, b| a.name.cmp(&b.name)),
            2 => map.layers.sort_by_key(|l| l.key.lo),
            3 => map.layers.sort_by_key(|l| l.vel.lo),
            4 => map.layers.sort_by_key(|l| l.sel.lo),
            _ => {}
        }
        changed |= sort_pick != 0;
        if small_button(ui, "Norm", 32.0).clicked() {
            panel.sampling(SamplingCommand::NormalizeLayerVolume(selected));
        }
        if small_button(ui, "NPan", 32.0).clicked() {
            panel.sampling(SamplingCommand::NormalizeLayerPan(selected));
        }
    });

    let list = Rect::from_min_size(
        rect.min + Vec2::new(3.0, 22.0),
        Vec2::new(150.0, rect.height() - 78.0),
    );
    let editor = Rect::from_min_size(
        Pos2::new(list.right() + 4.0, list.top()),
        Vec2::new(rect.right() - list.right() - 8.0, list.height()),
    );
    // Layer list.
    let mut list_ui = ui.new_child(egui::UiBuilder::new().max_rect(list));
    let row_h = 15.0;
    ScrollArea::vertical()
        .id_salt(id.with("list"))
        .auto_shrink([false, false])
        .show(&mut list_ui, |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            let mut active = [0f32; multi::ACTIVE_WORDS];
            if map.auto_select {
                if let Some(v) = panel.view {
                    v.read(multi::VIEW_ACTIVE, &mut active);
                }
            }
            let sounding = |i: usize| {
                map.auto_select
                    && (active[(i / 16).min(multi::ACTIVE_WORDS - 1)] as u32) & (1 << (i % 16)) != 0
            };
            for (i, layer) in map.layers.iter().enumerate() {
                let on = i == selected || sounding(i);
                if crate::ui::widgets::state_button(
                    ui,
                    Vec2::new(140.0, row_h),
                    on,
                    pal().accent,
                    &layer.name,
                )
                .clicked()
                {
                    panel.set(ParamId(sp::LAYER_SELECT), i as f64);
                }
            }
        });
    // Zone editor.
    let scale = 127u8;
    let lane_h = 14.0;
    let response = ui.interact(editor, id.with("editor"), Sense::click_and_drag());
    let painter = ui.painter_at(editor);
    painter.rect_filled(editor, 0.0, pal().grid_bg);
    let lanes: Vec<(usize, Rect)> = (0..map.layers.len())
        .map(|i| {
            let lane = Rect::from_min_size(
                editor.min + Vec2::new(0.0, i as f32 * (lane_h + 1.0)),
                Vec2::new(editor.width(), lane_h),
            );
            (i, lane)
        })
        .collect();
    let zone_of = |l: &Layer| match view {
        1 => l.vel,
        2 => l.sel,
        _ => l.key,
    };
    for (i, lane) in &lanes {
        let z = zone_of(&map.layers[*i]);
        let r = zone_rect(*lane, &z, scale);
        painter.rect_filled(
            r,
            1.0,
            if *i == selected {
                pal().accent
            } else {
                pal().raised
            },
        );
    }
    let drag = id.with("drag-zone");
    if response.drag_started() {
        if let Some(pos) = response.interact_pointer_pos() {
            let hit = lanes
                .iter()
                .find(|(_, lane)| lane.contains(pos))
                .map(|(i, _)| *i);
            if let Some(i) = hit {
                panel.set(ParamId(sp::LAYER_SELECT), i as f64);
                let lane = lanes[i].1;
                let z = zone_of(&map.layers[i]);
                let r = zone_rect(lane, &z, scale);
                let edge = if (pos.x - r.left()).abs() < 6.0 {
                    1
                } else if (pos.x - r.right()).abs() < 6.0 {
                    2
                } else {
                    3
                };
                remember(ui, drag, (i, edge, pos.x, z.lo, z.hi));
            }
        }
    }
    if response.dragged() {
        let (i, edge, x0, lo0, hi0): (usize, i32, f32, u8, u8) = memory(ui, drag);
        if edge != 0 && i < map.layers.len() {
            if let Some(pos) = response.interact_pointer_pos() {
                let delta = ((pos.x - x0) / editor.width() * scale as f32) as i32;
                let top = scale as i32;
                let lo_min = if view == 1 { 1 } else { 0 };
                let z = match view {
                    1 => &mut map.layers[i].vel,
                    2 => &mut map.layers[i].sel,
                    _ => &mut map.layers[i].key,
                };
                match edge {
                    1 => z.lo = (lo0 as i32 + delta).clamp(lo_min, hi0 as i32) as u8,
                    2 => z.hi = (hi0 as i32 + delta).clamp(lo0 as i32, top) as u8,
                    _ => {
                        let width = hi0 as i32 - lo0 as i32;
                        let lo = (lo0 as i32 + delta).clamp(lo_min, top - width);
                        z.lo = lo as u8;
                        z.hi = (lo + width) as u8;
                    }
                }
                changed = true;
            }
        }
    }
    if response.drag_stopped() {
        remember(ui, drag, (0usize, 0i32, 0.0f32, 0u8, 0u8));
    }

    // The selected layer's own controls.
    if let Some(layer) = map.layers.get_mut(selected) {
        let fields = Rect::from_min_max(
            Pos2::new(rect.left() + 3.0, list.bottom() + 3.0),
            Pos2::new(rect.right() - 3.0, rect.bottom() - 2.0),
        );
        let mut field_ui = ui.new_child(egui::UiBuilder::new().max_rect(fields));
        layer_fields(&mut field_ui, layer, view, &mut changed);
    }

    let mut bar = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(
        Pos2::new(rect.right() - 80.0, rect.bottom() - 18.0),
        rect.max,
    )));
    if small_button(&mut bar, "Load Sample", 76.0).clicked() {
        let slot = map.layers.get(selected).map_or(0, |l| l.slot as usize);
        panel.load_sample(slot);
    }

    if changed && map != original {
        panel.set_state(map.serialize());
    }
}

fn layer_fields(ui: &mut egui::Ui, layer: &mut Layer, view: usize, changed: &mut bool) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(4.0, 2.0);
        let mut text = layer.name.clone();
        if ui
            .add(
                egui::TextEdit::singleline(&mut text)
                    .desired_width(90.0)
                    .font(font(10.0)),
            )
            .changed()
        {
            layer.name = text;
            *changed = true;
        }
        let zone = match view {
            1 => &mut layer.vel,
            2 => &mut layer.sel,
            _ => &mut layer.key,
        };
        for (label, value) in [
            ("Fade Lo", &mut zone.fade_lo),
            ("Fade Hi", &mut zone.fade_hi),
        ] {
            ui.label(RichText::new(label).size(10.0).color(pal().text_dim));
            if ui
                .add(DragValue::new(value).speed(0.2).range(0..=127))
                .changed()
            {
                *changed = true;
            }
        }
        let mut drag = |ui: &mut egui::Ui,
                        label: &str,
                        value: &mut f32,
                        step: f64,
                        range: std::ops::RangeInclusive<f32>| {
            ui.label(RichText::new(label).size(10.0).color(pal().text_dim));
            if ui
                .add(DragValue::new(value).speed(step).range(range))
                .changed()
            {
                *changed = true;
            }
        };
        drag(ui, "Root", &mut layer.root, 1.0, 0.0..=127.0);
        drag(ui, "Detune", &mut layer.detune, 1.0, -50.0..=50.0);
        drag(ui, "Vol", &mut layer.volume_db, 0.2, -70.0..=24.0);
        drag(ui, "Pan", &mut layer.pan, 0.01, -1.0..=1.0);
        let mut drag64 = |ui: &mut egui::Ui, label: &str, value: &mut f64| {
            ui.label(RichText::new(label).size(10.0).color(pal().text_dim));
            let mut v = *value;
            if ui
                .add(DragValue::new(&mut v).speed(1.0).range(0.0..=f64::MAX))
                .changed()
            {
                *value = v;
                *changed = true;
            }
        };
        drag64(ui, "Loop Start", &mut layer.loop_start);
        drag64(ui, "Loop End", &mut layer.loop_end);
        drag64(ui, "XFade", &mut layer.crossfade);
        drag64(ui, "Rel Loop", &mut layer.release_loop);
        let mut menu = |ui: &mut egui::Ui, label: &str, value: &mut u8, options: &[&str]| {
            ui.label(RichText::new(label).size(10.0).color(pal().text_dim));
            let current = (*value as usize).min(options.len() - 1);
            let mut picked = current;
            egui::ComboBox::from_id_salt(label)
                .width(74.0)
                .selected_text(RichText::new(options[current]).size(10.0))
                .show_ui(ui, |ui| {
                    for (i, o) in options.iter().enumerate() {
                        ui.selectable_value(&mut picked, i, *o);
                    }
                });
            if picked != current {
                *value = picked as u8;
                *changed = true;
            }
        };
        menu(
            ui,
            "Sustain",
            &mut layer.sustain,
            &["Off", "Loop", "Back+Forth"],
        );
        menu(
            ui,
            "Release",
            &mut layer.release,
            &["Off", "Release", "Release Loop", "BF Loop"],
        );
        let mut link = layer.link;
        if ui
            .checkbox(&mut link, RichText::new("Link").size(10.0))
            .changed()
        {
            layer.link = link;
            *changed = true;
        }
    });
}
