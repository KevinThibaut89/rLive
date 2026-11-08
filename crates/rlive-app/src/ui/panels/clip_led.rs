//! A small clip indicator for the Glue Compressor and the Limiter.
//!
//! Reads one value at view 3: 0 is off, 1 is red (over 0 dB), 2 is yellow (soft clipping).

use super::{PanelCtx, PanelDef};
use crate::ui::theme::{self, *};
use egui::{Align2, Color32, Pos2, Vec2};
use rlive_core::ViewId;

const LED: ViewId = ViewId(3);

pub const PANEL: PanelDef = PANEL_DEF;

const PANEL_DEF: PanelDef = PanelDef {
    name: "clip_led",
    size: Vec2::new(30.0, 34.0),
    views: 4,
    draw,
};

fn colour(value: f32) -> Option<Color32> {
    if value >= 1.5 {
        Some(pal().activator)
    } else if value >= 0.5 {
        Some(pal().arm_red)
    } else {
        None
    }
}

fn draw(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    let mut value = [0.0];
    let read = panel.view.is_some_and(|v| v.read(LED, &mut value));
    let lit = if read { colour(value[0]) } else { None };
    let centre = Pos2::new(rect.center().x, rect.top() + 10.0);
    ui.painter()
        .circle_filled(centre, 4.0, lit.unwrap_or(pal().knob_track));
    ui.painter().text(
        Pos2::new(rect.center().x, rect.bottom() - 8.0),
        Align2::CENTER_CENTER,
        "Clip",
        theme::font(10.0),
        pal().text_dim,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_led_shows_clipping_and_soft_clipping_in_their_own_colours() {
        assert_eq!(colour(0.0), None);
        assert_eq!(colour(1.0), Some(pal().arm_red));
        assert_eq!(colour(2.0), Some(pal().activator));
    }
}
