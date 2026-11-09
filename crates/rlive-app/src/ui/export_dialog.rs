//! The Export Audio/Video dialog (manual pp. 126-130).

use super::{pal, RliveApp};
use crate::session::{ExportOptions, ExportRange, RenderedTrack};
use rlive_audio_io::{BitDepth, Dither, ExportType, PcmFormat};
use rlive_core::PPQ;
use std::path::PathBuf;

/// What the dialog is working on.
#[derive(Default)]
pub struct ExportUi {
    pub options: ExportOptions,
    scene: bool,
    scene_index: usize,
    scene_bars: u32,
    pending: Option<(PathBuf, Vec<PathBuf>)>,
}

const RATES: [u32; 7] = [22_050, 32_000, 44_100, 48_000, 88_200, 96_000, 192_000];

impl ExportUi {
    pub fn is_blocking(&self) -> bool {
        self.pending.is_some()
    }
}

fn ticks_row(
    ui: &mut egui::Ui,
    label: &str,
    ticks: &mut u64,
    ticks_per_bar: u64,
    ticks_per_beat: u64,
) {
    let mut bars = *ticks / ticks_per_bar;
    let mut beats = (*ticks % ticks_per_bar) / ticks_per_beat;
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(egui::DragValue::new(&mut bars).suffix(" bars"));
        ui.add(
            egui::DragValue::new(&mut beats)
                .range(0..=(ticks_per_bar / ticks_per_beat).saturating_sub(1))
                .suffix(" beats"),
        );
    });
    *ticks = bars * ticks_per_bar + beats * ticks_per_beat;
}

impl RliveApp {
    /// Sets the dialog up for the moment it opens: the whole Set, or the time selection.
    pub(super) fn open_export_dialog(&mut self) {
        let e = &mut self.ui.files.export;
        e.scene = false;
        let selected = self
            .ui
            .arr
            .time
            .as_ref()
            .filter(|t| !t.is_point())
            .map(|t| t.ticks());
        e.options.range = match selected {
            Some((a, b)) => ExportRange::Timeline {
                start: a,
                length: b - a,
            },
            None => self.session.whole_set_range(),
        };
        if e.options.rendered == RenderedTrack::AllIndividual
            || matches!(e.options.rendered, RenderedTrack::Selected(_))
        {
            e.options.rendered = RenderedTrack::Main;
        }
        self.ui.show_export = true;
    }

