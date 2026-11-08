//! The Clip View's panels: clip and loop settings, launch and program, the audio utilities, the
//! note tools, and Quantize.

use crate::session::{ClipRef, MultiClip, Session, SetMarker};
use crate::ui::piano_roll::grid_label;
use crate::ui::sample_editor;
use crate::ui::theme::*;
use crate::ui::widgets::{self, info};
use crate::ui::UiState;
use egui::{Pos2, RichText, Sense, Shape, Stroke, Vec2};
use rlive_core::{TimeSignature, PPQ};
use rlive_devices::devices::b1::common::SCALES;
use rlive_project::clip_edit::{self as edit, merged};
use rlive_project::{Clip, ClipContent, MidiProgram};

const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// The settings of the note tools between uses.
pub struct ToolState {
    interval: i32,
    transpose: i32,
    stretch: f64,
    duration: u64,
    humanize: f64,
    /// Hot-Swap is waiting for a groove from the browser.
    pub hot_swap: bool,
}

impl Default for ToolState {
    fn default() -> Self {
        Self {
            interval: 12,
            transpose: 1,
            stretch: 1.0,
            duration: PPQ / 4,
            humanize: 0.5,
            hot_swap: false,
        }
    }
}

fn field_label(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).color(pal().text_dim));
}

/// A position as Live shows it: bars.beats.sixteenths in a warped or MIDI clip, and
/// minutes:seconds:milliseconds in an unwarped one (manual p. 208).
pub fn position_text(ticks: f64, beat_time: bool, sig: TimeSignature, tempo: f64) -> String {
    if beat_time {
        sig.format_position(ticks)
    } else {
        let ms = (ticks / (tempo / 60.0 * PPQ as f64) * 1000.0).round() as u64;
        format!("{}:{:02}:{:03}", ms / 60_000, ms / 1000 % 60, ms % 1000)
    }
}

/// A position as typed, back into ticks.
pub fn parse_position(text: &str, beat_time: bool, sig: TimeSignature, tempo: f64) -> Option<f64> {
    let parts: Vec<f64> = text
        .split(['.', ':', ' '])
        .filter(|p| !p.is_empty())
        .map(|p| p.parse::<f64>().ok())
        .collect::<Option<_>>()?;
    if beat_time {
        let at = |i: usize, default: f64| parts.get(i).copied().unwrap_or(default);
        let ticks = (at(0, 1.0) - 1.0) * sig.ticks_per_bar() as f64
            + (at(1, 1.0) - 1.0) * sig.ticks_per_beat() as f64
            + (at(2, 1.0) - 1.0) * (PPQ / 4) as f64;
        Some(ticks.max(0.0))
    } else {
        let at = |i: usize| parts.get(i).copied().unwrap_or(0.0);
        let seconds = at(0) * 60.0 + at(1) + at(2) / 1000.0;
        Some((seconds * tempo / 60.0 * PPQ as f64).max(0.0))
    }
}

/// A length as Live shows it, from nothing: bars.beats.sixteenths.
pub fn length_text(ticks: f64, sig: TimeSignature) -> String {
    let ticks = ticks.max(0.0) as u64;
    let bar = sig.ticks_per_bar();
    let beat = sig.ticks_per_beat();
    format!(
        "{}.{}.{}",
        ticks / bar,
        (ticks % bar) / beat,
        (ticks % beat) / (PPQ / 4)
    )
}

/// A length as typed, into ticks.
pub fn parse_length(text: &str, sig: TimeSignature) -> Option<f64> {
    let parts: Vec<f64> = text
        .split(['.', ':', ' '])
        .filter(|p| !p.is_empty())
        .map(|p| p.parse::<f64>().ok())
        .collect::<Option<_>>()?;
    let at = |i: usize| parts.get(i).copied().unwrap_or(0.0);
    Some(
        at(0) * sig.ticks_per_bar() as f64
            + at(1) * sig.ticks_per_beat() as f64
            + at(2) * (PPQ / 4) as f64,
    )
}

/// How a panel's content reads a clip position, and how it's formatted.
#[derive(Clone, Copy)]
struct TimeFormat {
    beat_time: bool,
    sig: TimeSignature,
    tempo: f64,
}

