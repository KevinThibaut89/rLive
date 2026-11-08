//! The editing grid: its width, and what snapping does with it.

use crate::ui::keymap::{self, Platform};
use crate::ui::timeline;
use egui::Modifiers;
use rlive_core::time::BarMap;
use rlive_core::PPQ;

const FINEST: u64 = PPQ / 8;
const WIDEST_LEVEL: i32 = -2;
const NARROWEST_LEVEL: i32 = 2;

/// Width of the grid, and whether it's on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridState {
    pub snap: bool,
    /// Fixed width, rather than following the zoom.
    pub fixed: bool,
    pub triplet: bool,
    /// How much denser than the default the adaptive grid is, in doublings.
    pub level: i32,
    /// The width while fixed, in ticks.
    pub width: u64,
}

impl Default for GridState {
    fn default() -> Self {
        Self {
            snap: true,
            fixed: false,
            triplet: false,
            level: 0,
            width: PPQ,
        }
    }
}

impl GridState {
    /// The spacing of the grid at this zoom.
    pub fn ticks(&self, px_per_beat: f32, ticks_per_bar: u64) -> u64 {
        let base = if self.fixed {
            self.width
        } else {
            let adaptive = timeline::adaptive_grid(px_per_beat, ticks_per_bar);
            if self.level >= 0 {
                (adaptive >> self.level).max(FINEST)
            } else {
                adaptive << -self.level
            }
        };
        if self.triplet {
            (base * 2 / 3).max(1)
        } else {
            base
        }
    }

    /// Doubles the density of the lines.
    pub fn narrow(&mut self) {
        if self.fixed {
            self.width = (self.width / 2).max(FINEST);
        } else {
            self.level = (self.level + 1).min(NARROWEST_LEVEL);
        }
    }

    /// Halves the density of the lines.
    pub fn widen(&mut self, ticks_per_bar: u64) {
        if self.fixed {
            self.width = (self.width * 2).min(ticks_per_bar * 8);
        } else {
            self.level = (self.level - 1).max(WIDEST_LEVEL);
        }
    }

    /// Whether a drag with these modifiers snaps: the bypass key flips the switch, so it can
    /// also turn a switched-off grid on for a moment.
    pub fn snapping(&self, m: Modifiers, platform: Platform) -> bool {
        self.snap != keymap::ignores_grid(m, platform)
    }

    /// The spacing as a note length, for the display beside the time ruler.
    pub fn label(&self, px_per_beat: f32, ticks_per_bar: u64) -> String {
        if !self.snap {
            return "Grid off".into();
        }
        spacing_label(self.ticks(px_per_beat, ticks_per_bar), ticks_per_bar)
    }
}

/// A grid spacing as note values or bars.
pub fn spacing_label(ticks: u64, ticks_per_bar: u64) -> String {
    let triplet = !ticks.is_multiple_of(PPQ / 8);
    let plain = if triplet { ticks * 3 / 2 } else { ticks };
    let suffix = if triplet { "T" } else { "" };
    if plain >= ticks_per_bar && plain.is_multiple_of(ticks_per_bar) {
        let bars = plain / ticks_per_bar;
        return if bars == 1 {
            format!("1 Bar{suffix}")
        } else {
            format!("{bars} Bars{suffix}")
        };
    }
    format!("1/{}{suffix}", PPQ * 4 / plain.max(1))
}

/// `target` snapped: to whichever of `objects` (clip edges, locators, signature markers) is
/// nearer than `tolerance`, or else to the grid, which restarts at each signature change.
pub fn snap_tick(
    target: f64,
    grid: u64,
    snapping: bool,
    bars: &BarMap,
    objects: &[u64],
    tolerance: f64,
) -> u64 {
    if !snapping || grid == 0 {
        return target.round().max(0.0) as u64;
    }
    let nearest = objects
        .iter()
        .map(|o| *o as f64)
        .min_by(|a, b| (a - target).abs().total_cmp(&(b - target).abs()));
    if let Some(o) = nearest.filter(|o| (o - target).abs() <= tolerance) {
        return o.max(0.0) as u64;
    }
    snap_to_grid(target, grid, bars)
}

