//! The CPU meter and Overload indicator in the Control Bar, and the arithmetic behind the mixer's
//! Performance Impact meters (manual pp. 983-985, 988).

use super::commands::options::Options;
use super::theme::{self, pal, shade};
use super::{widgets, RliveApp};
use crate::settings::CpuText;
use egui::{Align2, Rect, Sense, Vec2};
use rlive_core::TrackId;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// How long the indicator stays lit after a dropout, so that a brief one can be seen.
const OVERLOAD_LIT: Duration = Duration::from_secs(2);
/// How many rectangles a Performance Impact meter has (manual p. 985).
pub const IMPACT_SEGMENTS: usize = 6;
/// A track using this share of the Set's total load lights every segment.
const IMPACT_FULL_SHARE: f32 = 0.4;
/// How fast the reading follows the load, per second.
const IMPACT_SMOOTHING: f32 = 3.0;

/// The meter as it is drawn: what it read last, and when something last overloaded.
#[derive(Default)]
pub struct CpuMeterState {
    average: f32,
    current: f32,
    /// When the indicator was last lit, and for what.
    cpu_until: Option<Instant>,
    disk_until: Option<Instant>,
    seen_overloads: u32,
    /// How many recording frames had been lost when last looked at.
    seen_dropped: u64,
    /// Accumulated rendering time per track, as last read, and the smoothed share of it.
    tracks: HashMap<TrackId, TrackImpact>,
}

#[derive(Default)]
struct TrackImpact {
    nanos: u64,
    share: f32,
}

impl CpuMeterState {
    /// Read the meter's inputs from the engine for one frame. `dropped` is how many recording
    /// frames have been lost so far.
    pub fn update(&mut self, shared: &rlive_engine::shared::EngineShared, dropped: u64, dt: f32) {
        self.average = shared.cpu_load.load();
        let peak = shared.cpu_current.take();
        // Fall back from the peak gently so the last spike stays visible for a moment.
        self.current = (self.current - dt * 0.7).max(peak);
        let overloads = shared.cpu_overloads.load(Ordering::Relaxed);
        if overloads != self.seen_overloads {
            self.seen_overloads = overloads;
            self.cpu_until = Some(Instant::now() + OVERLOAD_LIT);
        }
        if dropped != self.seen_dropped {
            self.seen_dropped = dropped;
            self.disk_until = Some(Instant::now() + OVERLOAD_LIT);
        }
    }

    /// Follow every track's share of the load, given its accumulated rendering time.
    pub fn update_tracks(&mut self, tracks: impl Iterator<Item = (TrackId, u64)>, dt: f32) {
        let mut sum = 0u64;
        let mut seen: Vec<(TrackId, u64)> = Vec::new();
        for (id, nanos) in tracks {
            let entry = self.tracks.entry(id).or_default();
            let delta = nanos.saturating_sub(entry.nanos);
            entry.nanos = nanos;
            sum += delta;
            seen.push((id, delta));
        }
        let amount = 1.0 - (-IMPACT_SMOOTHING * dt).exp();
        for (id, delta) in &seen {
            if let Some(entry) = self.tracks.get_mut(id) {
                let share = if sum > 0 {
                    *delta as f32 / sum as f32
                } else {
                    0.0
                };
                entry.share += (share - entry.share) * amount;
            }
        }
        self.tracks
            .retain(|id, _| seen.iter().any(|(t, _)| t == id));
    }

    /// How many of a track's six rectangles are lit.
    pub fn impact(&self, track: TrackId) -> usize {
        self.tracks
            .get(&track)
            .map_or(0, |t| impact_segments(t.share))
    }
}

/// Which share of the Set's load lights how many rectangles.
fn impact_segments(share: f32) -> usize {
    if share < 0.005 {
        0
    } else {
        ((share / IMPACT_FULL_SHARE * IMPACT_SEGMENTS as f32).ceil() as usize).min(IMPACT_SEGMENTS)
    }
}