/// A position field: `value` of `None` means the clips selected differ.
fn position_field(
    ui: &mut egui::Ui,
    value: Option<u64>,
    fmt: TimeFormat,
    tip: &str,
) -> Option<u64> {
    let mut v = value.unwrap_or(0) as f64;
    let before = v;
    let r = ui.add(
        egui::DragValue::new(&mut v)
            .range(0.0..=f64::MAX)
            .speed((PPQ / 4) as f64 / 4.0)
            .custom_formatter(move |t, _| match value {
                Some(_) => position_text(t, fmt.beat_time, fmt.sig, fmt.tempo),
                None => "*".to_string(),
            })
            .custom_parser(move |s| parse_position(s, fmt.beat_time, fmt.sig, fmt.tempo)),
    );
    info(r, tip);
    (v != before).then_some(v.round() as u64)
}

fn length_field(
    ui: &mut egui::Ui,
    value: Option<u64>,
    sig: TimeSignature,
    tip: &str,
) -> Option<u64> {
    let mut v = value.unwrap_or(0) as f64;
    let before = v;
    let r = ui.add(
        egui::DragValue::new(&mut v)
            .range(1.0..=f64::MAX)
            .speed((PPQ / 4) as f64 / 4.0)
            .custom_formatter(move |t, _| match value {
                Some(_) => length_text(t, sig),
                None => "*".to_string(),
            })
            .custom_parser(move |s| parse_length(s, sig)),
    );
    info(r, tip);
    (v != before).then_some(v.round() as u64)
}

/// One value if every clip agrees, for a field that shows them as one.
fn agreed(values: impl IntoIterator<Item = u64>) -> Option<u64> {
    merged(values).filter(|(lo, hi)| lo == hi).map(|(v, _)| v)
}

/// A slider whose handles show the range of what's selected, split when they differ (manual
/// p. 222). Returns how far it was dragged.
fn range_slider(ui: &mut egui::Ui, (lo, hi): (f32, f32), min: f32, max: f32) -> Option<f32> {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 14.0), Sense::drag());
    let x = |v: f32| rect.left() + (v - min) / (max - min) * rect.width();
    let p = ui.painter();
    let mid = rect.center().y;
    p.line_segment(
        [Pos2::new(rect.left(), mid), Pos2::new(rect.right(), mid)],
        Stroke::new(2.0, shade(0x505050)),
    );
    if hi > lo {
        p.line_segment(
            [Pos2::new(x(lo), mid), Pos2::new(x(hi), mid)],
            Stroke::new(2.0, pal().accent),
        );
    }
    for (v, up) in [(lo, false), (hi, true)] {
        let cx = x(v);
        let dy = if up { -6.0 } else { 6.0 };
        p.add(Shape::convex_polygon(
            vec![
                Pos2::new(cx, mid),
                Pos2::new(cx - 4.0, mid + dy),
                Pos2::new(cx + 4.0, mid + dy),
            ],
            pal().accent,
            Stroke::NONE,
        ));
    }
    let dx = resp.drag_delta().x;
    (dx != 0.0).then(|| dx / rect.width().max(1.0) * (max - min))
}

struct Toggled {
    clicked: bool,
}

fn toggle(ui: &mut egui::Ui, on: bool, label: &str, tip: &str) -> Toggled {
    let r = widgets::state_button(ui, Vec2::new(44.0, 16.0), on, pal().accent, label);
    Toggled {
        clicked: info(r, tip).clicked(),
    }
}

fn set_button(
    ui: &mut egui::Ui,
    session: &mut Session,
    cref: ClipRef,
    which: SetMarker,
    tip: &str,
) {
    let playing = session.clip_playhead(cref).is_some();
    let r = ui.add_enabled(playing, egui::Button::new("Set").small());
    if info(r, tip).clicked() {
        session.set_marker_at_playhead(cref, which);
    }
}

// ------------------------------------------------------------- the panels

#[derive(Clone, Copy, PartialEq)]
enum Panel {
    Main,
    Extended,
    Audio,
    PitchTime,
    Transform,
}

impl Panel {
    fn title(self) -> &'static str {
        match self {
            Self::Main => "CLIP",
            Self::Extended => "PROGRAM",
            Self::Audio => "AUDIO",
            Self::PitchTime => "PITCH AND TIME",
            Self::Transform => "TRANSFORM",
        }
    }
}

