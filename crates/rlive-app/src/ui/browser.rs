//! Live-style browser: categories on the left, items on the right, search on top.
//! Drag items onto tracks/devices, or double-click to add them to the selected track.

use super::theme::{self, *};
use super::widgets::info;
use super::{BrowserItem, UiState};
use crate::session::{ChainRef, Session};
use crate::settings::Settings;
use egui::{Align2, Id, Pos2, Rect, Sense, Stroke, Vec2};
use rlive_core::PluginFormat;
use rlive_devices::DeviceRole;
use rlive_project::TrackKind;
use std::path::{Path, PathBuf};

pub fn list_samples(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        if depth > 4 || out.len() > 5000 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, depth + 1, out);
            } else if p.extension().is_some_and(|x| {
                rlive_audio_io::AUDIO_EXTENSIONS
                    .iter()
                    .any(|e| x.eq_ignore_ascii_case(e))
            }) {
                out.push(p);
            }
        }
    }
    walk(dir, 0, &mut out);
    out.sort();
    out
}

/// The audio clip the Clip View is showing, which a sample from the browser goes into.
fn sample_target(session: &Session, state: &UiState) -> Option<crate::session::ClipRef> {
    let cref = state.selected_clip?;
    let showing = state.show_detail && state.detail == crate::ui::Detail::Clip;
    (showing && session.clip_at(cref).is_some_and(|c| !c.is_midi())).then_some(cref)
}

fn matches(filter: &str, text: &str) -> bool {
    filter.is_empty() || text.to_lowercase().contains(&filter.to_lowercase())
}

/// Apply a browser item to the selected track (double-click action).
pub fn apply(session: &mut Session, ui_state: &mut UiState, item: &BrowserItem) {
    if let (BrowserItem::Sample(path), Some(cref)) = (item, sample_target(session, ui_state)) {
        session.replace_clip_sample(cref, path);
        return;
    }
    let chain = ui_state.selected.unwrap_or_else(|| {
        let id = session.add_track(TrackKind::Midi, None);
        ChainRef::Track(id)
    });
    match item {
        BrowserItem::Builtin(id) => session.add_builtin(chain, id),
        BrowserItem::Plugin(desc) => session.add_plugin(chain, desc),
        BrowserItem::Sample(path) => {
            // Put the sample into the first empty slot of the selected audio track (or a new one).
            let track = match chain {
                ChainRef::Track(t)
                    if session
                        .project
                        .track(t)
                        .is_some_and(|t| t.kind == TrackKind::Audio) =>
                {
                    t
                }
                _ => session.add_track(TrackKind::Audio, None),
            };
            match session.audio_clip_from_file(path, track) {
                Ok(clip) => {
                    let slot = session
                        .project
                        .track(track)
                        .and_then(|t| t.clips.iter().position(|c| c.is_none()))
                        .unwrap_or(0);
                    session.set_clip(track, slot, Some(clip));
                    ui_state.selected = Some(ChainRef::Track(track));
                }
                Err(e) => ui_state.status = e,
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Category {
    Instruments,
    AudioEffects,
    Plugins,
    Samples,
}

impl Category {
    const ALL: [Category; 4] = [
        Category::Instruments,
        Category::AudioEffects,
        Category::Plugins,
        Category::Samples,
    ];

    fn label(&self) -> &'static str {
        match self {
            Category::Instruments => "Instruments",
            Category::AudioEffects => "Audio Effects",
            Category::Plugins => "Plug-Ins",
            Category::Samples => "Samples",
        }
    }
}

const ROW_H: f32 = 19.0;

/// Where a row records that it was clicked, for the browser to preview it once it's drawn.
fn clicked_row() -> Id {
    Id::unique("rlive_browser_clicked_row")
}

/// Where the browser remembers the row Enter would load.
fn enter_row() -> Id {
    Id::unique("rlive_browser_enter_row")
}

/// One list row: hover highlight, drag source, double-click to apply.
fn row(
    ui: &mut egui::Ui,
    item: BrowserItem,
    text: &str,
    sub: Option<&str>,
    hover: &str,
) -> Option<BrowserItem> {
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), ROW_H),
        Sense::click_and_drag(),
    );
    let p = ui.painter();
    if resp.hovered() || resp.dragged() {
        p.rect_filled(rect, 0.0, shade(0x3d4a57));
    }
    let mut x = rect.left() + 8.0;
    if let Some(sub) = sub {
        let r = p.text(
            Pos2::new(rect.right() - 6.0, rect.center().y),
            Align2::RIGHT_CENTER,
            sub,
            theme::font(9.5),
            pal().text_faint,
        );
        let _ = r;
    }
    let galley = p.layout_no_wrap(text.to_string(), theme::font(11.5), pal().text);
    let clip = Rect::from_min_max(
        Pos2::new(x, rect.top()),
        Pos2::new(
            rect.right() - if sub.is_some() { 40.0 } else { 4.0 },
            rect.bottom(),
        ),
    );
    p.with_clip_rect(clip.intersect(ui.clip_rect())).galley(
        Pos2::new(x, rect.center().y - galley.size().y / 2.0),
        galley,
        pal().text,
    );
    x += 0.0;
    let _ = x;
    if resp.dragged() {
        if let Some(pos) = ui.ctx().pointer_latest_pos() {
            egui::Area::new(Id::unique("browser_drag_preview"))
                .order(egui::Order::Tooltip)
                .fixed_pos(pos + egui::vec2(12.0, 4.0))
                .interactable(false)
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| ui.label(text));
                });
        }
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }
    let double = resp.double_clicked();
    if resp.clicked() {
        ui.data_mut(|d| d.insert_temp(clicked_row(), Some(item.clone())));
    }
    resp.dnd_set_drag_payload(item.clone());
    info(resp, hover);
    double.then_some(item)
}

