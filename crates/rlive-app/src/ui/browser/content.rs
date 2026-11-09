//! The right half: Filter View, Results bar, content pane and Quick Tags.

use super::super::theme::{self, *};
use super::super::widgets::info;
use super::super::UiState;
use super::{filters, load};
use crate::session::Session;
use egui::{Align2, Pos2, Rect, Sense, Stroke, UiBuilder, Vec2};
use rlive_browser::library::COLLECTION_COUNT;
use rlive_browser::query::{Column, Sort};
use rlive_browser::{ItemKey, LibraryData, Op, Row, Scope};
use std::sync::Arc;

const ROW_H: f32 = 19.0;
const HEADER_H: f32 = 18.0;
const COLUMN_W: f32 = 66.0;
/// How many colours a row shows at most.
const MAX_SWATCHES: usize = 3;

pub(super) fn right_side(
    ui: &mut egui::Ui,
    rect: Rect,
    session: &mut Session,
    state: &mut UiState,
) {
    let lib = state
        .browser
        .db
        .as_ref()
        .map_or_else(|| Arc::new(LibraryData::default()), |d| d.library().clone());
    let mut top = rect.top();
    if lib.show_filters {
        let h = state.browser.filter_h.clamp(40.0, rect.height() * 0.6);
        let area = Rect::from_min_size(rect.min, Vec2::new(rect.width(), h));
        let mut fu = ui.new_child(UiBuilder::new().max_rect(area));
        fu.set_clip_rect(area.intersect(ui.clip_rect()));
        egui::ScrollArea::vertical()
            .id_salt("browser_filters")
            .auto_shrink([false, false])
            .show(&mut fu, |ui| filters::filter_view(ui, state));
        top = area.bottom();
        ui.painter().line_segment(
            [Pos2::new(rect.left(), top), Pos2::new(rect.right(), top)],
            Stroke::new(1.0, pal().separator),
        );
        // The line under the filters moves.
        let split = Rect::from_center_size(
            Pos2::new(rect.center().x, top),
            Vec2::new(rect.width(), 4.0),
        );
        let r = ui.interact(split, ui.scope_id().with("filter_split"), Sense::drag());
        if r.hovered() || r.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        }
        if r.dragged() {
            state.browser.filter_h = (state.browser.filter_h + r.drag_delta().y).clamp(40.0, 400.0);
        }
    }
    let searching = state.browser.model.rows.search;
    if searching {
        let bar = Rect::from_min_size(Pos2::new(rect.left(), top), Vec2::new(rect.width(), 20.0));
        results_bar(ui, bar, state);
        top = bar.bottom();
    }
    let quick_h = if lib.show_quick_tags {
        filters::QUICK_H
    } else {
        0.0
    };
    let pane = Rect::from_min_max(
        Pos2::new(rect.left(), top),
        Pos2::new(rect.right(), rect.bottom() - quick_h),
    );
    let mut cu = ui.new_child(UiBuilder::new().max_rect(pane));
    cu.set_clip_rect(pane.intersect(ui.clip_rect()));
    content_pane(&mut cu, pane, &lib, session, state);
    if lib.show_quick_tags {
        let q = Rect::from_min_max(Pos2::new(rect.left(), pane.bottom()), rect.max);
        let mut qu = ui.new_child(UiBuilder::new().max_rect(q));
        qu.set_clip_rect(q.intersect(ui.clip_rect()));
        filters::quick_tags(&mut qu, q, state);
    }
}