/// How many panels a clip has, to make room for them across.
pub(super) fn panel_count(clip: &Clip) -> usize {
    panels_for(clip).len()
}

fn panels_for(clip: &Clip) -> Vec<Panel> {
    let mut list = vec![Panel::Main];
    // The launch controls are in the strip beside the Clip View: this is the MIDI program.
    if clip.is_midi() {
        list.push(Panel::Extended);
    }
    if clip.is_midi() {
        list.push(Panel::PitchTime);
    } else {
        list.push(Panel::Audio);
        list.push(Panel::Transform);
    }
    list
}

pub(super) fn show(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    cref: ClipRef,
    group: &[ClipRef],
    vertical: bool,
) {
    let Some(clip) = session.clip_at(cref).cloned() else {
        return;
    };
    let list = panels_for(&clip);
    let draw = |ui: &mut egui::Ui, panel: Panel, session: &mut Session, state: &mut UiState| {
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(6, 4))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = Vec2::new(4.0, 3.0);
                widgets::section_label(ui, panel.title());
                match panel {
                    Panel::Main => main_panel(ui, session, state, cref, group),
                    Panel::Extended => extended_panel(ui, session, group),
                    Panel::Audio => audio_panel(ui, session, state, cref, group),
                    Panel::PitchTime => pitch_time_panel(ui, session, state, cref),
                    Panel::Transform => sample_editor::quantize_panel(ui, session, state, cref),
                }
            });
    };
    if vertical {
        egui::ScrollArea::vertical()
            .id_salt("clip_panels")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for panel in &list {
                    draw(ui, *panel, session, state);
                    ui.separator();
                }
            });
    } else {
        egui::ScrollArea::horizontal()
            .id_salt("clip_panels_h")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    let h = ui.available_height();
                    for (i, panel) in list.iter().enumerate() {
                        let size = Vec2::new(super::PANEL_W, h);
                        let layout = egui::Layout::top_down(egui::Align::Min);
                        ui.allocate_ui_with_layout(size, layout, |ui| {
                            ui.set_width(super::PANEL_W);
                            egui::ScrollArea::vertical()
                                .id_salt(("panel", i))
                                .auto_shrink([false, false])
                                .show(ui, |ui| draw(ui, *panel, session, state));
                        });
                        let x = ui.cursor().left();
                        ui.painter().line_segment(
                            [
                                Pos2::new(x, ui.max_rect().top()),
                                Pos2::new(x, ui.max_rect().bottom()),
                            ],
                            Stroke::new(1.0, pal().separator),
                        );
                    }
                });
            });
    }
}

fn clips_of<'a>(session: &'a Session, group: &[ClipRef]) -> Vec<&'a Clip> {
    group.iter().filter_map(|r| session.clip_at(*r)).collect()
}