fn group_header(ui: &mut egui::Ui, text: &str) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 18.0), Sense::hover());
    ui.painter().text(
        Pos2::new(rect.left() + 6.0, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        theme::font_bold(10.0),
        pal().text_dim,
    );
}

fn list_category(
    ui: &mut egui::Ui,
    session: &Session,
    state: &mut UiState,
    settings: &mut Settings,
    cat: Category,
    filter: &str,
    activate: &mut Option<BrowserItem>,
) -> usize {
    let mut shown = 0;
    match cat {
        Category::Instruments | Category::AudioEffects => {
            let role = if cat == Category::Instruments {
                DeviceRole::Instrument
            } else {
                DeviceRole::AudioEffect
            };
            let shown_here = |b: &&&rlive_devices::BuiltinInfo| b.role == role && b.visible();
            for b in rlive_devices::builtins().iter().filter(shown_here) {
                if !matches(filter, b.name) {
                    continue;
                }
                shown += 1;
                if let Some(it) = row(
                    ui,
                    BrowserItem::Builtin(b.id),
                    b.name,
                    None,
                    &format!("{} — drag onto a track or double-click to add", b.name),
                ) {
                    *activate = Some(it);
                }
            }
        }
        Category::Plugins => {
            if session.plugins.plugins.is_empty() && filter.is_empty() {
                ui.add_space(4.0);
                ui.label(egui::RichText::new("No plug-ins scanned yet.").color(pal().text_dim));
                if ui.button("Scan Plug-Ins").clicked() {
                    state.request_scan = true;
                }
            }
            for format in PluginFormat::ALL {
                let mut list: Vec<_> = session
                    .plugins_of(format)
                    .filter(|p| matches(filter, &p.name) || matches(filter, &p.vendor))
                    .cloned()
                    .collect();
                if list.is_empty() {
                    continue;
                }
                list.sort_by_key(|p| (p.vendor.to_lowercase(), p.name.to_lowercase()));
                group_header(ui, format.label());
                for p in list {
                    shown += 1;
                    let hover = format!(
                        "{} — {} {} ({})",
                        p.name,
                        p.vendor,
                        p.version,
                        if p.is_instrument {
                            "instrument"
                        } else {
                            "effect"
                        }
                    );
                    let sub = if p.is_instrument { "INST" } else { "FX" };
                    if let Some(it) = row(
                        ui,
                        BrowserItem::Plugin(p.clone()),
                        &p.name,
                        Some(sub),
                        &hover,
                    ) {
                        *activate = Some(it);
                    }
                }
            }
        }
        Category::Samples => {
            if filter.is_empty() {
                ui.horizontal(|ui| {
                    let label = settings
                        .sample_dir
                        .as_ref()
                        .and_then(|d| d.file_name())
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "Choose Folder…".into());
                    if ui.button(label).clicked() {
                        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                            state.sample_files = list_samples(&dir);
                            settings.sample_dir = Some(dir);
                            settings.save();
                        }
                    }
                    if settings.sample_dir.is_some() && ui.small_button("Refresh").clicked() {
                        if let Some(d) = &settings.sample_dir {
                            state.sample_files = list_samples(d);
                        }
                    }
                    let r = ui.toggle_value(&mut state.preview_samples, "Preview");
                    info(
                        r,
                        "Preview — click a sample to hear it, through Cue when it's on",
                    );
                });
            }
            let base = settings.sample_dir.clone().unwrap_or_default();
            for path in state.sample_files.iter().take(3000) {
                let rel = path
                    .strip_prefix(&base)
                    .unwrap_or(path)
                    .display()
                    .to_string();
                if !matches(filter, &rel) {
                    continue;
                }
                shown += 1;
                let name = path
                    .file_stem()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if let Some(it) = row(
                    ui,
                    BrowserItem::Sample(path.clone()),
                    &name,
                    Some("WAV"),
                    &rel,
                ) {
                    *activate = Some(it);
                }
            }
        }
    }
    shown
}

