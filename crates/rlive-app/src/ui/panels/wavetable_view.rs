//! Wavetable's table view: both oscillators, linear or polar, with the current position marked,
//! the arrows that step through categories and a button to load a file.

use super::{PanelCtx, PanelDef};
use crate::ui::theme::{self, *};
use egui::{Align2, CornerRadius, Pos2, Rect, Sense, Shape, Stroke, Vec2};
use rlive_core::ParamId;
use rlive_devices::devices::b11::wavetable as wt;

pub const PANEL: PanelDef = PanelDef {
    name: "wavetable_view",
    size: Vec2::new(430.0, 170.0),
    views: wt::VIEW_LEN,
    draw,
};

fn table_name(cat: usize, table: usize) -> &'static str {
    let (_, names) = wt::CATEGORIES[cat.min(wt::CATEGORIES.len() - 1)];
    names[table.min(names.len() - 1)]
}

fn next_table(cat: usize, table: usize, step: i32) -> (usize, usize) {
    let (cat, table) = (cat as i32, table as i32);
    let count = wt::CATEGORIES[cat as usize].1.len() as i32;
    if step > 0 {
        if table + 1 < count {
            (cat as usize, (table + 1) as usize)
        } else {
            ((cat as usize + 1) % wt::CATEGORIES.len(), 0)
        }
    } else if table > 0 {
        (cat as usize, (table - 1) as usize)
    } else {
        let prev = (cat as usize + wt::CATEGORIES.len() - 1) % wt::CATEGORIES.len();
        (prev, wt::CATEGORIES[prev].1.len() - 1)
    }
}

fn draw(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    let polar_id = ui.make_persistent_id(("wavetable_polar", panel.device.id));
    let mut polar: bool = ui.data(|d| d.get_temp(polar_id).unwrap_or(false));

    let mut view = vec![0.0f32; wt::VIEW_LEN];
    let live = panel.view.is_some_and(|v| v.read(wt::VIEW, &mut view));

    let width = (rect.width() - 6.0) * 0.5;
    for slot in 0..2 {
        let left = rect.left() + (width + 6.0) * slot as f32;
        let area =
            Rect::from_min_size(Pos2::new(left, rect.top()), Vec2::new(width, rect.height()));
        draw_osc(ui, panel, area, slot, &view, live, polar);
    }
    let button = Rect::from_min_size(
        Pos2::new(rect.right() - 48.0, rect.bottom() - 18.0),
        Vec2::new(46.0, 16.0),
    );
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(button));
    if child
        .small_button(if polar { "Linear" } else { "Polar" })
        .clicked()
    {
        polar = !polar;
    }
    ui.data_mut(|d| d.insert_temp(polar_id, polar));
}

fn draw_osc(
    ui: &mut egui::Ui,
    panel: &mut PanelCtx,
    area: Rect,
    slot: usize,
    view: &[f32],
    live: bool,
    polar: bool,
) {
    let base = if slot == 0 { wt::OSC1 } else { wt::OSC2 };
    let category = panel
        .value(ParamId(base + wt::O_CATEGORY))
        .unwrap_or(0.0)
        .round() as usize;
    let table = panel
        .value(ParamId(base + wt::O_TABLE))
        .unwrap_or(0.0)
        .round() as usize;
    let position = panel.value(ParamId(base + wt::O_POSITION)).unwrap_or(0.0) as f32;

    let header = Rect::from_min_size(area.min, Vec2::new(area.width(), 18.0));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(header));
    child.horizontal(|ui| {
        if ui.small_button("<").clicked() {
            let (c, t) = next_table(category, table, -1);
            panel.set(ParamId(base + wt::O_CATEGORY), c as f64);
            panel.set(ParamId(base + wt::O_TABLE), t as f64);
        }
        if ui.small_button(">").clicked() {
            let (c, t) = next_table(category, table, 1);
            panel.set(ParamId(base + wt::O_CATEGORY), c as f64);
            panel.set(ParamId(base + wt::O_TABLE), t as f64);
        }
        ui.label(egui::RichText::new(table_name(category, table)).size(10.0));
        if ui.small_button("Load").clicked() {
            panel.load_sample(slot);
        }
    });

    let display = Rect::from_min_max(
        Pos2::new(area.left(), area.top() + 20.0),
        area.right_bottom(),
    );
    let resp = ui.interact(
        display,
        ui.make_persistent_id(("wavetable_position", panel.device.id, slot)),
        Sense::drag(),
    );
    if resp.dragged() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let v = 1.0 - ((pos.y - display.top()) / display.height()).clamp(0.0, 1.0);
            panel.set(ParamId(base + wt::O_POSITION), v as f64);
        }
    }

    let p = ui.painter();
    p.rect_filled(display, CornerRadius::same(2), shade(0x121212));
    if !live {
        p.text(
            display.center(),
            Align2::CENTER_CENTER,
            "Play or open to see the table",
            theme::font(9.0),
            pal().text_dim,
        );
        return;
    }
    let frames = wt::VIEW_FRAMES;
    let points = wt::VIEW_POINTS;
    let start = 2 + slot * frames * points;
    let at = |f: usize, i: usize| view[start + f * points + i];
    let current = (position * (frames - 1) as f32).round() as usize;
    for f in 0..frames {
        let color = if f == current {
            pal().accent
        } else {
            pal().text_dim.gamma_multiply(0.45)
        };
        let stroke = Stroke::new(if f == current { 1.6 } else { 1.0 }, color);
        let mut line = Vec::with_capacity(points + 1);
        for i in 0..=points {
            let s = at(f, i % points);
            line.push(if polar {
                let center = display.center();
                let t = f as f32 / (frames - 1) as f32;
                let radius = (0.15 + 0.75 * t) * display.height() * 0.5 + s * 5.0;
                let angle = i as f32 / points as f32 * std::f32::consts::TAU;
                Pos2::new(
                    center.x + radius * angle.cos(),
                    center.y + radius * angle.sin(),
                )
            } else {
                let t = f as f32 / (frames - 1) as f32;
                let y0 = display.bottom() - 6.0 - t * (display.height() - 18.0);
                Pos2::new(
                    display.left() + i as f32 / points as f32 * display.width(),
                    y0 - s * 9.0,
                )
            });
        }
        p.add(Shape::line(line, stroke));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_arrows_roll_over_into_the_next_category() {
        assert_eq!(next_table(0, 2, 1), (1, 0));
        assert_eq!(next_table(1, 0, -1), (0, 2));
        assert_eq!(next_table(3, 0, 1), (0, 0));
    }
}
