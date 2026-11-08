//! Vector icons drawn with the painter, so they look the same with any font.

use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, Vec2};

fn s(rect: Rect, frac: f32) -> f32 {
    rect.width().min(rect.height()) * frac
}

pub fn play(p: &Painter, rect: Rect, color: Color32) {
    let c = rect.center();
    let r = s(rect, 0.3);
    p.add(Shape::convex_polygon(
        vec![
            Pos2::new(c.x - r * 0.75, c.y - r),
            Pos2::new(c.x + r, c.y),
            Pos2::new(c.x - r * 0.75, c.y + r),
        ],
        color,
        Stroke::NONE,
    ));
}

pub fn stop(p: &Painter, rect: Rect, color: Color32) {
    let r = s(rect, 0.24);
    p.rect_filled(
        Rect::from_center_size(rect.center(), Vec2::splat(r * 2.0)),
        0.0,
        color,
    );
}

pub fn record(p: &Painter, rect: Rect, color: Color32) {
    p.circle_filled(rect.center(), s(rect, 0.27), color);
}

pub fn record_ring(p: &Painter, rect: Rect, color: Color32) {
    p.circle_stroke(rect.center(), s(rect, 0.25), Stroke::new(1.2, color));
}

/// Metronome: a trapezoid with a pendulum.
pub fn metronome(p: &Painter, rect: Rect, color: Color32) {
    let c = rect.center();
    let h = s(rect, 0.36);
    let body = vec![
        Pos2::new(c.x - h * 0.35, c.y - h),
        Pos2::new(c.x + h * 0.35, c.y - h),
        Pos2::new(c.x + h * 0.8, c.y + h),
        Pos2::new(c.x - h * 0.8, c.y + h),
    ];
    p.add(Shape::closed_line(body, Stroke::new(1.3, color)));
    p.line_segment(
        [
            Pos2::new(c.x, c.y + h * 0.6),
            Pos2::new(c.x + h * 0.55, c.y - h * 0.7),
        ],
        Stroke::new(1.3, color),
    );
}

/// Power symbol (device activator).
pub fn power(p: &Painter, rect: Rect, color: Color32) {
    let c = rect.center();
    let r = s(rect, 0.28);
    let n = 20;
    let pts: Vec<Pos2> = (0..=n)
        .map(|i| {
            let a = -std::f32::consts::FRAC_PI_2
                + 0.65
                + (std::f32::consts::TAU - 1.3) * i as f32 / n as f32;
            c + Vec2::angled(a) * r
        })
        .collect();
    p.add(Shape::line(pts, Stroke::new(1.4, color)));
    p.line_segment(
        [c - Vec2::new(0.0, r * 1.15), c - Vec2::new(0.0, r * 0.2)],
        Stroke::new(1.4, color),
    );
}

/// Small triangle pointing right (folded) or down (unfolded).
pub fn fold(p: &Painter, rect: Rect, open: bool, color: Color32) {
    let c = rect.center();
    let r = s(rect, 0.22);
    let pts = if open {
        vec![
            Pos2::new(c.x - r, c.y - r * 0.6),
            Pos2::new(c.x + r, c.y - r * 0.6),
            Pos2::new(c.x, c.y + r * 0.7),
        ]
    } else {
        vec![
            Pos2::new(c.x - r * 0.6, c.y - r),
            Pos2::new(c.x + r * 0.7, c.y),
            Pos2::new(c.x - r * 0.6, c.y + r),
        ]
    };
    p.add(Shape::convex_polygon(pts, color, Stroke::NONE));
}

pub fn plus(p: &Painter, rect: Rect, color: Color32) {
    let c = rect.center();
    let r = s(rect, 0.25);
    let st = Stroke::new(1.5, color);
    p.line_segment([c - Vec2::new(r, 0.0), c + Vec2::new(r, 0.0)], st);
    p.line_segment([c - Vec2::new(0.0, r), c + Vec2::new(0.0, r)], st);
}

/// Panel toggle: a rectangle with a filled side (left = browser, bottom = detail view).
pub fn panel_toggle(p: &Painter, rect: Rect, side: PanelSide, on: bool, color: Color32) {
    let r = Rect::from_center_size(
        rect.center(),
        Vec2::new(s(rect, 0.62) * 1.25, s(rect, 0.62)),
    );
    p.rect_stroke(r, 1.0, Stroke::new(1.0, color), egui::StrokeKind::Inside);
    let fill = match side {
        PanelSide::Left => Rect::from_min_size(r.min, Vec2::new(r.width() * 0.35, r.height())),
        PanelSide::Bottom => {
            Rect::from_min_max(Pos2::new(r.left(), r.bottom() - r.height() * 0.4), r.max)
        }
    };
    if on {
        p.rect_filled(fill, 0.0, color);
    } else {
        p.rect_stroke(fill, 0.0, Stroke::new(1.0, color), egui::StrokeKind::Inside);
    }
}

