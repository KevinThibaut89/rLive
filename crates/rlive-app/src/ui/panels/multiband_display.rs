//! Multiband Dynamics' block display.
//!
//! One row per band. The blocks on the left and right hold what's under Below and over Above;
//! drag a block's inner edge to move its threshold and its middle to change its ratio.
//! Cmd or Ctrl applies a drag to every band, Option or Alt to both blocks of the band, and Shift
//! works more finely. Double-click a block to put its ratio back. The column on the right
//! switches between Time, Below and Above.

use super::{PanelCtx, PanelDef};
use crate::ui::theme::{self, *};
use egui::{Align2, Pos2, Rect, Sense, Stroke, Vec2};
use rlive_core::{gain_to_db, ParamId};
use rlive_devices::devices::b3::multiband_dynamics as mb;

pub const PANEL: PanelDef = PanelDef {
    name: "multiband_display",
    size: Vec2::new(340.0, 140.0),
    views: mb::VIEW_LEN,
    draw,
};

const FLOOR_DB: f32 = -80.0;
const COLUMN_W: f32 = 68.0;

#[derive(Clone, Copy, PartialEq, Default)]
enum Column {
    Time,
    Below,
    #[default]
    Above,
}

#[derive(Clone, Copy, PartialEq)]
enum Grip {
    Edge,
    Middle,
}

#[derive(Clone, Copy)]
struct Drag {
    band: usize,
    above: bool,
    grip: Grip,
    start: f64,
    other_start: f64,
}

#[derive(Clone, Default)]
struct State {
    column: Column,
    drag: Option<Drag>,
    drag_from: Pos2,
}

fn db_to_x(rect: Rect, db: f32) -> f32 {
    let t = ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0);
    rect.left() + t * rect.width()
}

fn linear_db(v: f32) -> f32 {
    gain_to_db(v).max(FLOOR_DB)
}

fn threshold_id(band: usize, above: bool) -> u32 {
    mb::band_param(
        band,
        if above {
            mb::B_ABOVE_THRESHOLD
        } else {
            mb::B_BELOW_THRESHOLD
        },
    )
}

fn ratio_id(band: usize, above: bool) -> u32 {
    mb::band_param(
        band,
        if above {
            mb::B_ABOVE_RATIO
        } else {
            mb::B_BELOW_RATIO
        },
    )
}

/// A vertical drag, up is louder, as a change to a ratio: lower ratios are louder in both blocks.
fn drag_ratio(start: f64, dy: f32, fine: bool) -> f64 {
    let per_px = if fine { 0.002 } else { 0.01 };
    (start * 2f64.powf(-dy as f64 * per_px * 12.0)).clamp(0.1, 100.0)
}

fn value(panel: &PanelCtx, id: u32) -> f64 {
    panel.value(ParamId(id)).unwrap_or(0.0)
}

