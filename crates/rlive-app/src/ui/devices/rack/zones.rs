//! The Key, Velocity and Chain Select editors (manual pp. 497-500).
//!
//! A zone's lower section is its whole extent, from the far edge of one fade to the far edge of
//! the other. Its narrow upper section is the part at full level. Dragging an edge of either
//! section resizes it, and dragging anywhere else on the lower section moves the zone.

use super::super::DrawCx;
use super::chain_list::ROW_H;
use super::{ChainActivityView, RackAction, RackRef};
use crate::session::ZoneKind;
use crate::ui::devices::Action;
use crate::ui::theme::{self, pal, shade};
use crate::ui::widgets::{info, rgb};
use egui::{Align2, Color32, CursorIcon, Pos2, Rect, Sense, Stroke, Vec2};
use rlive_core::param::rack::SCALE;
use rlive_core::ChainId;
use rlive_project::{Chain, Zone};

/// The height of the ruler above the rows.
pub const RULER_H: f32 = 18.0;
/// How close to an edge counts as grabbing it, in points.
const EDGE: f32 = 4.0;
/// Height of a zone's upper section.
const UPPER_H: f32 = 5.0;

/// The part of a zone the pointer is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoneHit {
    Move,
    ExtentLow,
    ExtentHigh,
    PlateauLow,
    PlateauHigh,
}

/// A drag in progress on a zone.
#[derive(Debug, Clone, Copy)]
pub struct ZoneDrag {
    pub chain: ChainId,
    pub kind: ZoneKind,
    pub hit: ZoneHit,
    pub original: Zone,
    pub start_x: f32,
}

/// How values map onto an editor's width. A value owns a cell, `lowest` the first.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ZoneScale {
    pub left: f32,
    pub width: f32,
    pub lowest: u8,
    pub highest: u8,
}

impl ZoneScale {
    pub fn of(kind: ZoneKind, left: f32, width: f32) -> Self {
        let lowest = if kind == ZoneKind::Velocity { 1 } else { 0 };
        Self {
            left,
            width,
            lowest,
            highest: 127,
        }
    }

    fn cells(&self) -> f32 {
        (self.highest - self.lowest + 1) as f32
    }

    pub fn cell_w(&self) -> f32 {
        self.width / self.cells()
    }

    /// The left edge of a value's cell.
    pub fn x_of(&self, value: u8) -> f32 {
        self.left + (value.saturating_sub(self.lowest)) as f32 * self.cell_w()
    }

    /// The right edge of a value's cell.
    pub fn end_of(&self, value: u8) -> f32 {
        self.x_of(value) + self.cell_w()
    }

    /// The value whose cell holds `x`, clamped to the scale.
    pub fn value_at(&self, x: f32) -> u8 {
        let cell = ((x - self.left) / self.cell_w()).floor().max(0.0) as u32;
        (cell.min((self.highest - self.lowest) as u32) as u8) + self.lowest
    }

    /// How many values a drag of `dx` points covers.
    pub fn units(&self, dx: f32) -> i32 {
        (dx / self.cell_w()).round() as i32
    }
}

/// Which part of a zone a point is on, or `None` if it misses. `y_upper` says the point is in the
/// row's upper section.
pub fn hit_test(zone: Zone, scale: &ZoneScale, x: f32, y_upper: bool) -> Option<ZoneHit> {
    let (left, right) = (scale.x_of(zone.fade_low), scale.end_of(zone.fade_high));
    if x < left - EDGE || x > right + EDGE {
        return None;
    }
    if y_upper {
        let (plateau_l, plateau_r) = (scale.x_of(zone.low), scale.end_of(zone.high));
        if (x - plateau_l).abs() <= EDGE && (x - plateau_l).abs() <= (x - plateau_r).abs() {
            return Some(ZoneHit::PlateauLow);
        }
        if (x - plateau_r).abs() <= EDGE {
            return Some(ZoneHit::PlateauHigh);
        }
    }
    if (x - left).abs() <= EDGE {
        return Some(ZoneHit::ExtentLow);
    }
    if (x - right).abs() <= EDGE {
        return Some(ZoneHit::ExtentHigh);
    }
    (x >= left && x <= right).then_some(ZoneHit::Move)
}

