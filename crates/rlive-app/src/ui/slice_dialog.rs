//! The Slicing dialog (manual p. 341).

use super::{pal, RliveApp};
use crate::session::{ClipRef, SliceBy, SliceOptions, MAX_SLICES};
use rlive_core::PPQ;
use std::path::PathBuf;

/// The choices in the dialog, for the clip it was opened on.
pub struct SliceDialog {
    pub clip: ClipRef,
    division: usize,
    preserve: bool,
    preset: Option<PathBuf>,
}

const DIVISIONS: [(&str, Option<f64>); 8] = [
    ("Transient", None),
    ("Warp Marker", None),
    ("1 Bar", Some(4.0)),
    ("1/2", Some(2.0)),
    ("1/4", Some(1.0)),
    ("1/8", Some(0.5)),
    ("1/16", Some(0.25)),
    ("1/32", Some(0.125)),
];

fn slice_by(division: usize, ticks_per_bar: u64) -> SliceBy {
    match division {
        0 => SliceBy::Transients,
        1 => SliceBy::WarpMarkers,
        2 => SliceBy::Grid(ticks_per_bar),
        n => SliceBy::Grid(
            (DIVISIONS[n.min(DIVISIONS.len() - 1)].1.unwrap_or(1.0) * PPQ as f64) as u64,
        ),
    }
}

impl SliceDialog {
    pub fn new(clip: ClipRef) -> Self {
        Self {
            clip,
            division: 0,
            preserve: true,
            preset: None,
        }
    }
}

impl RliveApp {
    /// The Slicing dialog, when it's open.
    pub(super) fn slice_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.ui.files.slice.take() else {
            return;
        };
        let presets = self.session.defaults().slicing();
        let bar = self.session.project.time_signature.ticks_per_bar();
        let by = slice_by(d.division, bar);
        let plan = self.session.slice_plan(d.clip, by);
        let count = plan.as_ref().map_or(0, |p| p.count());
        let (mut keep_open, mut run) = (true, false);
        egui::Window::new("Slice to New MIDI Track")
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                egui::ComboBox::from_label("Slice by")
                    .selected_text(DIVISIONS[d.division].0)
                    .show_ui(ui, |ui| {
                        for (i, (name, _)) in DIVISIONS.iter().enumerate() {
                            ui.selectable_value(&mut d.division, i, *name);
                        }
                    });
                let preset_name = d.preset.as_ref().and_then(|p| p.file_stem()).map_or_else(
                    || "Built-in".to_string(),
                    |s| s.to_string_lossy().into_owned(),
                );
                egui::ComboBox::from_label("Slicing Preset")
                    .selected_text(preset_name)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut d.preset, None, "Built-in");
                        for p in &presets {
                            let name = p
                                .file_stem()
                                .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
                            ui.selectable_value(&mut d.preset, Some(p.clone()), name);
                        }
                    });
                ui.checkbox(&mut d.preserve, "Preserve warped timing");
                match &plan {
                    Err(e) => {
                        ui.label(egui::RichText::new(e).color(pal().arm_red));
                    }
                    Ok(_) if count > MAX_SLICES => {
                        ui.label(
                            egui::RichText::new(format!(
                                "{count} slices is more than a Rack holds ({MAX_SLICES})"
                            ))
                            .color(pal().arm_red),
                        );
                    }
                    Ok(_) => {
                        ui.label(format!("{count} slices"));
                    }
                }
                let can = plan.is_ok() && count > 0 && count <= MAX_SLICES;
                ui.horizontal(|ui| {
                    if ui.add_enabled(can, egui::Button::new("OK")).clicked() {
                        run = true;
                    }
                    if ui.button("Cancel").clicked() {
                        keep_open = false;
                    }
                });
            });
        if run {
            let options = SliceOptions {
                by,
                preserve_warped: d.preserve,
                preset: d.preset.clone(),
            };
            match self.session.slice_to_new_track(d.clip, &options) {
                Ok(Some(track)) => {
                    self.ui.selected = Some(crate::session::ChainRef::Track(track));
                    self.ui.status = "Sliced to a new MIDI track".into();
                }
                Ok(None) => self.ui.status = "Slicing…".into(),
                Err(e) => self.ui.status = e,
            }
            keep_open = false;
        }
        if keep_open {
            self.ui.files.slice = Some(d);
        }
    }
}
