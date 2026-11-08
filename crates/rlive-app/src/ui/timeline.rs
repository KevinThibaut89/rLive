//! Time-axis helpers shared by the Arrangement and the piano roll.

use rlive_core::PPQ;

/// Maps between song ticks and horizontal screen positions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimeAxis {
    pub left: f32,
    pub px_per_beat: f32,
    pub scroll: f32,
}

impl TimeAxis {
    pub fn x(&self, tick: f64) -> f32 {
        self.left + (tick / PPQ as f64) as f32 * self.px_per_beat - self.scroll
    }

    pub fn tick(&self, x: f32) -> f64 {
        ((x - self.left + self.scroll) / self.px_per_beat) as f64 * PPQ as f64
    }

    /// Scroll position that keeps the tick under `px` in place after zooming.
    pub fn scroll_after_zoom(&self, px: f32, new_px_per_beat: f32) -> f32 {
        let beat = (px - self.left + self.scroll) / self.px_per_beat;
        beat * new_px_per_beat - (px - self.left)
    }
}

pub fn snap(ticks: f64, grid: u64, enabled: bool) -> i64 {
    if !enabled || grid == 0 {
        return ticks.round() as i64;
    }
    ((ticks / grid as f64).round() as i64) * grid as i64
}

/// The grid to snap and draw at this zoom: the finest with lines at least 18 px apart.
pub fn adaptive_grid(px_per_beat: f32, ticks_per_bar: u64) -> u64 {
    let mut step = PPQ / 4;
    while (step as f32 / PPQ as f32) * px_per_beat < 18.0 {
        step = if step < PPQ {
            step * 2
        } else if step < ticks_per_bar {
            ticks_per_bar
        } else {
            step * 2
        };
    }
    step
}

/// How many bars apart ruler numbers should be to stay legible.
pub fn ruler_bar_step(px_per_beat: f32, ticks_per_bar: u64) -> u64 {
    let bar_px = (ticks_per_bar as f32 / PPQ as f32) * px_per_beat;
    let mut bars = 1;
    while bars as f32 * bar_px < 44.0 {
        bars *= 2;
    }
    bars
}

#[cfg(test)]
mod tests {
    use super::*;

    const AXIS: TimeAxis = TimeAxis {
        left: 100.0,
        px_per_beat: 20.0,
        scroll: 30.0,
    };

    #[test]
    fn axis_round_trips() {
        for t in [0.0, 960.0, 12_345.0] {
            assert!((AXIS.tick(AXIS.x(t)) - t).abs() < 0.5);
        }
    }

    #[test]
    fn zoom_keeps_the_tick_under_the_pointer() {
        let px = 250.0;
        let tick = AXIS.tick(px);
        let zoomed = TimeAxis {
            scroll: AXIS.scroll_after_zoom(px, 55.0),
            px_per_beat: 55.0,
            ..AXIS
        };
        assert!((zoomed.tick(px) - tick).abs() < 0.5);
    }

    #[test]
    fn snap_rounds_to_the_grid_unless_disabled() {
        assert_eq!(snap(1000.0, 960, true), 960);
        assert_eq!(snap(1500.0, 960, true), 1920);
        assert_eq!(snap(1000.0, 960, false), 1000);
    }

    #[test]
    fn grid_widens_as_zoom_shrinks() {
        let bar = 3840;
        assert_eq!(adaptive_grid(400.0, bar), PPQ / 4);
        assert_eq!(adaptive_grid(20.0, bar), PPQ);
        assert_eq!(adaptive_grid(5.0, bar), bar);
        assert_eq!(adaptive_grid(1.0, bar), bar * 8);
    }
}
