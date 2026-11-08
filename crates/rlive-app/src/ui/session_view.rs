//! Session view: tracks × scenes clip-launcher grid with a mixer strip under each track.

use super::widgets::{self, rgb, ACCENT, PLAY_GREEN, SLOT_BG, TEXT_DIM};
use super::{BrowserItem, Detail, UiState};
use crate::session::{ChainRef, Session};
use egui::{Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use rlive_core::TrackId;
use rlive_engine::shared::{NO_SLOT, STOP_QUEUED};
use rlive_project::{Clip, TrackKind, PALETTE};
use std::sync::atomic::Ordering;

const COL_W: f32 = 118.0;
const SLOT_H: f32 = 24.0;
const HEADER_H: f32 = 24.0;
const MIXER_H: f32 = 190.0;
const SCENE_W: f32 = 96.0;

pub fn duplicate_clip(session: &mut Session, track: TrackId, slot: usize) {
    let Some(clip) = session.clip(track, slot).cloned() else {
        return;
    };
    let scenes = session.project.scenes.len();
    let target = (slot + 1..scenes).find(|s| session.clip(track, *s).is_none());
    let target = match target {
        Some(t) => t,
        None => {
            session.add_scene();
            session.project.scenes.len() - 1
        }
    };
    session.set_clip(track, target, Some(clip));
}

pub fn show(ui: &mut egui::Ui, session: &mut Session, state: &mut UiState) {
    let scenes = session.project.scenes.len();
    let blink = (ui.input(|i| i.time) * 4.0).fract() < 0.5;
    egui::ScrollArea::both()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let ids: Vec<TrackId> = session.project.tracks.iter().map(|t| t.id).collect();
                for id in ids {
                    track_column(ui, session, state, id, scenes, blink);
                }
                // Drop zone to create a new track.
                new_track_zone(ui, session, state, scenes);
                scene_column(ui, session, state, scenes);
                master_column(ui, session, state, scenes);
            });
        });
}

