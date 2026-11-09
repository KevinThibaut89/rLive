//! Spectrum's display: level over frequency, with Max, hover readout and the amplitude legend.

use super::{PanelCtx, PanelDef};
use crate::ui::theme::{self, *};
use egui::{Align2, Pos2, Rect, Sense, Stroke, Vec2};
use rlive_core::ParamId;
use rlive_devices::devices::b8::spectrum as sp;

pub const PANEL: PanelDef = PanelDef {
    name: "spectrum_display",
    size: Vec2::new(420.0, 180.0),
    views: sp::VIEW_LEN,
    draw,
};

/// What the panel keeps between frames: whether the larger view is open.
#[derive(Clone, Default)]
struct State {
    expanded: bool,
}

fn draw(ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let rect = ui.max_rect();
    let id = ui.make_persistent_id(("spectrum_display", panel.device.id));
    let mut state: State = ui.data(|d| d.get_temp(id).unwrap_or_default());
    let toggled = plot(ui, panel, rect, id.with("main"));
    if toggled {
        state.expanded = !state.expanded;
    }
    if state.expanded {
        let mut open = true;
        egui::Window::new("Spectrum")
            .id(id.with("expanded"))
            .default_size([760.0, 360.0])
            .resizable(true)
            .open(&mut open)
            .show(ui.ctx(), |ui| {
                let area = ui.available_rect_before_wrap();
                let area = Rect::from_min_size(
                    area.min,
                    Vec2::new(area.width().max(320.0), area.height().max(200.0)),
                );
                ui.allocate_rect(area, Sense::hover());
                if plot(ui, panel, area, id.with("large")) {
                    state.expanded = false;
                }
            });
        if !open {
            state.expanded = false;
        }
    }
    ui.data_mut(|d| d.insert_temp(id, state));
}