fn main_panel(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    cref: ClipRef,
    group: &[ClipRef],
) {
    let shared = session.multi_clip(group);
    let multi = group.len() > 1;
    let clips: Vec<Clip> = clips_of(session, group).into_iter().cloned().collect();
    let Some(clip) = clips.first().cloned() else {
        return;
    };
    let sig = clip.signature.unwrap_or(session.project.time_signature);
    let fmt = TimeFormat {
        beat_time: clip.is_midi() || clip.is_warped(),
        sig,
        tempo: session.project.tempo,
    };
    let can_loop = clips.iter().all(|c| c.is_midi() || c.is_warped());
    let looped = shared.looped.is_some_and(|(_, hi)| hi);
    ui.horizontal(|ui| {
        ui.add_enabled_ui(can_loop, |ui| {
            if toggle(ui, looped, "Loop", "Clip Loop — repeat the loop region").clicked {
                session.set_clip_looped(group, !looped);
            }
        });
        if !multi && ui.small_button("Dupl.").clicked() {
            session.duplicate_loop(cref);
        }
    });

    egui::Grid::new("clip_region")
        .num_columns(3)
        .spacing([4.0, 3.0])
        .show(ui, |ui| {
            let row = |ui: &mut egui::Ui,
                       session: &mut Session,
                       label: &str,
                       value: Option<u64>,
                       tip: &str,
                       which: SetMarker,
                       apply: &mut dyn FnMut(&mut Session, u64)| {
                field_label(ui, label);
                if let Some(v) = position_field(ui, value, fmt, tip) {
                    apply(session, v);
                }
                if multi {
                    ui.label("");
                } else {
                    set_button(
                        ui,
                        session,
                        cref,
                        which,
                        "Set — move this to the playhead, rounded to the global quantization",
                    );
                }
                ui.end_row();
            };
            row(
                ui,
                session,
                "Start",
                agreed(clips.iter().map(|c| c.start)),
                "Clip Start — where playback begins",
                SetMarker::Start,
                &mut |s, v| {
                    s.edit_clips(group, "Clip Start", |c| edit::set_start(c, v));
                },
            );
            row(
                ui,
                session,
                "End",
                agreed(clips.iter().map(|c| c.end_marker())),
                "Clip End — where an unlooped clip stops",
                SetMarker::End,
                &mut |s, v| {
                    s.edit_clips(group, "Clip End", |c| edit::set_end(c, v));
                },
            );
            row(
                ui,
                session,
                "Position",
                agreed(clips.iter().map(|c| c.loop_start)),
                "Loop Position — where the loop begins",
                SetMarker::LoopPosition,
                &mut |s, v| {
                    s.edit_clips(group, "Loop Position", |c| edit::set_loop_position(c, v));
                },
            );
            field_label(ui, "Length");
            let length = agreed(clips.iter().map(|c| c.length));
            if let Some(v) = length_field(ui, length, sig, "Loop Length — how long the loop is") {
                session.edit_clips(group, "Loop Length", |c| edit::set_loop_length(c, v));
            }
            if multi {
                ui.label("");
            } else {
                set_button(
                    ui,
                    session,
                    cref,
                    SetMarker::LoopLength,
                    "Set — end the loop at the playhead",
                );
            }
            ui.end_row();
            field_label(ui, "Signature");
            let (mut num, mut den) = (sig.numerator, sig.denominator);
            let changed = ui
                .horizontal(|ui| {
                    let a = ui.add(egui::DragValue::new(&mut num).range(1..=32));
                    ui.label("/");
                    let b = egui::ComboBox::from_id_salt("clip_sig_den")
                        .width(38.0)
                        .selected_text(den.to_string())
                        .show_ui(ui, |ui| {
                            let mut changed = false;
                            for d in [2u8, 4, 8, 16] {
                                changed |=
                                    ui.selectable_value(&mut den, d, d.to_string()).changed();
                            }
                            changed
                        });
                    a.changed() | b.inner.unwrap_or(false)
                })
                .inner;
            if changed {
                session.set_clip_signature(
                    group,
                    Some(TimeSignature {
                        numerator: num,
                        denominator: den,
                    }),
                );
            }
            ui.label("");
            ui.end_row();
        });

    groove_row(ui, session, state, cref, group, &clip);
    scale_rows(ui, session, group, &clip);
}

fn groove_row(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    _cref: ClipRef,
    group: &[ClipRef],
    clip: &Clip,
) {
    field_label(ui, "Groove");
    ui.horizontal(|ui| {
        let pool = session.project.groove_pool.grooves.clone();
        let current = clip
            .groove
            .and_then(|id| pool.iter().find(|g| g.id == id))
            .map_or_else(|| "None".to_string(), |g| g.name.clone());
        let mut chosen = clip.groove;
        let r = egui::ComboBox::from_id_salt("clip_groove")
            .width(96.0)
            .selected_text(current)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut chosen, None, "None");
                for g in &pool {
                    ui.selectable_value(&mut chosen, Some(g.id), g.name.as_str());
                }
            });
        info(r.response, "Clip Groove — from the Groove Pool");
        if chosen != clip.groove {
            session.set_clip_groove(group, chosen);
        }
        let hot = state.clip_view.tools.hot_swap;
        if toggle(
            ui,
            hot,
            "Swap",
            "Hot-Swap Groove — the next groove loaded from the browser replaces this one",
        )
        .clicked
        {
            state.clip_view.tools.hot_swap = !hot;
        }
    });
}

