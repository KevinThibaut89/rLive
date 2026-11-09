//! The small controls the tools' panels are made of.

use crate::ui::piano_roll::grid_label;
use crate::ui::theme::*;
use crate::ui::widgets::{self, info};
use egui::{RichText, Vec2};
use rlive_core::PPQ;
use rlive_project::note_view::{spell, Spelling};

const LABEL_W: f32 = 62.0;

fn label(ui: &mut egui::Ui, text: &str) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(LABEL_W, 15.0), egui::Sense::hover());
    ui.painter().text(
        rect.left_center(),
        egui::Align2::LEFT_CENTER,
        text,
        font(10.5),
        pal().text_dim,
    );
}

/// A share shown as a percentage.
pub fn percent(ui: &mut egui::Ui, name: &str, v: &mut f64, lo: f64, hi: f64, tip: &str) {
    ui.horizontal(|ui| {
        label(ui, name);
        let mut shown = *v * 100.0;
        let r = ui.add(
            egui::DragValue::new(&mut shown)
                .range(lo * 100.0..=hi * 100.0)
                .speed(0.5)
                .suffix("%")
                .max_decimals(1),
        );
        if info(r, tip).changed() {
            *v = shown / 100.0;
        }
    });
}

pub fn int(ui: &mut egui::Ui, name: &str, v: &mut i32, lo: i32, hi: i32, unit: &str, tip: &str) {
    ui.horizontal(|ui| {
        label(ui, name);
        let r = ui.add(
            egui::DragValue::new(v)
                .range(lo..=hi)
                .speed(0.1)
                .suffix(unit),
        );
        info(r, tip);
    });
}

pub fn count(ui: &mut egui::Ui, name: &str, v: &mut u32, lo: u32, hi: u32, tip: &str) {
    let mut shown = *v as i32;
    int(ui, name, &mut shown, lo as i32, hi as i32, "", tip);
    *v = shown.max(0) as u32;
}

pub fn float(ui: &mut egui::Ui, name: &str, v: &mut f64, lo: f64, hi: f64, unit: &str, tip: &str) {
    ui.horizontal(|ui| {
        label(ui, name);
        let r = ui.add(
            egui::DragValue::new(v)
                .range(lo..=hi)
                .speed((hi - lo) / 200.0)
                .suffix(unit)
                .max_decimals(2),
        );
        info(r, tip);
    });
}

/// Velocity from 1 to 127.
pub fn velocity(ui: &mut egui::Ui, name: &str, v: &mut f32, tip: &str) {
    ui.horizontal(|ui| {
        label(ui, name);
        let mut shown = *v * 127.0;
        let r = ui.add(
            egui::DragValue::new(&mut shown)
                .range(1.0..=127.0)
                .speed(0.5)
                .max_decimals(0),
        );
        if info(r, tip).changed() {
            *v = shown / 127.0;
        }
    });
}

/// A key, spelled as the piano ruler does.
pub fn key(ui: &mut egui::Ui, name: &str, v: &mut u8, tip: &str) {
    ui.horizontal(|ui| {
        label(ui, name);
        let mut shown = i32::from(*v);
        let r = ui.add(
            egui::DragValue::new(&mut shown)
                .range(0..=127)
                .speed(0.2)
                .custom_formatter(|x, _| spell(x as u8, Spelling::Sharps, None)),
        );
        if info(r, tip).changed() {
            *v = shown.clamp(0, 127) as u8;
        }
    });
}

fn rate_label(t: u64) -> String {
    match t {
        t if t == 4 * PPQ => "1".into(),
        t if t == 2 * PPQ => "1/2".into(),
        t if t == PPQ / 16 => "1/64".into(),
        t if t == PPQ / 32 => "1/128".into(),
        t => grid_label(t),
    }
}

const RATES: [u64; 12] = [
    4 * PPQ,
    2 * PPQ,
    PPQ,
    PPQ / 2,
    PPQ / 4,
    PPQ / 8,
    PPQ / 16,
    PPQ / 32,
    PPQ / 3,
    PPQ / 6,
    PPQ / 12,
    PPQ / 24,
];

