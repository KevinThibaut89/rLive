//! A gain-reduction bar for the Compressor.

use super::{PanelCtx, PanelDef};
use crate::ui::theme::{self, *};
use egui::{Align2, Pos2, Rect, Vec2};
use rlive_devices::effects::comp;

/// The scale of the bar, in dB.
const RANGE_DB: f32 = 24.0;

pub const PANEL: PanelDef = PanelDef {
    name: "gain_reduction",
    size: Vec2::new(40.0, 66.0),
    views: comp::VIEW_LEN,
    draw,
};

/// How far the bar falls this frame: at once when the reduction grows, slowly when it eases.
fn settle(shown: f32, target: f32, dt: f32) -> f32 {
    if target >= shown {
        target
    } else {
        target + (shown - target) * (-dt * 7.0).exp()
    }
}

fn draw(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    let mut value = [0.0];
    let read = panel
        .view
        .is_some_and(|v| v.read(comp::GAIN_REDUCTION, &mut value));
    let target = if read { value[0] } else { 0.0 };
    let id = ui.scope_id().with(("gain_reduction", panel.device.id));
    let db = ui.ctx().data_mut(|d| {
        let shown = d.get_temp_mut_or_default::<f32>(id);
        *shown = settle(*shown, target, panel.dt);
        *shown
    });

    let p = ui.painter();
    p.text(
        Pos2::new(rect.center().x, rect.top() + 6.0),
        Align2::CENTER_CENTER,
        "GR",
        theme::font(10.0),
        TEXT_DIM,
    );
    let bar = Rect::from_min_size(
        Pos2::new(rect.center().x - 5.0, rect.top() + 14.0),
        Vec2::new(10.0, rect.height() - 30.0),
    );
    p.rect_filled(bar, 0.0, KNOB_TRACK);
    let fill = (db / RANGE_DB).clamp(0.0, 1.0) * bar.height();
    p.rect_filled(
        Rect::from_min_size(bar.min, Vec2::new(bar.width(), fill)),
        0.0,
        ACCENT,
    );
    p.text(
        Pos2::new(rect.center().x, rect.bottom() - 8.0),
        Align2::CENTER_CENTER,
        format!("-{db:.1}"),
        theme::font(10.0),
        TEXT_DIM,
    );
}

#[cfg(test)]
mod tests {
    use super::settle;

    #[test]
    fn the_bar_jumps_down_at_once_and_eases_back_up() {
        assert_eq!(settle(1.0, 5.0, 0.016), 5.0);
        let eased = settle(5.0, 0.0, 0.016);
        assert!(eased > 0.0 && eased < 5.0, "{eased}");
    }
}