/// The grid position nearest `target`, counting from the signature marker in force there.
pub fn snap_to_grid(target: f64, grid: u64, bars: &BarMap) -> u64 {
    let target = target.max(0.0);
    let start = bars.segment_start(target);
    let snapped = start + ((target - start) / grid as f64).round() * grid as f64;
    snapped.max(0.0) as u64
}

/// The grid position at or before `tick`.
pub fn grid_floor(tick: f64, grid: u64, bars: &BarMap) -> u64 {
    let tick = tick.max(0.0);
    let start = bars.segment_start(tick);
    (start + ((tick - start) / grid as f64).floor() * grid as f64) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use rlive_core::TimeSignature;

    const BAR: u64 = PPQ * 4;

    #[test]
    fn narrow_and_widen_double_and_halve_the_lines() {
        let mut g = GridState {
            fixed: true,
            width: PPQ,
            ..GridState::default()
        };
        g.narrow();
        assert_eq!(g.width, PPQ / 2);
        g.widen(BAR);
        g.widen(BAR);
        assert_eq!(g.width, PPQ * 2);
    }

    #[test]
    fn adaptive_density_shifts_with_narrow_and_widen() {
        let mut g = GridState::default();
        let base = g.ticks(20.0, BAR);
        g.narrow();
        assert_eq!(g.ticks(20.0, BAR), base / 2);
        g.widen(BAR);
        g.widen(BAR);
        assert_eq!(g.ticks(20.0, BAR), base * 2);
    }

    #[test]
    fn triplets_are_two_thirds_of_the_width() {
        let g = GridState {
            fixed: true,
            width: PPQ / 2,
            triplet: true,
            ..GridState::default()
        };
        assert_eq!(g.ticks(100.0, BAR), PPQ / 3);
    }

    #[test]
    fn the_modifier_flips_snapping_either_way() {
        let cmd = Modifiers {
            mac_cmd: true,
            ..Modifiers::NONE
        };
        let mut g = GridState::default();
        assert!(g.snapping(Modifiers::NONE, Platform::Mac));
        assert!(!g.snapping(cmd, Platform::Mac));
        g.snap = false;
        assert!(!g.snapping(Modifiers::NONE, Platform::Mac));
        assert!(g.snapping(cmd, Platform::Mac), "temporarily enables it");
    }

    #[test]
    fn spacing_reads_as_note_values() {
        assert_eq!(spacing_label(PPQ / 4, BAR), "1/16");
        assert_eq!(spacing_label(PPQ, BAR), "1/4");
        assert_eq!(spacing_label(BAR, BAR), "1 Bar");
        assert_eq!(spacing_label(BAR * 4, BAR), "4 Bars");
        assert_eq!(spacing_label(PPQ * 2 / 3, BAR), "1/4T");
    }

    fn bars() -> BarMap {
        BarMap::default()
    }

    #[test]
    fn snapping_rounds_to_the_grid() {
        assert_eq!(snap_tick(1000.0, PPQ, true, &bars(), &[], 0.0), PPQ);
        assert_eq!(snap_tick(1500.0, PPQ, true, &bars(), &[], 0.0), PPQ * 2);
        assert_eq!(snap_tick(1000.0, PPQ, false, &bars(), &[], 0.0), 1000);
    }

    #[test]
    fn snapping_prefers_objects_in_range_over_the_grid() {
        assert_eq!(
            snap_tick(1100.0, PPQ, true, &bars(), &[1150], 100.0),
            1150,
            "a clip edge, locator or marker in range wins"
        );
        assert_eq!(
            snap_tick(1100.0, PPQ, true, &bars(), &[1300], 100.0),
            PPQ,
            "out of range, the grid decides"
        );
    }

    #[test]
    fn objects_are_ignored_when_snapping_is_bypassed() {
        assert_eq!(snap_tick(1100.0, PPQ, false, &bars(), &[1150], 100.0), 1100);
    }

    #[test]
    fn the_grid_restarts_at_a_signature_marker() {
        let mid_bar = BarMap::new(TimeSignature::default(), [(1000, TimeSignature::default())]);
        // Beats counted from 1000, not from zero.
        assert_eq!(snap_to_grid(1500.0, PPQ, &mid_bar), 1000 + 480 + 480);
        assert_eq!(grid_floor(2100.0, PPQ, &mid_bar), 1000 + PPQ);
        assert_eq!(
            snap_to_grid(100.0, PPQ, &mid_bar),
            0,
            "before it, from zero"
        );
    }
}