fn draw(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    let id = ui.scope_id().with(("multiband_display", panel.device.id));
    let mut state: State = ui.ctx().data_mut(|d| d.get_temp(id).unwrap_or_default());
    let meters = Rect::from_min_max(rect.min, Pos2::new(rect.right() - COLUMN_W, rect.bottom()));

    let mut levels = [0.0f32; mb::VIEW_LEN];
    if let Some(view) = panel.view {
        view.read(mb::LEVELS, &mut levels);
    }

    let row_h = meters.height() / mb::BANDS as f32;
    // High on top, as in the device.
    let row_of = |band: usize| {
        Rect::from_min_size(
            Pos2::new(
                meters.left(),
                meters.top() + (mb::BANDS - 1 - band) as f32 * row_h,
            ),
            Vec2::new(meters.width(), row_h - 2.0),
        )
    };

    let response = ui.interact(
        meters,
        ui.scope_id().with(("mb_blocks", id)),
        Sense::click_and_drag(),
    );
    let modifiers = ui.input(|i| i.modifiers);

    if response.drag_started() {
        if let Some(pos) = response.interact_pointer_pos() {
            for band in 0..mb::BANDS {
                let r = row_of(band);
                if !r.contains(pos) {
                    continue;
                }
                let below_x = db_to_x(r, value(panel, threshold_id(band, false)) as f32);
                let above_x = db_to_x(r, value(panel, threshold_id(band, true)) as f32);
                let (above, grip) = if (pos.x - above_x).abs() < 6.0 {
                    (true, Grip::Edge)
                } else if (pos.x - below_x).abs() < 6.0 {
                    (false, Grip::Edge)
                } else if pos.x > above_x {
                    (true, Grip::Middle)
                } else {
                    (false, Grip::Middle)
                };
                let target = if grip == Grip::Edge {
                    threshold_id(band, above)
                } else {
                    ratio_id(band, above)
                };
                let sibling = if grip == Grip::Edge {
                    threshold_id(band, !above)
                } else {
                    ratio_id(band, !above)
                };
                state.drag = Some(Drag {
                    band,
                    above,
                    grip,
                    start: value(panel, target),
                    other_start: value(panel, sibling),
                });
                state.drag_from = pos;
            }
        }
    }
    if response.dragged() {
        if let (Some(drag), Some(pos)) = (state.drag, response.interact_pointer_pos()) {
            let all_bands = modifiers.command;
            let both_blocks = modifiers.alt;
            let fine = modifiers.shift;
            let bands: Vec<usize> = if all_bands {
                (0..mb::BANDS).collect()
            } else {
                vec![drag.band]
            };
            let dx = pos.x - state.drag_from.x;
            let dy = state.drag_from.y - pos.y;
            for &band in &bands {
                match drag.grip {
                    Grip::Edge => {
                        let delta = dx as f64 * if fine { 0.05 } else { 0.25 };
                        panel.set(
                            ParamId(threshold_id(band, drag.above)),
                            (drag.start + delta).clamp(-80.0, 0.0),
                        );
                        if both_blocks {
                            panel.set(
                                ParamId(threshold_id(band, !drag.above)),
                                (drag.other_start + delta).clamp(-80.0, 0.0),
                            );
                        }
                    }
                    Grip::Middle => {
                        panel.set(
                            ParamId(ratio_id(band, drag.above)),
                            drag_ratio(drag.start, dy, fine),
                        );
                        if both_blocks {
                            panel.set(
                                ParamId(ratio_id(band, !drag.above)),
                                drag_ratio(drag.other_start, dy, fine),
                            );
                        }
                    }
                }
            }
        }
    }
    if response.drag_stopped() {
        state.drag = None;
    }
    if response.double_clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            for band in 0..mb::BANDS {
                let r = row_of(band);
                if r.contains(pos) {
                    let above_x = db_to_x(r, value(panel, threshold_id(band, true)) as f32);
                    panel.set(ParamId(ratio_id(band, pos.x > above_x)), 1.0);
                }
            }
        }
    }

    let p = ui.painter();
    for band in 0..mb::BANDS {
        let r = row_of(band);
        p.rect_filled(r, 2.0, pal().knob_track);
        let below_x = db_to_x(r, value(panel, threshold_id(band, false)) as f32);
        let above_x = db_to_x(r, value(panel, threshold_id(band, true)) as f32);
        for (from, to) in [(r.left(), below_x), (above_x, r.right())] {
            p.rect_filled(
                Rect::from_min_max(Pos2::new(from, r.top()), Pos2::new(to, r.bottom())),
                0.0,
                pal().panel_dark,
            );
        }
        for x in [below_x, above_x] {
            p.line_segment(
                [Pos2::new(x, r.top()), Pos2::new(x, r.bottom())],
                Stroke::new(1.0, pal().solo_blue),
            );
        }
        let output = linear_db(levels[mb::BANDS + band]);
        let input = linear_db(levels[band]);
        p.rect_filled(
            Rect::from_min_max(
                Pos2::new(r.left(), r.top() + r.height() * 0.3),
                Pos2::new(db_to_x(r, output), r.bottom() - 2.0),
            ),
            0.0,
            pal().accent,
        );
        p.rect_filled(
            Rect::from_min_max(
                Pos2::new(r.left(), r.top() + 2.0),
                Pos2::new(db_to_x(r, input), r.top() + r.height() * 0.22),
            ),
            0.0,
            pal().text_dim,
        );
        p.text(
            Pos2::new(r.left() + 3.0, r.center().y),
            Align2::LEFT_CENTER,
            mb::BAND_NAMES[band],
            theme::font(9.0),
            pal().text,
        );
    }

    // The column of numbers, and its switch.
    let column = Rect::from_min_max(Pos2::new(meters.right() + 2.0, rect.top()), rect.max);
    let switch = Rect::from_min_size(
        Pos2::new(column.left(), column.bottom() - 14.0),
        Vec2::new(column.width(), 14.0),
    );
    for (i, (c, label)) in [
        (Column::Time, "T"),
        (Column::Below, "B"),
        (Column::Above, "A"),
    ]
    .into_iter()
    .enumerate()
    {
        let w = switch.width() / 3.0;
        let r = Rect::from_min_size(
            Pos2::new(switch.left() + i as f32 * w, switch.top()),
            Vec2::new(w - 1.0, 14.0),
        );
        if ui
            .interact(r, ui.scope_id().with(("mb_col", label, id)), Sense::click())
            .clicked()
        {
            state.column = c;
        }
        let on = state.column == c;
        p.rect_filled(r, 2.0, if on { pal().accent } else { pal().raised });
        p.text(
            r.center(),
            Align2::CENTER_CENTER,
            label,
            theme::font(9.0),
            if on { pal().text_dark } else { pal().text_dim },
        );
    }
    let row_text = |band: usize| -> String {
        let id = |offset| mb::band_param(band, offset);
        match state.column {
            Column::Time => format!(
                "{:.0}/{:.0}",
                value(panel, id(mb::B_ATTACK)),
                value(panel, id(mb::B_RELEASE))
            ),
            Column::Below => format!(
                "{:.0} {:.1}",
                value(panel, id(mb::B_BELOW_THRESHOLD)),
                value(panel, id(mb::B_BELOW_RATIO))
            ),
            Column::Above => format!(
                "{:.0} {:.1}",
                value(panel, id(mb::B_ABOVE_THRESHOLD)),
                value(panel, id(mb::B_ABOVE_RATIO))
            ),
        }
    };
    for band in 0..mb::BANDS {
        let r = row_of(band);
        p.text(
            Pos2::new(column.center().x, r.center().y),
            Align2::CENTER_CENTER,
            row_text(band),
            theme::font(10.0),
            pal().text_dim,
        );
    }
    ui.ctx().data_mut(|d| d.insert_temp(id, state));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_threshold_lands_inside_its_row() {
        let r = Rect::from_min_size(Pos2::new(10.0, 0.0), Vec2::new(200.0, 40.0));
        assert_eq!(db_to_x(r, FLOOR_DB), 10.0);
        assert_eq!(db_to_x(r, 0.0), 210.0);
        assert_eq!(db_to_x(r, 6.0), 210.0, "past 0 dB it stays on the row");
    }

    #[test]
    fn dragging_up_is_louder_which_is_a_lower_ratio() {
        assert!(drag_ratio(4.0, 20.0, false) < 4.0);
        assert!(drag_ratio(4.0, -20.0, false) > 4.0);
    }

    #[test]
    fn shift_drags_more_finely() {
        let coarse = (drag_ratio(4.0, 20.0, false) - 4.0).abs();
        let fine = (drag_ratio(4.0, 20.0, true) - 4.0).abs();
        assert!(fine < coarse, "{fine} {coarse}");
    }

    #[test]
    fn the_ratio_stays_in_range_at_any_drag() {
        assert_eq!(drag_ratio(100.0, -500.0, false), 100.0);
        assert_eq!(drag_ratio(0.1, 500.0, false), 0.1);
    }

    #[test]
    fn the_band_ids_address_their_own_controls() {
        assert_eq!(
            threshold_id(0, true),
            mb::band_param(0, mb::B_ABOVE_THRESHOLD)
        );
        assert_eq!(ratio_id(2, false), mb::band_param(2, mb::B_BELOW_RATIO));
    }
}