fn scale_rows(ui: &mut egui::Ui, session: &mut Session, group: &[ClipRef], clip: &Clip) {
    let mut scale = clip.scale.clone();
    ui.horizontal(|ui| {
        if toggle(
            ui,
            scale.enabled,
            "Scale",
            "Scale — a scale for this clip, which scale-aware devices follow",
        )
        .clicked
        {
            scale.enabled = !scale.enabled;
        }
        egui::ComboBox::from_id_salt("clip_scale_root")
            .width(40.0)
            .selected_text(NOTE_NAMES[scale.root as usize % 12])
            .show_ui(ui, |ui| {
                for (n, name) in NOTE_NAMES.iter().enumerate() {
                    ui.selectable_value(&mut scale.root, n as u8, *name);
                }
            });
    });
    let r = egui::ComboBox::from_id_salt("clip_scale_name")
        .width(ui.available_width() - 8.0)
        .selected_text(scale.name.clone())
        .show_ui(ui, |ui| {
            for (name, _) in SCALES {
                ui.selectable_value(&mut scale.name, (*name).to_string(), *name);
            }
        });
    info(r.response, "Scale Name");
    if scale != clip.scale {
        session.set_clip_scale(group, scale);
    }
}

fn extended_panel(ui: &mut egui::Ui, session: &mut Session, group: &[ClipRef]) {
    let clips: Vec<Clip> = clips_of(session, group).into_iter().cloned().collect();
    let Some(clip) = clips.first().cloned() else {
        return;
    };
    if clips.iter().all(|c| c.is_midi()) {
        program_controls(ui, session, group, &clip);
    }
}

/// A chooser from "—" and then 0 to 127, in the order the clip's fields are stored.
fn program_field(ui: &mut egui::Ui, label: &str, value: &mut u8) {
    field_label(ui, label);
    let mut shown = if *value == edit::PROGRAM_UNSET {
        0
    } else {
        i32::from(*value) + 1
    };
    let r = ui.add(
        egui::DragValue::new(&mut shown)
            .range(0..=128)
            .custom_formatter(|v, _| {
                if v < 1.0 {
                    "—".to_string()
                } else {
                    format!("{}", v as i32 - 1)
                }
            }),
    );
    if r.changed() {
        *value = if shown <= 0 {
            edit::PROGRAM_UNSET
        } else {
            (shown - 1) as u8
        };
    }
    ui.end_row();
}

fn program_controls(ui: &mut egui::Ui, session: &mut Session, group: &[ClipRef], clip: &Clip) {
    let current = match &clip.content {
        ClipContent::Midi { program, .. } => program.unwrap_or(MidiProgram::UNSET),
        ClipContent::Audio { .. } => return,
    };
    let mut p = current;
    egui::Grid::new("clip_program")
        .num_columns(2)
        .spacing([4.0, 3.0])
        .show(ui, |ui| {
            program_field(ui, "Bank", &mut p.bank);
            program_field(ui, "Sub-Bank", &mut p.sub_bank);
            program_field(ui, "Program", &mut p.program);
        });
    if p != current {
        session.set_clip_program(group, p);
    }
}