fn track_column(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    id: TrackId,
    scenes: usize,
    blink: bool,
) {
    let Some(track) = session.project.track(id).cloned() else {
        return;
    };
    let shared = session.track_shared.get(&id).cloned();
    let playing_slot = shared
        .as_ref()
        .map_or(NO_SLOT, |s| s.playing_slot.load(Ordering::Relaxed));
    let queued_slot = shared
        .as_ref()
        .map_or(NO_SLOT, |s| s.queued_slot.load(Ordering::Relaxed));
    let position = shared.as_ref().map_or(0.0, |s| s.clip_position.load());
    let selected = state.selected == Some(ChainRef::Track(id));
    let color = rgb(track.color);

    ui.vertical(|ui| {
        ui.set_width(COL_W);
        ui.spacing_mut().item_spacing.y = 2.0;

        // ---- header
        let (rect, resp) =
            ui.allocate_exact_size(Vec2::new(COL_W, HEADER_H), Sense::click_and_drag());
        let p = ui.painter();
        p.rect_filled(
            rect,
            3.0,
            if selected {
                color
            } else {
                color.gamma_multiply(0.55)
            },
        );
        let renaming = matches!(&state.renaming, Some((t, _)) if *t == id);
        if renaming {
            let mut done = false;
            if let Some((_, name)) = &mut state.renaming {
                let r = ui.put(rect.shrink(2.0), egui::TextEdit::singleline(name));
                r.request_focus();
                if r.lost_focus() {
                    done = true;
                }
            }
            if done {
                if let Some((_, name)) = state.renaming.take() {
                    if let Some(t) = session.project.track_mut(id) {
                        t.name = name;
                    }
                    session.dirty = true;
                }
            }
        } else {
            p.text(
                rect.left_center() + Vec2::new(6.0, 0.0),
                Align2::LEFT_CENTER,
                &track.name,
                FontId::proportional(12.0),
                Color32::BLACK,
            );
        }
        if resp.clicked() {
            state.selected = Some(ChainRef::Track(id));
            state.detail = Detail::Devices;
        }
        if resp.double_clicked() {
            state.renaming = Some((id, track.name.clone()));
        }
        // Drop devices / plugins onto the header.
        if let Some(item) = resp.dnd_release_payload::<BrowserItem>() {
            drop_on_track(session, state, id, &item, None);
        }
        resp.context_menu(|ui| {
            if ui.button("Rename").clicked() {
                state.renaming = Some((id, track.name.clone()));
                ui.close();
            }
            ui.menu_button("Color", |ui| {
                ui.horizontal_wrapped(|ui| {
                    for c in PALETTE {
                        let (r, resp) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::click());
                        ui.painter().rect_filled(r, 2.0, rgb(c));
                        if resp.clicked() {
                            if let Some(t) = session.project.track_mut(id) {
                                t.color = c;
                            }
                            ui.close();
                        }
                    }
                });
            });
            let idx = session.project.track_index(id).unwrap_or(0);
            if ui.button("Move Left").clicked() && idx > 0 {
                session.move_track(id, idx - 1);
                ui.close();
            }
            if ui.button("Move Right").clicked() {
                session.move_track(id, idx + 1);
                ui.close();
            }
            ui.separator();
            if ui
                .button(RichText::new("Delete Track").color(Color32::LIGHT_RED))
                .clicked()
            {
                session.delete_track(id);
                if state.selected == Some(ChainRef::Track(id)) {
                    state.selected = None;
                }
                if matches!(state.selected_clip, Some((t, _)) if t == id) {
                    state.selected_clip = None;
                }
                ui.close();
            }
        });

        // ---- clip slots
        for slot in 0..scenes {
            let clip = track.clips.get(slot).cloned().flatten();
            let (rect, resp) =
                ui.allocate_exact_size(Vec2::new(COL_W, SLOT_H), Sense::click_and_drag());
            let p = ui.painter();
            let is_sel = state.selected_clip == Some((id, slot));
            let launch_rect = Rect::from_min_size(rect.min, Vec2::new(SLOT_H, SLOT_H));
            let hover_launch = resp
                .hover_pos()
                .is_some_and(|pos| launch_rect.contains(pos));
            let is_playing = playing_slot == slot as i32;
            let is_queued = queued_slot == slot as i32;

            match &clip {
                Some(c) => {
                    let fill = rgb(c.color);
                    p.rect_filled(
                        rect,
                        2.0,
                        if is_sel {
                            fill
                        } else {
                            fill.gamma_multiply(0.8)
                        },
                    );
                    let tri_color = if is_playing || (is_queued && blink) {
                        PLAY_GREEN
                    } else if hover_launch {
                        Color32::WHITE
                    } else {
                        Color32::from_gray(30)
                    };
                    widgets::play_triangle(p, launch_rect, tri_color);
                    p.text(
                        Pos2::new(rect.left() + SLOT_H, rect.center().y),
                        Align2::LEFT_CENTER,
                        &c.name,
                        FontId::proportional(11.0),
                        Color32::BLACK,
                    );
                    if is_playing && c.length > 0 {
                        let frac = (position / c.length as f64).clamp(0.0, 1.0) as f32;
                        let bar = Rect::from_min_size(
                            Pos2::new(rect.left(), rect.bottom() - 3.0),
                            Vec2::new(rect.width() * frac, 3.0),
                        );
                        p.rect_filled(bar, 0.0, PLAY_GREEN);
                    }
                    if is_sel {
                        widgets::outline(p, rect, Color32::WHITE);
                    }
                }
                None => {
                    p.rect_filled(rect, 2.0, SLOT_BG);
                    // Empty slots show a stop button (like Ableton) when the track is playing.
                    let stop_col = if playing_slot != NO_SLOT && hover_launch {
                        Color32::WHITE
                    } else if queued_slot == STOP_QUEUED && blink {
                        ACCENT
                    } else {
                        Color32::from_gray(80)
                    };
                    widgets::stop_square(p, launch_rect, stop_col);
                    if is_sel {
                        widgets::outline(p, rect, Color32::from_gray(160));
                    }
                }
            }
            if resp.dnd_hover_payload::<BrowserItem>().is_some() {
                widgets::outline(p, rect, ACCENT);
            }

            if resp.clicked() {
                state.selected = Some(ChainRef::Track(id));
                if hover_launch {
                    if clip.is_some() {
                        session.launch_clip(id, slot);
                    } else {
                        session.stop_track(id);
                    }
                } else {
                    state.selected_clip = Some((id, slot));
                    state.detail = if clip.is_some() {
                        Detail::Clip
                    } else {
                        Detail::Devices
                    };
                    state.piano.reset_view = true;
                }
            }
            if resp.double_clicked() && !hover_launch {
                if clip.is_none() && track.kind == TrackKind::Midi {
                    let c =
                        Clip::new_midi("MIDI Clip", track.color, 1, session.project.time_signature);
                    session.set_clip(id, slot, Some(c));
                    state.selected_clip = Some((id, slot));
                    state.detail = Detail::Clip;
                    state.piano.reset_view = true;
                } else if clip.is_some() {
                    state.detail = Detail::Clip;
                }
            }
            if let Some(item) = resp.dnd_release_payload::<BrowserItem>() {
                drop_on_track(session, state, id, &item, Some(slot));
            }
            // Drag a clip onto another slot to move it (alt = copy).
            if clip.is_some() {
                resp.dnd_set_drag_payload(ClipDrag { track: id, slot });
            }
            if let Some(src) = resp.dnd_release_payload::<ClipDrag>() {
                if (src.track, src.slot) != (id, slot) {
                    if let Some(c) = session.clip(src.track, src.slot).cloned() {
                        let copy = ui.input(|i| i.modifiers.alt);
                        session.set_clip(id, slot, Some(c));
                        if !copy {
                            session.set_clip(src.track, src.slot, None);
                        }
                        state.selected_clip = Some((id, slot));
                    }
                }
            }
            resp.context_menu(|ui| clip_menu(ui, session, state, id, slot, track.kind));
        }

        // ---- track stop button
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(COL_W, 18.0), Sense::click());
        ui.painter().rect_filled(rect, 2.0, Color32::from_gray(44));
        widgets::stop_square(
            ui.painter(),
            rect,
            if resp.hovered() {
                Color32::WHITE
            } else {
                Color32::from_gray(120)
            },
        );
        if resp.on_hover_text("Stop clips on this track").clicked() {
            session.stop_track(id);
        }

        mixer_strip(ui, session, state, id);
    });
}