/// How many tags are on, Clear, and Add Label (manual pp. 67-70).
fn results_bar(ui: &mut egui::Ui, bar: Rect, state: &mut UiState) {
    ui.painter().rect_filled(bar, 0.0, pal().raised);
    let tags = state.browser.model.nav.tags.len();
    let found = state.browser.model.rows.rows.len();
    let text = if tags > 0 {
        format!("{found} results, {tags} filters")
    } else {
        format!("{found} results")
    };
    ui.painter().text(
        bar.left_center() + Vec2::new(6.0, 0.0),
        Align2::LEFT_CENTER,
        text,
        theme::font(10.0),
        pal().text_dim,
    );
    let mut bu = ui.new_child(
        UiBuilder::new()
            .max_rect(bar.shrink2(Vec2::new(4.0, 1.0)))
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    let add = bu.add(egui::Button::new("Add Label").small());
    if info(
        add,
        "Add Label — keep these results as a label in the Library",
    )
    .clicked()
    {
        let nav = state.browser.model.nav.clone();
        let name = if nav.text.trim().is_empty() {
            "Filtered".to_string()
        } else {
            nav.text.trim().to_string()
        };
        state.browser.op(Op::AddCustomLabel {
            name,
            scope: nav.scope.clone(),
            text: nav.text,
            tags: nav.tags,
        });
    }
    let clear = bu.add(egui::Button::new("Clear").small());
    if info(clear, "Clear — remove the search words and every filter").clicked() {
        state.browser.model.clear_all();
    }
}

fn column_width(column: Column) -> f32 {
    match column {
        Column::Name => 0.0,
        Column::Tags => COLUMN_W * 1.5,
        _ => COLUMN_W,
    }
}

fn content_options(ui: &mut egui::Ui, lib: &LibraryData, state: &mut UiState) {
    ui.label(egui::RichText::new("Columns").color(pal().text_dim));
    for column in Column::OPTIONAL {
        let mut on = lib.content.columns.contains(&column);
        if ui.checkbox(&mut on, column.label()).changed() {
            let mut columns = lib.content.columns.clone();
            if on {
                columns.push(column);
            } else {
                columns.retain(|c| *c != column);
            }
            state.browser.op(Op::SetColumns(columns));
        }
    }
    ui.separator();
    let mut ext = lib.content.show_extensions;
    if ui.checkbox(&mut ext, "File Extensions").changed() {
        state.browser.op(Op::SetShowExtensions(ext));
    }
}

/// The column headers: click to sort, drag to reorder, right-click for options.
fn headers(
    ui: &mut egui::Ui,
    rect: Rect,
    lib: &LibraryData,
    state: &mut UiState,
) -> Vec<(Column, Rect)> {
    ui.painter().rect_filled(rect, 0.0, pal().panel_dark);
    let mut spots = Vec::new();
    let mut right = rect.right() - 4.0;
    for column in lib.content.columns.iter().rev() {
        let w = column_width(*column);
        spots.push((
            *column,
            Rect::from_min_max(
                Pos2::new(right - w, rect.top()),
                Pos2::new(right, rect.bottom()),
            ),
        ));
        right -= w;
    }
    spots.push((
        Column::Name,
        Rect::from_min_max(rect.min, Pos2::new(right, rect.bottom())),
    ));
    spots.reverse();
    let sort = lib.content.sort;
    let mut swap = None;
    for (column, r) in &spots {
        let id = ui.scope_id().with(("browser_column", *column));
        let sense = if *column == Column::Name {
            Sense::click()
        } else {
            Sense::click_and_drag()
        };
        let resp = ui.interact(*r, id, sense);
        let arrow = if sort.column == *column {
            if sort.descending {
                " ▼"
            } else {
                " ▲"
            }
        } else {
            ""
        };
        ui.painter().with_clip_rect(*r).text(
            r.left_center() + Vec2::new(4.0, 0.0),
            Align2::LEFT_CENTER,
            format!("{}{arrow}", column.label()),
            theme::font_bold(9.5),
            pal().text_dim,
        );
        if resp.clicked() {
            let descending = sort.column == *column && !sort.descending;
            let next = Sort {
                column: *column,
                descending,
            };
            state.browser.model.set_sort(next);
            state.browser.op(Op::SetSort(next));
        }
        if *column != Column::Name {
            resp.dnd_set_drag_payload(*column);
            if let Some(from) = crate::ui::widgets::released::<Column>(&resp) {
                if *from != *column {
                    swap = Some((*from, *column));
                }
            }
            if resp.dragged() {
                super::drag_label(ui, column.label());
            }
        }
        resp.context_menu(|ui| content_options(ui, lib, state));
    }
    if let Some((a, b)) = swap {
        let mut columns = lib.content.columns.clone();
        let (Some(i), Some(j)) = (
            columns.iter().position(|c| *c == a),
            columns.iter().position(|c| *c == b),
        ) else {
            return spots;
        };
        columns.swap(i, j);
        state.browser.op(Op::SetColumns(columns));
    }
    spots
}

fn format_size(bytes: u64) -> String {
    if bytes == 0 {
        String::new()
    } else if bytes < 1024 * 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn column_text(row: &Row, column: Column, state: &UiState) -> String {
    match column {
        Column::Name => row.item.name.clone(),
        Column::Type => row.item.kind.label().to_string(),
        Column::Size => format_size(row.item.size),
        Column::Modified => {
            if row.item.modified == 0 {
                String::new()
            } else {
                rlive_browser::backup::stamp(row.item.modified)[..10].to_string()
            }
        }
        Column::Tags => state.browser.item_tags(&row.item).join(", "),
    }
}

/// One line of the content pane.
fn draw_row(
    ui: &mut egui::Ui,
    rect: Rect,
    row: &Row,
    columns: &[(Column, Rect)],
    lib: &LibraryData,
    session: &mut Session,
    state: &mut UiState,
) {
    let selected = state.browser.model.selected.contains(&row.item.key);
    let cursor = state.browser.model.cursor.as_ref() == Some(&row.item.key);
    let resp = ui.interact(
        rect,
        ui.scope_id().with(("browser_row", &row.item.key)),
        Sense::click_and_drag(),
    );
    let p = ui.painter().clone();
    if selected {
        p.rect_filled(rect, 0.0, shade(0x46525e));
    } else if resp.hovered() || resp.dragged() {
        p.rect_filled(rect, 0.0, shade(0x3d4a57));
    }
    if cursor && state.focus == crate::ui::keymap::FocusArea::Browser {
        p.rect_stroke(
            rect.shrink(0.5),
            0.0,
            Stroke::new(1.0, pal().accent),
            egui::StrokeKind::Inside,
        );
    }
    let mut x = rect.left() + 6.0 + row.depth as f32 * 12.0;
    if row.has_children {
        let arrow = Rect::from_center_size(Pos2::new(x + 5.0, rect.center().y), Vec2::splat(11.0));
        let ar = ui.interact(
            arrow,
            ui.scope_id().with(("browser_fold", &row.item.key)),
            Sense::click(),
        );
        super::super::icons::fold(
            &p,
            arrow,
            row.expanded,
            if ar.hovered() {
                shade(0xffffff)
            } else {
                pal().text_dim
            },
        );
        if ar.clicked() {
            let keep = ui.input(|i| i.modifiers.command);
            state.browser.model.toggle_folder(&row.item.key, keep);
        }
    }
    x += 13.0;
    // Up to three colours (manual p. 81).
    let colors: Vec<usize> = (0..COLLECTION_COUNT)
        .filter(|i| row.colors & (1 << i) != 0)
        .collect();
    let mut name = row.item.name.clone();
    if lib.content.show_extensions && !row.item.ext.is_empty() {
        name = format!("{name}.{}", row.item.ext);
    }
    let name_col = columns
        .iter()
        .find(|(c, _)| *c == Column::Name)
        .map_or(rect, |(_, r)| *r);
    let swatch_x = name_col.right() - 8.0 - (colors.len().min(MAX_SWATCHES) as f32) * 9.0;
    let clip = Rect::from_min_max(Pos2::new(x, rect.top()), Pos2::new(swatch_x, rect.bottom()));
    p.with_clip_rect(clip.intersect(ui.clip_rect())).text(
        Pos2::new(x, rect.center().y),
        Align2::LEFT_CENTER,
        &name,
        theme::font(11.0),
        pal().text,
    );
    for (i, color) in colors.iter().take(MAX_SWATCHES).enumerate() {
        let r = Rect::from_center_size(
            Pos2::new(swatch_x + 4.0 + i as f32 * 9.0, rect.center().y),
            Vec2::splat(7.0),
        );
        if let Some(c) = lib.collections.get(*color) {
            super::color_square(ui, r, c.color);
        }
    }
    for (column, r) in columns {
        if *column == Column::Name {
            continue;
        }
        p.with_clip_rect(*r).text(
            Pos2::new(r.left() + 4.0, rect.center().y),
            Align2::LEFT_CENTER,
            column_text(row, *column, state),
            theme::font(10.0),
            pal().text_dim,
        );
    }

    let key = row.item.key.clone();
    let double = resp.double_clicked();
    if resp.clicked() {
        let (add, range) = ui.input(|i| (i.modifiers.command, i.modifiers.shift));
        state.browser.model.click(key.clone(), add, range);
        state.browser.model.pane = rlive_browser::model::Pane::Content;
        state.focus = crate::ui::keymap::FocusArea::Browser;
        state.browser.apply_after_move = true;
    }
    if double {
        if row.has_children && row.item.kind == rlive_browser::ItemKind::Folder {
            let keep = ui.input(|i| i.modifiers.command);
            state.browser.model.toggle_folder(&key, keep);
        } else {
            super::apply_selection(session, state);
        }
    }
    // A drag carries the whole selection, or just this row if it isn't part of it.
    if resp.drag_started() && !selected {
        state.browser.model.click(key.clone(), false, false);
    }
    // A groove travels as the Groove Pool's own drag, so clips and MIDI tracks treat it the same.
    if resp.drag_started() {
        state.browser.dragged_groove = load::pool_groove(session, state, row);
    }
    if resp.dragged() {
        match state.browser.dragged_groove {
            Some(id) => resp.dnd_set_drag_payload(crate::ui::groove_pool::GrooveDrag(id)),
            None => {
                if let Some(item) = load::drag_item(session, state, row) {
                    resp.dnd_set_drag_payload(item);
                }
            }
        }
        super::drag_label(ui, &name);
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }
    let hover = if row.item.kind == rlive_browser::ItemKind::Tuning && !row.item.vendor.is_empty() {
        row.item.vendor.clone()
    } else {
        format!(
            "{} — {}. Drag it somewhere, or double-click to add it to the selected track",
            row.item.name,
            row.item.kind.label()
        )
    };
    let resp = info(resp, hover);
    resp.context_menu(|ui| item_menu(ui, state, &key, lib));
}

fn item_menu(ui: &mut egui::Ui, state: &mut UiState, key: &ItemKey, lib: &LibraryData) {
    if !state.browser.model.selected.contains(key) {
        state.browser.model.click(key.clone(), false, false);
    }
    ui.label(egui::RichText::new("Assign colour").color(pal().text_dim));
    for (i, c) in lib.collections.iter().enumerate() {
        let on = state
            .browser
            .model
            .selected
            .iter()
            .all(|k| lib.colors_of(k) & (1 << i) != 0);
        if ui
            .selectable_label(on, format!("{} ({})", c.name, i + 1))
            .clicked()
        {
            state.browser.assign_color(Some(i as u8));
            ui.close();
        }
    }
    if ui.button("Remove Colours (0)").clicked() {
        state.browser.assign_color(None);
        ui.close();
    }
    ui.separator();
    let is_file = state
        .browser
        .model
        .selected_rows()
        .iter()
        .all(|r| r.item.path.is_some());
    if ui
        .add_enabled(is_file, egui::Button::new("Delete File(s)"))
        .clicked()
    {
        for row in state.browser.model.selected_rows() {
            if let Some(path) = &row.item.path {
                let _ = rlive_browser::backup::to_trash(path);
            }
        }
        if let Some(db) = &state.browser.db {
            db.rescan();
        }
        ui.close();
    }
}

fn content_pane(
    ui: &mut egui::Ui,
    pane: Rect,
    lib: &Arc<LibraryData>,
    session: &mut Session,
    state: &mut UiState,
) {
    let head = Rect::from_min_size(pane.min, Vec2::new(pane.width(), HEADER_H));
    let columns = headers(ui, head, lib, state);
    let list = Rect::from_min_max(Pos2::new(pane.left(), head.bottom()), pane.max);
    let mut lu = ui.new_child(UiBuilder::new().max_rect(list));
    lu.set_clip_rect(list.intersect(ui.clip_rect()));
    let rows = state.browser.model.rows.clone();
    let total = rows.rows.len();
    let scroll_to = std::mem::take(&mut state.browser.scroll_to_cursor)
        .then(|| {
            state
                .browser
                .model
                .cursor
                .as_ref()
                .and_then(|k| rows.rows.iter().position(|r| r.item.key == *k))
        })
        .flatten();
    let mut area = egui::ScrollArea::vertical()
        .id_salt(("browser_items", state.browser.model.nav.scope.clone()))
        .auto_shrink([false, false]);
    if let Some(i) = scroll_to {
        // Keep the row in view whether it is above or below what's showing.
        let y = i as f32 * ROW_H - list.height() * 0.4;
        area = area.vertical_scroll_offset(y.max(0.0));
    }
    area.show_rows(&mut lu, ROW_H, total, |ui, range| {
        ui.spacing_mut().item_spacing.y = 0.0;
        // Cmd+Alt and a drag scrolls (manual p. 117).
        let (held, delta) = ui.input(|i| {
            (
                i.modifiers.command && i.modifiers.alt && i.pointer.primary_down(),
                i.pointer.delta(),
            )
        });
        if held && ui.rect_contains_pointer(list) {
            ui.scroll_with_delta(Vec2::new(0.0, delta.y));
        }
        for i in range {
            let row = &rows.rows[i];
            let (rect, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::hover());
            let spots: Vec<(Column, Rect)> = columns
                .iter()
                .map(|(c, r)| {
                    (
                        *c,
                        Rect::from_min_max(
                            Pos2::new(r.left(), rect.top()),
                            Pos2::new(r.right(), rect.bottom()),
                        ),
                    )
                })
                .collect();
            draw_row(ui, rect, row, &spots, lib, session, state);
        }
    });
    if total == 0 {
        let text = if state.browser.db.is_none() {
            "The browser isn't running"
        } else if state.browser.model.nav.text.trim().is_empty() {
            "Nothing here"
        } else {
            "No results"
        };
        ui.painter().text(
            list.center_top() + Vec2::new(0.0, 22.0),
            Align2::CENTER_CENTER,
            text,
            theme::font(11.0),
            pal().text_dim,
        );
        // Plug-ins aren't looked for until asked (manual p. 480).
        if matches!(
            state.browser.model.nav.scope,
            Scope::Label(rlive_browser::item::Label::Plugins)
        ) && session.plugins.plugins.is_empty()
        {
            let r = Rect::from_center_size(
                list.center_top() + Vec2::new(0.0, 48.0),
                Vec2::new(80.0, 20.0),
            );
            if ui.put(r, egui::Button::new("Activate")).clicked() {
                state.request_scan = true;
            }
        }
    }
    if matches!(state.browser.model.nav.scope, Scope::Place(_)) {
        receive_drop_into_place(ui, list, session, state);
    }
}

/// Dragging a device to a folder in Places saves a preset there (manual p. 463).
fn receive_drop_into_place(
    ui: &mut egui::Ui,
    list: Rect,
    session: &mut Session,
    state: &mut UiState,
) {
    let resp = ui.interact(
        list,
        ui.scope_id().with("browser_place_drop"),
        Sense::hover(),
    );
    if let Some(drag) = crate::ui::widgets::released::<crate::ui::devices::DeviceDrag>(&resp) {
        let Scope::Place(folder) = state.browser.model.nav.scope.clone() else {
            return;
        };
        let name = session
            .devices(drag.chain)
            .iter()
            .find(|d| d.id == drag.device)
            .map(|d| d.name.clone())
            .unwrap_or_default();
        let path = rlive_browser::preset::unused_path(&folder, &name);
        match session.save_device_preset_as(drag.chain, drag.device, &path) {
            Ok(()) => {
                if let Some(db) = &state.browser.db {
                    db.rescan();
                }
                state.status = format!("Saved {}", path.display());
            }
            Err(e) => state.status = e,
        }
    }
}