#[derive(Clone, Copy)]
pub enum PanelSide {
    Left,
    Bottom,
}

/// Simple piano keyboard glyph (computer MIDI keyboard indicator).
pub fn keyboard(p: &Painter, rect: Rect, color: Color32) {
    let r = Rect::from_center_size(rect.center(), Vec2::new(s(rect, 0.8), s(rect, 0.5)));
    p.rect_stroke(r, 1.0, Stroke::new(1.0, color), egui::StrokeKind::Inside);
    for i in 1..4 {
        let x = r.left() + r.width() * i as f32 / 4.0;
        p.line_segment(
            [Pos2::new(x, r.top()), Pos2::new(x, r.bottom())],
            Stroke::new(1.0, color),
        );
        if i != 2 {
            p.rect_filled(
                Rect::from_center_size(
                    Pos2::new(x, r.top() + r.height() * 0.3),
                    Vec2::new(2.5, r.height() * 0.6),
                ),
                0.0,
                color,
            );
        }
    }
}

/// Automation Arm: a bent envelope line with a point on it.
pub fn automation_arm(p: &Painter, r: Rect, c: Color32) {
    let (x, y) = (r.center().x, r.center().y);
    let stroke = Stroke::new(1.5, c);
    p.line_segment(
        [Pos2::new(x - 7.0, y + 5.0), Pos2::new(x - 1.0, y - 3.0)],
        stroke,
    );
    p.line_segment(
        [Pos2::new(x - 1.0, y - 3.0), Pos2::new(x + 7.0, y + 2.0)],
        stroke,
    );
    p.circle_filled(Pos2::new(x - 1.0, y - 3.0), 2.5, c);
}

/// Re-Enable Automation: an arrow turning back on itself.
pub fn re_enable(p: &Painter, r: Rect, c: Color32) {
    let (x, y) = (r.center().x, r.center().y);
    let stroke = Stroke::new(1.5, c);
    p.line_segment(
        [Pos2::new(x - 5.0, y - 3.0), Pos2::new(x + 6.0, y - 3.0)],
        stroke,
    );
    p.line_segment(
        [Pos2::new(x + 6.0, y - 3.0), Pos2::new(x + 6.0, y + 4.0)],
        stroke,
    );
    p.line_segment(
        [Pos2::new(x + 6.0, y + 4.0), Pos2::new(x - 2.0, y + 4.0)],
        stroke,
    );
    p.line_segment(
        [Pos2::new(x - 5.0, y - 3.0), Pos2::new(x - 2.0, y - 6.0)],
        stroke,
    );
    p.line_segment([Pos2::new(x - 5.0, y - 3.0), Pos2::new(x - 2.0, y)], stroke);
}

/// Session Record: a dot inside a square.
pub fn session_record(p: &Painter, rect: Rect, color: Color32) {
    let r = rect.shrink(rect.width().min(rect.height()) * 0.2);
    p.rect_stroke(r, 1.0, Stroke::new(1.2, color), egui::StrokeKind::Inside);
    p.circle_filled(r.center(), r.width().min(r.height()) * 0.28, color);
}

/// Capture MIDI: a circle with a note inside.
pub fn capture_midi(p: &Painter, rect: Rect, color: Color32) {
    let c = rect.center();
    let r = rect.width().min(rect.height()) * 0.36;
    p.circle_stroke(c, r, Stroke::new(1.2, color));
    let head = c + Vec2::new(-1.0, r * 0.4);
    p.circle_filled(head, r * 0.28, color);
    p.line_segment(
        [
            head + Vec2::new(r * 0.26, 0.0),
            head + Vec2::new(r * 0.26, -r * 1.05),
        ],
        Stroke::new(1.2, color),
    );
}

/// The small downward arrow that opens a menu.
pub fn menu_arrow(p: &Painter, rect: Rect, color: Color32) {
    let c = rect.center();
    p.add(egui::Shape::convex_polygon(
        vec![
            c + Vec2::new(-3.0, -1.5),
            c + Vec2::new(3.0, -1.5),
            c + Vec2::new(0.0, 2.5),
        ],
        color,
        Stroke::NONE,
    ));
}
