//! Device view, styled after Live: devices sit side by side (signal flows left → right),
//! each with a title bar (power, fold, name) and a body of knobs that reflows to the
//! height of the detail view.

use super::icons;
use super::theme::{self, *};
use super::widgets::{self, info, rgb};
use super::{BrowserItem, UiState};
use crate::session::{ChainRef, Session};
use egui::{Align2, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, UiBuilder, Vec2};
use rlive_core::{DeviceId, ParamFlags, ParamInfo};
use rlive_project::DeviceKind;

const TITLE_H: f32 = 20.0;
const KNOB_W: f32 = 60.0;
const KNOB_H: f32 = 66.0;
const FOLDED_W: f32 = 24.0;
const MAX_PARAMS: usize = 64;

enum Action {
    Remove(DeviceId),
    Move(DeviceId, usize),
    Bypass(DeviceId, bool),
    Fold(DeviceId, bool),
    Param(DeviceId, u32, f64),
    OpenEditor(DeviceId),
    CloseEditor(DeviceId),
    LoadSample(DeviceId),
    Reload(DeviceId),
    ShowAll(DeviceId),
}

pub fn show(ui: &mut egui::Ui, session: &mut Session, state: &mut UiState) {
    let full = ui.max_rect();
    // Claim the whole panel so egui keeps its size (children don't advance the cursor).
    ui.expand_to_include_rect(full);
    ui.painter().rect_filled(full, 0.0, PANEL_DARK);
    let Some(chain) = state.selected else {
        ui.painter().text(
            full.center(),
            Align2::CENTER_CENTER,
            "Select a track to see its devices",
            theme::font(12.0),
            TEXT_DIM,
        );
        return;
    };
    let (title, color) = match chain {
        ChainRef::Master => ("Master".to_string(), hex(0x9a9a9a)),
        ChainRef::Track(t) => session
            .project
            .track(t)
            .map(|t| (t.name.clone(), rgb(t.color)))
            .unwrap_or_default(),
        ChainRef::Nested(_) => ("Rack Chain".to_string(), hex(0x9a9a9a)),
    };
    // Left strip in the track colour with the track name (like Live's detail view title).
    let strip = Rect::from_min_size(full.min, Vec2::new(18.0, full.height()));
    ui.painter().rect_filled(strip, 0.0, color);
    let galley = ui
        .painter()
        .layout_no_wrap(title, theme::font_bold(11.0), theme::text_on(color));
    let pos = Pos2::new(
        strip.center().x - galley.size().y / 2.0,
        strip.top() + 8.0 + galley.size().x,
    );
    ui.painter().add(
        egui::epaint::TextShape::new(pos, galley, theme::text_on(color))
            .with_angle(-std::f32::consts::FRAC_PI_2),
    );

    let area = Rect::from_min_max(
        Pos2::new(strip.right() + 6.0, full.top() + 6.0),
        Pos2::new(full.right(), full.bottom() - 6.0),
    );
    let mut au = ui.new_child(UiBuilder::new().max_rect(area));
    let devices = session.devices(chain).to_vec();
    let mut actions = Vec::new();
    let show_all_id = egui::Id::unique("devices_show_all");
    let show_all: Vec<DeviceId> = au
        .ctx()
        .data(|d| d.get_temp(show_all_id).unwrap_or_default());
    let height = area.height();
    egui::ScrollArea::horizontal()
        .id_salt("device_chain")
        .auto_shrink([false, false])
        .show(&mut au, |ui| {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for (index, dev) in devices.iter().enumerate() {
                    let all = show_all.contains(&dev.id);
                    device(
                        ui,
                        session,
                        chain,
                        dev,
                        index,
                        devices.len(),
                        height,
                        all,
                        &mut actions,
                    );
                }
                drop_zone(ui, session, chain, height);
            });
        });

    for a in actions {
        match a {
            Action::Remove(id) => session.remove_device(chain, id),
            Action::Move(id, to) => session.move_device(chain, id, to),
            Action::Bypass(id, b) => session.set_bypass(chain, id, b),
            Action::Fold(id, expanded) => session.set_device_expanded(chain, id, expanded),
            Action::Param(id, p, v) => session.set_param(chain, id, p, v),
            Action::OpenEditor(id) => {
                if let Some(h) = session
                    .runtimes
                    .get_mut(&id)
                    .and_then(|r| r.plugin.as_mut())
                {
                    if let Err(e) = h.open_editor() {
                        state.status = format!("Editor: {e}");
                    }
                }
            }
            Action::CloseEditor(id) => {
                if let Some(h) = session
                    .runtimes
                    .get_mut(&id)
                    .and_then(|r| r.plugin.as_mut())
                {
                    h.close_editor();
                }
            }
            Action::LoadSample(id) => {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("WAV", &["wav"])
                    .pick_file()
                {
                    session.set_device_sample(chain, id, path);
                }
            }
            Action::Reload(id) => session.reload_device(chain, id),
            Action::ShowAll(id) => ui.ctx().data_mut(|d| {
                let v: &mut Vec<DeviceId> = d.get_temp_mut_or_default(show_all_id);
                if let Some(i) = v.iter().position(|x| *x == id) {
                    v.remove(i);
                } else {
                    v.push(id);
                }
            }),
        }
    }
}