/// The zone after a drag of `units` values on `hit`, from the zone as it was when the drag began.
/// Everything stays in order and in `lowest..=highest`.
pub fn apply_drag(z: Zone, hit: ZoneHit, units: i32, lowest: u8, highest: u8) -> Zone {
    let (lo, hi) = (lowest as i32, highest as i32);
    let (fl, l, h, fh) = (
        z.fade_low as i32,
        z.low as i32,
        z.high as i32,
        z.fade_high as i32,
    );
    let (fl, l, h, fh) = match hit {
        ZoneHit::Move => {
            let d = units.clamp(lo - fl, hi - fh);
            (fl + d, l + d, h + d, fh + d)
        }
        ZoneHit::ExtentLow => {
            // With no fade on this side the zone's edge is the plateau's, so they go together.
            let nf = (fl + units).clamp(lo, fh);
            let nl = if fl == l { nf } else { l.max(nf) };
            (nf, nl.min(h), h.max(nl), fh)
        }
        ZoneHit::ExtentHigh => {
            let nf = (fh + units).clamp(fl, hi);
            let nh = if fh == h { nf } else { h.min(nf) };
            (fl, l.min(nh), nh.max(l), nf)
        }
        ZoneHit::PlateauLow => (fl, (l + units).clamp(fl, h), h, fh),
        ZoneHit::PlateauHigh => (fl, l, (h + units).clamp(l, fh), fh),
    };
    Zone {
        low: l as u8,
        high: h as u8,
        fade_low: fl as u8,
        fade_high: fh as u8,
    }
}

/// The value the Chain selector takes at `x`.
pub fn selector_value(scale: &ZoneScale, x: f32) -> f64 {
    (scale.value_at(x) as f64).clamp(0.0, SCALE)
}

fn zone_of(c: &Chain, kind: ZoneKind) -> Zone {
    match kind {
        ZoneKind::Key => c.zones.key,
        ZoneKind::Velocity => c.zones.velocity,
        ZoneKind::Select => c.zones.select,
    }
}

/// Draws the editor over `rect`, with its rows lined up to the chain list beside it.
pub fn editor(
    ui: &mut egui::Ui,
    cx: &mut DrawCx,
    r: &RackRef,
    kind: ZoneKind,
    rect: Rect,
    scroll: f32,
    activity: &ChainActivityView,
) {
    let scale = ZoneScale::of(kind, rect.left() + 2.0, rect.width() - 4.0);
    let ruler = Rect::from_min_size(rect.min, Vec2::new(rect.width(), RULER_H));
    ui.painter().rect_filled(rect, 1.0, pal().field_bg);
    draw_ruler(ui, cx, r, kind, ruler, &scale);
    let rows = Rect::from_min_max(Pos2::new(rect.left(), ruler.bottom()), rect.max);
    let clip = rows.intersect(ui.clip_rect());
    let mut row_ui = ui.new_child(egui::UiBuilder::new().max_rect(rows));
    row_ui.set_clip_rect(clip);
    for (i, chain) in r.chains.iter().enumerate() {
        let top = rows.top() + i as f32 * ROW_H - scroll;
        let row = Rect::from_min_size(
            Pos2::new(rect.left(), top),
            Vec2::new(rect.width(), ROW_H - 1.0),
        );
        if row.bottom() < rows.top() || row.top() > rows.bottom() {
            continue;
        }
        zone_row(&mut row_ui, cx, r, kind, chain, row, &scale, activity);
    }
}