#[derive(Clone, Copy)]
struct ClipDrag {
    track: TrackId,
    slot: usize,
}

fn clip_menu(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    id: TrackId,
    slot: usize,
    kind: TrackKind,
) {
    let has = session.clip(id, slot).is_some();
    if !has {
        if kind == TrackKind::Midi && ui.button("Insert MIDI Clip").clicked() {
            let color = session
                .project
                .track(id)
                .map(|t| t.color)
                .unwrap_or(PALETTE[0]);
            let c = Clip::new_midi("MIDI Clip", color, 1, session.project.time_signature);
            session.set_clip(id, slot, Some(c));
            state.selected_clip = Some((id, slot));
            state.detail = Detail::Clip;
            ui.close();
        }
        if kind == TrackKind::Audio && ui.button("Load Audio File…").clicked() {
            ui.close();
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("WAV", &["wav"])
                .pick_file()
            {
                match session.audio_clip_from_file(&path) {
                    Ok(c) => session.set_clip(id, slot, Some(c)),
                    Err(e) => state.status = e,
                }
            }
        }
        return;
    }
    if ui.button("Launch").clicked() {
        session.launch_clip(id, slot);
        ui.close();
    }
    if ui.button("Edit").clicked() {
        state.selected_clip = Some((id, slot));
        state.detail = Detail::Clip;
        ui.close();
    }
    if ui.button("Duplicate  ⌘D").clicked() {
        duplicate_clip(session, id, slot);
        ui.close();
    }
    let mut looped = session.clip(id, slot).is_some_and(|c| c.looped);
    if ui.checkbox(&mut looped, "Loop").changed() {
        if let Some(c) = session.clip_mut(id, slot) {
            c.looped = looped;
        }
        session.sync_clip(id, slot);
    }
    ui.menu_button("Color", |ui| {
        ui.horizontal_wrapped(|ui| {
            for c in PALETTE {
                let (r, resp) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::click());
                ui.painter().rect_filled(r, 2.0, rgb(c));
                if resp.clicked() {
                    if let Some(clip) = session.clip_mut(id, slot) {
                        clip.color = c;
                    }
                    session.dirty = true;
                    ui.close();
                }
            }
        });
    });
    if let Some(c) = session.clip_mut(id, slot) {
        ui.horizontal(|ui| {
            ui.label("Name");
            if ui.text_edit_singleline(&mut c.name).changed() {
                // Name only lives in the project.
            }
        });
    }
    ui.separator();
    if ui
        .button(RichText::new("Delete").color(Color32::LIGHT_RED))
        .clicked()
    {
        session.set_clip(id, slot, None);
        if state.selected_clip == Some((id, slot)) {
            state.selected_clip = None;
        }
        ui.close();
    }
}

