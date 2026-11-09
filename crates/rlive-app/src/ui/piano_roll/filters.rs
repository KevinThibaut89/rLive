//! The Find and Select filters (manual pp. 260-262).

use super::{Shown, UiState};
use crate::session::Session;
use crate::ui::theme::*;
use crate::ui::widgets::{info, state_button};
use egui::{Rect, RichText, UiBuilder, Vec2};
use rlive_core::PPQ;
use rlive_devices::devices::b1::common::SCALES;
use rlive_project::note_select::{find, ActiveFilter, Condition, Filter};

pub const HEIGHT: f32 = 44.0;

const NAMES: [&str; 8] = [
    "Pitch",
    "Time",
    "Chance",
    "Condition",
    "Count",
    "Duration",
    "Scale",
    "Velocity",
];

const KEY_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// A filter's settings and whether it's applied.
pub struct FilterRow {
    pub filter: Filter,
    pub invert: bool,
    /// Whether it's in the selection now, shown by the dot.
    pub applied: bool,
}

pub struct FilterState {
    /// Find and Select is showing.
    pub open: bool,
    /// Which filter's controls are shown.
    pub current: usize,
    pub rows: [FilterRow; 8],
    /// Use Clip Scale for the Scale filter.
    pub use_clip_scale: bool,
    pub scale_root: u8,
    pub scale_index: usize,
}

impl Default for FilterState {
    fn default() -> Self {
        let row = |filter| FilterRow {
            filter,
            invert: false,
            applied: false,
        };
        Self {
            open: false,
            current: 0,
            rows: [
                row(Filter::Pitch(1)),
                row(Filter::Time {
                    start: 0,
                    length: PPQ,
                    repeat: 0,
                }),
                row(Filter::Chance { min: 0.0, max: 1.0 }),
                row(Filter::Condition(Condition::Active)),
                row(Filter::Count {
                    every: 2,
                    offset: 0,
                    quantized: false,
                }),
                row(Filter::Duration {
                    min: 0,
                    max: 4 * PPQ,
                }),
                row(Filter::Scale((0, 0b1010_1011_0101))),
                row(Filter::Velocity { min: 0.0, max: 1.0 }),
            ],
            use_clip_scale: true,
            scale_root: 0,
            scale_index: 0,
        }
    }
}

impl FilterState {
    /// The filters applied now.
    pub fn active(&self) -> Vec<ActiveFilter> {
        self.rows
            .iter()
            .filter(|r| r.applied)
            .map(|r| ActiveFilter {
                filter: r.filter.clone(),
                invert: r.invert,
            })
            .collect()
    }

    /// Stops applying them all, as a click in the editor does.
    pub fn clear_applied(&mut self) {
        for r in &mut self.rows {
            r.applied = false;
        }
    }

    /// Whether any filter is applied.
    pub fn any_applied(&self) -> bool {
        self.rows.iter().any(|r| r.applied)
    }

    /// The Time filter.
    pub fn set_time(&mut self, filter: Filter) {
        self.rows[1].filter = filter;
        self.rows[1].applied = true;
    }
}

/// Selects what the applied filters find, in each clip that can be edited now.
pub fn apply(session: &Session, state: &mut UiState, shown: &[Shown], fg: usize) {
    let filters = state.piano.filters.active();
    let grid = state.piano.grid;
    let focused = state.piano.focused();
    let scale = session.clip_at(shown[fg].r).and_then(Session::pitch_scale);
    let resolved: Vec<ActiveFilter> = filters
        .into_iter()
        .map(|mut f| {
            if state.piano.filters.use_clip_scale {
                if let (Filter::Scale(s), Some(clip)) = (&mut f.filter, scale) {
                    *s = clip;
                }
            }
            f
        })
        .collect();
    state.piano.selected = find(shown[fg].clip.notes(), &resolved, grid);
    state.piano.others.clear();
    if !focused {
        for (i, s) in shown.iter().enumerate() {
            if i != fg {
                let found = find(s.clip.notes(), &resolved, grid);
                if !found.is_empty() {
                    state.piano.others.insert(s.r, found);
                }
            }
        }
    }
}

