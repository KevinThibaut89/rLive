//! Little activity LEDs for the MIDI effects.

use super::{PanelCtx, PanelDef};
use crate::ui::theme::*;
use egui::{Align2, Pos2, Vec2};
use rlive_core::ViewId;
use rlive_devices::devices::b1;

/// Reads `labels.len()` LED levels from `view` and draws a row of them, at most a few wide.
fn draw_row(ui: &mut egui::Ui, panel: &mut PanelCtx, view: ViewId, labels: &[&str]) {
    let mut levels = [0.0f32; 8];
    let n = labels.len().min(levels.len());
    let read = panel.view.is_some_and(|v| v.read(view, &mut levels[..n]));
    let rect = ui.max_rect();
    let p = ui.painter();
    let width = rect.width() / n.max(1) as f32;
    for (i, label) in labels.iter().enumerate().take(n) {
        let centre = Pos2::new(
            rect.left() + width * (i as f32 + 0.5),
            rect.center().y - 5.0,
        );
        let level = if read { levels[i].clamp(0.0, 1.0) } else { 0.0 };
        p.circle_filled(centre, 4.0, darken(pal().knob_arc, 1.0 - level));
        if !label.is_empty() {
            p.text(
                centre + Vec2::new(0.0, 10.0),
                Align2::CENTER_CENTER,
                *label,
                font(9.0),
                pal().text_dim,
            );
        }
    }
}

macro_rules! led_panel {
    ($const_name:ident, $name:literal, $count:expr, $view:expr, $labels:expr, $draw:ident) => {
        pub const $const_name: PanelDef = PanelDef {
            name: $name,
            size: Vec2::new(60.0, 26.0),
            views: $count,
            draw: $draw,
        };
        fn $draw(ui: &mut egui::Ui, panel: &mut PanelCtx) {
            draw_row(ui, panel, $view, $labels);
        }
    };
}

led_panel!(
    ARP,
    "arpeggiator_led",
    b1::arpeggiator::LED_COUNT,
    b1::arpeggiator::LED,
    &[""],
    draw_arp
);
led_panel!(
    PITCH,
    "pitch_led",
    b1::pitch::LED_COUNT,
    b1::pitch::LED,
    &[""],
    draw_pitch
);
led_panel!(
    VELOCITY,
    "velocity_led",
    b1::velocity::LED_COUNT,
    b1::velocity::LED,
    &[""],
    draw_velocity
);
led_panel!(
    RANDOM,
    "random_leds",
    b1::random::LED_COUNT,
    b1::random::LEDS,
    &["+", "0", "-"],
    draw_random
);

pub const CHORD: PanelDef = PanelDef {
    name: "chord_leds",
    size: Vec2::new(150.0, 22.0),
    views: b1::chord::LED_COUNT,
    draw: draw_chord,
};

fn draw_chord(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    draw_row(ui, panel, b1::chord::LEDS, &["1", "2", "3", "4", "5", "6"]);
}