pub fn show(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut UiState,
    settings: &mut Settings,
) {
    let full = ui.max_rect();
    // Claim the whole panel so egui keeps its size (children don't advance the cursor).
    ui.expand_to_include_rect(full);
    let previewing = state.preview_samples;
    // Search
    let search = Rect::from_min_size(
        full.min + Vec2::new(6.0, 6.0),
        Vec2::new(full.width() - 12.0, 20.0),
    );
    ui.put(
        search,
        egui::TextEdit::singleline(&mut state.browser_filter)
            .hint_text("Search (all categories)")
            .font(theme::font(11.5)),
    );
    let body = Rect::from_min_max(Pos2::new(full.left(), search.bottom() + 6.0), full.max);
    ui.painter().line_segment(
        [body.left_top(), body.right_top()],
        Stroke::new(1.0, pal().separator),
    );
    let filter = state.browser_filter.clone();
    let mut activate: Option<BrowserItem> = None;

    // Categories column
    let cat_w = (full.width() * 0.38).clamp(92.0, 130.0);
    let cats = Rect::from_min_max(body.min, Pos2::new(body.left() + cat_w, body.bottom()));
    ui.painter().rect_filled(cats, 0.0, pal().panel_dark);
    ui.painter().line_segment(
        [cats.right_top(), cats.right_bottom()],
        Stroke::new(1.0, pal().separator),
    );
    let mut cu = ui.new_child(egui::UiBuilder::new().max_rect(cats.shrink2(Vec2::new(0.0, 4.0))));
    widgets_label(&mut cu, "CATEGORIES");
    for c in Category::ALL {
        let (r, resp) = cu.allocate_exact_size(Vec2::new(cat_w, 20.0), Sense::click());
        let sel = state.browser_category == c && filter.is_empty();
        if sel {
            cu.painter().rect_filled(r, 0.0, shade(0x46525e));
        } else if resp.hovered() {
            cu.painter().rect_filled(r, 0.0, pal().raised);
        }
        cu.painter().text(
            Pos2::new(r.left() + 10.0, r.center().y),
            Align2::LEFT_CENTER,
            c.label(),
            theme::font(11.5),
            if sel { shade(0xffffff) } else { pal().text },
        );
        if info(
            resp,
            format!("{} — show this part of the Browser", c.label()),
        )
        .clicked()
        {
            state.browser_category = c;
            state.browser_filter.clear();
        }
    }

    // Items column
    let items = Rect::from_min_max(Pos2::new(cats.right() + 1.0, body.top()), body.max);
    let mut iu = ui.new_child(egui::UiBuilder::new().max_rect(items));
    iu.set_clip_rect(items.intersect(ui.clip_rect()));
    egui::ScrollArea::vertical()
        .id_salt("browser_items")
        .auto_shrink([false, false])
        .show(&mut iu, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            if filter.is_empty() {
                list_category(
                    ui,
                    session,
                    state,
                    settings,
                    state.browser_category,
                    &filter,
                    &mut activate,
                );
            } else {
                let mut total = 0;
                for c in Category::ALL {
                    group_header(ui, &c.label().to_uppercase());
                    total += list_category(ui, session, state, settings, c, &filter, &mut activate);
                }
                if total == 0 {
                    ui.label(egui::RichText::new("No results").color(pal().text_dim));
                }
            }
        });

    if let Some(item) = activate {
        apply(session, state, &item);
    }
    let clicked: Option<BrowserItem> = ui.data_mut(|d| d.remove_temp(clicked_row())).flatten();
    if let (Some(BrowserItem::Sample(path)), true) = (&clicked, state.preview_samples) {
        session.preview_file(path);
    }
    // Enter loads the sample last clicked, while the browser has the keyboard.
    if let Some(item) = clicked {
        ui.data_mut(|d| d.insert_temp(enter_row(), Some(item)));
    }
    let enter = state.focus == crate::ui::keymap::FocusArea::Browser
        && !ui.ctx().egui_wants_keyboard_input()
        && ui.input(|i| i.key_pressed(egui::Key::Enter));
    if enter {
        let chosen: Option<BrowserItem> = ui.data(|d| d.get_temp(enter_row())).flatten();
        if let Some(item) = chosen {
            apply(session, state, &item);
        }
    }
    if previewing && !state.preview_samples {
        session.stop_preview();
    }
}

fn widgets_label(ui: &mut egui::Ui, text: &str) {
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(text)
                .font(theme::font_bold(9.5))
                .color(pal().text_faint),
        );
    });
}