fn drop_zone(ui: &mut egui::Ui, session: &mut Session, chain: ChainRef, height: f32) {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(170.0, height), Sense::hover());
    let hovering = resp.dnd_hover_payload::<BrowserItem>().is_some();
    ui.painter().rect_stroke(
        rect.shrink(1.0),
        2.0,
        Stroke::new(1.0, if hovering { ACCENT } else { hex(0x3a3a3a) }),
        StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        "Drop Instruments or\nAudio Effects Here",
        theme::font(11.0),
        if hovering { ACCENT } else { TEXT_FAINT },
    );
    if let Some(item) = resp.dnd_release_payload::<BrowserItem>() {
        match &*item {
            BrowserItem::Builtin(t) => session.add_builtin(chain, t),
            BrowserItem::Plugin(p) => session.add_plugin(chain, p),
            BrowserItem::Sample(path) => {
                // Dropping a sample on the chain adds a Sampler loaded with it.
                session.add_builtin(chain, "rlive.sampler");
                if let Some(d) = session
                    .devices(chain)
                    .iter()
                    .rev()
                    .find(|d| matches!(&d.kind, DeviceKind::Builtin { type_id, .. } if type_id == "rlive.sampler"))
                {
                    let id = d.id;
                    session.set_device_sample(chain, id, path.clone());
                }
            }
        }
    }
    info(
        resp,
        "Drag devices, plug-ins or samples from the browser into the chain",
    );
}