/// Six rectangles, left to right, for one track.
pub fn paint_impact(painter: &egui::Painter, rect: Rect, lit: usize) {
    let gap = 1.0;
    let w = (rect.width() - gap * (IMPACT_SEGMENTS - 1) as f32) / IMPACT_SEGMENTS as f32;
    for i in 0..IMPACT_SEGMENTS {
        let x = rect.left() + i as f32 * (w + gap);
        let r = Rect::from_min_size(egui::pos2(x, rect.top()), Vec2::new(w, rect.height()));
        let color = if i < lit {
            if i >= 4 {
                pal().arm_red
            } else {
                pal().accent
            }
        } else {
            pal().field_bg
        };
        painter.rect_filled(r, 0.5, color);
    }
}

impl RliveApp {
    /// The CPU meter and the Overload indicator.
    pub(super) fn cpu_meter(&mut self, ui: &mut egui::Ui, h: f32, cramped: bool) {
        let settings = self.settings.cpu;
        let (rect, response) = ui.allocate_exact_size(
            Vec2::new(if cramped { 34.0 } else { 74.0 }, h),
            Sense::click(),
        );
        let meter_on = settings.meter_average || settings.meter_current;
        if meter_on {
            let painter = ui.painter();
            painter.rect_filled(rect, 1.0, pal().field_bg);
            let cpu = &self.ui.cpu;
            let bar = |level: f32, color: egui::Color32| {
                let width = rect.width() * level.clamp(0.0, 1.0);
                painter.rect_filled(
                    Rect::from_min_size(rect.min, Vec2::new(width, rect.height())),
                    1.0,
                    color,
                );
            };
            let over = cpu.average > 0.8 || cpu.current > 1.0;
            let tint = if over { pal().arm_red } else { shade(0x3c5a3c) };
            if settings.meter_current {
                bar(cpu.current, tint);
            }
            if settings.meter_average {
                let lighter = if over { pal().arm_red } else { shade(0x4a7050) };
                bar(cpu.average, lighter);
            }
            let reading = match settings.meter_text {
                CpuText::Average => cpu.average,
                CpuText::Current => cpu.current,
            };
            let percent = format!("{:.0}%", reading * 100.0);
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                if cramped {
                    percent
                } else {
                    format!("CPU {percent}")
                },
                theme::font(10.5),
                pal().text,
            );
        }
        let response = widgets::info(
            response,
            "CPU Load — how much of the audio deadline the engine is using. Right-click for options",
        );
        response.context_menu(|ui| self.cpu_menu(ui));
        if !meter_on {
            ui.painter().rect_stroke(
                rect,
                1.0,
                egui::Stroke::new(1.0, pal().field_bg),
                egui::StrokeKind::Inside,
            );
        }
        self.overload_indicator(ui, h);
    }

    fn cpu_menu(&mut self, ui: &mut egui::Ui) {
        let settings = self.settings.cpu;
        let mut item = |ui: &mut egui::Ui, on: bool, text: &str, command| {
            if ui.selectable_label(on, text).clicked() {
                self.options_command(command);
                ui.close();
            }
        };
        ui.label(egui::RichText::new("CPU Meter").color(pal().text_dim));
        item(
            ui,
            settings.meter_average,
            "Average",
            Options::ToggleCpuAverage,
        );
        item(
            ui,
            settings.meter_current,
            "Current",
            Options::ToggleCpuCurrent,
        );
        ui.separator();
        ui.label(egui::RichText::new("Meter Text Shows").color(pal().text_dim));
        item(
            ui,
            settings.meter_text == CpuText::Average,
            "Average",
            Options::CpuTextAverage,
        );
        item(
            ui,
            settings.meter_text == CpuText::Current,
            "Current",
            Options::CpuTextCurrent,
        );
        ui.separator();
        item(
            ui,
            settings.warn_overload,
            "Warn on Current CPU Overload",
            Options::ToggleCpuOverloadWarning,
        );
    }

    /// Lights up with "CPU" or "Disk" when the engine or disk has fallen behind.
    fn overload_indicator(&mut self, ui: &mut egui::Ui, h: f32) {
        let now = Instant::now();
        let (rect, response) = ui.allocate_exact_size(Vec2::new(32.0, h), Sense::click());
        let cpu = &self.ui.cpu;
        let disk_lit = cpu.disk_until.is_some_and(|t| now < t);
        let cpu_lit = self.settings.cpu.warn_overload && cpu.cpu_until.is_some_and(|t| now < t);
        let (text, background) = if disk_lit {
            ("Disk", pal().arm_red)
        } else if cpu_lit {
            ("CPU", pal().arm_red)
        } else {
            ("", pal().field_bg)
        };
        let painter = ui.painter();
        painter.rect_filled(rect, 1.0, background);
        if !text.is_empty() {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                text,
                theme::font(10.5),
                pal().text,
            );
        }
        let response = widgets::info(
            response,
            "Overload — lights on \"Disk\" when the disk can't keep up, and on \"CPU\" when warnings are on",
        );
        response.context_menu(|ui| {
            let warn = self.settings.cpu.warn_overload;
            if ui
                .selectable_label(warn, "CPU Overload Indicator")
                .clicked()
            {
                self.options_command(Options::ToggleCpuOverloadWarning);
                ui.close();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_track_with_nothing_to_do_lights_nothing() {
        assert_eq!(impact_segments(0.0), 0);
        assert_eq!(impact_segments(0.001), 0);
    }

    #[test]
    fn more_of_the_load_lights_more_rectangles() {
        let segments: Vec<usize> = [0.01, 0.1, 0.2, 0.35, 0.5, 1.0]
            .into_iter()
            .map(impact_segments)
            .collect();
        assert_eq!(segments, vec![1, 2, 3, 6, 6, 6]);
    }

    #[test]
    fn a_track_with_half_the_load_reads_more_than_one_with_a_tenth() {
        let (a, b) = (TrackId::new(), TrackId::new());
        let mut state = CpuMeterState::default();
        let mut nanos = 0;
        for _ in 0..200 {
            nanos += 1000;
            state.update_tracks([(a, nanos), (b, nanos / 9)].into_iter(), 0.05);
        }
        assert!(state.impact(a) >= state.impact(b));
        assert_eq!(
            state.impact(a),
            6,
            "the busy track is nearly all of the load"
        );
    }

    #[test]
    fn a_meter_that_stops_being_fed_follows_nothing_for_a_track_gone() {
        let (a, b) = (TrackId::new(), TrackId::new());
        let mut state = CpuMeterState::default();
        state.update_tracks([(a, 100), (b, 100)].into_iter(), 0.1);
        state.update_tracks([(a, 200)].into_iter(), 0.1);
        assert_eq!(state.impact(b), 0, "the deleted track is gone");
    }

    #[test]
    fn the_indicator_lights_on_a_new_overload_and_goes_out_after_a_while() {
        let shared = rlive_engine::shared::EngineShared::default();
        let mut state = CpuMeterState::default();
        state.update(&shared, 0, 0.016);
        assert!(state.cpu_until.is_none());
        shared.cpu_overloads.store(3, Ordering::Relaxed);
        state.update(&shared, 0, 0.016);
        let until = state.cpu_until.expect("lit");
        assert!(until > Instant::now());
        // A second update with no new overload doesn't extend it.
        state.update(&shared, 0, 0.016);
        assert_eq!(state.cpu_until, Some(until));
    }

    #[test]
    fn a_slow_disk_lights_the_disk_indicator() {
        let shared = rlive_engine::shared::EngineShared::default();
        let mut state = CpuMeterState::default();
        state.update(&shared, 0, 0.016);
        assert!(state.disk_until.is_none());
        state.update(&shared, 256, 0.016);
        assert!(state.disk_until.is_some());
    }

    #[test]
    fn the_current_reading_falls_back_gently_from_a_spike() {
        let shared = rlive_engine::shared::EngineShared::default();
        let mut state = CpuMeterState::default();
        shared.cpu_current.store_max(0.9);
        state.update(&shared, 0, 0.016);
        assert_eq!(state.current, 0.9);
        state.update(&shared, 0, 0.016);
        assert!(state.current < 0.9 && state.current > 0.5);
    }
}
