//! What the EQ and filter displays share: a log-frequency plot and a spectrum drawn behind it.

use crate::ui::theme::*;
use egui::{Painter, Pos2, Rect, Shape, Stroke};
use rlive_core::SCOPE_VALUES;

pub const MIN_HZ: f32 = 20.0;
pub const MAX_HZ: f32 = 20_000.0;

/// How many points a drawn curve has, across the plot's width.
const CURVE_POINTS: usize = 180;

pub fn hz_to_x(rect: Rect, hz: f32) -> f32 {
    let t = (hz.max(1.0) / MIN_HZ).log2() / (MAX_HZ / MIN_HZ).log2();
    rect.left() + t.clamp(0.0, 1.0) * rect.width()
}

pub fn x_to_hz(rect: Rect, x: f32) -> f32 {
    let t = ((x - rect.left()) / rect.width()).clamp(0.0, 1.0);
    MIN_HZ * (MAX_HZ / MIN_HZ).powf(t)
}

/// `y` for a level of `db` in a plot showing `-range_db..range_db`.
pub fn db_to_y(rect: Rect, db: f32, range_db: f32) -> f32 {
    let t = (db + range_db) / (2.0 * range_db);
    rect.bottom() - t.clamp(0.0, 1.0) * rect.height()
}

/// The background, with a line every decade and at 0 dB.
pub fn background(p: &Painter, rect: Rect, range_db: f32) {
    p.rect_filled(rect, 2.0, shade(0x121212));
    let line = Stroke::new(1.0, pal().separator);
    for hz in [100.0, 1_000.0, 10_000.0] {
        let x = hz_to_x(rect, hz);
        p.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            line,
        );
    }
    let y = db_to_y(rect, 0.0, range_db);
    p.line_segment(
        [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
        line,
    );
}

/// Draws a spectrum feed's bands, which span 20 Hz to Nyquist.
pub fn spectrum(p: &Painter, rect: Rect, bands: &[f32; SCOPE_VALUES], sample_rate: f32) {
    let nyquist = sample_rate * 0.5;
    let points: Vec<Pos2> = bands
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let hz = 20.0 * (nyquist / 20.0).powf(i as f32 / SCOPE_VALUES as f32);
            Pos2::new(
                hz_to_x(rect, hz),
                rect.bottom() - v.clamp(0.0, 1.0) * rect.height(),
            )
        })
        .collect();
    p.add(Shape::line(points, Stroke::new(1.0, pal().knob_track)));
}

/// Draws a response, given as a function from frequency to decibels.
pub fn curve(
    p: &Painter,
    rect: Rect,
    range_db: f32,
    stroke: Stroke,
    response: impl Fn(f32) -> f32,
) {
    let points: Vec<Pos2> = (0..CURVE_POINTS)
        .map(|i| {
            let x = rect.left() + rect.width() * i as f32 / (CURVE_POINTS - 1) as f32;
            Pos2::new(x, db_to_y(rect, response(x_to_hz(rect, x)), range_db))
        })
        .collect();
    p.add(Shape::line(points, stroke));
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Vec2;

    #[test]
    fn a_position_reads_back_the_frequency_it_was_drawn_at() {
        let rect = Rect::from_min_size(Pos2::new(10.0, 0.0), Vec2::new(300.0, 100.0));
        for hz in [20.0, 440.0, 18_000.0] {
            assert!((x_to_hz(rect, hz_to_x(rect, hz)) / hz - 1.0).abs() < 1e-3);
        }
        assert_eq!(db_to_y(rect, 15.0, 15.0), rect.top());
        assert_eq!(db_to_y(rect, -15.0, 15.0), rect.bottom());
    }
}