fn draw_ruler(
    ui: &mut egui::Ui,
    cx: &mut DrawCx,
    r: &RackRef,
    kind: ZoneKind,
    ruler: Rect,
    scale: &ZoneScale,
) {
    let p = ui.painter().clone();
    p.rect_filled(ruler, 0.0, pal().raised);
    match kind {
        ZoneKind::Key => {
            for octave in 0..=10u8 {
                let note = octave * 12;
                let x = scale.x_of(note);
                p.line_segment(
                    [
                        Pos2::new(x, ruler.bottom() - 4.0),
                        Pos2::new(x, ruler.bottom()),
                    ],
                    Stroke::new(1.0, pal().text_faint),
                );
                if octave % 2 == 0 {
                    p.text(
                        Pos2::new(x + 2.0, ruler.center().y),
                        Align2::LEFT_CENTER,
                        super::pads::note_name(note),
                        theme::font(9.0),
                        pal().text_dim,
                    );
                }
            }
        }
        ZoneKind::Velocity | ZoneKind::Select => {
            let lowest = scale.lowest;
            for v in (lowest..=127).step_by(21) {
                let x = scale.x_of(v);
                p.text(
                    Pos2::new(x + 2.0, ruler.center().y),
                    Align2::LEFT_CENTER,
                    v.to_string(),
                    theme::font(9.0),
                    pal().text_dim,
                );
            }
        }
    }
    if kind != ZoneKind::Select {
        info(
            ui.interact(ruler, ui.scope_id().with("zone_ruler"), Sense::hover()),
            if kind == ZoneKind::Key {
                "Key Zone Editor — the notes each chain responds to"
            } else {
                "Velocity Zone Editor — the velocities each chain responds to"
            },
        );
        return;
    }
    // The Chain selector, a draggable marker along the ruler.
    let value = cx.session.chain_selector(r.chain, r.dev.id);
    let x = scale.x_of(value.round().clamp(0.0, 127.0) as u8) + scale.cell_w() / 2.0;
    let marker = Rect::from_center_size(
        Pos2::new(x, ruler.center().y),
        Vec2::new(10.0, ruler.height()),
    );
    let resp = ui.interact(
        ruler,
        ui.scope_id().with(("selector", r.dev.id)),
        Sense::click_and_drag(),
    );
    if resp.dragged() || resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let v = selector_value(scale, pos.x);
            cx.queue
                .push(r.chain, Action::Rack(r.dev.id, RackAction::Selector(v)));
        }
    }
    p.rect_filled(marker.shrink2(Vec2::new(0.0, 2.0)), 1.0, pal().accent);
    let filters = r.select_filters_ctrl;
    let resp = info(
        resp,
        "Chain selector — drag it to pick which chains play; right-click for options",
    );
    let (chain, rack) = (r.chain, r.dev.id);
    resp.context_menu(|ui| {
        if ui
            .selectable_label(filters, "Chain Selector Filters MIDI Ctrl")
            .clicked()
        {
            cx.queue
                .push(chain, Action::Rack(rack, RackAction::FiltersCtrl(!filters)));
            ui.close();
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn zone_row(
    ui: &mut egui::Ui,
    cx: &mut DrawCx,
    r: &RackRef,
    kind: ZoneKind,
    chain: &Chain,
    row: Rect,
    scale: &ZoneScale,
    activity: &ChainActivityView,
) {
    let z = zone_of(chain, kind);
    let selected = r.view.selection.contains(&chain.id);
    let p = ui.painter().clone();
    if selected {
        p.rect_filled(row, 0.0, shade(0x333b44));
    }
    let color = chain.color.map_or(pal().accent, rgb);
    let lower = Rect::from_min_max(
        Pos2::new(scale.x_of(z.fade_low), row.top() + UPPER_H + 2.0),
        Pos2::new(scale.end_of(z.fade_high), row.bottom() - 1.0),
    );
    let upper_y = (row.top() + 1.0, row.top() + 1.0 + UPPER_H);
    p.rect_filled(
        Rect::from_min_max(
            Pos2::new(lower.left(), upper_y.0),
            Pos2::new(lower.right(), upper_y.1),
        ),
        0.0,
        theme::darken(color, 0.7),
    );
    let plateau = Rect::from_min_max(
        Pos2::new(scale.x_of(z.low), upper_y.0),
        Pos2::new(scale.end_of(z.high), upper_y.1),
    );
    p.rect_filled(plateau, 0.0, color);
    p.rect_filled(lower, 1.0, theme::darken(color, 0.45));
    // The fades ramp from nothing at the far edge to the full level at the plateau.
    if z.fade_low < z.low {
        fade_ramp(&p, lower, plateau.left(), lower.left(), color);
    }
    if z.fade_high > z.high {
        fade_ramp(&p, lower, plateau.right(), lower.right(), color);
    }
    if activity.is_active(chain.id) {
        p.circle_filled(
            Pos2::new(row.right() - 5.0, row.center().y),
            2.5,
            pal().accent,
        );
    }

    let id = ui.scope_id().with(("zone", kind_tag(kind), chain.id));
    let resp = ui.interact(row, id, Sense::click_and_drag());
    let hit_at = |pos: Pos2| hit_test(z, scale, pos.x, pos.y <= upper_y.1 + 1.0);
    if let Some(pos) = resp.hover_pos() {
        match hit_at(pos) {
            Some(
                ZoneHit::ExtentLow
                | ZoneHit::ExtentHigh
                | ZoneHit::PlateauLow
                | ZoneHit::PlateauHigh,
            ) => ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal),
            Some(ZoneHit::Move) => ui.ctx().set_cursor_icon(CursorIcon::Grab),
            None => {}
        }
    }
    if resp.drag_started() {
        // A drag only starts once the pointer has moved, so ask where it was pressed.
        if let Some(pos) = ui.input(|i| i.pointer.press_origin()) {
            if let Some(hit) = hit_at(pos) {
                cx.ui.zone_drag = Some(ZoneDrag {
                    chain: chain.id,
                    kind,
                    hit,
                    original: z,
                    start_x: pos.x,
                });
            }
        }
    }
    if resp.dragged() {
        if let (Some(drag), Some(pos)) = (cx.ui.zone_drag, resp.interact_pointer_pos()) {
            if drag.chain == chain.id && drag.kind == kind {
                let units = scale.units(pos.x - drag.start_x);
                let next = apply_drag(drag.original, drag.hit, units, scale.lowest, scale.highest);
                if next != z {
                    cx.queue.push(
                        r.chain,
                        Action::Rack(r.dev.id, RackAction::Zone(chain.id, kind, next)),
                    );
                }
            }
        }
    }
    if resp.drag_stopped() {
        cx.ui.zone_drag = None;
    }
    info(
        resp,
        match kind {
            ZoneKind::Key => "Key Zone — drag to move, drag its edges to resize or fade",
            ZoneKind::Velocity => "Velocity Zone — drag to move, drag its edges to resize or fade",
            ZoneKind::Select => {
                "Chain Select Zone — drag to move, drag its edges to resize or fade"
            }
        },
    );
}

fn fade_ramp(p: &egui::Painter, lower: Rect, full_x: f32, far_x: f32, color: Color32) {
    p.add(egui::Shape::convex_polygon(
        vec![
            Pos2::new(far_x, lower.bottom()),
            Pos2::new(full_x, lower.top()),
            Pos2::new(full_x, lower.bottom()),
        ],
        color,
        Stroke::NONE,
    ));
}

fn kind_tag(kind: ZoneKind) -> u8 {
    match kind {
        ZoneKind::Key => 0,
        ZoneKind::Velocity => 1,
        ZoneKind::Select => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn zone(fade_low: u8, low: u8, high: u8, fade_high: u8) -> Zone {
        Zone {
            low,
            high,
            fade_low,
            fade_high,
        }
    }

    fn scale() -> ZoneScale {
        ZoneScale {
            left: 0.0,
            width: 128.0,
            lowest: 0,
            highest: 127,
        }
    }

    #[test]
    fn a_scale_has_one_cell_per_value() {
        let s = scale();
        assert_eq!(s.x_of(10), 10.0);
        assert_eq!(s.end_of(10), 11.0);
        assert_eq!(s.value_at(10.5), 10);
        assert_eq!(s.value_at(-5.0), 0);
        assert_eq!(s.value_at(500.0), 127);
        assert_eq!(s.units(7.4), 7);
    }

    #[test]
    fn the_velocity_scale_starts_at_one() {
        let s = ZoneScale::of(ZoneKind::Velocity, 0.0, 127.0);
        assert_eq!(s.lowest, 1);
        assert_eq!(s.x_of(1), 0.0);
        assert_eq!(s.value_at(0.5), 1);
        assert_eq!(s.value_at(126.5), 127);
    }

    #[test]
    fn edges_of_the_lower_section_resize_and_its_middle_moves() {
        let z = zone(40, 40, 80, 80);
        let s = scale();
        assert_eq!(hit_test(z, &s, 40.0, false), Some(ZoneHit::ExtentLow));
        assert_eq!(hit_test(z, &s, 81.0, false), Some(ZoneHit::ExtentHigh));
        assert_eq!(hit_test(z, &s, 60.0, false), Some(ZoneHit::Move));
        assert_eq!(hit_test(z, &s, 10.0, false), None);
        assert_eq!(hit_test(z, &s, 100.0, false), None);
    }

    #[test]
    fn edges_of_the_upper_section_set_the_fades() {
        let z = zone(30, 50, 70, 90);
        let s = scale();
        assert_eq!(hit_test(z, &s, 50.0, true), Some(ZoneHit::PlateauLow));
        assert_eq!(hit_test(z, &s, 71.0, true), Some(ZoneHit::PlateauHigh));
        assert_eq!(hit_test(z, &s, 60.0, true), Some(ZoneHit::Move));
        assert_eq!(hit_test(z, &s, 30.0, true), Some(ZoneHit::ExtentLow));
    }

    #[test]
    fn moving_keeps_the_shape_and_stops_at_the_ends() {
        let z = zone(30, 50, 70, 90);
        assert_eq!(
            apply_drag(z, ZoneHit::Move, 10, 0, 127),
            zone(40, 60, 80, 100)
        );
        assert_eq!(
            apply_drag(z, ZoneHit::Move, 200, 0, 127),
            zone(67, 87, 107, 127)
        );
        assert_eq!(
            apply_drag(z, ZoneHit::Move, -200, 0, 127),
            zone(0, 20, 40, 60)
        );
    }

    #[test]
    fn resizing_an_edge_with_no_fade_takes_the_plateau_along() {
        let z = zone(40, 40, 80, 80);
        assert_eq!(
            apply_drag(z, ZoneHit::ExtentLow, -10, 0, 127),
            zone(30, 30, 80, 80)
        );
        assert_eq!(
            apply_drag(z, ZoneHit::ExtentHigh, 5, 0, 127),
            zone(40, 40, 85, 85)
        );
    }

    #[test]
    fn resizing_an_edge_with_a_fade_keeps_the_plateau_unless_it_is_overrun() {
        let z = zone(30, 50, 70, 90);
        assert_eq!(
            apply_drag(z, ZoneHit::ExtentLow, 10, 0, 127),
            zone(40, 50, 70, 90),
            "the fade got shorter"
        );
        assert_eq!(
            apply_drag(z, ZoneHit::ExtentLow, 30, 0, 127),
            zone(60, 60, 70, 90),
            "the fade ran into the plateau"
        );
        assert_eq!(
            apply_drag(z, ZoneHit::ExtentHigh, -30, 0, 127),
            zone(30, 50, 60, 60),
        );
    }

    #[test]
    fn plateau_edges_stay_inside_the_zone() {
        let z = zone(30, 50, 70, 90);
        assert_eq!(
            apply_drag(z, ZoneHit::PlateauLow, -40, 0, 127),
            zone(30, 30, 70, 90)
        );
        assert_eq!(
            apply_drag(z, ZoneHit::PlateauLow, 40, 0, 127),
            zone(30, 70, 70, 90)
        );
        assert_eq!(
            apply_drag(z, ZoneHit::PlateauHigh, 40, 0, 127),
            zone(30, 50, 90, 90)
        );
    }

    #[test]
    fn dragging_a_velocity_zone_never_goes_below_one() {
        let z = zone(1, 1, 127, 127);
        assert_eq!(
            apply_drag(z, ZoneHit::ExtentLow, -50, 1, 127),
            zone(1, 1, 127, 127)
        );
    }

    #[test]
    fn the_extent_edges_cannot_cross() {
        let z = zone(50, 50, 60, 60);
        let down = apply_drag(z, ZoneHit::ExtentHigh, -30, 0, 127);
        assert_eq!(down, zone(50, 50, 50, 50));
        let up = apply_drag(z, ZoneHit::ExtentLow, 30, 0, 127);
        assert_eq!(up, zone(60, 60, 60, 60));
    }

    #[test]
    fn the_selector_lands_on_whole_values() {
        let s = scale();
        assert_eq!(selector_value(&s, 10.4), 10.0);
        assert_eq!(selector_value(&s, 500.0), 127.0);
    }
}