fn audio_panel(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    cref: ClipRef,
    group: &[ClipRef],
) {
    let shared: MultiClip = session.multi_clip(group);
    if !shared.all_audio {
        return;
    }
    let multi = group.len() > 1;
    sample_editor::utilities(ui, session, state, cref);
    ui.horizontal_wrapped(|ui| {
        let slot = cref.as_slot().is_some();
        let busy = session.clip_job_running();
        // One at a time in the Session (manual p. 217).
        ui.add_enabled_ui(!busy && !(slot && multi), |ui| {
            let r = ui.small_button("Reverse");
            if info(r, "Reverse — make a reversed copy of the sample").clicked() {
                for r in group {
                    session.reverse_clip(*r);
                }
            }
        });
        let r = ui.add_enabled(!session.is_playing(), egui::Button::new("Edit").small());
        if info(
            r,
            "Edit — open the sample in your Sample Editor; stop playback first",
        )
        .clicked()
        {
            if let Err(e) = session.edit_sample_externally(cref) {
                state.status = e;
            }
        }
    });
    let on = |range: Option<(bool, bool)>| range.is_some_and(|(_, hi)| hi);
    ui.horizontal_wrapped(|ui| {
        if cref.as_slot().is_some()
            && toggle(
                ui,
                on(shared.edge_fade),
                "Fade",
                "Clip Fade — a short fade at the clip's edges to avoid clicks",
            )
            .clicked
        {
            session.set_clip_edge_fade(group, !on(shared.edge_fade));
        }
        if toggle(
            ui,
            on(shared.ram),
            "RAM",
            "RAM Mode — keep this clip's audio in memory",
        )
        .clicked
        {
            session.set_clip_ram(group, !on(shared.ram));
        }
        if toggle(
            ui,
            on(shared.hi_q),
            "HiQ",
            "High Quality — better interpolation when resampling, at more CPU",
        )
        .clicked
        {
            session.set_clip_hi_q(group, !on(shared.hi_q));
        }
    });

    if let Some((lo, hi)) = shared.gain_db {
        field_label(
            ui,
            &if lo == hi {
                format!("Gain {lo:.1} dB")
            } else {
                format!("Gain {lo:.1} … {hi:.1} dB")
            },
        );
        if let Some(by) = range_slider(ui, (lo, hi), -70.0, 24.0) {
            session.shift_clip_gain(group, by);
        }
    }
    if let Some((lo, hi)) = shared.transpose {
        let pitch_tip = "Transpose — shift the clip's pitch, in semitones";
        ui.horizontal(|ui| {
            field_label(ui, "Pitch");
            let mut v = lo;
            let r = ui.add(
                egui::DragValue::new(&mut v)
                    .range(-48..=48)
                    .custom_formatter(move |x, _| {
                        if lo == hi {
                            format!("{x:.0} st")
                        } else {
                            "*".to_string()
                        }
                    }),
            );
            if info(r, pitch_tip).changed() {
                session.shift_clip_transpose(group, v - lo);
            }
            if let Some((d_lo, d_hi)) = shared.detune_cents {
                let mut c = d_lo;
                let r = ui.add(
                    egui::DragValue::new(&mut c)
                        .range(-50.0..=50.0)
                        .speed(0.5)
                        .custom_formatter(move |x, _| {
                            if d_lo == d_hi {
                                format!("{x:.0} ct")
                            } else {
                                "*".to_string()
                            }
                        }),
                );
                if info(r, "Detune — in cents").changed() {
                    session.shift_clip_detune(group, c - d_lo);
                }
            }
        });
    }
}

fn pitch_time_panel(ui: &mut egui::Ui, session: &mut Session, state: &mut UiState, cref: ClipRef) {
    let Some(clip) = session.clip_at(cref).cloned() else {
        return;
    };
    let selected = state.piano.selected.clone();
    let scale = Session::pitch_scale(&clip);
    let grid = state.piano.grid;
    let tools = &mut state.clip_view.tools;
    field_label(ui, "Pitch");
    ui.horizontal(|ui| {
        ui.add(egui::DragValue::new(&mut tools.transpose).range(-48..=48));
        if ui.small_button("Transpose").clicked() {
            let by = tools.transpose;
            session.edit_notes(cref, "Transpose", |n| {
                edit::transpose(n, &selected, by, scale)
            });
        }
    });
    ui.horizontal_wrapped(|ui| {
        let r = ui.add_enabled(scale.is_some(), egui::Button::new("Fit").small());
        if info(r, "Fit to Scale — move notes onto the clip's scale").clicked() {
            if let Some(s) = scale {
                session.edit_notes(cref, "Fit to Scale", |n| {
                    edit::fit_to_scale(n, &selected, s)
                });
            }
        }
        if info(ui.small_button("Invert"), "Invert — flip upside-down").clicked() {
            session.edit_notes(cref, "Invert", |n| edit::invert(n, &selected));
        }
    });
    ui.horizontal(|ui| {
        ui.add(egui::DragValue::new(&mut tools.interval).range(-48..=48));
        let r = ui.small_button("Add Interval");
        if info(r, "Interval — add a note this far from each selected one").clicked() {
            let by = tools.interval;
            session.edit_notes(cref, "Add Interval", |n| {
                edit::add_interval(n, &selected, by, scale)
            });
        }
    });
    ui.separator();
    field_label(ui, "Time");
    ui.horizontal(|ui| {
        ui.add(
            egui::DragValue::new(&mut tools.stretch)
                .range(0.1..=8.0)
                .speed(0.01)
                .max_decimals(2),
        );
        if ui.small_button("Stretch").clicked() {
            let factor = tools.stretch;
            session.edit_notes(cref, "Stretch", |n| edit::stretch(n, &selected, factor));
        }
    });
    ui.horizontal_wrapped(|ui| {
        if ui.small_button("×2").clicked() {
            session.scale_time(cref, &selected, 2.0);
        }
        if ui.small_button("/2").clicked() {
            session.scale_time(cref, &selected, 0.5);
        }
        if ui.small_button("Rev").clicked() {
            session.edit_notes(cref, "Reverse Notes", |n| edit::reverse(n, &selected));
        }
        if ui.small_button("Legato").clicked() {
            session.edit_notes(cref, "Legato", |n| edit::legato(n, &selected));
        }
    });
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("note_duration")
            .width(52.0)
            .selected_text(grid_label(tools.duration))
            .show_ui(ui, |ui| {
                for g in [PPQ, PPQ / 2, PPQ / 4, PPQ / 8] {
                    ui.selectable_value(&mut tools.duration, g, grid_label(g));
                }
            });
        if ui.small_button("Set Length").clicked() {
            let length = tools.duration;
            session.edit_notes(cref, "Set Length", |n| {
                edit::set_length(n, &selected, length)
            });
        }
    });
    ui.horizontal(|ui| {
        ui.add(
            egui::DragValue::new(&mut tools.humanize)
                .range(0.0..=1.0)
                .speed(0.01)
                .max_decimals(2),
        );
        if ui.small_button("Humanize").clicked() {
            let amount = tools.humanize;
            let mut rng = session.tool_random();
            session.edit_notes(cref, "Humanize", |n| {
                edit::humanize(n, &selected, amount, grid, &mut rng)
            });
        }
    });
}