/// The filter strip: the filter chooser, its controls, Invert and Select.
pub fn strip(
    ui: &mut egui::Ui,
    rect: Rect,
    session: &mut Session,
    state: &mut UiState,
    shown: &[Shown],
    fg: usize,
) {
    ui.painter().rect_filled(rect, 0.0, shade(0x2c2c2c));
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(rect.shrink2(Vec2::new(6.0, 3.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    child.spacing_mut().item_spacing = Vec2::new(4.0, 3.0);
    let f = &mut state.piano.filters;
    let mut changed = false;
    let label = format!(
        "{}{}",
        if f.rows[f.current].applied {
            "● "
        } else {
            ""
        },
        NAMES[f.current]
    );
    let dots: Vec<bool> = f.rows.iter().map(|r| r.applied).collect();
    let current = &mut f.current;
    egui::ComboBox::from_id_salt("note_filter")
        .width(82.0)
        .selected_text(label)
        .show_ui(&mut child, |ui| {
            for (i, name) in NAMES.iter().enumerate() {
                let text = if dots[i] {
                    format!("● {name}")
                } else {
                    (*name).to_string()
                };
                ui.selectable_value(current, i, text);
            }
        });
    let i = f.current;
    changed |= filter_controls(&mut child, f, i);
    let r = child.add(
        egui::Button::new("Invert")
            .selected(f.rows[i].invert)
            .small(),
    );
    if info(r, "Invert — select what the filter doesn't find").clicked() {
        f.rows[i].invert = !f.rows[i].invert;
        changed = true;
    }
    let select = child.small_button("Select");
    let select = info(select, "Select — apply this filter again").clicked();
    if changed || select {
        f.rows[i].applied = true;
        apply(session, state, shown, fg);
    }
}

fn slider_u(ui: &mut egui::Ui, label: &str, v: &mut u64, lo: u64, hi: u64, speed: f64) -> bool {
    ui.label(RichText::new(label).color(pal().text_dim));
    let mut x = *v as f64;
    let changed = ui
        .add(
            egui::DragValue::new(&mut x)
                .range(lo as f64..=hi as f64)
                .speed(speed),
        )
        .changed();
    if changed {
        *v = x as u64;
    }
    changed
}

fn range_01(ui: &mut egui::Ui, min: &mut f32, max: &mut f32, scale: f32) -> bool {
    let (mut lo, mut hi) = (*min * scale, *max * scale);
    let a = ui.add(
        egui::DragValue::new(&mut lo)
            .range(0.0..=scale)
            .speed(1.0)
            .max_decimals(0),
    );
    let b = ui.add(
        egui::DragValue::new(&mut hi)
            .range(0.0..=scale)
            .speed(1.0)
            .max_decimals(0),
    );
    if a.changed() || b.changed() {
        *min = lo / scale;
        *max = hi / scale;
        true
    } else {
        false
    }
}

/// The current filter's controls. Returns whether they were changed.
fn filter_controls(ui: &mut egui::Ui, f: &mut FilterState, i: usize) -> bool {
    let mut changed = false;
    match &mut f.rows[i].filter {
        Filter::Pitch(mask) => {
            for (k, name) in KEY_NAMES.iter().enumerate() {
                let on = *mask & (1 << k) != 0;
                if state_button(ui, Vec2::new(20.0, 16.0), on, pal().accent, name).clicked() {
                    *mask ^= 1 << k;
                    changed = true;
                }
            }
        }
        Filter::Time {
            start,
            length,
            repeat,
        } => {
            changed |= slider_u(ui, "Start", start, 0, u64::MAX / 2, 60.0);
            changed |= slider_u(ui, "Length", length, 1, u64::MAX / 2, 60.0);
            changed |= slider_u(ui, "Repeat", repeat, 0, u64::MAX / 2, 60.0);
        }
        Filter::Chance { min, max } => {
            ui.label(RichText::new("Chance %").color(pal().text_dim));
            changed |= range_01(ui, min, max, 100.0);
        }
        Filter::Condition(c) => {
            for (kind, name) in [
                (Condition::Active, "Active"),
                (Condition::Chance, "Chance"),
                (Condition::VelocityDeviation, "Velocity Deviation"),
            ] {
                if ui.selectable_label(*c == kind, name).clicked() {
                    *c = kind;
                    changed = true;
                }
            }
        }
        Filter::Count {
            every,
            offset,
            quantized,
        } => {
            ui.label(RichText::new("Every").color(pal().text_dim));
            changed |= ui.add(egui::DragValue::new(every).range(1..=64)).changed();
            ui.label(RichText::new("Offset").color(pal().text_dim));
            changed |= ui.add(egui::DragValue::new(offset).range(0..=64)).changed();
            if ui.selectable_label(*quantized, "Quantized").clicked() {
                *quantized = !*quantized;
                changed = true;
            }
        }
        Filter::Duration { min, max } => {
            changed |= slider_u(ui, "Min", min, 0, u64::MAX / 2, 30.0);
            changed |= slider_u(ui, "Max", max, 0, u64::MAX / 2, 30.0);
        }
        Filter::Scale(scale) => {
            let toggled = ui
                .selectable_label(f.use_clip_scale, "Use Clip Scale")
                .clicked();
            if toggled {
                f.use_clip_scale = !f.use_clip_scale;
                changed = true;
            }
            if !f.use_clip_scale {
                let before = (f.scale_root, f.scale_index);
                egui::ComboBox::from_id_salt("filter_scale_root")
                    .width(38.0)
                    .selected_text(KEY_NAMES[f.scale_root as usize % 12])
                    .show_ui(ui, |ui| {
                        for (n, name) in KEY_NAMES.iter().enumerate() {
                            ui.selectable_value(&mut f.scale_root, n as u8, *name);
                        }
                    });
                egui::ComboBox::from_id_salt("filter_scale_name")
                    .width(100.0)
                    .selected_text(SCALES[f.scale_index].0)
                    .show_ui(ui, |ui| {
                        for (n, (name, _)) in SCALES.iter().enumerate() {
                            ui.selectable_value(&mut f.scale_index, n, *name);
                        }
                    });
                if before != (f.scale_root, f.scale_index) {
                    changed = true;
                }
                *scale = (f.scale_root, SCALES[f.scale_index].1);
            }
        }
        Filter::Velocity { min, max } => {
            ui.label(RichText::new("Velocity").color(pal().text_dim));
            changed |= range_01(ui, min, max, 127.0);
        }
    }
    changed
}
