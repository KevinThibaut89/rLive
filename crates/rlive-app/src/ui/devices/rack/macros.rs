//! Macro Controls and Macro Variations (manual pp. 504-508).

use super::super::DrawCx;
use super::{RackAction, RackRef, Rename, RenameTarget};
use crate::session::{ChainRef, InfoTarget};
use crate::ui::devices::Action;
use crate::ui::session_view::color_picker;
use crate::ui::theme::{self, pal, shade};
use crate::ui::widgets::{self, info, rgb, ControlState, Led};
use egui::{Align2, Pos2, Rect, Sense, Vec2};
use rlive_core::param::rack::{MACRO_COUNT, SCALE};
use rlive_core::{DeviceId, ParamInfo};
use rlive_project::Macro;

const CELL_W: f32 = 60.0;
const CELL_H: f32 = 78.0;
const HEADER_H: f32 = 18.0;
/// Width of the Variations view.
pub const VARIATIONS_W: f32 = 150.0;
const VARIATION_ROW: f32 = 20.0;

/// Knobs are laid out in two rows.
pub fn columns(visible: u8) -> usize {
    (visible as usize).div_ceil(2).max(1)
}

pub fn section_width(visible: u8) -> f32 {
    columns(visible) as f32 * CELL_W + 4.0
}

/// How a macro's value reads, in its mapping's units when it has them (manual p. 505).
pub fn value_text(value: f64, unit: &str) -> String {
    if unit.is_empty() {
        format!("{value:.0}")
    } else if value.abs() >= 100.0 {
        format!("{value:.0} {unit}")
    } else {
        format!("{value:.1} {unit}")
    }
}

/// The count after pressing the + or - selector, 1 to 16 (manual p. 504).
pub fn stepped_count(visible: u8, up: bool) -> u8 {
    let next = if up {
        visible.saturating_add(1)
    } else {
        visible.saturating_sub(1)
    };
    next.clamp(1, MACRO_COUNT as u8)
}

/// Draws the knobs and their header.
pub fn section(ui: &mut egui::Ui, cx: &mut DrawCx, r: &RackRef, rect: Rect) {
    ui.painter().rect_filled(rect, 2.0, shade(0x333333));
    let header = Rect::from_min_size(rect.min, Vec2::new(rect.width(), HEADER_H));
    let rack = r.dev.id;
    let map_on = cx.ui.map.is_some_and(|m| m.rack == rack);
    let map_r = Rect::from_min_size(header.min + Vec2::new(2.0, 2.0), Vec2::new(32.0, 14.0));
    let resp = super::tiny_button(ui, map_r, ("map_mode", rack), "Map", map_on, pal().accent);
    if info(
        resp,
        "Macro Map Mode — click parameters, then a macro's Map button",
    )
    .clicked()
    {
        cx.queue
            .push(r.chain, Action::Rack(rack, RackAction::ToggleMap));
    }
    let minus = Rect::from_min_size(
        Pos2::new(header.right() - 34.0, header.top() + 2.0),
        Vec2::new(15.0, 14.0),
    );
    let plus = minus.translate(Vec2::new(17.0, 0.0));
    let visible = r.visible_macros;
    let resp = super::tiny_button(ui, minus, ("macros_less", rack), "-", false, pal().accent);
    if info(resp, "Show fewer Macro Controls").clicked() {
        cx.queue.push(
            r.chain,
            Action::Rack(rack, RackAction::MacroCount(stepped_count(visible, false))),
        );
    }
    let resp = super::tiny_button(ui, plus, ("macros_more", rack), "+", false, pal().accent);
    if info(resp, "Show more Macro Controls").clicked() {
        cx.queue.push(
            r.chain,
            Action::Rack(rack, RackAction::MacroCount(stepped_count(visible, true))),
        );
    }

    let cols = columns(visible);
    let area_top = header.bottom() + 1.0;
    for n in 0..visible as usize {
        let col = n % cols;
        let row = n / cols;
        let cell = Rect::from_min_size(
            Pos2::new(
                rect.left() + 2.0 + col as f32 * CELL_W,
                area_top + row as f32 * CELL_H,
            ),
            Vec2::new(CELL_W, CELL_H),
        );
        let mut cu = ui.new_child(egui::UiBuilder::new().max_rect(cell));
        macro_cell(&mut cu, cx, r, n, cell, map_on);
    }
}

