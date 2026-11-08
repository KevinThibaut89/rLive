//! EQ Three's three signal LEDs.

use super::{PanelCtx, PanelDef};
use crate::ui::theme::{self, *};
use egui::{Align2, Pos2, Vec2};
use rlive_devices::effects::eq;

pub const PANEL: PanelDef = PanelDef {
    name: "eq_three_leds",
    size: Vec2::new(66.0, 34.0),
    views: eq::VIEW_LEN,
    draw,
};

fn draw(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    let mut values = [0.0; eq::VIEW_LEN];
    let read = panel.view.is_some_and(|v| v.read(eq::LEDS, &mut values));
    let spacing = rect.width() / 3.0;
    for (i, label) in ["L", "M", "H"].into_iter().enumerate() {
        let x = rect.left() + spacing * (i as f32 + 0.5);
        let lit = read && values[i] >= 0.5;
        ui.painter().circle_filled(
            Pos2::new(x, rect.top() + 10.0),
            4.0,
            if lit {
                pal().play_green
            } else {
                pal().knob_track
            },
        );
        ui.painter().text(
            Pos2::new(x, rect.bottom() - 8.0),
            Align2::CENTER_CENTER,
            label,
            theme::font(10.0),
            pal().text_dim,
        );
    }
}