fn drop_on_track(
    session: &mut Session,
    state: &mut UiState,
    id: TrackId,
    item: &BrowserItem,
    slot: Option<usize>,
) {
    match item {
        BrowserItem::Builtin(t) => session.add_builtin(ChainRef::Track(id), t),
        BrowserItem::Plugin(p) => session.add_plugin(ChainRef::Track(id), p),
        BrowserItem::Sample(path) => {
            let is_audio = session
                .project
                .track(id)
                .is_some_and(|t| t.kind == TrackKind::Audio);
            if !is_audio {
                state.status =
                    "Drop samples onto an audio track (or into an empty area to create one)".into();
                return;
            }
            match session.audio_clip_from_file(path) {
                Ok(c) => {
                    let slot = slot
                        .or_else(|| {
                            session
                                .project
                                .track(id)
                                .and_then(|t| t.clips.iter().position(|c| c.is_none()))
                        })
                        .unwrap_or(0);
                    session.set_clip(id, slot, Some(c));
                    state.selected_clip = Some((id, slot));
                }
                Err(e) => state.status = e,
            }
        }
    }
    state.selected = Some(ChainRef::Track(id));
}

fn new_track_zone(ui: &mut egui::Ui, session: &mut Session, state: &mut UiState, scenes: usize) {
    let h = HEADER_H + scenes as f32 * (SLOT_H + 2.0) + 20.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(60.0, h), Sense::click());
    let hovering = resp.dnd_hover_payload::<BrowserItem>().is_some();
    let p = ui.painter();
    p.rect_stroke(
        rect,
        3.0,
        Stroke::new(
            1.0,
            if hovering {
                ACCENT
            } else {
                Color32::from_gray(55)
            },
        ),
        egui::StrokeKind::Inside,
    );
    p.text(
        rect.center(),
        Align2::CENTER_CENTER,
        "+",
        FontId::proportional(22.0),
        Color32::from_gray(90),
    );
    if let Some(item) = resp.dnd_release_payload::<BrowserItem>() {
        let kind = match &*item {
            BrowserItem::Sample(_) => TrackKind::Audio,
            BrowserItem::Builtin(id)
                if rlive_devices::find(id)
                    .is_some_and(|b| b.role == rlive_devices::DeviceRole::AudioEffect) =>
            {
                TrackKind::Audio
            }
            BrowserItem::Plugin(p) if !p.is_instrument => TrackKind::Audio,
            _ => TrackKind::Midi,
        };
        let id = session.add_track(kind, None);
        // A dropped instrument replaces the default synth.
        if matches!(&*item, BrowserItem::Plugin(p) if p.is_instrument)
            || matches!(&*item, BrowserItem::Builtin(b) if rlive_devices::find(b).is_some_and(|b| b.role == rlive_devices::DeviceRole::Instrument))
        {
            let existing: Vec<_> = session
                .devices(ChainRef::Track(id))
                .iter()
                .map(|d| d.id)
                .collect();
            for d in existing {
                session.remove_device(ChainRef::Track(id), d);
            }
        }
        drop_on_track(session, state, id, &item, None);
    }
    resp.on_hover_text("Click: new MIDI track · Drop a device, plugin or sample to create a track")
        .clicked()
        .then(|| {
            let id = session.add_track(TrackKind::Midi, None);
            state.selected = Some(ChainRef::Track(id));
        });
}