/// What sits on the tab row over the notes: the grid and new-note controls.
pub(super) fn notes_row(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    cref: ClipRef,
) {
    ui.add_space(10.0);
    field_label(ui, "Grid");
    let r = egui::ComboBox::from_id_salt("grid")
        .width(52.0)
        .selected_text(grid_label(state.piano.grid))
        .show_ui(ui, |ui| {
            for g in [PPQ, PPQ / 2, PPQ / 4, PPQ / 8, PPQ / 3, PPQ / 6] {
                ui.selectable_value(&mut state.piano.grid, g, grid_label(g));
            }
        });
    info(r.response, "Grid — for drawing and snapping notes");
    field_label(ui, "Vel");
    let r = ui.add(
        egui::DragValue::new(&mut state.piano.new_velocity)
            .range(0.05..=1.0)
            .speed(0.01)
            .fixed_decimals(2),
    );
    info(r, "New Note Velocity — how hard new notes are played");
    if info(
        ui.small_button("Quantize"),
        "Quantize — snap selected, or all, notes to the grid",
    )
    .clicked()
    {
        let grid = state.piano.grid;
        let selected = state.piano.selected.clone();
        session.edit_notes(cref, "Quantize", |notes| {
            for (i, n) in notes.iter_mut().enumerate() {
                if selected.is_empty() || selected.contains(&i) {
                    n.start = crate::ui::timeline::snap(n.start as f64, grid, true).max(0) as u64;
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIG: TimeSignature = TimeSignature {
        numerator: 4,
        denominator: 4,
    };

    #[test]
    fn a_warped_position_reads_in_beats_and_comes_back() {
        let text = position_text((4 * PPQ + PPQ + PPQ / 4) as f64, true, SIG, 120.0);
        assert_eq!(text, "2.2.2");
        assert_eq!(
            parse_position(&text, true, SIG, 120.0),
            Some((4 * PPQ + PPQ + PPQ / 4) as f64)
        );
    }

    #[test]
    fn an_unwarped_position_reads_in_time_and_comes_back() {
        // At 120 BPM a beat is half a second.
        let text = position_text(PPQ as f64 * 3.0, false, SIG, 120.0);
        assert_eq!(text, "0:01:500");
        let back = parse_position(&text, false, SIG, 120.0).unwrap();
        assert!((back - PPQ as f64 * 3.0).abs() < 1e-6);
    }

    #[test]
    fn a_length_counts_from_nothing() {
        assert_eq!(length_text((4 * PPQ + PPQ / 2) as f64, SIG), "1.0.2");
        assert_eq!(parse_length("1.0.2", SIG), Some((4 * PPQ + PPQ / 2) as f64));
    }

    #[test]
    fn agreed_is_only_for_when_they_do() {
        assert_eq!(agreed([4, 4]), Some(4));
        assert_eq!(agreed([4, 5]), None);
    }
}