fn macro_cell(ui: &mut egui::Ui, cx: &mut DrawCx, r: &RackRef, n: usize, cell: Rect, map_on: bool) {
    let rack = r.dev.id;
    let chain = r.chain;
    let display = cx.session.macro_display(chain, rack, n);
    let knob = cx.session.macro_value(chain, rack, n);
    let param = ParamInfo::continuous(n as u32, &display.name, 0.0, SCALE, 0.0, &display.unit);
    let text = value_text(display.value, &display.unit);
    let cell_id = cx.session.device_cell(chain, rack, n as u32);
    let cell_state = cell_id.map_or(Default::default(), |c| cx.session.cell_state(chain, c));
    let m = r.macros.get(n);
    let changed = widgets::knob(
        ui,
        &param,
        knob,
        &text,
        ControlState {
            effective: None,
            disabled: false,
            led: Led::of(cell_state),
            re_enable: None,
        },
    );
    if let Some(v) = changed {
        cx.queue
            .push(chain, Action::Rack(rack, RackAction::MacroValue(n, v)));
    }
    if let Some(c) = m.and_then(|m| m.color) {
        let swatch = Rect::from_min_size(cell.min + Vec2::new(0.0, 2.0), Vec2::new(3.0, 12.0));
        ui.painter().rect_filled(swatch, 0.0, rgb(c));
    }
    if map_on {
        let map_btn = Rect::from_min_size(
            Pos2::new(cell.left() + 12.0, cell.bottom() - 15.0),
            Vec2::new(cell.width() - 24.0, 13.0),
        );
        let on = m.is_some_and(|m| !m.mappings.is_empty());
        let resp = super::tiny_button(ui, map_btn, ("map_macro", n), "Map", on, pal().accent);
        if info(resp, "Map — assign the selected parameter to this macro").clicked() {
            cx.queue.push(chain, Action::Rack(rack, RackAction::Map(n)));
        }
        return;
    }
    // Clicks go to the menu and double-clicks reset it, so this sits over the knob, which keeps
    // its drags.
    let overlay = Rect::from_min_size(cell.min, Vec2::new(cell.width(), 62.0));
    let resp = ui.interact(
        overlay,
        ui.scope_id().with(("macro_menu", n)),
        Sense::click(),
    );
    if resp.double_clicked() {
        cx.queue
            .push(chain, Action::Rack(rack, RackAction::MacroValue(n, 0.0)));
    }
    let hover = format!("{} — right-click for options", display.name);
    let resp = info(resp, hover);
    let renaming = matches!(&cx.ui.rename, Some(Rename { target: RenameTarget::Macro(d, i), .. }) if *d == rack && *i == n);
    if renaming {
        rename_field(ui, cx, chain, rack, n, cell);
    }
    let excl_rand = m.is_some_and(|m| m.exclude_random);
    let excl_var = m.is_some_and(|m| m.exclude_variations);
    let name = m.map_or(String::new(), |m| m.name.clone());
    resp.context_menu(|ui| macro_menu(ui, cx, chain, rack, n, m, name, excl_rand, excl_var));
}

#[allow(clippy::too_many_arguments)]
fn macro_menu(
    ui: &mut egui::Ui,
    cx: &mut DrawCx,
    chain: ChainRef,
    rack: DeviceId,
    n: usize,
    m: Option<&Macro>,
    name: String,
    excl_rand: bool,
    excl_var: bool,
) {
    if ui.button("Rename").clicked() {
        cx.ui.rename = Some(Rename {
            target: RenameTarget::Macro(rack, n),
            text: name,
        });
        ui.close();
    }
    if ui.button("Edit Info Text").clicked() {
        cx.queue.push(
            chain,
            Action::EditInfoTarget(InfoTarget::Macro(chain, rack, n)),
        );
        ui.close();
    }
    ui.separator();
    if let Some(color) = color_picker(ui) {
        cx.queue.push(
            chain,
            Action::Rack(rack, RackAction::MacroColor(n, Some(color))),
        );
        ui.close();
    }
    if m.and_then(|m| m.color).is_some() && ui.button("Clear Color").clicked() {
        cx.queue
            .push(chain, Action::Rack(rack, RackAction::MacroColor(n, None)));
        ui.close();
    }
    ui.separator();
    if ui
        .selectable_label(excl_rand, "Exclude Macro from Randomization")
        .clicked()
    {
        cx.queue.push(
            chain,
            Action::Rack(rack, RackAction::MacroExcludeRandom(n, !excl_rand)),
        );
        ui.close();
    }
    if ui
        .selectable_label(excl_var, "Exclude Macro From Variations")
        .clicked()
    {
        cx.queue.push(
            chain,
            Action::Rack(rack, RackAction::MacroExcludeVariations(n, !excl_var)),
        );
        ui.close();
    }
}

fn rename_field(
    ui: &mut egui::Ui,
    cx: &mut DrawCx,
    chain: ChainRef,
    rack: DeviceId,
    n: usize,
    cell: Rect,
) {
    let mut done = None;
    if let Some(rename) = &mut cx.ui.rename {
        let r = ui.put(
            Rect::from_min_size(cell.min, Vec2::new(cell.width(), 16.0)),
            egui::TextEdit::singleline(&mut rename.text).font(theme::font(10.0)),
        );
        r.request_focus();
        if r.lost_focus() {
            done = Some(rename.text.clone());
        }
    }
    if let Some(text) = done {
        cx.ui.rename = None;
        cx.queue
            .push(chain, Action::Rack(rack, RackAction::MacroName(n, text)));
    }
}