/// Draws the plot. `true` when it was double-clicked, to toggle the expanded view.
fn plot(ui: &mut egui::Ui, panel: &mut PanelCtx, rect: Rect, id: egui::Id) -> bool {
    let response = ui.interact(rect, id, Sense::click_and_drag());
    let (mut curve, mut peaks) = (vec![0.0f32; sp::POINTS], vec![0.0f32; sp::POINTS]);
    let mut meta = [48_000.0f32, 4096.0, -96.0, 0.0];
    let live = panel.view.is_some_and(|v| {
        v.read(sp::CURVE, &mut curve)
            && v.read(sp::PEAKS, &mut peaks)
            && v.read(sp::META, &mut meta)
    });
    let sr = if meta[0] > 0.0 { meta[0] } else { 48_000.0 };
    let auto = panel.value(ParamId(sp::AUTO)).unwrap_or(0.0) >= 0.5;
    let (min_db, max_db) = if auto {
        (meta[2], meta[3])
    } else {
        (
            panel.value(ParamId(sp::RANGE_MIN)).unwrap_or(-96.0) as f32,
            panel.value(ParamId(sp::RANGE_MAX)).unwrap_or(6.0) as f32,
        )
    };
    let (min_db, max_db) = (min_db.min(max_db - 1.0), max_db);
    let scale_x = panel.value(ParamId(sp::SCALE_X)).unwrap_or(1.0).round() as usize;
    let show_max = panel.value(ParamId(sp::MAX)).unwrap_or(1.0) >= 0.5;
    let bins = panel.value(ParamId(sp::GRAPH)).unwrap_or(0.0).round() as usize == sp::GRAPH_BINS;

    let legend = 34.0;
    let plot = Rect::from_min_max(Pos2::new(rect.left() + legend, rect.top()), rect.max);
    let db_y = |db: f32| {
        let t = ((db - min_db) / (max_db - min_db)).clamp(0.0, 1.0);
        plot.bottom() - t * plot.height()
    };
    let col_x = |i: usize| plot.left() + i as f32 / (sp::POINTS - 1) as f32 * plot.width();

    let p = ui.painter_at(rect);
    p.rect_filled(rect, 2.0, shade(0x121212));
    let line = Stroke::new(1.0, pal().separator);
    // Amplitude grid every 12 dB.
    let mut level = (min_db / 12.0).ceil() as i32 * 12;
    while (level as f32) < max_db {
        let y = db_y(level as f32);
        p.line_segment(
            [Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)],
            line,
        );
        p.text(
            Pos2::new(rect.left() + 2.0, y),
            Align2::LEFT_CENTER,
            format!("{level}"),
            theme::font(9.0),
            pal().text_dim,
        );
        level += 12;
    }
    // Frequency grid.
    let label = |hz: f32| -> String {
        if scale_x == sp::X_SEMITONE {
            let note = (69.0 + 12.0 * (hz / 440.0).log2())
                .round()
                .clamp(0.0, 127.0);
            rlive_core::note_name(note as u8)
        } else if hz >= 1_000.0 {
            format!("{:.0}k", hz / 1_000.0)
        } else {
            format!("{hz:.0}")
        }
    };
    let ticks: &[f32] = if scale_x == sp::X_LINEAR {
        &[2_000.0, 5_000.0, 10_000.0, 15_000.0, 20_000.0]
    } else {
        &[
            50.0, 100.0, 200.0, 500.0, 1_000.0, 2_000.0, 5_000.0, 10_000.0,
        ]
    };
    for &hz in ticks {
        if hz >= sr * 0.5 {
            continue;
        }
        let x = col_x(sp::hz_point(hz, scale_x, sr));
        p.line_segment(
            [Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())],
            line,
        );
        p.text(
            Pos2::new(x + 2.0, plot.top() + 7.0),
            Align2::LEFT_CENTER,
            label(hz),
            theme::font(9.0),
            pal().text_faint,
        );
    }

    let draw_trace = |values: &[f32], colour: egui::Color32| {
        if bins {
            for (i, value) in values.iter().enumerate() {
                if *value <= sp::FLOOR_DB + 0.5 {
                    continue;
                }
                let x = col_x(i);
                p.line_segment(
                    [Pos2::new(x, db_y(*value)), Pos2::new(x, plot.bottom())],
                    Stroke::new(1.0, colour),
                );
            }
        } else {
            let points: Vec<Pos2> = values
                .iter()
                .enumerate()
                .map(|(i, v)| Pos2::new(col_x(i), db_y(*v)))
                .collect();
            p.add(egui::Shape::line(points, Stroke::new(1.0, colour)));
        }
    };
    if live && show_max {
        draw_trace(&peaks, shade(0x8f6020));
    }
    if live {
        draw_trace(&curve, pal().accent);
    }

    // Hover readout.
    if let Some(pos) = response.hover_pos() {
        if plot.contains(pos) {
            let i = (((pos.x - plot.left()) / plot.width()) * (sp::POINTS - 1) as f32)
                .round()
                .clamp(0.0, (sp::POINTS - 1) as f32) as usize;
            let hz = sp::point_hz(i, scale_x, sr);
            let note = (69.0 + 12.0 * (hz.max(1.0) / 440.0).log2())
                .round()
                .clamp(0.0, 127.0);
            p.text(
                Pos2::new(plot.right() - 4.0, plot.top() + 7.0),
                Align2::RIGHT_CENTER,
                format!(
                    "{:.1} dB  {:.0} Hz  {}",
                    curve[i],
                    hz,
                    rlive_core::note_name(note as u8)
                ),
                theme::font(10.0),
                pal().text,
            );
        }
    }

    // Dragging the legend scrolls and zooms the amplitude range.
    if response.dragged() && !auto {
        if let Some(pos) = response.interact_pointer_pos() {
            if pos.x < plot.left() {
                let delta = response.drag_delta();
                let per_px = (max_db - min_db) / plot.height();
                let scroll = delta.y * per_px;
                let zoom = delta.x * 0.2;
                let (nmin, nmax) = (min_db + scroll + zoom, max_db + scroll - zoom);
                if nmax - nmin > 6.0 {
                    panel.set(ParamId(sp::RANGE_MIN), nmin.clamp(-120.0, 0.0) as f64);
                    panel.set(ParamId(sp::RANGE_MAX), nmax.clamp(-96.0, 24.0) as f64);
                }
            }
        }
    }
    if response.double_clicked() {
        return true;
    }
    if response.clicked() && show_max {
        let reset = panel.value(ParamId(sp::RESET_MAX)).unwrap_or(0.0);
        panel.set(ParamId(sp::RESET_MAX), reset + 1.0);
    }
    false
}