fn scene_column(ui: &mut egui::Ui, session: &mut Session, _state: &mut UiState, scenes: usize) {
    ui.vertical(|ui| {
        ui.set_width(SCENE_W);
        ui.spacing_mut().item_spacing.y = 2.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::new(SCENE_W, HEADER_H), Sense::hover());
        ui.painter().rect_filled(rect, 3.0, Color32::from_gray(60));
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            "Scenes",
            FontId::proportional(12.0),
            Color32::from_gray(220),
        );
        let mut delete = None;
        for i in 0..scenes {
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(SCENE_W, SLOT_H), Sense::click());
            let p = ui.painter();
            p.rect_filled(
                rect,
                2.0,
                Color32::from_gray(if resp.hovered() { 70 } else { 58 }),
            );
            let launch = Rect::from_min_size(rect.min, Vec2::splat(SLOT_H));
            widgets::play_triangle(
                p,
                launch,
                if resp.hovered() {
                    Color32::WHITE
                } else {
                    Color32::from_gray(150)
                },
            );
            p.text(
                Pos2::new(rect.left() + SLOT_H, rect.center().y),
                Align2::LEFT_CENTER,
                &session.project.scenes[i].name,
                FontId::proportional(11.0),
                Color32::from_gray(220),
            );
            if resp.clicked() {
                session.launch_scene(i);
            }
            resp.context_menu(|ui| {
                if let Some(scene) = session.project.scenes.get_mut(i) {
                    ui.horizontal(|ui| {
                        ui.label("Name");
                        ui.text_edit_singleline(&mut scene.name);
                    });
                }
                if ui.button("Insert Scene").clicked() {
                    session.add_scene();
                    ui.close();
                }
                if ui
                    .button(RichText::new("Delete Scene").color(Color32::LIGHT_RED))
                    .clicked()
                {
                    delete = Some(i);
                    ui.close();
                }
            });
        }
        if let Some(i) = delete {
            session.delete_scene(i);
        }
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(SCENE_W, 18.0), Sense::click());
        ui.painter().rect_filled(rect, 2.0, Color32::from_gray(44));
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            "Stop All",
            FontId::proportional(10.0),
            TEXT_DIM,
        );
        if resp.clicked() {
            session.stop_all_clips();
        }
        if ui
            .add_sized([SCENE_W, 18.0], egui::Button::new("+ Scene"))
            .clicked()
        {
            session.add_scene();
        }
    });
}