/// A length from a list of note values, with triplets.
pub fn rate(ui: &mut egui::Ui, salt: &str, name: &str, v: &mut u64, tip: &str) {
    ui.horizontal(|ui| {
        label(ui, name);
        let r = egui::ComboBox::from_id_salt(("tool_rate", salt))
            .width(70.0)
            .selected_text(rate_label(*v))
            .show_ui(ui, |ui| {
                for r in RATES {
                    ui.selectable_value(v, r, rate_label(r));
                }
            });
        info(r.response, tip);
    });
}

/// A row of buttons, one lit: choosing between a few named things.
pub fn choice<T: PartialEq + Copy>(ui: &mut egui::Ui, v: &mut T, options: &[(T, &str)], tip: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(2.0, 2.0);
        let w = (ui.available_width() / options.len() as f32 - 2.0).clamp(30.0, 70.0);
        for (option, text) in options {
            let on = *v == *option;
            let r = widgets::state_button(ui, Vec2::new(w, 16.0), on, pal().accent, text);
            if info(r, tip).clicked() {
                *v = *option;
            }
        }
    });
}

/// A drop-down of named values.
pub fn menu<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    salt: &str,
    name: &str,
    v: &mut T,
    options: &[(T, &str)],
    tip: &str,
) {
    ui.horizontal(|ui| {
        label(ui, name);
        let current = options.iter().find(|(o, _)| o == v).map_or("", |(_, l)| *l);
        let r = egui::ComboBox::from_id_salt(("tool_menu", salt))
            .width(86.0)
            .selected_text(current)
            .show_ui(ui, |ui| {
                for (option, text) in options {
                    ui.selectable_value(v, *option, *text);
                }
            });
        info(r.response, tip);
    });
}

pub fn switch(ui: &mut egui::Ui, on: &mut bool, name: &str, tip: &str) {
    let r = widgets::state_button(ui, Vec2::new(70.0, 16.0), *on, pal().accent, name);
    if info(r, tip).clicked() {
        *on = !*on;
    }
}

/// A two-handled slider; handles that meet make a single value.
pub fn range_u8(ui: &mut egui::Ui, name: &str, v: &mut (u8, u8), purple: bool, tip: &str) {
    let color = if purple { hex(0xa77bff) } else { pal().accent };
    ui.label(RichText::new(name).color(pal().text_dim).small());
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), 16.0),
        egui::Sense::click_and_drag(),
    );
    let x = |k: u8| rect.left() + f32::from(k) / 127.0 * rect.width();
    let p = ui.painter();
    p.line_segment(
        [rect.left_center(), rect.right_center()],
        egui::Stroke::new(2.0, shade(0x505050)),
    );
    let (a, b) = (x(v.0), x(v.1));
    p.line_segment(
        [
            egui::pos2(a, rect.center().y),
            egui::pos2(b, rect.center().y),
        ],
        egui::Stroke::new(2.0, color),
    );
    for at in [a, b] {
        p.circle_filled(egui::pos2(at, rect.center().y), 4.0, color);
    }
    p.text(
        rect.right_top(),
        egui::Align2::RIGHT_TOP,
        format!(
            "{} - {}",
            spell(v.0, Spelling::Sharps, None),
            spell(v.1, Spelling::Sharps, None)
        ),
        font(9.0),
        pal().text_dim,
    );
    if resp.dragged() || resp.drag_started() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let k = (((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0) * 127.0).round() as u8;
            if (pos.x - a).abs() <= (pos.x - b).abs() && v.0 != v.1 || pos.x < a {
                v.0 = k.min(v.1);
            } else {
                v.1 = k.max(v.0);
            }
        }
    }
    if resp.clicked() && v.0 == v.1 {
        if let Some(pos) = resp.interact_pointer_pos() {
            let k = (((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0) * 127.0).round() as u8;
            if k < v.0 {
                v.0 = k;
            } else {
                v.1 = k;
            }
        }
    }
    info(resp, tip);
}