/// Draws the Variations view: New, then a row per variation (manual pp. 507-508).
pub fn variations(ui: &mut egui::Ui, cx: &mut DrawCx, r: &RackRef, rect: Rect) {
    let rack = r.dev.id;
    let chain = r.chain;
    ui.painter().rect_filled(rect, 2.0, shade(0x333333));
    let new_r = Rect::from_min_size(
        rect.min + Vec2::new(4.0, 3.0),
        Vec2::new(rect.width() - 8.0, 16.0),
    );
    let resp = super::tiny_button(ui, new_r, ("var_new", rack), "New", false, pal().accent);
    if info(resp, "New — store the macros as a new variation").clicked() {
        cx.queue
            .push(chain, Action::Rack(rack, RackAction::StoreVariation));
    }
    for (i, v) in r.variations.iter().enumerate() {
        let row = Rect::from_min_size(
            Pos2::new(
                rect.left() + 2.0,
                new_r.bottom() + 3.0 + i as f32 * VARIATION_ROW,
            ),
            Vec2::new(rect.width() - 4.0, VARIATION_ROW - 2.0),
        );
        if row.bottom() > rect.bottom() {
            break;
        }
        let selected = r.view.selected_variation == Some(i);
        let bg = ui.interact(
            row,
            ui.scope_id().with(("var_row", rack, i)),
            Sense::click(),
        );
        ui.painter().rect_filled(
            row,
            1.0,
            if selected {
                pal().raised_hover
            } else if bg.hovered() {
                pal().raised
            } else {
                pal().field_bg
            },
        );
        if bg.clicked() {
            cx.queue
                .push(chain, Action::Rack(rack, RackAction::SelectVariation(i)));
        }
        let renaming = matches!(&cx.ui.rename, Some(Rename { target: RenameTarget::Variation(d, n), .. }) if *d == rack && *n == i);
        if renaming {
            let mut done = None;
            if let Some(rename) = &mut cx.ui.rename {
                let edit = ui.put(
                    row.shrink2(Vec2::new(18.0, 0.0)),
                    egui::TextEdit::singleline(&mut rename.text).font(theme::font(10.0)),
                );
                edit.request_focus();
                if edit.lost_focus() {
                    done = Some(rename.text.clone());
                }
            }
            if let Some(text) = done {
                cx.ui.rename = None;
                cx.queue.push(
                    chain,
                    Action::Rack(rack, RackAction::RenameVariation(i, text)),
                );
            }
        } else {
            ui.painter().with_clip_rect(row.shrink(2.0)).text(
                Pos2::new(row.left() + 19.0, row.center().y),
                Align2::LEFT_CENTER,
                &v.name,
                theme::font(10.5),
                pal().text,
            );
        }
        // Overwrite on the left, Launch on the right, as Live has them.
        let over = Rect::from_min_size(
            row.min + Vec2::new(2.0, 2.0),
            Vec2::new(14.0, row.height() - 4.0),
        );
        let launch = Rect::from_min_size(
            Pos2::new(row.right() - 16.0, row.top() + 2.0),
            Vec2::new(14.0, row.height() - 4.0),
        );
        let resp = super::tiny_button(ui, over, ("var_over", rack, i), "O", false, pal().accent);
        if info(
            resp,
            "Overwrite Macro Variation — store the macros in this one",
        )
        .clicked()
        {
            cx.queue
                .push(chain, Action::Rack(rack, RackAction::OverwriteVariation(i)));
        }
        let resp = super::tiny_button(ui, launch, ("var_go", rack, i), ">", false, pal().accent);
        if info(resp, "Launch Macro Variation").clicked() {
            cx.queue
                .push(chain, Action::Rack(rack, RackAction::LaunchVariation(i)));
        }
        let name = v.name.clone();
        bg.context_menu(|ui| {
            if ui.button("Rename").clicked() {
                cx.ui.rename = Some(Rename {
                    target: RenameTarget::Variation(rack, i),
                    text: name,
                });
                ui.close();
            }
            if ui.button("Duplicate").clicked() {
                cx.queue
                    .push(chain, Action::Rack(rack, RackAction::DuplicateVariation(i)));
                ui.close();
            }
            if ui
                .button(egui::RichText::new("Delete").color(pal().arm_red))
                .clicked()
            {
                cx.queue
                    .push(chain, Action::Rack(rack, RackAction::DeleteVariation(i)));
                ui.close();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knobs_share_two_rows() {
        assert_eq!(columns(8), 4);
        assert_eq!(columns(16), 8);
        assert_eq!(columns(1), 1);
        assert_eq!(columns(3), 2);
    }

    #[test]
    fn a_macro_shows_in_its_own_units_or_on_the_plain_scale() {
        assert_eq!(value_text(64.2, ""), "64");
        assert_eq!(value_text(-12.34, "dB"), "-12.3 dB");
        assert_eq!(value_text(440.0, "Hz"), "440 Hz");
    }

    #[test]
    fn the_selector_buttons_step_within_the_range() {
        assert_eq!(stepped_count(8, true), 9);
        assert_eq!(stepped_count(8, false), 7);
        assert_eq!(stepped_count(16, true), 16);
        assert_eq!(stepped_count(1, false), 1);
    }
}