#[allow(clippy::too_many_arguments)]
fn device(
    ui: &mut egui::Ui,
    session: &Session,
    chain: ChainRef,
    dev: &rlive_project::Device,
    index: usize,
    count: usize,
    height: f32,
    show_all: bool,
    actions: &mut Vec<Action>,
) {
    let rt = session.runtimes.get(&dev.id);
    let on = !dev.bypass;
    let params: Vec<&ParamInfo> = rt
        .map(|r| {
            r.params
                .iter()
                .filter(|p| !p.flags.contains(ParamFlags::HIDDEN))
                .collect()
        })
        .unwrap_or_default();
    let shown = if show_all {
        params.len()
    } else {
        params.len().min(MAX_PARAMS)
    };

    // Extra rows above the knobs (plugin editor button, sample, errors).
    let mut extra_h = 0.0;
    if rt.is_some_and(|r| r.error.is_some()) {
        extra_h += 34.0;
    }
    let uses_sample = rt.is_some_and(|r| r.sample_slots > 0);
    if uses_sample {
        extra_h += 24.0;
    }
    if params.len() > MAX_PARAMS {
        extra_h += 20.0;
    }
    let body_h = (height - TITLE_H - 10.0 - extra_h).max(KNOB_H);
    let rows = ((body_h / KNOB_H).floor() as usize).max(1);
    let cols = shown.div_ceil(rows).max(1);
    let is_plugin = matches!(dev.kind, DeviceKind::Plugin { .. });
    let width = if dev.expanded {
        (cols as f32 * KNOB_W + 12.0).max(if is_plugin { 170.0 } else { 150.0 })
    } else {
        FOLDED_W
    };

    let (rect, bg) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
    let p = ui.painter().clone();
    p.rect_filled(rect, 1.0, if on { hex(0x3a3a3a) } else { hex(0x2f2f2f) });
    p.rect_stroke(rect, 1.0, Stroke::new(1.0, SEPARATOR), StrokeKind::Inside);

    // ---- title bar
    let title = Rect::from_min_size(rect.min, Vec2::new(width, TITLE_H));
    let mut tu = ui.new_child(
        UiBuilder::new()
            .max_rect(title.shrink2(Vec2::new(2.0, 1.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    tu.spacing_mut().item_spacing.x = 2.0;
    if dev.expanded {
        p.rect_filled(title, 1.0, if on { hex(0x505050) } else { hex(0x3a3a3a) });
    }
    let r = widgets::bare_icon(&mut tu, Vec2::new(18.0, 18.0), |p, r, c| {
        icons::power(p, r, if on { ACCENT } else { c })
    });
    if info(
        r,
        if on {
            "Device on — click to bypass"
        } else {
            "Device bypassed — click to activate"
        },
    )
    .clicked()
    {
        actions.push(Action::Bypass(dev.id, on));
    }
    if dev.expanded {
        let r = widgets::bare_icon(&mut tu, Vec2::new(14.0, 18.0), |p, r, c| {
            icons::fold(p, r, true, c)
        });
        if info(r, "Fold device").clicked() {
            actions.push(Action::Fold(dev.id, false));
        }
        let name_w =
            (title.right() - tu.cursor().left() - if is_plugin { 46.0 } else { 6.0 }).max(10.0);
        let (nr, _) = tu.allocate_exact_size(Vec2::new(name_w, 18.0), Sense::hover());
        let galley = tu.painter().layout_no_wrap(
            dev.name.clone(),
            theme::font_bold(11.0),
            if on { TEXT } else { TEXT_DIM },
        );
        tu.painter().with_clip_rect(nr).galley(
            Pos2::new(nr.left() + 2.0, nr.center().y - galley.size().y / 2.0),
            galley,
            TEXT,
        );
        if let Some(h) = rt.and_then(|r| r.plugin.as_ref()) {
            if h.has_editor() {
                let open = h.is_editor_open();
                let r = widgets::state_button(&mut tu, Vec2::new(40.0, 15.0), open, ACCENT, "Edit");
                if info(r, "Show/hide the plug-in's own editor window").clicked() {
                    actions.push(if open {
                        Action::CloseEditor(dev.id)
                    } else {
                        Action::OpenEditor(dev.id)
                    });
                }
            }
        }
    } else {
        // Folded: just the fold arrow and the name written vertically.
        let r = widgets::bare_icon(&mut tu, Vec2::new(0.0, 0.0), |_, _, _| {});
        let _ = r;
        let arrow = Rect::from_min_size(
            Pos2::new(rect.left() + 3.0, rect.top() + 22.0),
            Vec2::splat(18.0),
        );
        let ar = ui.interact(
            arrow,
            ui.scope_id().with(("unfold", dev.id)),
            Sense::click(),
        );
        icons::fold(
            &p,
            arrow,
            false,
            if ar.hovered() {
                hex(0xffffff)
            } else {
                TEXT_DIM
            },
        );
        if info(ar, "Unfold device").clicked() {
            actions.push(Action::Fold(dev.id, true));
        }
        let galley = p.layout_no_wrap(
            dev.name.clone(),
            theme::font_bold(11.0),
            if on { TEXT } else { TEXT_DIM },
        );
        let pos = Pos2::new(
            rect.center().x - galley.size().y / 2.0,
            rect.top() + 46.0 + galley.size().x,
        );
        p.add(
            egui::epaint::TextShape::new(pos, galley, TEXT)
                .with_angle(-std::f32::consts::FRAC_PI_2),
        );
    }

    let resp = info(bg, format!("{} — right-click for options", dev.name));
    resp.context_menu(|ui| {
        if index > 0 && ui.button("Move Left").clicked() {
            actions.push(Action::Move(dev.id, index - 1));
            ui.close();
        }
        if index + 1 < count && ui.button("Move Right").clicked() {
            actions.push(Action::Move(dev.id, index + 1));
            ui.close();
        }
        if ui
            .button(if dev.expanded { "Fold" } else { "Unfold" })
            .clicked()
        {
            actions.push(Action::Fold(dev.id, !dev.expanded));
            ui.close();
        }
        if is_plugin && ui.button("Reload Plug-In").clicked() {
            actions.push(Action::Reload(dev.id));
            ui.close();
        }
        ui.separator();
        if ui.button(RichText::new("Delete").color(ARM_RED)).clicked() {
            actions.push(Action::Remove(dev.id));
            ui.close();
        }
    });

    if !dev.expanded {
        return;
    }

    // ---- body
    let body = Rect::from_min_max(
        Pos2::new(rect.left() + 6.0, title.bottom() + 6.0),
        rect.max - Vec2::new(6.0, 4.0),
    );
    let mut bu = ui.new_child(UiBuilder::new().max_rect(body));
    bu.set_clip_rect(body.intersect(ui.clip_rect()));
    bu.spacing_mut().item_spacing = Vec2::new(0.0, 2.0);
    if !on {
        bu.multiply_opacity(0.55);
    }
    let Some(rt) = rt else {
        bu.label(RichText::new("Not loaded").color(ARM_RED));
        return;
    };
    if let Some(err) = &rt.error {
        bu.label(RichText::new(err).color(ARM_RED).size(10.5));
        if is_plugin && bu.small_button("Retry").clicked() {
            actions.push(Action::Reload(dev.id));
        }
    }
    if let DeviceKind::Builtin { .. } = &dev.kind {
        if uses_sample {
            bu.horizontal(|ui| {
                let name = match dev.sample(0) {
                    Some(file) => file
                        .path
                        .file_stem()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    None => "No sample — drop one here".into(),
                };
                let (r, resp) =
                    ui.allocate_exact_size(Vec2::new(body.width() - 50.0, 18.0), Sense::hover());
                ui.painter().rect_filled(r, 1.0, FIELD_BG);
                let g = ui.painter().layout_no_wrap(name, theme::font(10.5), ACCENT);
                ui.painter().with_clip_rect(r).galley(
                    Pos2::new(r.left() + 4.0, r.center().y - g.size().y / 2.0),
                    g,
                    ACCENT,
                );
                info(resp, "Sample used by this Sampler");
                if ui.small_button("Load").clicked() {
                    actions.push(Action::LoadSample(dev.id));
                }
            });
        }
    }
    // Knobs, column-major like Live's device panels.
    let grid_top = bu.cursor().top();
    for (i, prm) in params.iter().take(shown).enumerate() {
        let col = i / rows;
        let row = i % rows;
        let r = Rect::from_min_size(
            Pos2::new(
                body.left() + col as f32 * KNOB_W,
                grid_top + row as f32 * KNOB_H,
            ),
            Vec2::new(KNOB_W, KNOB_H),
        );
        let mut ku = bu.new_child(UiBuilder::new().max_rect(r));
        let value = session.param_value(chain, dev.id, prm);
        let text = rt.format_value(prm, value);
        let effective = session.effective_norm(chain, dev.id, prm.id);
        if let Some(v) = widgets::knob(&mut ku, prm, value, &text, effective) {
            actions.push(Action::Param(dev.id, prm.id, v));
        }
    }
    if params.len() > MAX_PARAMS {
        let r = Rect::from_min_size(
            Pos2::new(body.left(), body.bottom() - 18.0),
            Vec2::new(body.width(), 18.0),
        );
        let label = if show_all {
            "Show fewer parameters".to_string()
        } else {
            format!("Show all {} parameters", params.len())
        };
        if bu
            .put(r, egui::Button::new(RichText::new(label).size(10.5)))
            .clicked()
        {
            actions.push(Action::ShowAll(dev.id));
        }
    }
    if params.is_empty() && rt.error.is_none() {
        bu.label(RichText::new("No parameters").color(TEXT_DIM));
    }
}