    fn track_chooser(&mut self, ui: &mut egui::Ui) {
        let tracks: Vec<_> = self
            .session
            .project
            .tracks
            .iter()
            .chain(&self.session.project.returns)
            .map(|t| (t.id, t.name.clone()))
            .collect();
        let selected = self.ui.selected_tracks.clone();
        let e = &mut self.ui.files.export;
        let label = match &e.options.rendered {
            RenderedTrack::Main => "Main".to_string(),
            RenderedTrack::AllIndividual => "All Individual Tracks".to_string(),
            RenderedTrack::Selected(_) => "Selected Tracks Only".to_string(),
            RenderedTrack::One(id) => tracks
                .iter()
                .find(|t| t.0 == *id)
                .map_or_else(|| "Track".to_string(), |t| t.1.clone()),
        };
        ui.horizontal(|ui| {
            ui.label("Rendered Track");
            egui::ComboBox::from_id_salt("rendered track")
                .selected_text(label)
                .width(200.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut e.options.rendered, RenderedTrack::Main, "Main");
                    ui.selectable_value(
                        &mut e.options.rendered,
                        RenderedTrack::AllIndividual,
                        "All Individual Tracks",
                    );
                    if !selected.is_empty() {
                        ui.selectable_value(
                            &mut e.options.rendered,
                            RenderedTrack::Selected(selected.clone()),
                            "Selected Tracks Only",
                        );
                    }
                    ui.separator();
                    for (id, name) in &tracks {
                        ui.selectable_value(&mut e.options.rendered, RenderedTrack::One(*id), name);
                    }
                });
        });
    }

    fn range_section(&mut self, ui: &mut egui::Ui) {
        let scenes = self.session.project.scenes.len();
        let sig = self.session.project.time_signature;
        let (bar, beat) = (sig.ticks_per_bar(), sig.ticks_per_beat());
        let whole = self.session.whole_set_range();
        let e = &mut self.ui.files.export;
        ui.horizontal(|ui| {
            ui.radio_value(&mut e.scene, false, "Arrangement");
            ui.radio_value(&mut e.scene, true, "Scene");
        });
        if e.scene {
            e.options.render_as_loop = false;
            ui.horizontal(|ui| {
                ui.label("Scene");
                ui.add(
                    egui::DragValue::new(&mut e.scene_index)
                        .range(0..=scenes.saturating_sub(1))
                        .custom_formatter(|v, _| format!("{}", v as usize + 1)),
                );
                if e.scene_bars == 0 {
                    e.scene_bars = 4;
                }
                ui.add(
                    egui::DragValue::new(&mut e.scene_bars)
                        .range(1..=512)
                        .suffix(" bars"),
                );
            });
            e.options.range = ExportRange::Scene {
                scene: e.scene_index,
                bars: e.scene_bars,
            };
            return;
        }
        let (mut start, mut length) = match e.options.range {
            ExportRange::Timeline { start, length } => (start, length),
            ExportRange::Scene { .. } => match whole {
                ExportRange::Timeline { start, length } => (start, length),
                _ => (0, bar),
            },
        };
        ticks_row(ui, "Render Start", &mut start, bar, beat);
        ticks_row(ui, "Render Length", &mut length, bar, beat);
        if length == 0 {
            length = PPQ;
        }
        e.options.range = ExportRange::Timeline { start, length };
    }

    fn encoding_section(&mut self, ui: &mut egui::Ui) {
        let e = &mut self.ui.files.export;
        let mut use_pcm = e.options.pcm.is_some();
        ui.checkbox(&mut use_pcm, "Encode PCM");
        if use_pcm {
            let mut f = e.options.pcm.unwrap_or(PcmFormat {
                kind: ExportType::Wav,
                depth: BitDepth::Bits24,
                dither: Dither::Triangular,
            });
            ui.horizontal(|ui| {
                ui.label("File Type");
                egui::ComboBox::from_id_salt("pcm file type")
                    .selected_text(f.kind.label())
                    .show_ui(ui, |ui| {
                        for k in ExportType::ALL {
                            ui.selectable_value(&mut f.kind, k, k.label());
                        }
                    });
                ui.label("Bit Depth");
                egui::ComboBox::from_id_salt("pcm bits")
                    .selected_text(f.depth.label())
                    .show_ui(ui, |ui| {
                        for d in BitDepth::ALL {
                            ui.selectable_value(&mut f.depth, d, d.label());
                        }
                    });
            });
            ui.add_enabled_ui(f.depth != BitDepth::Bits32, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Dither");
                    egui::ComboBox::from_id_salt("pcm dither")
                        .selected_text(f.dither.label())
                        .show_ui(ui, |ui| {
                            for d in Dither::ALL {
                                ui.selectable_value(&mut f.dither, d, d.label());
                            }
                        });
                });
            });
            e.options.pcm = Some(f);
        } else {
            e.options.pcm = None;
        }
        ui.add_enabled(
            rlive_audio_io::MP3_AVAILABLE,
            egui::Checkbox::new(&mut e.options.mp3, "Encode MP3 (CBR 320 kbps)"),
        );
    }

    /// The Export dialog, when it's open.
    pub(super) fn export_dialog(&mut self, ctx: &egui::Context) {
        if !self.ui.show_export {
            return;
        }
        let mut open = self.ui.show_export;
        let mut chosen = None;
        egui::Window::new("Export Audio/Video")
            .open(&mut open)
            .resizable(false)
            .default_width(340.0)
            .show(ctx, |ui| {
                ui.label(egui::RichText::new("Selection").strong());
                self.track_chooser(ui);
                self.range_section(ui);
                ui.separator();
                ui.label(egui::RichText::new("Rendering").strong());
                let e = &mut self.ui.files.export;
                let main_only = e.options.rendered == RenderedTrack::Main;
                ui.add_enabled(
                    !main_only,
                    egui::Checkbox::new(
                        &mut e.options.include_returns_and_main,
                        "Include Return and Main Effects",
                    ),
                );
                ui.add_enabled_ui(!e.scene, |ui| {
                    ui.checkbox(&mut e.options.render_as_loop, "Render as Loop");
                });
                ui.checkbox(&mut e.options.mono, "Convert to Mono");
                ui.checkbox(&mut e.options.normalize, "Normalize");
                ui.checkbox(&mut e.options.create_analysis, "Create Analysis File");
                ui.horizontal(|ui| {
                    ui.label("Sample Rate");
                    egui::ComboBox::from_id_salt("export rate")
                        .selected_text(format!("{}", e.options.sample_rate))
                        .show_ui(ui, |ui| {
                            for r in RATES {
                                ui.selectable_value(&mut e.options.sample_rate, r, r.to_string());
                            }
                        });
                });
                ui.separator();
                ui.label(egui::RichText::new("Encoding").strong());
                self.encoding_section(ui);
                ui.separator();
                let e = &self.ui.files.export;
                let can = e.options.pcm.is_some() || e.options.mp3;
                ui.horizontal(|ui| {
                    if ui.add_enabled(can, egui::Button::new("Export…")).clicked() {
                        chosen = Some(());
                    }
                    if !can {
                        ui.label(
                            egui::RichText::new("Choose PCM or MP3")
                                .small()
                                .color(pal().text_dim),
                        );
                    }
                });
            });
        self.ui.show_export &= open;
        if chosen.is_some() {
            self.pick_export_file();
        }
        self.confirm_replace_exports(ctx);
    }

    fn pick_export_file(&mut self) {
        let ext = self
            .ui
            .files
            .export
            .options
            .pcm
            .map_or("mp3", |p| p.kind.extension());
        let stem = self
            .session
            .path
            .as_deref()
            .and_then(|p| p.file_stem())
            .map_or_else(
                || "Export".to_string(),
                |s| s.to_string_lossy().into_owned(),
            );
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Audio", &[ext])
            .set_file_name(format!("{stem}.{ext}"))
            .save_file()
        else {
            return;
        };
        let replacing: Vec<PathBuf> = self
            .session
            .export_outputs(&self.ui.files.export.options, &path)
            .into_iter()
            .filter(|p| p.exists())
            .collect();
        if replacing.is_empty() {
            self.run_export(&path);
        } else {
            self.ui.files.export.pending = Some((path, replacing));
        }
    }

    fn confirm_replace_exports(&mut self, ctx: &egui::Context) {
        let Some((path, replacing)) = self.ui.files.export.pending.clone() else {
            return;
        };
        let mut close = false;
        egui::Modal::new(egui::Id::unique("confirm export replace")).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.label(format!(
                "{} of these files already exist. Replace them?",
                replacing.len()
            ));
            for f in replacing.iter().take(6) {
                ui.label(
                    egui::RichText::new(f.display().to_string())
                        .small()
                        .color(pal().text_dim),
                );
            }
            ui.horizontal(|ui| {
                if ui.button("Replace").clicked() {
                    self.run_export(&path);
                    close = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });
        if close {
            self.ui.files.export.pending = None;
        }
    }

    fn run_export(&mut self, path: &std::path::Path) {
        let options = self.ui.files.export.options.clone();
        match self.session.start_export(&options, path) {
            Ok(files) => {
                self.ui.status = format!("Exporting {} files", files.len());
                self.ui.show_export = false;
            }
            Err(e) => self.ui.status = format!("Export failed: {e}"),
        }
    }
}