fn master_column(ui: &mut egui::Ui, session: &mut Session, state: &mut UiState, scenes: usize) {
    ui.vertical(|ui| {
        ui.set_width(COL_W);
        let selected = state.selected == Some(ChainRef::Master);
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(COL_W, HEADER_H), Sense::click());
        ui.painter().rect_filled(
            rect,
            3.0,
            Color32::from_gray(if selected { 150 } else { 90 }),
        );
        ui.painter().text(
            rect.left_center() + Vec2::new(6.0, 0.0),
            Align2::LEFT_CENTER,
            "Master",
            FontId::proportional(12.0),
            Color32::BLACK,
        );
        if resp.clicked() {
            state.selected = Some(ChainRef::Master);
            state.detail = super::Detail::Devices;
        }
        if let Some(item) = resp.dnd_release_payload::<BrowserItem>() {
            match &*item {
                BrowserItem::Builtin(t) => session.add_builtin(ChainRef::Master, t),
                BrowserItem::Plugin(p) => session.add_plugin(ChainRef::Master, p),
                BrowserItem::Sample(_) => {}
            }
        }
        ui.add_space(scenes as f32 * (SLOT_H + 2.0) + 20.0 + 4.0);
        // Master mixer strip
        let frame = egui::Frame::new()
            .fill(Color32::from_gray(42))
            .inner_margin(4.0)
            .corner_radius(3.0);
        frame.show(ui, |ui| {
            ui.set_width(COL_W - 8.0);
            ui.set_height(MIXER_H - 8.0);
            ui.horizontal(|ui| {
                let mut vol = session.project.master.volume_db;
                if ui
                    .add(
                        egui::Slider::new(&mut vol, -60.0..=6.0)
                            .vertical()
                            .show_value(false)
                            .trailing_fill(true),
                    )
                    .double_clicked()
                {
                    vol = 0.0;
                }
                if vol != session.project.master.volume_db {
                    session.set_master_volume(vol);
                }
                widgets::meter(ui, Vec2::new(12.0, 140.0), state.master_meter.get());
            });
            ui.label(
                RichText::new(format!("{:.1} dB", session.project.master.volume_db))
                    .size(10.0)
                    .color(TEXT_DIM),
            );
        });
    });
}

fn mixer_strip(ui: &mut egui::Ui, session: &mut Session, state: &mut UiState, id: TrackId) {
    let meter = state.meters.get(&id).copied().unwrap_or_default();
    let frame = egui::Frame::new()
        .fill(Color32::from_gray(42))
        .inner_margin(4.0)
        .corner_radius(3.0);
    frame.show(ui, |ui| {
        ui.set_width(COL_W - 8.0);
        ui.set_height(MIXER_H - 8.0);
        let Some(t) = session.project.track(id) else {
            return;
        };
        let (mut vol, mut pan, mut mute, mut solo, arm) =
            (t.volume_db, t.pan, t.mute, t.solo, t.arm);
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            if widgets::toggle_box(
                ui,
                !mute,
                &format!("{}", session.project.track_index(id).unwrap_or(0) + 1),
                Color32::from_rgb(250, 200, 60),
                "Track activator (mute)",
            )
            .clicked()
            {
                mute = !mute;
                changed = true;
            }
            if widgets::toggle_box(ui, solo, "S", Color32::from_rgb(80, 160, 255), "Solo").clicked()
            {
                solo = !solo;
                changed = true;
            }
            if widgets::toggle_box(
                ui,
                arm,
                "R",
                Color32::from_rgb(240, 70, 60),
                "Arm for MIDI input (⌘-click to arm several)",
            )
            .clicked()
            {
                let exclusive = !ui.input(|i| i.modifiers.command);
                session.set_arm(id, !arm, exclusive);
            }
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new("Pan").size(10.0).color(TEXT_DIM));
            let r = ui.add(egui::Slider::new(&mut pan, -1.0..=1.0).show_value(false));
            if r.double_clicked() {
                pan = 0.0;
            }
            changed |= r.changed() || r.double_clicked();
        });
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::Slider::new(&mut vol, -60.0..=6.0)
                    .vertical()
                    .show_value(false)
                    .trailing_fill(true),
            );
            if r.double_clicked() {
                vol = 0.0;
            }
            changed |= r.changed() || r.double_clicked();
            widgets::meter(ui, Vec2::new(12.0, 110.0), meter.get());
            ui.vertical(|ui| {
                let db = if vol <= -60.0 {
                    "-inf".to_string()
                } else {
                    format!("{vol:.1}")
                };
                ui.label(RichText::new(db).size(10.0).color(TEXT_DIM));
                ui.label(RichText::new("dB").size(9.0).color(TEXT_DIM));
            });
        });
        if changed {
            if let Some(t) = session.project.track_mut(id) {
                t.volume_db = if vol <= -60.0 { -96.0 } else { vol };
                t.pan = pan;
                t.mute = mute;
                t.solo = solo;
            }
            session.sync_mix(id);
        }
    });
}
